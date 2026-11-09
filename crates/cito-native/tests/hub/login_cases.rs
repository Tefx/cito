use super::*;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("cito-login-{}", crate::plugins::random_uuid_v4()));
        fs::create_dir_all(path.join("home")).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct ProviderState {
    refresh_requests: usize,
    authorization_requests: usize,
    code_requests: usize,
    refresh_status: u16,
    subject: String,
    omit_refresh: bool,
    bad_signature: bool,
    bad_nonce: bool,
    scopes_supported: Option<Vec<String>>,
    requested_scopes: String,
    requested_prompt: Option<String>,
    challenge: String,
    redirect: String,
    nonce: String,
    stall: String,
}
struct Provider {
    issuer: String,
    state: Arc<Mutex<ProviderState>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
fn sign(issuer: &str, sub: &str, nonce: &str, bad_signature: bool) -> String {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("login-key".into());
    let claims = json!({"iss":issuer,"aud":"client","sub":sub,"preferred_username":"Alice","exp":crate::plugins::now_ms()/1000+600,"nonce":nonce});
    let key = EncodingKey::from_ec_pem(include_bytes!("fixtures/ec-private.pem")).unwrap();
    let token = encode(&header, &claims, &key).unwrap();
    if !bad_signature {
        return token;
    }
    let mut parts: Vec<_> = token.split('.').map(str::to_owned).collect();
    let mut signature = URL_SAFE_NO_PAD.decode(&parts[2]).unwrap();
    signature[0] ^= 1;
    parts[2] = URL_SAFE_NO_PAD.encode(signature);
    parts.join(".")
}
fn reply(stream: &mut TcpStream, status: u16, body: &str, location: Option<&str>) {
    let _ = write!(stream, "HTTP/1.1 {status} Fixture\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}\r\n{body}", body.len(), location.map(|url|format!("Location: {url}\r\n")).unwrap_or_default());
}
impl Provider {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let issuer = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let state = Arc::new(Mutex::new(ProviderState {
            refresh_status: 200,
            subject: "subject".into(),
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (server_issuer, server_state, server_stop) =
            (issuer.clone(), state.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut requests = Vec::new();
            while !server_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let (issuer, state, stop) = (
                            server_issuer.clone(),
                            server_state.clone(),
                            server_stop.clone(),
                        );
                        requests.push(std::thread::spawn(move || {
                            stream.set_nonblocking(false).unwrap();
                            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                            let mut reader = BufReader::new(stream.try_clone().unwrap());
                            let mut line = String::new();
                            if reader.read_line(&mut line).is_err() { return; }
                            let target = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                            let mut length = 0;
                            loop {
                                line.clear();
                                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" { break; }
                                if let Some((key, value)) = line.split_once(':') {
                                    if key.eq_ignore_ascii_case("content-length") { length = value.trim().parse::<usize>().unwrap(); }
                                }
                            }
                            let mut body = vec![0;length];
                            if reader.read_exact(&mut body).is_err() { return; }
                            let url = url::Url::parse(&format!("{issuer}{target}")).unwrap();
                            if state.lock().unwrap().stall == url.path() {
                                while !stop.load(Ordering::SeqCst) { std::thread::sleep(Duration::from_millis(5)); }
                                return;
                            }
                            if state.lock().unwrap().stall == format!("body{}", url.path()) {
                                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{");
                                while !stop.load(Ordering::SeqCst) { std::thread::sleep(Duration::from_millis(5)); }
                                return;
                            }
                            match url.path() {
                                "/.well-known/openid-configuration" => {
                                    let supported = state.lock().unwrap().scopes_supported.clone();
                                    let mut discovery = json!({"issuer":issuer,"authorization_endpoint":format!("{issuer}/authorize"),"token_endpoint":format!("{issuer}/token"),"jwks_uri":format!("{issuer}/jwks")});
                                    if let Some(scopes) = supported { discovery["scopes_supported"] = json!(scopes); }
                                    reply(&mut stream,200,&discovery.to_string(),None);
                                }
                                "/jwks" => {
                                    let der = include_bytes!("fixtures/ec-public.der");
                                    let point = &der[der.len()-65..];
                                    let keys = json!({"keys":[{"kty":"EC","crv":"P-256","alg":"ES256","use":"sig","kid":"login-key","x":URL_SAFE_NO_PAD.encode(&point[1..33]),"y":URL_SAFE_NO_PAD.encode(&point[33..])}]});
                                    reply(&mut stream,200,&keys.to_string(),None);
                                }
                                "/authorize" => {
                                    let params: HashMap<_,_> = url.query_pairs().into_owned().collect();
                                    let mut state = state.lock().unwrap();
                                    state.authorization_requests += 1;
                                    state.requested_scopes = params["scope"].clone();
                                    state.requested_prompt = params.get("prompt").cloned();
                                    state.challenge = params["code_challenge"].clone();
                                    state.redirect = params["redirect_uri"].clone();
                                    state.nonce = params["nonce"].clone();
                                    assert_eq!(params["code_challenge_method"], "S256");
                                    let mut callback = url::Url::parse(&state.redirect).unwrap();
                                    callback.query_pairs_mut().append_pair("state", &params["state"]).append_pair("code", "authorization-code");
                                    reply(&mut stream,302,"",Some(callback.as_str()));
                                }
                                "/token" => {
                                    let params: HashMap<_,_> = url::form_urlencoded::parse(&body).into_owned().collect();
                                    let mut state = state.lock().unwrap();
                                    if params["grant_type"] == "refresh_token" {
                                        state.refresh_requests += 1;
                                        if state.refresh_status != 200 { reply(&mut stream,state.refresh_status,"{}",None);return; }
                                    } else {
                                        state.code_requests += 1;
                                        assert_eq!(params["code"], "authorization-code");
                                        assert_eq!(params["redirect_uri"], state.redirect);
                                        assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(params["code_verifier"].as_bytes())), state.challenge);
                                    }
                                    let nonce = if state.bad_nonce { "incorrect" } else { &state.nonce };
                                    let mut body = json!({"id_token":sign(&issuer,&state.subject,nonce,state.bad_signature)});
                                    if !state.omit_refresh { body["refresh_token"] = json!("refresh-new"); }
                                    reply(&mut stream,200,&body.to_string(),None);
                                }
                                _ => reply(&mut stream,404,"{}",None),
                            }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("provider: {error}"),
                }
            }
            for request in requests {
                request.join().unwrap();
            }
        });
        Self {
            issuer,
            state,
            stop,
            thread: Some(thread),
        }
    }
    fn config(&self) -> Value {
        json!({"issuer":self.issuer,"clientId":"client"})
    }
    fn seed(&self, dir: &Path, expires: u64) -> String {
        let token = sign(&self.issuer, "subject", "", false);
        let cache = json!({"issuer":self.issuer,"client_id":"client","id_token":token,"refresh_token":"refresh-old","token_sha256":hash(&token),"expires_at":expires,"verified_principal_id":format!("{}#subject",self.issuer),"verified_user":"alice"});
        let deadline = Deadline {
            at: Instant::now() + LOGIN_TIMEOUT,
            cancelled: &|| false,
        };
        publish(&cache_path(dir, "work").unwrap(), &cache, &deadline).unwrap();
        token
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn read(dir: &Path) -> Value {
    serde_json::from_slice(&fs::read(cache_path(dir, "work").unwrap()).unwrap()).unwrap()
}
fn worker(dir: &Path, config: &Value, rejected: Option<&str>, timeout: u64) -> Child {
    fs::write(dir.join("config.json"), config.to_string()).unwrap();
    let opener = dir.join("browser.py");
    fs::write(&opener,"#!/usr/bin/env python3\n# purpose: Follow the fixture's real HTTP PKCE callback.\n# usage: browser.py AUTHORIZATION_URL\n# effects: Loopback HTTP only.\n# requires: python3 standard library.\nimport os, sys, time, urllib.request\nwith open(__file__+'.pid','w') as record: record.write(str(os.getpid()))\nif os.environ.get('CITO_LOGIN_TEST_BROWSER_WAIT') == '1': time.sleep(60)\nwith urllib.request.urlopen(sys.argv[-1], timeout=5) as response: response.read()\n").unwrap();
    fs::set_permissions(&opener, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "core::login::tests::login_worker",
            "--ignored",
            "--nocapture",
        ])
        .env("HOME", dir.join("home"))
        .env("CITO_STATE_DIR", dir)
        .env("CITO_CONFIG_PATH", dir.join("config.json"))
        .env("CITO_BROWSER_BIN", opener)
        .env(
            "CITO_LOGIN_TEST_BROWSER_WAIT",
            if config["test_browser_wait"] == true {
                "1"
            } else {
                "0"
            },
        )
        .env("CITO_LOGIN_TEST_DIR", dir)
        .env("CITO_LOGIN_TEST_TIMEOUT_MS", timeout.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(rejected) = rejected {
        command.env("CITO_LOGIN_TEST_REJECTED", rejected);
    } else {
        command.env_remove("CITO_LOGIN_TEST_REJECTED");
    }
    command.spawn().unwrap()
}
fn result(dir: &Path, process: Child) -> Value {
    let Output {
        status,
        stdout,
        stderr,
    } = process.wait_with_output().unwrap();
    assert!(
        status.success(),
        "worker failed: {} {}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    serde_json::from_slice(&fs::read(dir.join("result.json")).unwrap()).unwrap()
}
#[test]
#[ignore = "isolated login process invoked by native OIDC tests"]
fn login_worker() {
    let dir = PathBuf::from(std::env::var("CITO_LOGIN_TEST_DIR").unwrap());
    let config: Value =
        serde_json::from_slice(&fs::read(dir.join("config.json")).unwrap()).unwrap();
    let settings = Settings::new(&config).unwrap();
    let timeout = std::env::var("CITO_LOGIN_TEST_TIMEOUT_MS")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let cancelled = || dir.join("cancel").exists();
    let deadline = Deadline {
        at: Instant::now() + Duration::from_millis(timeout),
        cancelled: &cancelled,
    };
    let rejected = std::env::var("CITO_LOGIN_TEST_REJECTED").ok();
    let current = read_cache(&cache_path(&dir, "work").unwrap(), &settings).unwrap();
    let hash = if config["reuse_fresh"] == true {
        None
    } else {
        rejected.as_deref().or_else(|| {
            current
                .as_ref()
                .and_then(|value| value["token_sha256"].as_str())
        })
    };
    let outcome = recover_inner(
        &dir,
        "work",
        &settings,
        hash,
        true,
        config["force_account_select"] == true,
        &deadline,
    );
    let value = match outcome {
        Ok((token, identity)) => json!({"token":token,"identity":identity}),
        Err(error) => json!({"error":error}),
    };
    fs::write(dir.join("result.json"), value.to_string()).unwrap();
}

#[test]
fn switch_account_bypasses_fresh_cache_and_refresh_and_requests_account_picker() {
    for expires in [0, crate::plugins::now_ms() / 1000 + 600] {
        let dir = Temp::new();
        let provider = Provider::new();
        provider.seed(&dir.0, expires);
        let mut config = provider.config();
        config["force_account_select"] = json!(true);
        config["reuse_fresh"] = json!(true);
        let value = result(&dir.0, worker(&dir.0, &config, None, 3000));
        assert!(value.get("error").is_none(), "{value}");
        let state = provider.state.lock().unwrap();
        assert_eq!(state.refresh_requests, 0);
        assert_eq!(state.authorization_requests, 1);
        assert_eq!(state.requested_prompt.as_deref(), Some("select_account"));
    }
}
#[test]
fn real_pkce_scopes_signature_nonce_and_private_publication() {
    for (scopes, expected) in [
        (None, "openid email"),
        (Some(vec!["openid", "email"]), "openid email"),
        (Some(vec!["openid", "profile"]), "openid profile"),
    ] {
        let dir = Temp::new();
        let provider = Provider::new();
        let mut config = provider.config();
        provider.state.lock().unwrap().scopes_supported =
            Some(vec!["openid".into(), "email".into()]);
        if let Some(scopes) = scopes {
            config["scopes"] = json!(scopes);
        }
        let value = result(&dir.0, worker(&dir.0, &config, None, 3000));
        assert!(value.get("error").is_none(), "{value}");
        let state = provider.state.lock().unwrap();
        assert_eq!(state.requested_scopes, expected);
        assert_eq!(state.code_requests, 1);
        assert_eq!(state.challenge.len(), 43);
        drop(state);
        let cache = read(&dir.0);
        assert_eq!(cache["verified_user"], "alice");
        assert_eq!(
            cache["verified_principal_id"],
            format!("oidc:{}|subject", provider.issuer)
        );
        assert_eq!(
            fs::metadata(cache_path(&dir.0, "work").unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            offline_identity(&dir.0, "work", &config).unwrap()["principal_id"],
            cache["verified_principal_id"]
        );
    }
}
#[test]
fn refresh_identity_and_rejected_bearer_generation() {
    for changed in [false, true] {
        let dir = Temp::new();
        let provider = Provider::new();
        let token = provider.seed(&dir.0, crate::plugins::now_ms() / 1000 + 600);
        {
            let mut state = provider.state.lock().unwrap();
            state.omit_refresh = true;
            if changed {
                state.subject = "changed".into();
            }
        }
        let rejected = hash(&token);
        let value = recover(&dir.0, "work", &provider.config(), Some(&rejected), false).unwrap();
        assert_ne!(value.0, token);
        assert_eq!(
            read(&dir.0)["refresh_token"],
            if changed {
                Value::Null
            } else {
                json!("refresh-old")
            }
        );
        assert_eq!(provider.state.lock().unwrap().refresh_requests, 1);
        assert_eq!(
            recover(&dir.0, "work", &provider.config(), Some(&rejected), false)
                .unwrap()
                .0,
            value.0
        );
        assert_eq!(provider.state.lock().unwrap().refresh_requests, 1);
    }
}
#[test]
fn concurrent_expired_cache_waiters_refresh_once() {
    let dir = Temp::new();
    let provider = Provider::new();
    provider.seed(&dir.0, 0);
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let workers: Vec<_> = (0..3)
        .map(|_| {
            let path = dir.0.clone();
            let config = provider.config();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                recover(&path, "work", &config, None, false).unwrap().0
            })
        })
        .collect();
    barrier.wait();
    let tokens: Vec<_> = workers
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(tokens.iter().all(|token| *token == tokens[0]));
    assert_eq!(provider.state.lock().unwrap().refresh_requests, 1);
}
#[test]
fn concurrent_unexpired_rejection_waiters_reuse_one_verified_winner() {
    let dir = Temp::new();
    let provider = Provider::new();
    let token = provider.seed(&dir.0, crate::plugins::now_ms() / 1000 + 600);
    let rejected = hash(&token);
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let workers: Vec<_> = (0..3)
        .map(|_| {
            let path = dir.0.clone();
            let config = provider.config();
            let barrier = barrier.clone();
            let rejected = rejected.clone();
            std::thread::spawn(move || {
                barrier.wait();
                recover(&path, "work", &config, Some(&rejected), false).unwrap()
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = workers
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(results.iter().all(|result| result == &results[0]));
    assert_ne!(results[0].0, token);
    let state = provider.state.lock().unwrap();
    assert_eq!(state.refresh_requests, 1);
    assert_eq!(state.authorization_requests, 0);
}
#[test]
fn refresh_rejection_uses_pkce_without_inheriting_old_refresh() {
    for status in [400, 401] {
        let dir = Temp::new();
        let provider = Provider::new();
        provider.seed(&dir.0, 0);
        {
            let mut state = provider.state.lock().unwrap();
            state.refresh_status = status;
            state.omit_refresh = true;
            state.subject = "changed".into();
        }
        let value = result(&dir.0, worker(&dir.0, &provider.config(), None, 3000));
        assert!(value.get("error").is_none(), "{value}");
        assert!(read(&dir.0)["refresh_token"].is_null());
        let state = provider.state.lock().unwrap();
        assert_eq!(state.authorization_requests, 1);
        assert_eq!(state.refresh_requests, 1);
    }
}
#[test]
fn refresh_unavailable_preserves_previous_verified_cache() {
    let dir = Temp::new();
    let provider = Provider::new();
    provider.seed(&dir.0, 0);
    let before = fs::read(cache_path(&dir.0, "work").unwrap()).unwrap();
    provider.state.lock().unwrap().refresh_status = 503;
    assert!(recover(&dir.0, "work", &provider.config(), None, false)
        .unwrap_err()
        .contains("AUTH_UNAVAILABLE"));
    assert_eq!(
        fs::read(cache_path(&dir.0, "work").unwrap()).unwrap(),
        before
    );
    assert_eq!(provider.state.lock().unwrap().authorization_requests, 0);
}
#[test]
fn invalid_signature_nonce_and_missing_openid_never_publish() {
    for failure in ["signature", "nonce", "openid"] {
        let dir = Temp::new();
        let provider = Provider::new();
        {
            let mut state = provider.state.lock().unwrap();
            state.bad_signature = failure == "signature";
            state.bad_nonce = failure == "nonce";
            if failure == "openid" {
                state.scopes_supported = Some(vec!["profile".into()]);
            }
        }
        let value = result(&dir.0, worker(&dir.0, &provider.config(), None, 3000));
        assert!(value.get("error").is_some(), "{failure}: {value}");
        assert!(!cache_path(&dir.0, "work").unwrap().exists());
        if failure == "openid" {
            assert_eq!(provider.state.lock().unwrap().authorization_requests, 0);
        }
    }
}
#[test]
fn scopes_and_cache_identity_validation_are_hub_local() {
    let dir = Temp::new();
    let provider = Provider::new();
    let config = provider.config();
    provider.seed(&dir.0, 0);
    fs::write(dir.0.join("oidc-token.json"), "legacy").unwrap();
    assert!(offline_identity(&dir.0, "work", &config).is_some());
    fs::set_permissions(
        cache_path(&dir.0, "work").unwrap(),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(offline_identity(&dir.0, "work", &config).is_none());
    fs::set_permissions(
        cache_path(&dir.0, "work").unwrap(),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(offline_identity(&dir.0, "other", &config).is_none());
    assert!(offline_identity(
        &dir.0,
        "work",
        &json!({"issuer":provider.issuer,"clientId":"other"})
    )
    .is_none());
    let mut cache = read(&dir.0);
    cache["id_token"] = json!("changed");
    fs::write(cache_path(&dir.0, "work").unwrap(), cache.to_string()).unwrap();
    assert!(offline_identity(&dir.0, "work", &config).is_none());
    assert_eq!(
        fs::read_to_string(dir.0.join("oidc-token.json")).unwrap(),
        "legacy"
    );
    for scopes in [json!([]), json!(["email"]), json!(["openid", "bad scope"])] {
        let mut config = config.clone();
        config["scopes"] = scopes;
        assert!(Settings::new(&config).is_err());
    }
    let settings = Settings::new(&config).unwrap();
    assert_eq!(
        settings.scopes(&json!({})).unwrap(),
        "openid profile email offline_access"
    );
}
#[test]
fn overall_deadline_includes_discovery_token_and_jwks() {
    for stall in [
        "/.well-known/openid-configuration",
        "/token",
        "/jwks",
        "body/.well-known/openid-configuration",
        "body/token",
        "body/jwks",
    ] {
        let dir = Temp::new();
        let provider = Provider::new();
        provider.seed(&dir.0, 0);
        provider.state.lock().unwrap().stall = stall.into();
        let before = fs::read(cache_path(&dir.0, "work").unwrap()).unwrap();
        let began = Instant::now();
        let value = result(&dir.0, worker(&dir.0, &provider.config(), None, 200));
        assert!(
            value["error"]
                .as_str()
                .unwrap()
                .contains("OIDC_LOGIN_TIMEOUT"),
            "{stall}: {value}"
        );
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "{stall}: {:?}",
            began.elapsed()
        );
        assert_eq!(
            fs::read(cache_path(&dir.0, "work").unwrap()).unwrap(),
            before
        );
    }
}
#[test]
fn cache_lock_deadline_and_cancellation_do_not_open_a_browser() {
    let dir = Temp::new();
    let provider = Provider::new();
    let deadline = Deadline {
        at: Instant::now() + LOGIN_TIMEOUT,
        cancelled: &|| false,
    };
    let lock = FileLock::acquire(
        &cache_path(&dir.0, "work")
            .unwrap()
            .with_extension("json.lock"),
        &deadline,
    )
    .unwrap();
    let value = result(&dir.0, worker(&dir.0, &provider.config(), None, 100));
    assert!(value["error"]
        .as_str()
        .unwrap()
        .contains("OIDC_LOGIN_TIMEOUT"));
    let process = worker(&dir.0, &provider.config(), None, 3000);
    fs::write(dir.0.join("cancel"), "").unwrap();
    let value = result(&dir.0, process);
    assert!(value["error"].as_str().unwrap().contains("cancelled"));
    assert_eq!(provider.state.lock().unwrap().authorization_requests, 0);
    drop(lock);
}
#[test]
fn queued_fixed_port_cancellation_opens_no_browser() {
    let dir = Temp::new();
    let provider = Provider::new();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let deadline = Deadline {
        at: Instant::now() + LOGIN_TIMEOUT,
        cancelled: &|| false,
    };
    // SAFETY: geteuid has no preconditions and does not access Rust memory.
    let uid = unsafe { libc::geteuid() };
    let lock_path = PathBuf::from(format!("/tmp/cito-oidc-callback-{uid}-{port}.lock"));
    let _lock = FileLock::acquire(&lock_path, &deadline).unwrap();
    let mut config = provider.config();
    config["redirectPort"] = json!(port);
    let process = worker(&dir.0, &config, None, 3000);
    // Cancel while the callback-port owner still holds the lock.
    std::thread::sleep(Duration::from_millis(100));
    fs::write(dir.0.join("cancel"), "").unwrap();
    let value = result(&dir.0, process);
    assert!(value["error"].as_str().unwrap().contains("cancelled"));
    assert_eq!(provider.state.lock().unwrap().authorization_requests, 0);
    assert!(!dir.0.join("browser.py.pid").exists());
    assert!(!cache_path(&dir.0, "work").unwrap().exists());
}
#[test]
fn cancellation_kills_and_reaps_the_owned_opener() {
    let dir = Temp::new();
    let provider = Provider::new();
    let mut config = provider.config();
    config["test_browser_wait"] = json!(true);
    let process = worker(&dir.0, &config, None, 3000);
    let began = Instant::now();
    let pid_path = dir.0.join("browser.py.pid");
    let pid: libc::pid_t = loop {
        if let Some(pid) = fs::read_to_string(&pid_path)
            .ok()
            .and_then(|pid| pid.parse().ok())
        {
            break pid;
        }
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "browser did not start"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    fs::write(dir.0.join("cancel"), "").unwrap();
    let value = result(&dir.0, process);
    assert!(value["error"].as_str().unwrap().contains("cancelled"));
    // SAFETY: signal zero only inspects the known test-owned opener PID.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert!(!cache_path(&dir.0, "work").unwrap().exists());
}
#[test]
fn fixed_port_serializes_independent_state_directories() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let a = Temp::new();
    let b = Temp::new();
    let first = Provider::new();
    let second = Provider::new();
    let mut one = first.config();
    one["redirectPort"] = json!(port);
    let mut two = second.config();
    two["redirectPort"] = json!(port);
    let one_process = worker(&a.0, &one, None, 3000);
    let two_process = worker(&b.0, &two, None, 3000);
    let one = result(&a.0, one_process);
    let two = result(&b.0, two_process);
    assert!(one.get("error").is_none(), "{one}");
    assert!(two.get("error").is_none(), "{two}");
    assert_eq!(first.state.lock().unwrap().authorization_requests, 1);
    assert_eq!(second.state.lock().unwrap().authorization_requests, 1);
}
