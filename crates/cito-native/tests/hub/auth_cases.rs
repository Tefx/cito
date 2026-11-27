use super::*;

#[test]
fn oidc_network_errors_redact_discovery_paths_and_jwks_query_credentials() {
    use std::io::{BufRead, BufReader, Write};
    for jwks_failure in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}/private-issuer", listener.local_addr().unwrap());
        let discovery =
            json!({"issuer":issuer,"jwks_uri":format!("{issuer}/keys?credential=private-query")})
                .to_string();
        let server = std::thread::spawn(move || {
            for index in 0..if jwks_failure { 2 } else { 1 } {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(&mut socket);
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                drop(reader);
                let (status, body) = if jwks_failure && index == 0 {
                    ("200 OK", discovery.as_str())
                } else {
                    ("403 Forbidden", "private diagnostic")
                };
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let verifier = OidcVerifier::new(&issuer, "client").unwrap();
        let error = verifier.refresh(None).unwrap_err();
        server.join().unwrap();
        assert!(error.starts_with("AUTH_UNAVAILABLE:"));
        for secret in ["private-issuer", "private-query", "private diagnostic"] {
            assert!(!error.contains(secret), "{error}");
        }
    }
}

#[test]
fn health_advertises_names_without_disclosing_credentials() {
    for (mode, token) in [("none", None), ("token", Some("secret".into()))] {
        let auth = HubAuth::new(mode, token, None, None).unwrap();
        assert!(auth.health().get("oidc").is_none());
        assert_eq!(auth.health()["auth_mode"], mode);
        assert!(auth.health().get("name").is_none());
        assert!(auth.health().get("token").is_none());
    }
    let mut auth = HubAuth::new(
        "oidc",
        None,
        Some("https://id.example.com".into()),
        Some("client".into()),
    )
    .unwrap();
    assert_eq!(
        auth.health()["oidc"],
        json!({"issuer":"https://id.example.com","client_id":"client"})
    );
    auth.advertise_name(None).unwrap();
    assert_eq!(auth.health()["name"], "example");
    auth.advertise_name(Some("Office Mac")).unwrap();
    assert_eq!(auth.health()["name"], "office-mac");
    for raw in ["!!!", "Local"] {
        assert!(auth
            .advertise_name(Some(raw))
            .unwrap_err()
            .starts_with("INVALID_INPUT:"));
        assert_eq!(auth.health()["name"], "office-mac");
    }
    auth.redirect_port = Some(0);
    assert!(auth.health()["oidc"].get("redirect_port").is_none());
    auth.redirect_port = Some(65535);
    assert_eq!(auth.health()["oidc"]["redirect_port"], 65535);
}

#[test]
fn hub_landing_page_displays_the_advertised_name_in_an_adaptive_card() {
    use crate::core::hub::{HubEngine, TestDirectory, TestEngine};
    use std::{io::Write, net::TcpStream};

    let dir = TestDirectory(std::env::temp_dir().join(format!(
        "cito-hub-landing-{}",
        crate::plugins::random_uuid_v4()
    )));
    let engine = TestEngine(HubEngine::open(&dir).unwrap());
    for advertised in [None, Some("Office Mac")] {
        let mut auth = HubAuth::new("token", Some("private-token".into()), None, None).unwrap();
        if let Some(name) = advertised {
            auth.advertise_name(Some(name)).unwrap();
        }
        let server = engine.start_server("127.0.0.1", 0, auth).unwrap();
        let mut stream = TcpStream::connect(server.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(stream, "GET / HTTP/1.1\r\nHost: {}\r\n\r\n", server.address).unwrap();
        let response = crate::plugins::read_http_response(&mut stream).unwrap();
        assert_eq!(response.status, 200);
        let body = response.body;
        for part in [
            "<main><h1>Cito Hub</h1>",
            "--bg:#ECEFF3; --card:#FFFFFF",
            "@media (prefers-color-scheme: dark) { :root { --bg:#1E2024; --card:#26292E",
            "place-items:center",
            "border-radius:14px",
            "This Hub is running.",
            "Your shared inbox connects all attached Macs and channels.",
        ] {
            assert!(body.contains(part), "missing {part}");
        }
        assert_eq!(
            body.contains("<strong>office-mac</strong>"),
            advertised.is_some()
        );
        assert!(!body.contains("private-token"));
    }
}

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{encode, EncodingKey, Header};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
fn key_material(alg: Algorithm) -> (Value, EncodingKey) {
    let mut jwk = json!({"kid":"test-key","alg":format!("{alg:?}"),"use":"sig"});
    let key = if matches!(
        alg,
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 | Algorithm::PS256
    ) {
        let modulus = include_str!("fixtures/rsa-modulus.txt")
            .trim()
            .strip_prefix("Modulus=")
            .unwrap();
        let bytes: Vec<u8> = (0..modulus.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&modulus[i..i + 2], 16).unwrap())
            .collect();
        jwk["kty"] = json!("RSA");
        jwk["n"] = json!(URL_SAFE_NO_PAD.encode(bytes));
        jwk["e"] = json!("AQAB");
        EncodingKey::from_rsa_pem(include_bytes!("fixtures/rsa-private.pem")).unwrap()
    } else if matches!(alg, Algorithm::ES256 | Algorithm::ES384) {
        let (der, pem, size, curve): (&[u8], &[u8], usize, &str) = if alg == Algorithm::ES256 {
            (
                include_bytes!("fixtures/ec-public.der"),
                include_bytes!("fixtures/ec-private.pem"),
                32,
                "P-256",
            )
        } else {
            (
                include_bytes!("fixtures/p384-public.der"),
                include_bytes!("fixtures/p384-private.pem"),
                48,
                "P-384",
            )
        };
        let point = &der[der.len() - (size * 2 + 1)..];
        assert_eq!(point[0], 4);
        jwk["kty"] = json!("EC");
        jwk["crv"] = json!(curve);
        jwk["x"] = json!(URL_SAFE_NO_PAD.encode(&point[1..size + 1]));
        jwk["y"] = json!(URL_SAFE_NO_PAD.encode(&point[size + 1..]));
        EncodingKey::from_ec_pem(pem).unwrap()
    } else {
        assert_eq!(alg, Algorithm::EdDSA);
        let der = include_bytes!("fixtures/ed25519-public.der");
        jwk["kty"] = json!("OKP");
        jwk["crv"] = json!("Ed25519");
        jwk["x"] = json!(URL_SAFE_NO_PAD.encode(&der[der.len() - 32..]));
        EncodingKey::from_ed_pem(include_bytes!("fixtures/ed25519-private.pem")).unwrap()
    };
    (jwk, key)
}
fn verifier(alg: Algorithm) -> (KeyServer, OidcVerifier, EncodingKey) {
    let (jwk, key) = key_material(alg);
    let server = key_server(json!({"keys":[jwk]}));
    let verifier = OidcVerifier::new(&server.issuer, "client").unwrap();
    (server, verifier, key)
}
#[test]
fn missing_kid_selects_only_an_unambiguous_eligible_signing_key() {
    check_key_selection(false);
}

#[test]
fn shared_kid_selects_only_one_compatible_verification_key() {
    check_key_selection(true);
}
fn check_key_selection(with_kid: bool) {
    let (signing, key) = key_material(Algorithm::RS256);
    let mut encryption = signing.clone();
    encryption["use"] = json!("enc");
    let mut other_alg = signing.clone();
    other_alg["alg"] = json!("RS512");
    let mut keys = vec![encryption, other_alg, signing.clone()];
    let mut header = Header::new(Algorithm::RS256);
    if with_kid {
        header.kid = Some("test-key".into());
        let mut other = signing.clone();
        other["kid"] = json!("other-key");
        keys.push(other);
    }
    let server = key_server(json!({"keys":keys}));
    let claims = json!({"iss":server.issuer,"aud":"client","sub":"alice","exp":crate::plugins::now_ms()/1000+60});
    let jwt = encode(&header, &claims, &key).unwrap();
    let check = || {
        OidcVerifier::new(&server.issuer, "client")
            .unwrap()
            .verify(&jwt)
    };
    assert_eq!(check().unwrap()["sub"], "alice");
    keys.push(signing);
    *server.keys.lock().unwrap() = json!({"keys":keys});
    assert!(check().unwrap_err().starts_with("AUTHORIZATION_VIOLATION:"));
}

#[test]
fn real_supported_asymmetric_signatures_and_claim_validation() {
    for alg in [
        Algorithm::RS256,
        Algorithm::RS384,
        Algorithm::RS512,
        Algorithm::PS256,
        Algorithm::ES256,
        Algorithm::ES384,
        Algorithm::EdDSA,
    ] {
        let (server, v, key) = verifier(alg);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = json!({"iss":server.issuer,"sub":"subject","aud":["other","client"],"azp":"client","exp":now+300,"nbf":now-1,"preferred_username":"alice.smith"});
        let mut header = Header::new(alg);
        header.kid = Some("test-key".into());
        let signed = |claims: &Value| encode(&header, claims, &key).unwrap();
        let token = signed(&claims);
        let principal = v.verify(&token).unwrap();
        assert_eq!(
            principal["principal_id"],
            format!("oidc:{}|subject", server.issuer)
        );
        assert_eq!(principal["owner_user"], "alice.smith");
        let mut fallback = claims.clone();
        fallback["preferred_username"] = Value::Null;
        fallback["email"] = json!("alice@example.com");
        assert_eq!(v.verify(&signed(&fallback)).unwrap()["owner_user"], "alice");
        for (k, value) in [
            ("iss", json!("https://other.example.com")),
            ("aud", json!("other")),
            ("azp", json!("other")),
            ("azp", Value::Null),
            ("exp", json!(now - 30)),
            ("nbf", json!(now + 60)),
            ("nbf", json!("invalid")),
            ("sub", json!("")),
        ] {
            let mut invalid = claims.clone();
            invalid[k] = value;
            assert!(
                v.verify(&signed(&invalid)).is_err(),
                "{alg:?}: accepted invalid {k}"
            );
        }
        let mut parts: Vec<String> = token.split('.').map(String::from).collect();
        let mut sig = URL_SAFE_NO_PAD.decode(&parts[2]).unwrap();
        sig[0] ^= 1;
        parts[2] = URL_SAFE_NO_PAD.encode(sig);
        assert!(v.verify(&parts.join(".")).is_err());
        let mut wrong = Header::new(Algorithm::HS256);
        wrong.kid = Some("test-key".into());
        let token = encode(&wrong, &claims, &EncodingKey::from_secret(b"wrong")).unwrap();
        assert!(v.verify(&token).is_err());
    }
}
#[test]
fn https_discovery_jwks_fetch_and_unknown_kid_refresh() {
    use base64::engine::general_purpose::STANDARD;
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let issuer = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    listener.set_nonblocking(true).unwrap();
    let discovery = json!({"issuer":issuer,"jwks_uri":format!("{issuer}/keys.json")}).to_string();
    let (jwk, key) = key_material(Algorithm::RS256);
    let keys = Arc::new(Mutex::new(json!({"keys":[jwk]})));
    let cert = rustls::pki_types::CertificateDer::from(
        include_bytes!("fixtures/localhost-cert.der").to_vec(),
    );
    let pem = include_str!("fixtures/localhost-key.pem")
        .lines()
        .filter(|line| !line.starts_with("---"))
        .collect::<String>();
    let tls_key = rustls::pki_types::PrivatePkcs8KeyDer::from(STANDARD.decode(pem).unwrap());
    let config = Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], tls_key.into())
            .unwrap(),
    );
    struct TlsServer {
        stop: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for TlsServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(worker) = self.worker.take() {
                let result = worker.join();
                if !std::thread::panicking() {
                    result.unwrap();
                }
            }
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let served_keys = keys.clone();
    let worker = std::thread::spawn(move || {
        while !stopped.load(Ordering::SeqCst) {
            let socket = match listener.accept() {
                Ok((socket, _)) => socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut tls = rustls::StreamOwned::new(
                rustls::ServerConnection::new(config.clone()).unwrap(),
                socket,
            );
            let mut reader = BufReader::new(&mut tls);
            let mut request = String::new();
            assert!(reader.read_line(&mut request).unwrap() > 0);
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
            }
            drop(reader);
            let body = match request.split_whitespace().nth(1).unwrap() {
                "/.well-known/openid-configuration" => discovery.clone(),
                "/keys.json" => served_keys.lock().unwrap().to_string(),
                path => panic!("Unexpected OIDC request path: {path}"),
            };
            write!(
                tls,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            tls.conn.send_close_notify();
            tls.flush().unwrap();
        }
    });
    let _server = TlsServer {
        stop,
        worker: Some(worker),
    };
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let cooldown = Duration::from_millis(150);
    let mut verifier =
        OidcVerifier::with_cache_policy(&issuer, "client", Duration::from_secs(300), cooldown)
            .unwrap();
    verifier.tls = Arc::new(tls);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({"iss":issuer,"aud":"client","sub":"subject","exp":now+300});
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    let token = encode(&header, &claims, &key).unwrap();
    verifier.verify(&token).unwrap();
    keys.lock().unwrap()["keys"][0]["kid"] = json!("rotated-key");
    header.kid = Some("rotated-key".into());
    let token = encode(&header, &claims, &key).unwrap();
    assert!(verifier
        .verify(&token)
        .unwrap_err()
        .starts_with("AUTHORIZATION_VIOLATION"));
    std::thread::sleep(cooldown);
    verifier.verify(&token).unwrap();
}
#[test]
fn blank_environment_is_unset_and_explicit_auth_is_validated() {
    assert_eq!(
        crate::core::identity::principal("Alice.Example")["principal_id"],
        "none#alice.example"
    );
    assert!(HubAuth::new("token", Some("".into()), None, None)
        .err()
        .unwrap()
        .contains("CITO_HUB_TOKEN"));
    assert!(HubAuth::new("oidc", None, None, None)
        .err()
        .unwrap()
        .contains("CITO_OIDC_ISSUER"));
    assert!(HubAuth::new("invalid", None, None, None)
        .err()
        .unwrap()
        .contains("CITO_AUTH_MODE"));
    assert!(OidcVerifier::new("https://id.example.com", "")
        .err()
        .unwrap()
        .contains("CITO_OIDC_CLIENT_ID"));
    for invalid in [" leading", "trailing ", "embedded\x01control"] {
        assert!(HubAuth::new("token", Some(invalid.into()), None, None).is_err());
        assert!(OidcVerifier::new("https://id.example.com", invalid).is_err());
        assert!(crate::core::config::discover_hub_config(
            json!({"url":"local://","authMode":"token","token":invalid})
        )
        .is_err());
    }
    for url in [
        "http://id.example.com",
        "http://localhost:9000",
        "http://127.0.0.2:9000",
        "https://secret-user:secret-pass@id.example.com",
        "https://id.example.com?query=secret-pass",
        "https://id.example.com#secret-pass",
    ] {
        let error = OidcVerifier::new(url, "client").err().unwrap();
        assert!(error.contains("CITO_OIDC_ISSUER"));
        assert!(!error.contains("secret-pass"));
    }
    assert!(OidcVerifier::new("http://127.0.0.1:9000", "client").is_ok());
}

struct KeyServer {
    issuer: String,
    keys: Arc<Mutex<Value>>,
    requests: Arc<AtomicUsize>,
    discovery: Arc<Mutex<Value>>,
    stopped: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for KeyServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn key_server(keys: Value) -> KeyServer {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let issuer = format!("http://{}/", listener.local_addr().unwrap());
    let keys = Arc::new(Mutex::new(keys));
    let served_keys = keys.clone();
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let requested = requests.clone();
    let discovery = Arc::new(Mutex::new(
        json!({"issuer":issuer,"jwks_uri":format!("{issuer}keys.json")}),
    ));
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let advertised = discovery.clone();
    let worker = std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        continue;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                    }
                    let body = if path == "/.well-known/openid-configuration" {
                        advertised.lock().unwrap().to_string()
                    } else if path == "/keys.json" {
                        requested.fetch_add(1, Ordering::SeqCst);
                        served_keys.lock().unwrap().to_string()
                    } else {
                        panic!("Unexpected fixture path: {path}")
                    };
                    let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(e) => panic!("Loopback fixture accept failed: {e}"),
            }
        }
    });
    KeyServer {
        issuer,
        keys,
        requests,
        discovery,
        stopped,
        worker: Some(worker),
    }
}
#[test]
fn http_loopback_discovery_and_jwks_verify_real_signatures() {
    let (jwk, key) = key_material(Algorithm::RS256);
    let mut previous = jwk.clone();
    let mut modulus = URL_SAFE_NO_PAD.decode(jwk["n"].as_str().unwrap()).unwrap();
    modulus[30] ^= 1;
    previous["n"] = json!(URL_SAFE_NO_PAD.encode(modulus));
    let server = key_server(json!({"keys":[previous]}));
    let issuer = &server.issuer;
    let keys = &server.keys;
    let discovery = &server.discovery;
    let requests = &server.requests;
    let cooldown = Duration::from_millis(50);
    let lifetime = Duration::from_millis(400);
    let verifier = OidcVerifier::with_cache_policy(issuer, "client", lifetime, cooldown).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({"iss":issuer,"aud":"client","sub":"subject","exp":now+300,"nbf":now-1});
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    let token = encode(&header, &claims, &key).unwrap();
    assert!(verifier
        .verify(&token)
        .unwrap_err()
        .starts_with("AUTHORIZATION_VIOLATION"));
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    *keys.lock().unwrap() = json!({"keys":[jwk]});
    std::thread::sleep(cooldown);
    // Concurrent callers recover the rotated key with a single JWKS request.
    let before = requests.load(Ordering::SeqCst);
    let start = std::sync::Barrier::new(4);
    let deadline = Instant::now() + Duration::from_secs(3);
    std::thread::scope(|scope| {
        let callers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    start.wait();
                    verifier.verify_with_deadline(&token, HttpDeadline::uncancelled(deadline))
                })
            })
            .collect();
        for caller in callers {
            assert_eq!(
                caller.join().unwrap().unwrap()["principal_id"],
                format!("oidc:{issuer}|subject")
            );
        }
    });
    assert_eq!(requests.load(Ordering::SeqCst), before + 1);
    std::thread::sleep(lifetime);
    verifier.verify(&token).unwrap();
    assert_eq!(
        requests.load(Ordering::SeqCst),
        3,
        "Known keys have a bounded cache lifetime"
    );
    keys.lock().unwrap()["keys"][0]["use"] = json!("enc");
    std::thread::sleep(cooldown);
    let mut parts: Vec<String> = token.split('.').map(String::from).collect();
    let mut signature = URL_SAFE_NO_PAD.decode(&parts[2]).unwrap();
    signature[0] ^= 1;
    parts[2] = URL_SAFE_NO_PAD.encode(signature);
    assert!(
        verifier
            .verify(&parts.join("."))
            .unwrap_err()
            .contains("not a verification key"),
        "Refreshed keys retain all algorithm and use checks"
    );
    keys.lock().unwrap()["keys"][0]["use"] = json!("sig");
    std::thread::sleep(lifetime);
    verifier.verify(&token).unwrap();
    assert!(verifier
        .verify(&parts.join("."))
        .unwrap_err()
        .starts_with("AUTHORIZATION_VIOLATION"));
    discovery.lock().unwrap()["jwks_uri"] = json!("http://id.example.com/keys.json");
    header.kid = Some("unknown-key".into());
    std::thread::sleep(cooldown);
    let error = verifier
        .verify(&encode(&header, &claims, &key).unwrap())
        .unwrap_err();
    assert!(error.contains("jwks_uri must use HTTPS"), "{error}");
    assert!(
        verifier.verify(&token).is_ok(),
        "a failed refresh must preserve cached keys"
    );
    assert!(verifier
        .verify(&encode(&header, &claims, &key).unwrap())
        .unwrap_err()
        .starts_with("AUTH_UNAVAILABLE"));
}

