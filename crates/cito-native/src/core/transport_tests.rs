use super::*;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

#[test]
fn renewal_shares_authority_observations_and_updates_existing_rpc_clients() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = json!({"url":format!("http://{}",listener.local_addr().unwrap()),"authMode":"oidc","token":"old"});
    let mut old = HubClient::new("work".into(), config.clone(), "alice", None);
    old.expires_at = crate::plugins::now_ms() / 1000 + 20;
    old.cache_human(
        &json!({"server_now_ms":1000,"record":{"listener_id":"human","human_paused":true}}),
    );
    old.stream_live.store(true, Ordering::SeqCst);
    let mut renewed = HubClient::new("work".into(), config, "alice", None);
    renewed.bearer = Some("verified-new".into());
    renewed.expires_at = old.expires_at + 300;
    renewed.inherit_authority(&old);
    assert!(Arc::ptr_eq(&old.closed, &renewed.closed));
    assert!(Arc::ptr_eq(&old.directory_cache, &renewed.directory_cache));
    assert_eq!(renewed.cached_directory()[0]["human_paused"], true);
    assert!(renewed.stream_live.load(Ordering::SeqCst));
    old.reject_token(&Some("old".into()));
    assert!(
        renewed.auth_valid(),
        "A delayed rejection cannot reject new credentials"
    );
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        assert!(
            headers.contains("Authorization: Bearer verified-new"),
            "{headers}"
        );
        let body = r#"{"ok":true,"result":{}}"#;
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    old.rpc("fixture", &json!({})).unwrap();
    server.join().unwrap();
    renewed.reject_token(&Some("verified-new".into()));
    assert_eq!(renewed.connection_status(), "auth_required");
}

