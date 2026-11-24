use super::*;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

#[test]
fn silent_established_sse_reconnects_without_peer_eof() {
    use std::os::fd::AsRawFd;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = HubClient::new(
        "work".into(),
        json!({"url":format!("http://{}",listener.local_addr().unwrap())}),
        "alice",
        None,
    );
    let worker = client
        .start_events_with(Duration::from_millis(150), ureq::AgentBuilder::new())
        .unwrap();
    let accept = || {
        let mut fds = [libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        crate::support::poll_until(&mut fds, Some(Instant::now() + Duration::from_secs(3)))
            .unwrap();
        assert_ne!(fds[0].revents, 0, "No SSE connection");
        listener.accept().unwrap().0
    };
    let mut first = accept();
    let initial = client.event_revision();
    // A slow handshake may outlast the established-stream cancellation budget.
    thread::sleep(Duration::from_millis(120));
    first.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: first\n\n").unwrap();
    client.wait_for_event(initial, Instant::now() + Duration::from_secs(1));
    assert!(client.event_revision() > initial);
    let mut second = accept();
    crate::core::http_boundary::read_head(&mut BufReader::new(&second), 16_384).unwrap();
    assert!(!client.stream_live.load(Ordering::SeqCst));
    // Close through the renewed authority while the original reader awaits headers.
    let mut renewed = HubClient::new("work".into(), client.config.clone(), "alice", None);
    renewed.inherit_authority(&client);
    second
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    renewed.close();
    assert_eq!(second.read(&mut [0; 1]).unwrap(), 0);
    worker.join().unwrap();
}

#[test]
fn oversized_sse_hints_drain_without_reconnect_and_keep_following_events() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (closed, eof) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\nda\r\n4;ext=1\r\nta: \r\n").unwrap();
        // A maximum-codepoint signal expands beyond the bounded line buffer on the wire.
        let chunk = "x".repeat(8191);
        for _ in 0..40 {
            write!(socket, "{:x}\r\n{chunk}\r\n", chunk.len()).unwrap();
        }
        let tail = "\n\ndata: following\n\n";
        write!(socket, "{:x}\r\n{tail}\r\n", tail.len()).unwrap();
        socket.flush().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert_eq!(socket.read(&mut [0; 1]).unwrap(), 0);
        closed.send(()).unwrap();
    });
    let client = HubClient::new(
        "work".into(),
        json!({"url":format!("http://{address}")}),
        "alice",
        None,
    );
    let observed = client.event_revision();
    let worker = client.start_events().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while client.event_revision() < observed + 2 {
        assert!(
            Instant::now() < deadline,
            "Oversized and following hints must both invalidate"
        );
        thread::yield_now();
    }
    assert!(client.stream_live.load(Ordering::SeqCst));
    client.close();
    eof.recv_timeout(Duration::from_millis(200))
        .expect("close must release the active SSE socket");
    worker.join().unwrap();
    server.join().unwrap();
}