#[test]
fn discovery_requires_the_configured_issuer_before_fetching_keys() {
    let (server, _, key) = verifier(Algorithm::RS256);
    let jwt = encode(&Header::new(Algorithm::RS256), &json!({"iss":server.issuer,"sub":"alice","aud":"client","exp":crate::plugins::now_ms()/1000+60}), &key).unwrap();
    for issuer in [None, Some(json!("https://other.example.com"))] {
        let mut doc = server.discovery.lock().unwrap();
        doc.as_object_mut().unwrap().remove("issuer");
        if let Some(issuer) = issuer {
            doc["issuer"] = issuer;
        }
        drop(doc);
        assert!(OidcVerifier::new(&server.issuer, "client")
            .unwrap()
            .verify(&jwt)
            .unwrap_err()
            .contains("discovery issuer mismatch"));
        assert_eq!(server.requests.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn sse_closes_when_verified_token_reaches_expiry_even_with_validation_leeway() {
    use std::io::{Read, Write};
    let (issuer, verifier, key) = verifier(Algorithm::RS256);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    let token = encode(
        &header,
        &json!({"iss":issuer.issuer,"aud":"client","sub":"subject","exp":now as f64-0.5}),
        &key,
    )
    .unwrap();
    assert!(
        verifier.verify(&token).is_ok(),
        "Clock leeway admits authentication, but must not extend SSE lifetime"
    );
    let auth = HubAuth {
        name: None,
        mode: "oidc".into(),
        redirect_port: None,
        token: None,
        verifier: Some(verifier),
    };
    let dir = std::env::temp_dir().join(format!(
        "cito-sse-expiry-{}",
        crate::plugins::random_uuid_v4()
    ));
    let engine = crate::core::hub::HubEngine::open(&dir).unwrap();
    let server = engine.start_server("127.0.0.1", 0, auth).unwrap();
    let mut stream = std::net::TcpStream::connect(server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(
        stream,
        "GET /v1/events HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {token}\r\n\r\n",
        server.address
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.ends_with(":ok\n\n"),
        "Expired streams emit no keepalives"
    );
    server.shutdown();
    engine.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn sse_valid_token_stops_before_an_event_arrives_after_expiry() {
    use std::io::{BufRead, BufReader, Read, Write};
    let (issuer, verifier, key) = verifier(Algorithm::RS256);
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        + 0.4;
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    let token = encode(
        &header,
        &json!({"iss":issuer.issuer,"aud":"client","sub":"subject","exp":expires}),
        &key,
    )
    .unwrap();
    let principal = verifier.verify(&token).unwrap();
    let dir = std::env::temp_dir().join(format!(
        "cito-sse-transition-{}",
        crate::plugins::random_uuid_v4()
    ));
    let engine = crate::core::hub::HubEngine::open(&dir).unwrap();
    let server = engine
        .start_server(
            "127.0.0.1",
            0,
            HubAuth {
                name: None,
                mode: "oidc".into(),
                redirect_port: None,
                token: None,
                verifier: Some(verifier),
            },
        )
        .unwrap();
    let mut stream = std::net::TcpStream::connect(server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(
        stream,
        "GET /v1/events HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {token}\r\n\r\n",
        server.address
    )
    .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == ":ok\n" {
            break;
        }
    }
    line.clear();
    reader.read_line(&mut line).unwrap();
    let publish = |key: &str| {
        engine.rpc("kv_create", &json!({"key":key,"data":{"principal_id":principal["principal_id"],"harness":"fixture","session_id":key,"scope":"workspace","state":"ACTIVE","channels":{}}}), None).unwrap()
    };
    publish("before-expiry");
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert!(
        line.starts_with("data: ") && line.contains("before-expiry"),
        "{line}"
    );
    line.clear();
    reader.read_line(&mut line).unwrap();
    // The fractional token lifetime is the test's short, explicit timing input.
    let remaining = (expires
        - std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64())
    .max(0.0);
    std::thread::sleep(Duration::from_secs_f64(remaining) + Duration::from_millis(20));
    publish("after-expiry");
    let mut trailing = String::new();
    reader.read_to_string(&mut trailing).unwrap();
    server.shutdown();
    engine.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
    assert!(
        trailing.is_empty(),
        "No event or keepalive may cross token expiry: {trailing}"
    );
}
#[test]
fn caller_deadline_bounds_cache_wait_and_cached_verification() {
    let (server, v, key) = verifier(Algorithm::RS256);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({"iss":server.issuer,"aud":"client","sub":"subject","exp":now+300});
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    let token = encode(&header, &claims, &key).unwrap();
    assert!(v
        .verify_with_deadline(
            &token,
            HttpDeadline::uncancelled(Instant::now() + Duration::from_secs(1))
        )
        .is_ok());
    assert!(v
        .verify_with_deadline(
            &token,
            HttpDeadline::uncancelled(Instant::now() - Duration::from_millis(1))
        )
        .unwrap_err()
        .starts_with("OIDC_LOGIN_TIMEOUT"));
    let v = OidcVerifier::new(&server.issuer, "client").unwrap();
    let keys = server.keys.lock().unwrap();
    let requests = server.requests.load(Ordering::SeqCst);
    std::thread::scope(|scope| {
        let refresh = scope.spawn(|| v.verify(&token));
        let start = Instant::now();
        while server.requests.load(Ordering::SeqCst) == requests {
            assert!(start.elapsed() < Duration::from_secs(1));
            std::thread::yield_now();
        }
        let result = v.verify_with_deadline(
            &token,
            HttpDeadline::uncancelled(Instant::now() + Duration::from_millis(50)),
        );
        drop(keys);
        assert!(result.unwrap_err().starts_with("OIDC_LOGIN_TIMEOUT"));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(refresh.join().unwrap().is_ok());
    });
}
#[test]
fn token_time_boundaries_match_jose_clock_tolerance() {
    assert!(validate_token_times(&json!({"exp":971,"nbf":1030}), 1000).is_ok());
    assert!(validate_token_times(&json!({"exp":970}), 1000).is_err());
    assert!(validate_token_times(&json!({"exp":970.5}), 1000).is_ok());
    assert!(validate_token_times(&json!({"exp":1100,"nbf":1030.5}), 1000).is_err());
    assert!(validate_token_times(&json!({"exp":"1100"}), 1000).is_err());
    assert!(validate_token_times(&json!({"exp":1100,"nbf":"1000"}), 1000).is_err());
}
