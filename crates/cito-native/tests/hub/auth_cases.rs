use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{encode, EncodingKey, Header};
fn verifier(alg: Algorithm) -> (OidcVerifier, EncodingKey) {
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
    let verifier = OidcVerifier::new("https://id.example.com", "client").unwrap();
    verifier.cache.lock().unwrap().keys =
        Some(serde_json::from_value(json!({"keys":[jwk]})).unwrap());
    (verifier, key)
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
        let (v, key) = verifier(alg);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = json!({"iss":"https://id.example.com","sub":"subject","aud":["other","client"],"exp":now+300,"nbf":now-1,"preferred_username":"alice.smith"});
        let mut header = Header::new(alg);
        header.kid = Some("test-key".into());
        let signed = |claims: &Value| encode(&header, claims, &key).unwrap();
        let token = signed(&claims);
        let principal = v.verify(&token).unwrap();
        assert_eq!(
            principal["principal_id"],
            "oidc:https://id.example.com|subject"
        );
        assert_eq!(principal["owner_user"], "alice.smith");
        let mut fallback = claims.clone();
        fallback["preferred_username"] = Value::Null;
        fallback["email"] = json!("alice@example.com");
        assert_eq!(v.verify(&signed(&fallback)).unwrap()["owner_user"], "alice");
        for (k, value) in [
            ("iss", json!("https://other.example.com")),
            ("aud", json!("other")),
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
    let (seed, key) = verifier(Algorithm::RS256);
    let keys = Arc::new(Mutex::new(
        serde_json::to_value(seed.cache.lock().unwrap().keys.as_ref().unwrap()).unwrap(),
    ));
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
    let mut verifier = OidcVerifier::new(&issuer, "client").unwrap();
    verifier.agent = ureq::AgentBuilder::new()
        .tls_config(std::sync::Arc::new(tls))
        .timeout(Duration::from_secs(3))
        .build();
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
    verifier.cache.lock().unwrap().refresh_at = Some(Instant::now() - Duration::from_secs(61));
    verifier.verify(&token).unwrap();
}
#[test]
fn blank_environment_is_unset_and_explicit_auth_is_validated() {
    assert_eq!(
        local_principal("Alice.Example")["principal_id"],
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

#[test]
fn http_loopback_discovery_and_jwks_verify_real_signatures() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let (seed, key) = verifier(Algorithm::RS256);
    let keys = serde_json::to_value(seed.cache.lock().unwrap().keys.as_ref().unwrap()).unwrap();
    let discovery = Arc::new(Mutex::new(
        json!({"issuer":issuer,"jwks_uri":format!("{issuer}/keys.json")}),
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
                        keys.to_string()
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
    struct Cleanup {
        stopped: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::SeqCst);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
    let _cleanup = Cleanup {
        stopped,
        worker: Some(worker),
    };
    let verifier = OidcVerifier::new(&issuer, "client").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({"iss":issuer,"aud":"client","sub":"subject","exp":now+300,"nbf":now-1});
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    let token = encode(&header, &claims, &key).unwrap();
    assert_eq!(
        verifier.verify(&token).unwrap()["principal_id"],
        format!("oidc:{issuer}|subject")
    );
    let mut parts: Vec<String> = token.split('.').map(String::from).collect();
    let mut signature = URL_SAFE_NO_PAD.decode(&parts[2]).unwrap();
    signature[0] ^= 1;
    parts[2] = URL_SAFE_NO_PAD.encode(signature);
    assert!(verifier
        .verify(&parts.join("."))
        .unwrap_err()
        .starts_with("AUTHORIZATION_VIOLATION"));
    discovery.lock().unwrap()["jwks_uri"] = json!("http://id.example.com/keys.json");
    header.kid = Some("unknown-key".into());
    verifier.cache.lock().unwrap().refresh_at = Some(Instant::now() - Duration::from_secs(61));
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
fn caller_deadline_bounds_cache_wait_and_cached_verification() {
    let (v, key) = verifier(Algorithm::RS256);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims =
        json!({"iss":"https://id.example.com","aud":"client","sub":"subject","exp":now+300});
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    let token = encode(&header, &claims, &key).unwrap();
    assert!(v
        .verify_with_deadline(&token, Instant::now() + Duration::from_secs(1))
        .is_ok());
    assert!(v
        .verify_with_deadline(&token, Instant::now() - Duration::from_millis(1))
        .unwrap_err()
        .starts_with("OIDC_LOGIN_TIMEOUT"));
    header.kid = Some("pending-key".into());
    let token = encode(&header, &claims, &key).unwrap();
    v.cache.lock().unwrap().refreshing = true;
    let start = Instant::now();
    assert!(v
        .verify_with_deadline(&token, start + Duration::from_millis(50))
        .unwrap_err()
        .starts_with("OIDC_LOGIN_TIMEOUT"));
    assert!(start.elapsed() < Duration::from_secs(1));
    v.cache.lock().unwrap().refreshing = false;
    v.refreshed.notify_all();
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
