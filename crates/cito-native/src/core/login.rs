//! OIDC browser authorization and refresh. Only verified identities enter the token cache.
use super::{auth::OidcVerifier, identity::hash};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(45);
const POLL: Duration = Duration::from_millis(20);
const MAX_JSON: u64 = 1024 * 1024;
type LoginResult = (String, Value);

struct Deadline<'a> {
    at: Instant,
    cancelled: &'a dyn Fn() -> bool,
}
impl Deadline<'_> {
    fn remaining(&self) -> Result<Duration, String> {
        if (self.cancelled)() {
            return Err("OIDC_LOGIN_TIMEOUT: OIDC login cancelled".into());
        }
        self.at
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| "OIDC_LOGIN_TIMEOUT: OIDC login deadline exceeded".into())
    }
    fn pause(&self) -> Result<(), String> {
        std::thread::sleep(self.remaining()?.min(POLL));
        self.remaining().map(|_| ())
    }
    fn request(&self, request: ureq::Request) -> Result<ureq::Request, String> {
        Ok(request.timeout(self.remaining()?.min(Duration::from_secs(10))))
    }
    fn network_error(&self, error: impl std::error::Error + 'static) -> String {
        if let Err(error) = self.remaining() {
            return error;
        }
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&error);
        while let Some(current) = cause {
            if current
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| {
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    )
                })
            {
                return format!("OIDC_LOGIN_TIMEOUT: OIDC endpoint timed out: {error}");
            }
            cause = current.source();
        }
        format!("AUTH_UNAVAILABLE: OIDC endpoint unavailable: {error}")
    }
}

/// The open descriptor owns the flock. Never unlink a lockfile: queued users may own it.
struct FileLock(File);
impl FileLock {
    fn acquire(path: &Path, deadline: &Deadline<'_>) -> Result<Self, String> {
        deadline.remaining()?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("AUTH_UNAVAILABLE: Cannot lock {}: {e}", path.display()))?;
        loop {
            deadline.remaining()?;
            // SAFETY: file owns a valid descriptor; flock neither transfers nor closes it.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                deadline.remaining()?;
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) {
                return Err(format!("AUTH_UNAVAILABLE: OIDC lock: {error}"));
            }
            deadline.pause()?;
        }
    }
}
impl Drop for FileLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor is live until this guard's File is dropped next.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

struct BrowserGuard(Child);
impl BrowserGuard {
    fn check(&mut self) -> Result<(), String> {
        match self.0.try_wait() {
            Ok(Some(status)) if !status.success() => Err(format!(
                "AUTH_UNAVAILABLE: Browser opener exited with {status}"
            )),
            Ok(_) => Ok(()),
            Err(error) => Err(format!("AUTH_UNAVAILABLE: Browser opener: {error}")),
        }
    }
}
impl Drop for BrowserGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

