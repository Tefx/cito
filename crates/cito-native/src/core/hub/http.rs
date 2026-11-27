//! HTTP/SSE serving and listener lifetime for the shared engine.
use super::*;
use crate::core::{config::advertised_name, login::CITO_CARD_STYLE};
use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;

const MAX_RPC_WORKERS: usize = 256;
const MAX_SSE_WORKERS: usize = 256;
type SseWorkers = Arc<Mutex<Vec<JoinHandle<()>>>>;

struct ConnectionSlot {
    id: u64,
    sockets: Arc<Mutex<HashMap<u64, TcpStream>>>,
}
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        if let Ok(mut sockets) = self.sockets.lock() {
            sockets.remove(&self.id);
        }
    }
}

fn reap_finished(workers: &mut Vec<JoinHandle<()>>) {
    for i in (0..workers.len()).rev() {
        if workers[i].is_finished() {
            let _ = workers.swap_remove(i).join();
        }
    }
}

/// Stopping the listener never closes its shared engine.
pub struct HubServer {
    pub address: SocketAddr,
    listener: std::sync::Weak<TcpListener>,
    running: Arc<AtomicBool>,
    pub(super) wake: Arc<crate::support::WakePipe>,
    sockets: Arc<Mutex<HashMap<u64, TcpStream>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
struct ListenerLifetime(Arc<AtomicBool>, Arc<crate::support::WakePipe>);
impl Drop for ListenerLifetime {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
        self.1.notify();
    }
}
impl HubServer {
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
    pub(super) fn wait_for_stop(&self, requested: &AtomicBool) -> Result<(), String> {
        while !requested.load(Ordering::SeqCst) {
            if !self.is_running() {
                return Err("Hub listener stopped unexpectedly".into());
            }
            crate::support::poll_until(
                &mut [libc::pollfd {
                    fd: self.wake.read_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                }],
                None,
            )?;
        }
        Ok(())
    }
    pub fn shutdown(&self) {
        self.running.store(false, Ordering::Release);
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
        Ok(PreparedHubServer::new(Arc::new(listener), auth.into())?.start(self))
    }
}