#[test]
fn rpc_stream_and_observation_capabilities_do_not_overwrite_each_other() {
    let client = HubClient::new(
        "work".into(),
        json!({"url":"http://127.0.0.1:1"}),
        "alice",
        None,
    );
    client.cache_human(&json!({"record":{"listener_id":"human"}}));
    client
        .parse_rpc_response(200, br#"{"ok":true}"#, &None)
        .unwrap();
    // A disconnected SSE stream cannot downgrade a successful RPC or observation.
    client.stream_live.store(false, Ordering::SeqCst);
    assert_eq!(client.connection_status(), "connected");
    assert!(client.observation_current());
    client.stream_live.store(true, Ordering::SeqCst);
    assert!(client.rpc("fixture", &json!({})).is_err());
    let state = client.capabilities();
    assert_eq!(state["status"], "connected");
    assert_eq!(state["rpc_status"], "disconnected");
    assert_eq!(state["sse_live"], true);
    assert_eq!(state["observation_current"], true);
    client.stream_live.store(false, Ordering::SeqCst);
    assert!(client.observation_current());
    assert_eq!(client.capabilities()["rpc_status"], "disconnected");
    assert_ne!(client.connection_status(), "connected");
    client.stream_live.store(true, Ordering::SeqCst);
    *client.observed_at.lock().unwrap() = Some(Instant::now() - Duration::from_secs(61));
    assert!(!client.observation_current());
    assert_eq!(client.connection_status(), "connected");
}

#[test]
fn human_cache_projection_uses_monotonic_hub_time_without_waiting_for_network_writers() {
    let client = HubClient::new(
        "work".into(),
        json!({"url":"http://127.0.0.1:1"}),
        "alice",
        None,
    );
    client.cache_human(&json!({"server_now_ms":1000,"record":{"listener_id":"c_fixture","harness":"human","human_paused":false,"channels":{"laptop:human":{"host_id":"laptop","state":"ACTIVE","updated_at_ms":1000}}}}));
    let before = client.server_now();
    client.observe_server_clock(&json!({"server_now_ms":1}));
    assert!(
        client.server_now() >= before,
        "A delayed response cannot roll the observed Hub clock backward"
    );
    let writer = client.directory_updates.lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let rows = thread::scope(|scope| {
        scope.spawn(|| tx.send(client.cached_directory()).unwrap());
        let rows = rx.recv_timeout(Duration::from_millis(200));
        drop(writer);
        rows.expect("Cached UI projections must not wait for an in-flight network writer")
    });
    assert_eq!(rows.len(), 1);
    assert_eq!(
        super::super::lease::evaluate_node_lease(&rows[0], u64::MAX),
        "ACTIVE"
    );
}

struct Fixture {
    engine: Arc<HubEngine>,
    server: super::super::hub::HubServer,
    dir: PathBuf,
    tls: Option<(Arc<AtomicBool>, thread::JoinHandle<()>)>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some((stop, worker)) = self.tls.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = worker.join();
        }
        self.server.shutdown();
        self.engine.shutdown();
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}
fn fixture(bind: &str, https: bool) -> (Fixture, HubClient) {
    let dir =
        std::env::temp_dir().join(format!("cito-backlog-{}", crate::plugins::random_uuid_v4()));
    let engine = Arc::new(HubEngine::open(&dir).unwrap());
    let server = engine
        .start_server(
            bind,
            0,
            super::super::auth::HubAuth::new("none", None, None, None).unwrap(),
        )
        .unwrap();
    let mut f = Fixture {
        engine,
        server,
        dir,
        tls: None,
    };
    let mut url = format!("http://{}", f.server.address);
    let mut agent = ureq::agent();
    if https {
        let cert = rustls::pki_types::CertificateDer::from(
            include_bytes!("../../tests/hub/fixtures/localhost-cert.der").to_vec(),
        );
        let key = include_str!("../../tests/hub/fixtures/localhost-key.pem")
            .lines()
            .filter(|line| !line.starts_with("---"))
            .collect::<String>();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(STANDARD.decode(key).unwrap());
        let config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![cert.clone()], key.into())
                .unwrap(),
        );
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        agent = ureq::AgentBuilder::new()
            .tls_config(Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            ))
            .build();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        url = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let target = f.server.address;
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
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
                let mut headers = String::new();
                let mut length = 0;
                let mut reader = BufReader::new(&mut tls);
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.to_ascii_lowercase().starts_with("content-length:") {
                        length = line.split(':').nth(1).unwrap().trim().parse().unwrap();
                    }
                    headers.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                drop(reader);
                let input: Value = serde_json::from_slice(&body).unwrap();
                if matches!(input["op"].as_str(), Some("fixture_error" | "fixture_null")) {
                    let response = if input["op"] == "fixture_error" {
                        json!({"ok":false,"error":{"code":"FORBIDDEN","message":"Request denied"}})
                    } else {
                        json!({"ok":true,"error":null,"result":{"value":"accepted"}})
                    };
                    let body = response.to_string();
                    write!(
                        tls,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .unwrap();
                    tls.conn.send_close_notify();
                    tls.flush().unwrap();
                    continue;
                }
                let mut upstream = TcpStream::connect(target).unwrap();
                upstream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                upstream.write_all(headers.as_bytes()).unwrap();
                upstream.write_all(&body).unwrap();
                let mut response = Vec::new();
                upstream.read_to_end(&mut response).unwrap();
                tls.write_all(&response).unwrap();
                tls.conn.send_close_notify();
                tls.flush().unwrap();
            }
        });
        f.tls = Some((stop, worker));
    }
    let mut client = HubClient::new("work".into(), json!({"url":url}), "alice", None);
    client.agent = agent;
    (f, client)
}

