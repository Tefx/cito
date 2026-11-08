use super::*;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

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
fn fixture(https: bool) -> (Fixture, HubClient) {
    let dir =
        std::env::temp_dir().join(format!("cito-backlog-{}", crate::plugins::random_uuid_v4()));
    let engine = Arc::new(HubEngine::open(&dir).unwrap());
    let server = engine
        .start_server(
            "127.0.0.1",
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
            include_bytes!("../../../../tests/native-hub/fixtures/localhost-cert.der").to_vec(),
        );
        let key = include_str!("../../../../tests/native-hub/fixtures/localhost-key.pem")
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
fn large_backlog_pages_round_trip_over_http_and_https() {
    for https in [false, true] {
        let (f, client) = fixture(https);
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