/// Own the bound port and wake pipe before committing a configuration change.
/// No connections are served with candidate credentials until `start`.
pub(in crate::core) struct PreparedHubServer {
    listener: Arc<TcpListener>,
    pub(in crate::core) address: SocketAddr,
    auth: Arc<HubAuth>,
    wake: Arc<crate::support::WakePipe>,
}
impl PreparedHubServer {
    pub(in crate::core) fn prepare(
        address: SocketAddr,
        auth: Arc<HubAuth>,
        current: Option<&HubServer>,
    ) -> Result<Self, String> {
        let listener = match current
            .filter(|s| s.address == address && s.is_running())
            .and_then(|s| s.listener.upgrade())
        {
            Some(listener) => listener,
            None => Arc::new(
                TcpListener::bind(address)
                    .map_err(|e| format!("INVALID_INPUT: Cannot bind host hub listener: {e}"))?,
            ),
        };
        Self::new(listener, auth)
    }
    fn new(listener: Arc<TcpListener>, auth: Arc<HubAuth>) -> Result<Self, String> {
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        Ok(Self {
            listener,
            address,
            auth,
            wake: Arc::new(crate::support::WakePipe::new()?),
        })
    }
    pub(in crate::core) fn start(self, engine: &HubEngine) -> HubServer {
        let Self {
            listener,
            address,
            auth,
            wake,
        } = self;
        let bound = Arc::downgrade(&listener);
        let running = Arc::new(AtomicBool::new(true));
        let sockets = Arc::new(Mutex::new(HashMap::<u64, TcpStream>::new()));
        let (engine, lifetime, ss) = (
            engine.clone(),
            ListenerLifetime(running.clone(), wake.clone()),
            sockets.clone(),
        );
        let handle = thread::spawn(move || {
            let mut workers = Vec::new();
            let sse_workers = SseWorkers::default();
            let mut next_socket = 0u64;
            while lifetime.0.load(Ordering::Acquire) {
                let mut ready = [
                    libc::pollfd {
                        fd: listener.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: lifetime.1.read_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                if crate::support::poll_until(&mut ready, None).is_err()
                    || !lifetime.0.load(Ordering::Acquire)
                {
                    break;
                }
                reap_finished(&mut workers);
                match listener.accept() {
                    Ok((stream, _)) => {
                        if !lifetime.0.load(Ordering::Acquire) {
                            let _ = stream.shutdown(Shutdown::Both);
                            break;
                        }
                        if workers.len() >= MAX_RPC_WORKERS {
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
                        let Ok(clone) = stream.try_clone() else {
                            continue;
                        };
                        let Ok(mut list) = ss.lock() else {
                            break;
                        };
                        list.insert(socket_id, clone);
                        drop(list);
                        let (e, a, s) = (engine.clone(), auth.clone(), lifetime.0.clone());
                        let shutdown_wake = lifetime.1.clone();
                        let sse = sse_workers.clone();
                        let connection = ConnectionSlot {
                            id: socket_id,
                            sockets: ss.clone(),
                        };
                        match thread::Builder::new()
                            .name("cito-hub-rpc".into())
                            .spawn(move || {
                                handle_http(
                                    stream,
                                    e,
                                    a,
                                    s,
                                    address,
                                    (shutdown_wake, &sse, connection),
                                );
                            }) {
                            Ok(worker) => workers.push(worker),
                            Err(error) => {
                                eprintln!("cito hub: cannot start request worker: {error}")
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => {
                        eprintln!("cito hub: accept: {e}");
                        break;
                    }
                }
            }
            drop(lifetime);
            if let Ok(sockets) = ss.lock() {
                for socket in sockets.values() {
                    let _ = socket.shutdown(Shutdown::Both);
                }
            }
            for worker in workers {
                let _ = worker.join();
            }
            // No RPC worker can hand off another stream after this point.
            let streams =
                std::mem::take(&mut *sse_workers.lock().unwrap_or_else(|e| e.into_inner()));
            for worker in streams {
                let _ = worker.join();
            }
        });
        HubServer {
            address,
            listener: bound,
            running,
            wake,
            sockets,
            thread: Mutex::new(Some(handle)),
        }
    }
}
fn http_json(stream: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    crate::core::http_boundary::write_json(stream, status, body.to_string().as_bytes())
}

fn http_error(stream: &mut TcpStream, err: &str) {
    let (code, _) = crate::core::http_boundary::coded_error(err);
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
    running: Arc<AtomicBool>,
    bound: SocketAddr,
    (shutdown_wake, sse_workers, connection): (
        Arc<crate::support::WakePipe>,
        &SseWorkers,
        ConnectionSlot,
    ),
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut connection = Some(connection);
    let result = (|| -> Result<(), String> {
        // Darwin accept() inherits the listener's O_NONBLOCK. Client workers use
        // blocking reads with timeouts, including between fragmented headers.
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let invalid = |stream: &mut TcpStream, status, message: &str| {
            http_json(stream, status, &json!({"ok":false,"error":{"code":"INVALID_INPUT","message":format!("INVALID_INPUT: {message}")}})).map_err(|e| e.to_string())
        };
        let head = match crate::core::http_boundary::read_head(&mut reader, 65_536) {
            Ok(head) => head,
            Err((status, message)) => return invalid(&mut stream, status, message),
        };
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
            let health = auth.health();
            // Advertised names are validated slugs, safe to render as HTML text.
            let name = advertised_name(&health)
                .map(|name| format!("<p><strong>{name}</strong></p>"))
                .unwrap_or_default();
            let body = format!(
                r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Cito Hub</title><style>{CITO_CARD_STYLE}</style></head>
<body><main><h1>Cito Hub</h1>{name}
<p>This Hub is running. On another Mac, open Cito → Settings… → Hubs &amp; Network → Join from Clipboard to connect using a copied invite.</p>
<p>Your shared inbox connects all attached Macs and channels. Incoming messages appear in the menu bar unread badge and Activity, where you can reply in one line.</p>
</main></body></html>"#
            );
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
            let mut workers = sse_workers.lock().map_err(|_| "SSE worker lock poisoned")?;
            reap_finished(&mut workers);
            if workers.len() >= MAX_SSE_WORKERS || !running.load(Ordering::Acquire) {
                return Err("UNAVAILABLE: Event stream capacity unavailable".into());
            }
            let expires_at = principal["token_expires_at"].as_f64();
            let events = engine.subscribe(Some(principal))?;
            let mut stream = stream.try_clone().map_err(|e| e.to_string())?;
            let connection = connection.take();
            workers.push(
                thread::Builder::new()
                    .name("cito-hub-sse".into())
                    .spawn(move || {
                        let _connection = connection;
                        let _ = serve_events(
                            &mut stream,
                            engine,
                            running,
                            shutdown_wake,
                            events,
                            expires_at,
                        );
                    })
                    .map_err(|e| format!("UNAVAILABLE: Cannot start event worker: {e}"))?,
            );
            return Ok(());
        }
        if path != "/v1/rpc" || method != "POST" {
            return Err("NOT_FOUND: Not found".into());
        }
        if length > MAX_RPC_BYTES {
            return invalid(&mut stream, 413, "RPC body exceeds 8 MiB");
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
    } else if connection.is_none() {
        return; // The SSE worker now owns the connection.
    }
    crate::core::http_boundary::finish_http(&mut stream);
}

fn serve_events(
    stream: &mut TcpStream,
    engine: HubEngine,
    running: Arc<AtomicBool>,
    shutdown_wake: Arc<crate::support::WakePipe>,
    events: Subscription,
    expires_at: Option<f64>,
) -> Result<(), String> {
    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache, no-transform\r\nConnection: keep-alive\r\n\r\n:ok\n\n").map_err(|e|e.to_string())?;
    while running.load(Ordering::Acquire)
        && !engine.inner.closed.load(Ordering::SeqCst)
        && expires_at.is_none_or(|exp| now_ms().max(0) as f64 / 1000.0 < exp)
    {
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
                    Err(mpsc::TryRecvError::Disconnected) => Err(mpsc::TryRecvError::Disconnected),
                    Err(mpsc::TryRecvError::Empty) => {
                        crate::support::poll_until(
                            &mut fds,
                            Some(std::time::Instant::now() + wait),
                        )?;
                        if fds[1].revents != 0 || !running.load(Ordering::Acquire) {
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
    Ok(())
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
