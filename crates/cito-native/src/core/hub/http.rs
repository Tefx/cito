//! HTTP/SSE serving and listener lifetime for the shared engine.
use super::*;
use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};

/// Stopping the listener never closes its shared engine. The daemon closes the engine last.
pub struct HubServer {
    pub address: SocketAddr,
    pub(in crate::core) auth: Arc<HubAuth>,
    stop: Arc<AtomicBool>,
    wake: Arc<crate::support::WakePipe>,
    sockets: Arc<Mutex<HashMap<u64, TcpStream>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
impl HubServer {
    pub fn shutdown(&self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        self.wake.notify();
        if let Ok(sockets) = self.sockets.lock() {
            for socket in sockets.values() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
        if let Ok(mut thread) = self.thread.lock() {
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
    }
    pub fn port(&self) -> u16 {
        self.address.port()
    }
}
impl Drop for HubServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}
impl HubEngine {
    pub fn start_server(
        &self,
        bind: &str,
        port: u16,
        auth: impl Into<Arc<HubAuth>>,
    ) -> Result<HubServer, String> {
        let listener = TcpListener::bind((bind, port)).map_err(|e| e.to_string())?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(crate::support::WakePipe::new()?);
        let worker_wake = wake.clone();
        let sockets = Arc::new(Mutex::new(HashMap::<u64, TcpStream>::new()));
        let auth = auth.into();
        let (engine, st, ss, worker_auth) =
            (self.clone(), stop.clone(), sockets.clone(), auth.clone());
        let handle = thread::spawn(move || {
            let mut workers: Vec<JoinHandle<()>> = Vec::new();
            let mut next_socket = 0u64;
            while !st.load(Ordering::SeqCst) {
                use std::os::fd::AsRawFd;
                let mut ready = [
                    libc::pollfd {
                        fd: listener.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: worker_wake.read_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                if crate::support::poll_until(&mut ready, None).is_err()
                    || st.load(Ordering::SeqCst)
                {
                    break;
                }
                for i in (0..workers.len()).rev() {
                    if workers[i].is_finished() {
                        let _ = workers.swap_remove(i).join();
                    }
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        if st.load(Ordering::SeqCst) {
                            let _ = stream.shutdown(Shutdown::Both);
                            break;
                        }
                        if workers.len() >= 256 {
                            let mut stream = stream;
                            let _ = http_json(
                                &mut stream,
                                503,
                                &json!({"ok":false,"error":{"code":"UNAVAILABLE","message":"Too many connections"}}),
                            );
                            continue;
                        }
                        next_socket += 1;
                        let socket_id = next_socket;
                        if let Ok(clone) = stream.try_clone() {
                            if let Ok(mut list) = ss.lock() {
                                list.insert(socket_id, clone);
                            }
                        }
                        let (e, a, s, connections) =
                            (engine.clone(), worker_auth.clone(), st.clone(), ss.clone());
                        let shutdown_wake = worker_wake.clone();
                        workers.push(thread::spawn(move || {
                            handle_http(stream, e, a, s, address, shutdown_wake);
                            if let Ok(mut list) = connections.lock() {
                                list.remove(&socket_id);
                            }
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => {
                        eprintln!("cito hub: accept: {e}");
                        break;
                    }
                }
            }
            if let Ok(sockets) = ss.lock() {
                for socket in sockets.values() {
                    let _ = socket.shutdown(Shutdown::Both);
                }
            }
            for worker in workers {
                let _ = worker.join();
            }
        });
        Ok(HubServer {
            address,
            auth,
            stop,
            wake,
            sockets,
            thread: Mutex::new(Some(handle)),
        })
    }
}
fn http_json(stream: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    crate::core::http_boundary::write_json(stream, status, body.to_string().as_bytes())
}

fn http_error(stream: &mut TcpStream, err: &str) {
    let code = err
        .split_once(':')
        .map(|(code, _)| code)
        .filter(|code| {
            !code.is_empty()
                && code
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
        .unwrap_or("INTERNAL_ERROR");
    let status = match code {
        "FORBIDDEN" | "PERMISSIONS_VIOLATION" | "FORBIDDEN_TARGET_SCOPE" => 403,
        "AUTHORIZATION_VIOLATION" => 401,
        "10037" | "10059" | "NOT_FOUND" | "LISTENER_NOT_FOUND" => 404,
        "10071" => 409,
        "AUTH_UNAVAILABLE" | "UNAVAILABLE" => 503,
        "INTERNAL_ERROR" => 500,
        _ => 400,
    };
    let mut error = json!({"code":code,"message":err});
    if let Ok(number) = code.parse::<u64>() {
        error["api_error"] = json!({"err_code":number});
    }
    let _ = http_json(stream, status, &json!({"ok":false,"error":error}));
}
pub(super) fn sse_wait(expires_at: Option<f64>, now: i64) -> Duration {
    let seconds = expires_at
        .map(|expires| (expires - now.max(0) as f64 / 1000.0).clamp(0.0, 30.0))
        .unwrap_or(30.0);
    Duration::from_secs_f64(seconds)
}
fn handle_http(
    mut stream: TcpStream,
    engine: HubEngine,
    auth: Arc<HubAuth>,
    stop: Arc<AtomicBool>,
    bound: SocketAddr,
    shutdown_wake: Arc<crate::support::WakePipe>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let result = (|| -> Result<(), String> {
        // Darwin accept() inherits the listener's O_NONBLOCK. Client workers use
        // blocking reads with timeouts, including between fragmented headers.
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let head = crate::core::http_boundary::read_head(&mut reader, 65_536)
            .map_err(|(_, message)| format!("INVALID_INPUT: {message}"))?;
        let method = head.method;
        let path = head.target.split('?').next().unwrap_or("/").to_owned();
        let headers = head.headers;
        let length = head.length;
        if let Err((status, message)) = crate::core::http_boundary::validate_hub(
            bound,
            &method,
            headers.get("host").map(String::as_str),
            headers.get("origin").map(String::as_str),
            headers.get("content-type").map(String::as_str),
            matches!(auth.mode.as_str(), "token" | "oidc"),
        ) {
            http_json(
                &mut stream,
                status,
                &json!({"ok":false,"error":{"message":message}}),
            )
            .map_err(|e| e.to_string())?;
            return Ok(());
        }
        if path == "/" && method == "GET" {
            let body = "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Cito Hub</title><style>body{background:#12151c;color:#f3f4f6;font:18px system-ui;max-width:42rem;margin:12vh auto;padding:2rem}h1{color:#7dd3fc}code{color:#7dd3fc}</style><body><h1>Cito Hub</h1><p>This Hub is running. On another Mac, open Cito → Settings… → Hubs &amp; Network → Join from Clipboard to connect using a copied invite.</p><p>Your shared inbox connects all attached Macs and channels. Incoming messages appear in the menu bar unread badge and Activity, where you can reply in one line.</p></body></html>";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        if path == "/health" && method == "GET" {
            http_json(&mut stream, 200, &auth.health()).map_err(|e| e.to_string())?;
            return Ok(());
        }
        let principal = auth.authenticate(&headers)?;
        if path == "/v1/events" && method == "GET" {
            let expires_at = principal["token_expires_at"].as_f64();
            let events = engine.subscribe(Some(principal))?;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache, no-transform\r\nConnection: keep-alive\r\n\r\n:ok\n\n").map_err(|e|e.to_string())?;
            while !stop.load(Ordering::SeqCst)
                && !engine.inner.closed.load(Ordering::SeqCst)
                && expires_at.is_none_or(|exp| now_ms().max(0) as f64 / 1000.0 < exp)
            {
                use std::os::fd::AsRawFd;
                let wait = sse_wait(expires_at, now_ms());
                let mut fds = [
                    libc::pollfd {
                        fd: events.wake.read_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: stream.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: shutdown_wake.read_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                let event = match events.try_recv() {
                    Ok(frame) => Ok(frame),
                    Err(mpsc::TryRecvError::Disconnected) => Err(mpsc::TryRecvError::Disconnected),
                    Err(mpsc::TryRecvError::Empty) => {
                        events.wake.drain();
                        // Recheck after draining so a concurrent commit cannot lose its wake byte.
                        match events.try_recv() {
                            Ok(frame) => Ok(frame),
                            Err(mpsc::TryRecvError::Disconnected) => {
                                Err(mpsc::TryRecvError::Disconnected)
                            }
                            Err(mpsc::TryRecvError::Empty) => {
                                crate::support::poll_until(
                                    &mut fds,
                                    Some(std::time::Instant::now() + wait),
                                )?;
                                if fds[1].revents != 0 || stop.load(Ordering::SeqCst) {
                                    break;
                                }
                                events.try_recv()
                            }
                        }
                    }
                }
                .map_err(|error| match error {
                    mpsc::TryRecvError::Empty => mpsc::RecvTimeoutError::Timeout,
                    mpsc::TryRecvError::Disconnected => mpsc::RecvTimeoutError::Disconnected,
                });
                if expires_at.is_some_and(|exp| now_ms().max(0) as f64 / 1000.0 >= exp) {
                    break;
                }
                match event {
                    Ok(frame) => {
                        write!(stream, "data: {frame}\n\n").map_err(|e| e.to_string())?;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        stream
                            .write_all(b":keepalive\n\n")
                            .map_err(|e| e.to_string())?;
                    }
                    Err(_) => break,
                }
                stream.flush().map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        if path != "/v1/rpc" || method != "POST" {
            return Err("NOT_FOUND: Not found".into());
        }
        if length > 8 * 1024 * 1024 {
            return Err("INVALID_INPUT: RPC body exceeds 8 MiB".into());
        }
        let mut body = vec![0u8; length];
        reader
            .read_exact(&mut body)
            .map_err(|e| format!("INVALID_INPUT: incomplete body: {e}"))?;
        let body: Value =
            serde_json::from_slice(&body).map_err(|_| "INVALID_INPUT: Invalid JSON body")?;
        let op = required(&body, "op")?;
        let params = body.get("params").unwrap_or(&body);
        let result = engine.rpc(op, params, Some(&principal))?;
        http_json(&mut stream, 200, &json!({"ok":true,"result":result}))
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(err) = result {
        http_error(&mut stream, &err);
    }
    let _ = stream.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod error_tests {
    use super::*;
    #[test]
    fn errors_preserve_valid_codes_and_classify_unstructured_failures_as_internal() {
        for (error, status, code) in [
            ("10071: Conflict", 409, "10071"),
            ("INVALID_INPUT: Invalid", 400, "INVALID_INPUT"),
            ("sqlite: I/O failed", 500, "INTERNAL_ERROR"),
            (": empty", 500, "INTERNAL_ERROR"),
            ("lower_case: failed", 500, "INTERNAL_ERROR"),
            ("Unclassified failure", 500, "INTERNAL_ERROR"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let (mut server, _) = listener.accept().unwrap();
            http_error(&mut server, error);
            drop(server);
            let response = crate::plugins::read_http_response(&mut client).unwrap();
            assert_eq!(response.status, status, "{error}");
            let body: Value = serde_json::from_str(&response.body).unwrap();
            assert_eq!(body["error"]["code"], code);
            assert_eq!(body["error"]["message"], error);
        }
    }
}