#[test]
fn https_rpc_extracts_error_envelopes_and_accepts_null_error() {
    let (_fixture, client) = fixture("127.0.0.1", true);
    assert_eq!(
        client.rpc("fixture_error", &json!({})).unwrap_err(),
        "FORBIDDEN: Request denied"
    );
    assert_eq!(
        client.rpc("fixture_null", &json!({})).unwrap()["value"],
        "accepted"
    );
}
#[test]
fn http_rpc_response_status_and_error_envelopes() {
    for (status, body, expected, state) in [
        (
            200,
            json!({"ok":true,"error":null,"result":{"value":"accepted"}}),
            Ok(json!({"value":"accepted"})),
            "connected",
        ),
        (
            200,
            json!({"error":{"code":"FORBIDDEN","message":"Denied"}}),
            Err("FORBIDDEN: Denied"),
            "connected",
        ),
        (
            200,
            json!({"ok":false,"message":"Retry"}),
            Err("NATS_UNAVAILABLE: Retry"),
            "connected",
        ),
        (
            401,
            json!({}),
            Err("UNAUTHORIZED: Hub rejected request"),
            "auth_required",
        ),
        (
            403,
            json!({}),
            Err("FORBIDDEN: Hub rejected request"),
            "connected",
        ),
        (
            500,
            json!({"message":"Retry"}),
            Err("INVALID_INPUT: Retry"),
            "connected",
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let raw = body.to_string();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.to_ascii_lowercase().starts_with("content-length:") {
                    length = line.split(':').nth(1).unwrap().trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            drop(reader);
            write!(
                socket,
                "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{raw}",
                raw.len()
            )
            .unwrap();
        });
        let client = HubClient::new(
            "work".into(),
            json!({"url":format!("http://{address}")}),
            "alice",
            None,
        );
        *client.status.lock().unwrap() = "reconnecting".into();
        let actual = client.rpc("fixture", &json!({}));
        server.join().unwrap();
        assert_eq!(actual, expected.map_err(str::to_owned));
        assert_eq!(client.connection_status(), state);
    }
}
#[test]
fn ipv6_loopback_hub_rpc_round_trip() {
    let (f, client) = fixture("::1", false);
    assert!(f.server.address.is_ipv6());
    let value = json!({"scope":"workspace","title":"IPv6 loopback"});
    let saved = client.kv_save("ipv6-round-trip", &value, None).unwrap();
    assert_eq!(
        client.kv_get("ipv6-round-trip").unwrap(),
        Some((value, saved["revision"].as_u64().unwrap()))
    );
}

#[test]
fn large_backlog_pages_round_trip_over_http_and_https() {
    for https in [false, true] {
        let (f, client) = fixture("127.0.0.1", https);
        let target = "c_0123456789abcdef";
        let registration = json!({"listener_id":target,"principal_id":"none#alice","owner_user":"alice","scope":"workspace","harness":"fixture","session_id":"s","binding_generation":1,"state":"ACTIVE"});
        f.engine
            .rpc(
                "kv_put",
                &json!({"key":target,"data_b64":STANDARD.encode(registration.to_string())}),
                None,
            )
            .unwrap();
        let message = "🦀".repeat(60_000);
        for i in 0..16 {
            let signal = json!({"signal_id":format!("large-{i}"),"to":target,"from":{"principal_id":"none#bob","listener_id":"sender","user":"bob"},"message":message});
            f.engine.rpc("js_publish", &json!({"subject":format!("cito.signal.{target}"),"data_b64":STANDARD.encode(signal.to_string()),"opts":{"msgID":format!("large-{i}")}}), None).unwrap();
        }
        let mut page = client.rpc("consumer_add", &json!({"config":{"durable_name":"large","filter_subject":format!("cito.signal.{target}"),"deliver_policy":"all"}})).unwrap();
        let mut received = Vec::new();
        let mut pages = 0;
        while !page["backlog"].as_array().unwrap().is_empty() {
            pages += 1;
            assert!(page.to_string().len() < 8 * 1024 * 1024);
            assert!(page.to_string().len() > 262_144);
            for item in page["backlog"].as_array().unwrap() {
                let signal: Value =
                    serde_json::from_slice(&STANDARD.decode(string(item, "data_b64")).unwrap())
                        .unwrap();
                assert_eq!(signal["message"], message);
                received.push(signal["signal_id"].clone());
                client
                    .rpc("consumer_ack", &json!({"name":"large","seq":item["seq"]}))
                    .unwrap();
            }
            page = client
                .rpc("consumer_backlog", &json!({"name":"large"}))
                .unwrap();
        }
        assert_eq!(pages, 2);
        assert_eq!(
            received,
            (0..16)
                .map(|i| json!(format!("large-{i}")))
                .collect::<Vec<_>>()
        );
    }
}