#[test]
fn embedded_and_http_events_invalidate_after_commits_and_shutdown_cleanly() {
    for http in [false, true] {
        let dir =
            std::env::temp_dir().join(format!("cito-events-{}", crate::plugins::random_uuid_v4()));
        let engine = Arc::new(HubEngine::open(&dir).unwrap());
        engine.set_plugin_roots(&[]).unwrap();
        let server = http.then(|| {
            engine
                .start_server(
                    "127.0.0.1",
                    0,
                    super::super::auth::HubAuth::new("none", None, None, None).unwrap(),
                )
                .unwrap()
        });
        let client = HubClient::new(
            "local".into(),
            json!({"url":server.as_ref().map(|s|format!("http://{}",s.address)).unwrap_or_else(||"local://".into()),"authMode":"none"}),
            "alice",
            (!http).then(|| engine.clone()),
        );
        let worker = client.start_events().unwrap();
        let wait = |condition: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !condition() {
                assert!(Instant::now() < deadline, "Event notification timed out");
                thread::sleep(Duration::from_millis(1));
            }
        };
        wait(&|| client.stream_live.load(Ordering::SeqCst));
        // Outlast the network cancellation deadline; it must not tear down a quiet SSE stream.
        if http {
            thread::sleep(Duration::from_millis(1100));
        }
        let observed = client.event_revision();
        let value = STANDARD.encode(json!({"listener_id":"node","harness":"agent","scope":"workspace","principal_id":"none#alice"}).to_string());
        engine
            .rpc(
                "kv_put",
                &json!({"key":"node","data_b64":value}),
                Some(&client.identity),
            )
            .unwrap();
        wait(&|| client.event_revision() > observed);
        assert_eq!(client.directory().unwrap()[0]["listener_id"], "node");
        let start = Instant::now();
        client.close();
        worker.join().unwrap();
        if let Some(server) = server {
            server.shutdown();
        }
        engine.shutdown();
        assert!(start.elapsed() < Duration::from_millis(1500));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn sse_failures_back_off_and_auth_failures_wait_for_reconfiguration() {
    use std::os::fd::AsRawFd;
    for (status, token) in [
        (503, Some("first")),
        (401, Some("first")),
        (403, Some("first")),
        (401, None),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(crate::support::WakePipe::new().unwrap());
        let stopped = stop.clone();
        let (sent, calls) = std::sync::mpsc::channel();
        let server = thread::spawn(move || loop {
            let mut fds = [
                libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: stopped.read_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            crate::support::poll_until(&mut fds, None).unwrap();
            if fds[1].revents != 0 {
                break;
            }
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            let _ = write!(
                socket,
                "HTTP/1.1 {status} Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            sent.send(Instant::now()).unwrap();
        });
        let config = json!({"url":format!("http://{address}"),"token":token});
        let client = HubClient::new("work".into(), config.clone(), "alice", None);
        let worker = client.start_events().unwrap();
        let first = calls.recv_timeout(Duration::from_secs(2)).unwrap();
        if status == 503 {
            let second = calls.recv_timeout(Duration::from_secs(2)).unwrap();
            let third = calls.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(second.duration_since(first) >= Duration::from_millis(200));
            assert!(third.duration_since(second) >= Duration::from_millis(450));
            assert!(client.rejected_bearer.lock().unwrap().is_none());
        } else {
            assert!(
                calls.recv_timeout(Duration::from_millis(600)).is_err(),
                "Permanent rejection must suspend the stream"
            );
            assert_eq!(
                client.rejected_bearer.lock().unwrap().is_some(),
                status == 401 && token.is_some()
            );
            let mut replacement = HubClient::new("work".into(), config, "alice", None);
            replacement.bearer = Some("replacement".into());
            replacement.inherit_authority(&client);
            calls.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        let start = Instant::now();
        client.close();
        worker.join().unwrap();
        assert!(start.elapsed() < Duration::from_millis(100));
        stop.notify();
        server.join().unwrap();
    }
}

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
    assert!(Arc::ptr_eq(&old.event_revision, &renewed.event_revision));
    assert!(Arc::ptr_eq(&old.event_wait, &renewed.event_wait));
    assert!(Arc::ptr_eq(&old.directory_cache, &renewed.directory_cache));
    assert_eq!(renewed.cached_directory()[0]["human_paused"], true);
    assert!(renewed.stream_live.load(Ordering::SeqCst));
    old.reject_token(&Some("old".into()));
    assert!(
        renewed.auth_valid(),
        "A delayed rejection cannot reject new credentials"
    );
    let server = thread::spawn(move || {
        let mut socket = crate::core::http_boundary::accept_test_client(&listener).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(&mut socket);
        let head = crate::core::http_boundary::read_head(&mut reader, 16_384).unwrap();
        assert_eq!(head.headers["authorization"], "Bearer verified-new");
        let mut request = vec![0; head.length];
        reader.read_exact(&mut request).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&request).unwrap(),
            json!({"op":"fixture","params":{}})
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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = HubClient::new(
        "work".into(),
        json!({"url":format!("http://{}",listener.local_addr().unwrap()),"authMode":"none","token":"unused-token"}),
        "alice",
        None,
    );
    let server = thread::spawn(move || {
        serve_rpc(
            &listener,
            "human_get",
            &json!({}),
            200,
            &json!({"ok":true,"result":{"record":{"listener_id":"human"}}}),
        );
        serve_rpc(
            &listener,
            "kv_snapshot",
            &json!({"after_key":null}),
            200,
            &json!({"ok":true,"result":{"entries":[]}}),
        );
        let (mut stream, _) = listener.accept().unwrap();
        drop(listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let head =
            crate::core::http_boundary::read_head(&mut BufReader::new(&stream), 16_384).unwrap();
        assert!(!head.headers.contains_key("authorization"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ndata: ready\n\n")
            .unwrap();
        assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    });
    client.human_request("human_get", &json!({})).unwrap();
    assert!(client.human_observation_current());
    assert_eq!(client.capabilities()["observation_current"], false);
    client.directory().unwrap();
    assert_eq!(client.connection_status(), "connected");
    assert_eq!(client.capabilities()["observation_current"], true);
    let revision = client.event_revision();
    let worker = client.start_events().unwrap();
    client.wait_for_event(revision, Instant::now() + Duration::from_secs(2));
    assert!(client.rpc("fixture", &json!({})).is_err());
    let state = client.capabilities();
    assert_eq!(state["status"], "connected");
    assert_eq!(state["rpc_status"], "disconnected");
    assert_eq!(state["sse_live"], true);
    assert_eq!(state["observation_current"], true);
    assert_eq!(
        client.capabilities_at(Instant::now() + Duration::from_secs(60))["observation_current"],
        false
    );
    client.close();
    worker.join().unwrap();
    server.join().unwrap();
    assert_eq!(client.connection_status(), "disconnected");
    assert_eq!(client.capabilities()["observation_current"], true);
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
fn loopback_tls() -> (Arc<rustls::ServerConfig>, ureq::AgentBuilder) {
    let cert = rustls::pki_types::CertificateDer::from(
        include_bytes!("../../tests/hub/fixtures/localhost-cert.der").to_vec(),
    );
    let key = include_str!("../../tests/hub/fixtures/localhost-key.pem")
        .lines()
        .filter(|line| !line.starts_with("---"))
        .collect::<String>();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(STANDARD.decode(key).unwrap());
    let server = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key.into())
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    (
        Arc::new(server),
        ureq::AgentBuilder::new().tls_config(Arc::new(tls)),
    )
}
#[test]
fn close_releases_quiet_https_subscription_within_budget() {
    let (config, builder) = loopback_tls();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let server = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut tls =
            rustls::StreamOwned::new(rustls::ServerConnection::new(config).unwrap(), socket);
        let head =
            crate::core::http_boundary::read_head(&mut BufReader::new(&mut tls), 16_384).unwrap();
        assert_eq!(head.target, "/v1/events");
        tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: ready\n\n").unwrap();
        tls.flush().unwrap();
        // Read TCP directly: a dropped ureq body need not send TLS close_notify.
        assert_eq!(tls.sock.read(&mut [0; 1]).unwrap(), 0);
    });
    let client = HubClient::new("work".into(), json!({"url":url}), "alice", None);
    let revision = client.event_revision();
    let worker = client
        .start_events_with(Duration::from_secs(90), builder)
        .unwrap();
    client.wait_for_event(revision, Instant::now() + Duration::from_secs(2));
    assert!(client.event_revision() > revision);
    let start = Instant::now();
    client.close();
    worker.join().unwrap();
    server.join().unwrap();
    assert!(start.elapsed() < Duration::from_millis(200));
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
        let (config, builder) = loopback_tls();
        agent = builder.build();
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
                    // The TLS terminator forwards to a different destination authority.
                    if line.to_ascii_lowercase().starts_with("host:") {
                        headers.push_str(&format!("Host: {target}\r\n"));
                    } else {
                        headers.push_str(&line);
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                drop(reader);
                let input: Value = serde_json::from_slice(&body).unwrap();
                if matches!(
                    input["op"].as_str(),
                    Some("fixture_short" | "fixture_oversized")
                ) {
                    let body = if input["op"] == "fixture_short" {
                        vec![b'x'; 1]
                    } else {
                        vec![b'x'; 8 * 1024 * 1024 + 1]
                    };
                    write!(
                        tls,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len().max(2)
                    )
                    .unwrap();
                    tls.write_all(&body).unwrap();
                    tls.conn.send_close_notify();
                    tls.flush().unwrap();
                    continue;
                }
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
fn https_transport_failures_mark_rpc_disconnected() {
    let (fixture, client) = fixture("127.0.0.1", true);
    for op in ["fixture_short", "fixture_oversized"] {
        client.rpc("fixture_null", &json!({})).unwrap();
        assert_eq!(client.capabilities()["rpc_status"], "connected");
        let error = client.rpc(op, &json!({})).unwrap_err();
        assert!(error.starts_with("NATS_UNAVAILABLE: "), "{error}");
        assert_eq!(client.capabilities()["rpc_status"], "disconnected");
    }
    client.rpc("fixture_null", &json!({})).unwrap();
    drop(fixture);
    assert!(client
        .rpc("fixture_null", &json!({}))
        .unwrap_err()
        .starts_with("NATS_UNAVAILABLE: "));
    assert_eq!(client.capabilities()["rpc_status"], "disconnected");
}
// Check the outgoing RPC as well as its response, including the cursor used for later pages.
fn serve_rpc(listener: &TcpListener, op: &str, params: &Value, status: u16, response: &Value) {
    use std::os::fd::AsRawFd;
    let mut fds = [libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    crate::support::poll_until(&mut fds, Some(Instant::now() + Duration::from_secs(3))).unwrap();
    assert_ne!(fds[0].revents, 0, "Missing {op} request");
    let (mut socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(&mut socket);
    let head = crate::core::http_boundary::read_head(&mut reader, 16_384).unwrap();
    assert!(!head.headers.contains_key("authorization"));
    let mut body = vec![0; head.length];
    reader.read_exact(&mut body).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"op":op,"params":params})
    );
    let raw = response.to_string();
    write!(
        socket,
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{raw}",
        raw.len()
    )
    .unwrap();
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
            200,
            json!({}),
            Err("NATS_UNAVAILABLE: Invalid Hub RPC envelope"),
            "connected",
        ),
        (
            200,
            json!({"ok":true}),
            Err("NATS_UNAVAILABLE: Missing Hub RPC result"),
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
            Err("NATS_UNAVAILABLE: Retry"),
            "connected",
        ),
        (
            500,
            json!({"error":{"code":"INTERNAL_ERROR","message":"Temporary failure"}}),
            Err("INTERNAL_ERROR: Temporary failure"),
            "connected",
        ),
        (
            503,
            json!({"error":{"code":"UNAVAILABLE","message":"Busy"}}),
            Err("UNAVAILABLE: Busy"),
            "connected",
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server =
            thread::spawn(move || serve_rpc(&listener, "fixture", &json!({}), status, &body));
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
fn malformed_snapshot_pages_preserve_the_last_complete_observation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = HubClient::new(
        "work".into(),
        json!({"url":format!("http://{}",listener.local_addr().unwrap())}),
        "alice",
        None,
    );
    let observe = || {
        (
            client.directory_cache.lock().unwrap().clone(),
            client.directory_revision(),
            *client.server_clock.lock().unwrap(),
            *client.directory_observed_at.lock().unwrap(),
            *client.human_observed_at.lock().unwrap(),
        )
    };
    let fetch = |pages: Vec<(Value, Value)>| {
        thread::scope(|scope| {
            let server = scope.spawn(|| {
                for (cursor, page) in pages {
                    serve_rpc(
                        &listener,
                        "kv_snapshot",
                        &json!({"after_key":cursor}),
                        200,
                        &json!({"ok":true,"result":page}),
                    );
                }
            });
            let result = client.directory();
            server.join().unwrap();
            result
        })
    };
    let page = |id: &str, clock: u64, next: Value| json!({"server_now_ms":clock,"entries":[{"value_b64":STANDARD.encode(json!({"listener_id":id}).to_string())}],"next_cursor":next});
    let committed = fetch(vec![(Value::Null, page("committed", 1000, Value::Null))]).unwrap();
    let before = observe();
    assert_eq!(before.0, committed);
    assert!(before.1 > 0 && before.2.is_some() && before.3.is_some() && before.4.is_some());
    for later in [false, true] {
        let encoding_error = STANDARD.decode("%%%").unwrap_err().to_string();
        let mut invalid = vec![
            (json!({}), "NATS_UNAVAILABLE: Missing directory entries"),
            (
                json!({"entries":[],"next_cursor":7}),
                "NATS_UNAVAILABLE: Invalid snapshot cursor",
            ),
            (
                json!({"entries":[{"value_b64":"%%%"}]}),
                encoding_error.as_str(),
            ),
            (
                json!({"entries":[{"value_b64":null}]}),
                "NATS_UNAVAILABLE: Missing directory record",
            ),
        ];
        if later {
            invalid.push((
                json!({"entries":[],"next_cursor":"middle"}),
                "NATS_UNAVAILABLE: Snapshot cursor did not advance",
            ));
        }
        for (mut bad, expected) in invalid {
            bad["server_now_ms"] = json!(999_999);
            let mut pages = Vec::new();
            let cursor = if later {
                pages.push((Value::Null, page("uncommitted", 888_888, json!("middle"))));
                json!("middle")
            } else {
                Value::Null
            };
            pages.push((cursor, bad));
            assert_eq!(fetch(pages).unwrap_err(), expected);
            assert_eq!(observe(), before);
        }
    }
    let mut first = page("first", 10_000, json!("middle"));
    first["entries"].as_array_mut().unwrap().extend(
        [json!([1]), json!({"title":"metadata"}), json!(null)]
            .into_iter()
            .map(|v| json!({"value_b64":STANDARD.encode(v.to_string())})),
    );
    first["entries"]
        .as_array_mut()
        .unwrap()
        .push(json!({"value_b64":STANDARD.encode([0xff, 0x00])}));
    let recovered = fetch(vec![
        (Value::Null, first),
        (json!("middle"), page("recovered", 100_000, Value::Null)),
    ])
    .unwrap();
    assert_eq!(recovered.len(), 2);
    assert!(recovered.iter().all(|row| row["server_now_ms"] == 100_000));
    let after = observe();
    assert_eq!(recovered[0]["listener_id"], "first");
    assert_eq!(recovered[1]["listener_id"], "recovered");
    assert_eq!(after.0, recovered);
    assert_eq!(after.1, before.1 + 1);
    assert_eq!(after.2.unwrap().0, 100_000);
    assert!(after.3 > before.3 && after.4 > before.4);
}

#[test]
fn directory_snapshot_pages_exceed_transport_total_budget() {
    for https in [false, true] {
        let (fixture, client) = fixture("127.0.0.1", https);
        for index in 0..90 {
            let value = json!({"listener_id":format!("node-{index:03}"),"harness":"agent","scope":"workspace","about":"x".repeat(100_000)});
            fixture.engine.rpc("kv_put",&json!({"key":value["listener_id"],"data_b64":STANDARD.encode(value.to_string())}),Some(&client.identity)).unwrap();
        }
        let page = client.rpc("kv_snapshot", &json!({})).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 2 * 1024 * 1024);
        assert!(page["next_cursor"].is_string());
        let rows = client.directory().unwrap();
        assert_eq!(rows.len(), 90);
        assert!(serde_json::to_vec(&rows).unwrap().len() > 8 * 1024 * 1024);
        assert_eq!(client.cached_directory().len(), 90);
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
        assert!(pages > 1 && pages <= received.len());
        assert_eq!(
            received,
            (0..16)
                .map(|i| json!(format!("large-{i}")))
                .collect::<Vec<_>>()
        );
    }
}