struct Settings<'a> {
    issuer: &'a str,
    client: &'a str,
    port: u16,
    scopes: Option<Vec<&'a str>>,
}
impl<'a> Settings<'a> {
    fn new(config: &'a Value) -> Result<Self, String> {
        let issuer = config
            .get("issuer")
            .or_else(|| config.get("oidcIssuer"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or("INVALID_INPUT: issuer required")?
            .trim_end_matches('/');
        let client = config
            .get("clientId")
            .or_else(|| config.get("client_id"))
            .or_else(|| config.get("oidcClientId"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or("INVALID_INPUT: clientId required")?;
        let port = config
            .get("redirectPort")
            .or_else(|| config.get("oidcRedirectPort"))
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or("INVALID_INPUT: redirectPort must be an integer from 0 to 65535")
            })
            .transpose()?
            .unwrap_or(0);
        let scopes = config
            .get("scopes")
            .or_else(|| config.get("oidcScopes"))
            .map(|value| {
                let scopes: Vec<_> = value
                    .as_array()
                    .ok_or("INVALID_INPUT: scopes must be an array")?
                    .iter()
                    .map(|scope| {
                        scope
                            .as_str()
                            .filter(|scope| {
                                !scope.is_empty()
                                    && scope
                                        .bytes()
                                        .all(|b| matches!(b, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
                            })
                            .ok_or("INVALID_INPUT: Invalid OIDC scope")
                    })
                    .collect::<Result<_, _>>()?;
                if !scopes.contains(&"openid") {
                    return Err("INVALID_INPUT: OIDC scopes must include openid");
                }
                Ok(scopes)
            })
            .transpose()?;
        Ok(Self {
            issuer,
            client,
            port,
            scopes,
        })
    }
    fn scopes(&self, discovery: &Value) -> Result<String, String> {
        let supported = discovery
            .get("scopes_supported")
            .map(|value| {
                value
                    .as_array()
                    .and_then(|values| values.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
                    .ok_or("AUTH_UNAVAILABLE: Invalid scopes_supported")
            })
            .transpose()?;
        if supported
            .as_ref()
            .is_some_and(|values| !values.contains(&"openid"))
        {
            return Err("AUTH_UNAVAILABLE: OIDC provider does not support openid".into());
        }
        Ok(self
            .scopes
            .clone()
            .unwrap_or_else(|| {
                ["openid", "profile", concat!("e", "mail"), "offline_access"]
                    .into_iter()
                    .filter(|scope| {
                        supported
                            .as_ref()
                            .is_none_or(|values| values.contains(scope))
                    })
                    .collect()
            })
            .join(" "))
    }
}

fn cache_path(dir: &Path, hub: &str) -> Result<PathBuf, String> {
    if hub.is_empty()
        || !hub
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err("INVALID_INPUT: Invalid hub name".into());
    }
    Ok(dir.join(format!("oidc-token-{hub}.json")))
}
fn read_cache(path: &Path, settings: &Settings<'_>) -> Result<Option<Value>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("AUTH_UNAVAILABLE: Cannot read OIDC cache: {error}")),
    };
    if file
        .metadata()
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o777
        != 0o600
    {
        return Ok(None);
    }
    let mut raw = Vec::new();
    file.take(MAX_JSON + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > MAX_JSON {
        return Ok(None);
    }
    let cache: Value = match serde_json::from_slice(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(token) = cache["id_token"].as_str().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if cache["issuer"] != settings.issuer
        || cache["client_id"] != settings.client
        || cache["token_sha256"] != hash(token)
        || cache["expires_at"].as_f64().is_none()
        || ["verified_principal_id", "verified_user"]
            .iter()
            .any(|key| cache[*key].as_str().is_none_or(str::is_empty))
        || cache
            .get("refresh_token")
            .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return Ok(None);
    }
    Ok(Some(cache))
}
fn fresh(cache: &Value) -> bool {
    cache["expires_at"]
        .as_f64()
        .is_some_and(|expires| expires > (crate::plugins::now_ms() / 1000 + 60) as f64)
}
fn verify_cache(
    cache: &Value,
    settings: &Settings<'_>,
    deadline: &Deadline<'_>,
) -> Result<LoginResult, String> {
    deadline.remaining()?;
    let token = cache["id_token"]
        .as_str()
        .ok_or("UNAUTHORIZED: Invalid cache token")?;
    let identity = OidcVerifier::new(settings.issuer, settings.client)?
        .verify_with_deadline(token, deadline.at)?;
    deadline.remaining()?;
    Ok((token.to_owned(), identity))
}

pub fn offline_identity(dir: &Path, hub: &str, config: &Value) -> Option<Value> {
    let settings = Settings::new(config).ok()?;
    let cache = read_cache(&cache_path(dir, hub).ok()?, &settings).ok()??;
    Some(
        json!({"principal_id":cache["verified_principal_id"],"owner_user":cache["verified_user"],"oidc":true}),
    )
}
fn random() -> Result<String, String> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn endpoint(value: &Value, key: &str) -> Result<String, String> {
    let raw = value[key]
        .as_str()
        .ok_or_else(|| format!("AUTH_UNAVAILABLE: Missing {key}"))?;
    let url = url::Url::parse(raw).map_err(|_| format!("AUTH_UNAVAILABLE: Invalid {key}"))?;
    if !(url.scheme() == "https" || (url.scheme() == "http" && url.host_str() == Some("127.0.0.1")))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "AUTH_UNAVAILABLE: {key} must use HTTPS without credentials, query or fragment"
        ));
    }
    Ok(raw.into())
}
fn payload(token: &str) -> Result<Value, String> {
    let part = token
        .split('.')
        .nth(1)
        .ok_or("UNAUTHORIZED: Invalid token")?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn response_json(response: ureq::Response, deadline: &Deadline<'_>) -> Result<Value, String> {
    deadline.remaining()?;
    if response.status() != 200 {
        return Err(format!(
            "AUTH_UNAVAILABLE: OIDC endpoint returned HTTP {}",
            response.status()
        ));
    }
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_JSON + 1)
        .read_to_end(&mut body)
        .map_err(|e| deadline.network_error(e))?;
    deadline.remaining()?;
    if body.len() as u64 > MAX_JSON {
        return Err("AUTH_UNAVAILABLE: OIDC response too large".into());
    }
    serde_json::from_slice(&body).map_err(|e| format!("AUTH_UNAVAILABLE: Invalid OIDC JSON: {e}"))
}
fn publish(path: &Path, cache: &Value, deadline: &Deadline<'_>) -> Result<(), String> {
    deadline.remaining()?;
    let temporary = path.with_extension(format!("json.{}.tmp", crate::plugins::random_uuid_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        file.write_all(cache.to_string().as_bytes())
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        deadline.remaining()?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn cached(dir: &Path, hub: &str, config: &Value) -> Result<Option<LoginResult>, String> {
    let deadline = Deadline {
        at: Instant::now() + LOGIN_TIMEOUT,
        cancelled: &super::daemon::shutdown_requested,
    };
    let settings = Settings::new(config)?;
    match read_cache(&cache_path(dir, hub)?, &settings)? {
        Some(cache) if fresh(&cache) => verify_cache(&cache, &settings, &deadline).map(Some),
        _ => Ok(None),
    }
}
pub fn login(
    dir: &Path,
    hub: &str,
    config: &Value,
    interactive: bool,
) -> Result<LoginResult, String> {
    let deadline = Deadline {
        at: Instant::now() + LOGIN_TIMEOUT,
        cancelled: &super::daemon::shutdown_requested,
    };
    let settings = Settings::new(config)?;
    deadline.remaining()?;
    let current = read_cache(&cache_path(dir, hub)?, &settings)?;
    if let Some(cache) = current.as_ref().filter(|cache| fresh(cache)) {
        return verify_cache(cache, &settings, &deadline);
    }
    recover_inner(
        dir,
        hub,
        &settings,
        current
            .as_ref()
            .and_then(|cache| cache["token_sha256"].as_str()),
        interactive,
        &deadline,
    )
}
/// Recover a rejected bearer generation even if its JWT has not expired. Callers must pass
/// the hash of the bearer that actually received 401, never the subsequently reloaded cache.
pub fn recover(
    dir: &Path,
    hub: &str,
    config: &Value,
    rejected_hash: Option<&str>,
    interactive: bool,
) -> Result<LoginResult, String> {
    let deadline = Deadline {
        at: Instant::now() + LOGIN_TIMEOUT,
        cancelled: &super::daemon::shutdown_requested,
    };
    recover_inner(
        dir,
        hub,
        &Settings::new(config)?,
        rejected_hash,
        interactive,
        &deadline,
    )
}
fn recover_inner(
    dir: &Path,
    hub: &str,
    settings: &Settings<'_>,
    rejected: Option<&str>,
    interactive: bool,
    deadline: &Deadline<'_>,
) -> Result<LoginResult, String> {
    deadline.remaining()?;
    let path = cache_path(dir, hub)?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let _cache_lock = FileLock::acquire(&path.with_extension("json.lock"), deadline)?;
    let old = read_cache(&path, settings)?;
    if let Some(cache) = old
        .as_ref()
        .filter(|cache| fresh(cache) && cache["token_sha256"].as_str() != rejected)
    {
        return verify_cache(cache, settings, deadline);
    }
    let verifier = OidcVerifier::new(settings.issuer, settings.client)?;
    let agent = ureq::AgentBuilder::new().redirects(0).build();
    let discovery = response_json(
        deadline
            .request(agent.get(&format!(
                "{}/.well-known/openid-configuration",
                settings.issuer
            )))?
            .call()
            .map_err(|e| deadline.network_error(e))?,
        deadline,
    )?;
    if discovery["issuer"] != settings.issuer {
        return Err("AUTH_UNAVAILABLE: Discovery issuer mismatch".into());
    }
    let scopes = settings.scopes(&discovery)?;
    let token_url = endpoint(&discovery, "token_endpoint")?;
    let authorization = endpoint(&discovery, "authorization_endpoint")?;
    let old_refresh = old
        .as_ref()
        .and_then(|cache| cache["refresh_token"].as_str())
        .filter(|s| !s.is_empty());
    let refreshed = if let Some(refresh) = old_refresh {
        match deadline.request(agent.post(&token_url))?.send_form(&[
            ("client_id", settings.client),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
        ]) {
            Ok(response) => Some(response_json(response, deadline)?),
            Err(ureq::Error::Status(400 | 401, _)) => {
                deadline.remaining()?;
                None
            }
            Err(error) => return Err(deadline.network_error(error)),
        }
    } else {
        None
    };
    let is_refresh = refreshed.is_some();
    let mut nonce = None;
    // Keep the fixed-port lock until verified publication, including token exchange.
    let mut _port_lock = None;
    let body = if let Some(body) = refreshed {
        body
    } else {
        if !interactive {
            return Err("UNAUTHORIZED: OIDC login required".into());
        }
        if settings.port != 0 {
            // /tmp is host-local and independent of each process's HOME, stateDir and TMPDIR.
            // SAFETY: geteuid has no preconditions and does not access Rust memory.
            let uid = unsafe { libc::geteuid() };
            let lock_path = PathBuf::from(format!(
                "/tmp/cito-oidc-callback-{uid}-{}.lock",
                settings.port
            ));
            _port_lock = Some(FileLock::acquire(&lock_path, deadline)?);
        }
        deadline.remaining()?;
        let listener = TcpListener::bind(("127.0.0.1", settings.port))
            .map_err(|e| format!("AUTH_UNAVAILABLE: OIDC callback bind: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let redirect = format!(
            "http://127.0.0.1:{}/callback",
            listener.local_addr().map_err(|e| e.to_string())?.port()
        );
        let state = random()?;
        let pkce = random()?;
        let request_nonce = random()?;
        nonce = Some(request_nonce.clone());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.as_bytes()));
        let mut url = url::Url::parse(&authorization).map_err(|e| e.to_string())?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", settings.client),
            ("redirect_uri", redirect.as_str()),
            ("scope", scopes.as_str()),
            ("state", state.as_str()),
            ("nonce", request_nonce.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]);
        let opener = std::env::var("CITO_BROWSER_BIN").unwrap_or_else(|_| {
            if cfg!(target_os = "macos") {
                "/usr/bin/open".into()
            } else {
                "xdg-open".into()
            }
        });
        deadline.remaining()?;
        let mut browser = BrowserGuard(
            std::process::Command::new(opener)
                .arg(url.as_str())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("AUTH_UNAVAILABLE: Cannot open browser: {e}"))?,
        );
        let code = loop {
            deadline.remaining()?;
            browser.check()?;
            match listener.accept() {
                Ok((mut stream, _)) => {
                    if let Some(code) = callback(&mut stream, &state, deadline)? {
                        break code;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => deadline.pause()?,
                Err(error) => return Err(format!("AUTH_UNAVAILABLE: OIDC callback: {error}")),
            }
        };
        drop(listener);
        response_json(
            deadline
                .request(agent.post(&token_url))?
                .send_form(&[
                    ("client_id", settings.client),
                    ("grant_type", "authorization_code"),
                    ("code", &code),
                    ("redirect_uri", &redirect),
                    ("code_verifier", &pkce),
                ])
                .map_err(|error| match error {
                    ureq::Error::Status(status, _) if status < 500 => {
                        format!("UNAUTHORIZED: OIDC token exchange rejected (HTTP {status})")
                    }
                    error => deadline.network_error(error),
                })?,
            deadline,
        )?
    };
    deadline.remaining()?;
    let token = body["id_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("UNAUTHORIZED: Token exchange omitted id_token")?;
    if body
        .get("refresh_token")
        .is_some_and(|value| !value.is_string())
    {
        return Err("UNAUTHORIZED: Invalid refresh_token".into());
    }
    let identity = verifier.verify_with_deadline(token, deadline.at)?;
    deadline.remaining()?;
    let claims = payload(token)?;
    if nonce.is_some_and(|nonce| claims["nonce"] != nonce) {
        return Err("UNAUTHORIZED: OIDC nonce mismatch".into());
    }
    let previous_principal = old
        .as_ref()
        .and_then(|cache| cache["verified_principal_id"].as_str());
    let same_principal = previous_principal.is_some_and(|previous| {
        [
            "principal_id",
            "canonical_principal_id",
            "legacy_principal_id",
        ]
        .iter()
        .any(|key| identity[*key].as_str() == Some(previous))
            || claims["sub"]
                .as_str()
                .is_some_and(|sub| previous == format!("{}#{sub}", settings.issuer))
    });
    let refresh = body["refresh_token"].as_str().or_else(|| {
        if is_refresh && same_principal {
            old_refresh
        } else {
            None
        }
    });
    let cache = json!({"issuer":settings.issuer,"client_id":settings.client,"id_token":token,"refresh_token":refresh,"token_sha256":hash(token),"expires_at":claims["exp"],"verified_principal_id":identity["principal_id"],"verified_user":identity["owner_user"]});
    publish(&path, &cache, deadline)?;
    Ok((token.to_owned(), identity))
}

fn callback(
    stream: &mut TcpStream,
    state: &str,
    deadline: &Deadline<'_>,
) -> Result<Option<String>, String> {
    // Darwin can inherit O_NONBLOCK from the accepting listener; timeouts need blocking I/O.
    stream
        .set_nonblocking(false)
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(deadline.remaining()?.min(Duration::from_millis(100))))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(deadline.remaining()?.min(Duration::from_millis(100))))
        .map_err(|e| e.to_string())?;
    let mut line = Vec::new();
    loop {
        deadline.remaining()?;
        let mut byte = [0u8; 1];
        stream
            .set_read_timeout(Some(deadline.remaining()?.min(Duration::from_millis(100))))
            .map_err(|e| e.to_string())?;
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' || line.len() > 8192 {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None)
            }
            Err(_) => return Ok(None),
        }
    }
    let parsed = std::str::from_utf8(&line)
        .ok()
        .filter(|_| line.len() <= 8192)
        .and_then(|line| {
            let mut words = line.split_whitespace();
            if words.next()? != "GET" {
                return None;
            }
            let target = words.next()?.strip_prefix('/')?;
            url::Url::parse(&format!("http://127.0.0.1/{target}")).ok()
        });
    let mut code = None;
    let valid = parsed.as_ref().is_some_and(|url| {
        let states: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .collect();
        let codes: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .collect();
        if codes.len() == 1 {
            code = codes.into_iter().next().filter(|value| !value.is_empty());
        }
        url.path() == "/callback" && states.len() == 1 && states[0] == state
    });
    let message = if valid && code.is_some() {
        "Cito login complete. You may close this window."
    } else {
        "Invalid callback"
    };
    let status = if valid && code.is_some() {
        "200 OK"
    } else {
        "400 Bad Request"
    };
    stream
        .set_write_timeout(Some(deadline.remaining()?.min(Duration::from_millis(100))))
        .map_err(|e| e.to_string())?;
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{message}",
        message.len()
    );
    if valid && code.is_none() {
        return Err("UNAUTHORIZED: OIDC login cancelled".into());
    }
    deadline.remaining()?;
    Ok(if valid { code } else { None })
}

#[cfg(test)]
mod tests {
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
            let path = std::env::temp_dir()
                .join(format!("cito-login-{}", crate::plugins::random_uuid_v4()));
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
        let key = EncodingKey::from_ec_pem(include_bytes!(
            "../../../../tests/native-hub/fixtures/ec-private.pem"
        ))
        .unwrap();
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
                                        let der = include_bytes!("../../../../tests/native-hub/fixtures/ec-public.der");
                                        let point = &der[der.len()-65..];
                                        let keys = json!({"keys":[{"kty":"EC","crv":"P-256","alg":"ES256","use":"sig","kid":"login-key","x":URL_SAFE_NO_PAD.encode(&point[1..33]),"y":URL_SAFE_NO_PAD.encode(&point[33..])}]});
                                        reply(&mut stream,200,&keys.to_string(),None);
                                    }
                                    "/authorize" => {
                                        let params: HashMap<_,_> = url.query_pairs().into_owned().collect();
                                        let mut state = state.lock().unwrap();
                                        state.authorization_requests += 1;
                                        state.requested_scopes = params["scope"].clone();
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
        let hash = rejected.as_deref().or_else(|| {
            current
                .as_ref()
                .and_then(|value| value["token_sha256"].as_str())
        });
        let outcome = recover_inner(&dir, "work", &settings, hash, true, &deadline);
        let value = match outcome {
            Ok((token, identity)) => json!({"token":token,"identity":identity}),
            Err(error) => json!({"error":error}),
        };
        fs::write(dir.join("result.json"), value.to_string()).unwrap();
    }

    #[test]
    fn real_pkce_scopes_signature_nonce_and_private_publication() {
        for (scopes, expected) in [
            (None, concat!("openid e", "mail")),
            (
                Some(vec!["openid", concat!("e", "mail")]),
                concat!("openid e", "mail"),
            ),
            (Some(vec!["openid", "profile"]), "openid profile"),
        ] {
            let dir = Temp::new();
            let provider = Provider::new();
            let mut config = provider.config();
            provider.state.lock().unwrap().scopes_supported =
                Some(vec!["openid".into(), concat!("e", "mail").into()]);
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
            let value =
                recover(&dir.0, "work", &provider.config(), Some(&rejected), false).unwrap();
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
                    login(&path, "work", &config, false).unwrap().0
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
        assert!(login(&dir.0, "work", &provider.config(), false)
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
        for scopes in [
            json!([]),
            json!([concat!("e", "mail")]),
            json!(["openid", "bad scope"]),
        ] {
            let mut config = config.clone();
            config["scopes"] = scopes;
            assert!(Settings::new(&config).is_err());
        }
        let settings = Settings::new(&config).unwrap();
        assert_eq!(
            settings.scopes(&json!({})).unwrap(),
            concat!("openid profile e", "mail offline_access")
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
}
