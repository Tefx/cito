use super::hub::HubEngine;
use super::identity::{principal, string};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;

fn hub_error(code: &str, message: &str) -> String {
    if message.starts_with(&format!("{code}:")) {
        message.into()
    } else {
        format!("{code}: {message}")
    }
}

fn authorization(bearer: Option<&str>) -> io::Result<Option<String>> {
    bearer
        .map(|token| {
            if super::config::valid_hub_token(token) {
                Ok(format!("Bearer {token}"))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid Hub bearer token",
                ))
            }
        })
        .transpose()
}

fn connect_http(url: &url::Url, deadline: Instant) -> io::Result<TcpStream> {
    let mut error = io::Error::new(io::ErrorKind::NotConnected, "Hub address unavailable");
    for address in url.socket_addrs(|| None)? {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&address, remaining.min(Duration::from_millis(500))) {
            Ok(socket) => return Ok(socket),
            Err(e) => error = e,
        }
    }
    Err(error)
}

static EVENT_TLS: LazyLock<Arc<rustls::ClientConfig>> = LazyLock::new(|| {
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::from_iter(
                webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
            ))
            .with_no_client_auth(),
    )
});

// Preserve partial chunk headers and terminators across idle socket timeouts.
struct ChunkedEvents<R> {
    reader: R,
    remaining: u64,
    terminator: bool,
    line: Vec<u8>,
}
impl<R: BufRead> Read for ChunkedEvents<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.remaining == 0 {
            let budget = 8192 - self.line.len();
            self.reader
                .by_ref()
                .take(budget as u64)
                .read_until(b'\n', &mut self.line)?;
            let line = self
                .line
                .strip_suffix(b"\r\n")
                .ok_or(io::ErrorKind::InvalidData)?;
            if self.terminator {
                if !line.is_empty() {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                self.terminator = false;
            } else {
                let digits = line.split(|b| *b == b';').next().unwrap_or_default();
                self.remaining = std::str::from_utf8(digits)
                    .ok()
                    .and_then(|s| u64::from_str_radix(s, 16).ok())
                    .ok_or(io::ErrorKind::InvalidData)?;
                if self.remaining == 0 {
                    return Ok(0);
                }
            }
            self.line.clear();
        }
        let n = self.reader.by_ref().take(self.remaining).read(out)?;
        self.remaining -= n as u64;
        self.terminator = self.remaining == 0;
        Ok(n)
    }
}

struct InterruptibleStream<'a> {
    socket: std::net::TcpStream,
    client: &'a HubClient,
    deadline: Instant,
}
impl InterruptibleStream<'_> {
    fn check(&self) -> io::Result<()> {
        if super::daemon::shutdown_requested() || self.client.closed.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Hub request cancelled",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Hub request deadline exceeded",
            ));
        }
        Ok(())
    }
}
impl Read for InterruptibleStream<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.socket.read(bytes) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue
                }
                result => return result,
            }
        }
    }
}
impl Write for InterruptibleStream<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.socket.write(bytes) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        self.socket.flush()
    }
}

#[derive(Clone)]
pub struct HubClient {
    pub name: String,
    pub config: Value,
    pub identity: Value,
    pub bearer: Option<String>,
    pub expires_at: u64,
    pub auth_retry_at: u64,
    pub rejected_bearer: Arc<Mutex<Option<String>>>,
    directory_cache: Arc<Mutex<Vec<Value>>>,
    directory_revision: Arc<AtomicU64>,
    // Serialize network writers without holding the cache lock used by UI projections.
    directory_updates: Arc<Mutex<()>>,
    server_clock: Arc<Mutex<Option<(u64, Instant)>>>,
    pub local: Option<Arc<HubEngine>>,
    // RPC reachability is independent of the optional event-stream hint.
    pub status: Arc<Mutex<String>>,
    stream_live: Arc<AtomicBool>,
    event_revision: Arc<AtomicU64>,
    event_wait: Arc<(Mutex<()>, std::sync::Condvar)>,
    local_event_wake: Arc<Mutex<Option<Arc<crate::support::WakePipe>>>>,
    event_socket: Arc<Mutex<Option<TcpStream>>>,
    directory_observed_at: Arc<Mutex<Option<Instant>>>,
    human_observed_at: Arc<Mutex<Option<Instant>>>,
    event_credentials: Arc<Mutex<Option<String>>>,
    pub closed: Arc<AtomicBool>,
    pub(super) commit_gate: Arc<Mutex<()>>,
    agent: ureq::Agent,
}
impl HubClient {
    pub fn new(name: String, config: Value, user: &str, local: Option<Arc<HubEngine>>) -> Self {
        let bearer = config["token"]
            .as_str()
            .filter(|token| config["authMode"] != "none" && super::config::valid_hub_token(token))
            .map(str::to_owned);
        let status = if local.is_some() {
            "connected"
        } else {
            "connecting"
        };
        Self {
            name,
            config,
            identity: principal(user),
            event_credentials: Arc::new(Mutex::new(bearer.clone())),
            bearer,
            expires_at: u64::MAX,
            auth_retry_at: 0,
            rejected_bearer: Arc::new(Mutex::new(None)),
            directory_cache: Arc::new(Mutex::new(Vec::new())),
            directory_revision: Arc::new(AtomicU64::new(0)),
            directory_updates: Arc::new(Mutex::new(())),
            server_clock: Arc::new(Mutex::new(None)),
            local,
            status: Arc::new(Mutex::new(status.into())),
            stream_live: Arc::new(AtomicBool::new(false)),
            event_revision: Arc::new(AtomicU64::new(1)),
            event_wait: Arc::new((Mutex::new(()), std::sync::Condvar::new())),
            local_event_wake: Arc::new(Mutex::new(None)),
            event_socket: Arc::default(),
            directory_observed_at: Arc::new(Mutex::new(None)),
            human_observed_at: Arc::new(Mutex::new(None)),
            closed: Arc::new(AtomicBool::new(false)),
            commit_gate: Arc::default(),
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout_connect(Duration::from_secs(5))
                .build(),
        }
    }
    pub fn rpc(&self, op: &str, params: &Value) -> Result<Value, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err("NATS_UNAVAILABLE: Hub disconnected".into());
        }
        if let Some(local) = &self.local {
            return local.rpc(op, params, Some(&self.identity));
        }
        let bearer = self
            .event_credentials
            .lock()
            .map_err(|_| "STATE_ERROR: Credentials lock poisoned")?
            .clone();
        let auth = authorization(bearer.as_deref()).map_err(|e| format!("INVALID_INPUT: {e}"))?;
        if string(&self.config, "url").starts_with("http://") {
            let reply = self.rpc_http(op, params, auth.as_deref())?;
            return self.parse_rpc_response(reply.status, reply.body.as_bytes(), &bearer);
        }
        let mut request = self
            .agent
            .post(&format!(
                "{}/v1/rpc",
                string(&self.config, "url").trim_end_matches('/')
            ))
            .timeout(Duration::from_secs(5))
            .set("X-Cito-User", string(&self.identity, "owner_user"));
        if let Some(value) = auth {
            request = request.set("Authorization", &value);
        }
        let result = match request.send_json(json!({"op":op,"params":params})) {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(self.disconnected(error)),
        };
        let status = result.status();
        let mut bytes = Vec::new();
        result
            .into_reader()
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| self.disconnected(e))?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(self.disconnected("Hub response exceeds frame limit"));
        }
        self.parse_rpc_response(status, &bytes, &bearer)
    }
    fn disconnected(&self, error: impl std::fmt::Display) -> String {
        if let Ok(mut status) = self.status.lock() {
            *status = "disconnected".into();
        }
        format!("NATS_UNAVAILABLE: {error}")
    }
    fn parse_rpc_response(
        &self,
        status: u16,
        bytes: &[u8],
        bearer: &Option<String>,
    ) -> Result<Value, String> {
        if status == 401 {
            self.reject_token(bearer);
        }
        if let Ok(mut state) = self.status.lock() {
            *state = "connected".into();
        }
        let response: Value = match serde_json::from_slice(bytes) {
            Ok(value) => value,
            Err(_) if status != 200 => Value::Null,
            Err(_) => return Err("NATS_UNAVAILABLE: Invalid Hub response JSON".into()),
        };
        if status != 200 || response["ok"] == false || !response["error"].is_null() {
            let fallback = match status {
                401 => "UNAUTHORIZED",
                403 => "FORBIDDEN",
                200 | 500..=599 => "NATS_UNAVAILABLE",
                _ => "INVALID_INPUT",
            };
            return Err(hub_error(
                response["error"]["code"].as_str().unwrap_or(fallback),
                response["error"]["message"]
                    .as_str()
                    .or_else(|| response["message"].as_str())
                    .unwrap_or("Hub rejected request"),
            ));
        }
        if response["ok"] != true {
            return Err("NATS_UNAVAILABLE: Invalid Hub RPC envelope".into());
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| "NATS_UNAVAILABLE: Missing Hub RPC result".into())
    }
    fn rpc_http(
        &self,
        op: &str,
        params: &Value,
        authorization: Option<&str>,
    ) -> Result<crate::plugins::HttpResponse, String> {
        let url = url::Url::parse(&format!(
            "{}/v1/rpc",
            string(&self.config, "url").trim_end_matches('/')
        ))
        .map_err(|e| e.to_string())?;
        let host = url.host_str().ok_or("Invalid Hub URL")?;
        let port = url.port_or_known_default().ok_or("Invalid Hub port")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let result = (|| {
            let socket = connect_http(&url, deadline).map_err(|e| e.to_string())?;
            socket
                .set_read_timeout(Some(Duration::from_millis(100)))
                .map_err(|e| e.to_string())?;
            socket
                .set_write_timeout(Some(Duration::from_millis(100)))
                .map_err(|e| e.to_string())?;
            let mut stream = InterruptibleStream {
                socket,
                client: self,
                deadline,
            };
            let body = json!({"op":op,"params":params}).to_string();
            let bearer = authorization
                .map(|v| format!("Authorization: {v}\r\n"))
                .unwrap_or_default();
            write!(stream,"POST {} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nX-Cito-User: {}\r\n{bearer}Connection: close\r\nContent-Length: {}\r\n\r\n{body}",url.path(),string(&self.identity,"owner_user"),body.len()).map_err(|e| e.to_string())?;
            crate::plugins::read_http_response(&mut stream)
        })();
        result.map_err(|e| self.disconnected(e))
    }
    fn reject_token(&self, bearer: &Option<String>) {
        // An old in-flight request must not reject a newly verified credential.
        let current = self.event_credentials.lock();
        if current.as_ref().is_ok_and(|token| **token == *bearer) {
            if let Ok(mut rejected) = self.rejected_bearer.lock() {
                *rejected = Some(super::identity::hash(bearer.as_deref().unwrap_or("")));
            }
        }
    }
    /// Preserve the authority-owned observations and running stream on renewal.
    pub fn inherit_authority(&mut self, old: &Self) {
        self.directory_cache = old.directory_cache.clone();
        self.directory_revision = old.directory_revision.clone();
        self.directory_updates = old.directory_updates.clone();
        self.server_clock = old.server_clock.clone();
        self.status = old.status.clone();
        self.stream_live = old.stream_live.clone();
        self.event_revision = old.event_revision.clone();
        self.event_wait = old.event_wait.clone();
        self.local_event_wake = old.local_event_wake.clone();
        self.event_socket = old.event_socket.clone();
        self.rejected_bearer = old.rejected_bearer.clone();
        self.directory_observed_at = old.directory_observed_at.clone();
        self.human_observed_at = old.human_observed_at.clone();
        self.closed = old.closed.clone();
        self.commit_gate = old.commit_gate.clone();
        self.event_credentials = old.event_credentials.clone();
        if let Ok(mut token) = self.event_credentials.lock() {
            *token = self.bearer.clone();
            // Verification of the replacement supersedes any old-token rejection
            // that raced recovery. reject_token uses this same lock.
            if let Ok(mut rejected) = self.rejected_bearer.lock() {
                *rejected = None;
            }
        }
        let _guard = self.event_wait.0.lock().unwrap_or_else(|e| e.into_inner());
        self.event_wait.1.notify_all();
    }
    fn observation_recent(&self, now: Instant) -> bool {
        self.local.is_some()
            || self.directory_observed_at.lock().is_ok_and(|at| {
                at.is_some_and(|at| now.duration_since(at) < Duration::from_secs(60))
            })
    }
    pub(super) fn human_observation_current(&self) -> bool {
        self.auth_valid()
            && (self.local.is_some()
                || self
                    .human_observed_at
                    .lock()
                    .is_ok_and(|at| at.is_some_and(|at| at.elapsed() < Duration::from_secs(60))))
    }
    fn auth_valid(&self) -> bool {
        (self.config["authMode"] != "oidc"
            || (self.bearer.is_some() && self.expires_at > crate::plugins::now_ms() / 1000))
            && self
                .rejected_bearer
                .lock()
                .is_ok_and(|rejected| rejected.is_none())
    }
    pub fn capabilities(&self) -> Value {
        self.capabilities_at(Instant::now())
    }
    fn capabilities_at(&self, now: Instant) -> Value {
        let authenticated = self.auth_valid();
        let rpc = self
            .status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| "disconnected".into());
        let stream = self.stream_live.load(Ordering::SeqCst);
        let current = authenticated && self.observation_recent(now);
        let status = if !authenticated {
            "auth_required"
        } else if self.local.is_some() || rpc == "connected" || stream {
            "connected"
        } else {
            &rpc
        };
        json!({"status":status,"auth_valid":authenticated,"rpc_status":rpc,"sse_live":stream,"observation_current":current})
    }
    pub fn connection_status(&self) -> String {
        self.capabilities()["status"]
            .as_str()
            .unwrap_or("disconnected")
            .into()
    }
    pub fn cached_directory(&self) -> Vec<Value> {
        self.directory_cache
            .lock()
            .map(|rows| {
                let mut rows = rows.clone();
                if let Ok(clock) = self.server_clock.lock() {
                    if let Some((now, observed)) = *clock {
                        for row in &mut rows {
                            row["server_now_ms"] = json!(now.saturating_add(
                                observed.elapsed().as_millis().min(u64::MAX as u128) as u64
                            ));
                        }
                    }
                }
                rows
            })
            .unwrap_or_default()
    }
    pub fn server_now(&self) -> u64 {
        self.server_clock
            .lock()
            .ok()
            .and_then(|clock| *clock)
            .map(|(now, observed)| {
                now.saturating_add(observed.elapsed().as_millis().min(u64::MAX as u128) as u64)
            })
            .unwrap_or(0)
    }
    fn observe_server_clock(&self, response: &Value) {
        if let Some(now) = response["server_now_ms"].as_u64() {
            if let Ok(mut clock) = self.server_clock.lock() {
                let previous = clock
                    .map(|(time, observed)| {
                        time.saturating_add(
                            observed.elapsed().as_millis().min(u64::MAX as u128) as u64
                        )
                    })
                    .unwrap_or(0);
                *clock = Some((now.max(previous), Instant::now()));
            }
        }
    }
    pub fn directory_revision(&self) -> u64 {
        self.directory_revision.load(Ordering::Acquire)
    }
    pub fn event_revision(&self) -> u64 {
        self.event_revision.load(Ordering::Acquire)
    }
    fn mutation(&self) {
        {
            let _guard = self.event_wait.0.lock().unwrap_or_else(|e| e.into_inner());
            self.event_revision.fetch_add(1, Ordering::Release);
            self.event_wait.1.notify_all();
        }
        super::daemon::wake_daemon();
    }
    pub fn wait_for_event(&self, revision: u64, deadline: Instant) {
        let guard = self.event_wait.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self
            .event_wait
            .1
            .wait_timeout_while(
                guard,
                deadline.saturating_duration_since(Instant::now()),
                |_| self.event_revision() == revision && !self.closed.load(Ordering::SeqCst),
            )
            .unwrap_or_else(|e| e.into_inner());
    }
    pub fn close(&self) {
        if let Ok(_guard) = self.event_wait.0.lock() {
            self.closed.store(true, Ordering::SeqCst);
            if let Some(socket) = self
                .event_socket
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                let _ = socket.shutdown(Shutdown::Both);
            }
            self.event_wait.1.notify_all();
            if let Ok(wake) = self.local_event_wake.lock() {
                if let Some(wake) = &*wake {
                    wake.notify();
                }
            }
        }
    }
    fn open_events(
        &self,
        url: &url::Url,
        bearer: Option<&str>,
        tls: &Arc<rustls::ClientConfig>,
    ) -> io::Result<(u16, Box<dyn Read>)> {
        let user = string(&self.identity, "owner_user");
        let auth = authorization(bearer)?;
        if !matches!(url.scheme(), "http" | "https") || user.chars().any(char::is_control) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let socket = connect_http(url, Instant::now() + Duration::from_secs(5))?;
        {
            // Publish before any request I/O. A concurrent close either takes
            // this socket or wins first and prevents its publication.
            let mut active = self.event_socket.lock().unwrap_or_else(|e| e.into_inner());
            if self.closed.load(Ordering::SeqCst) {
                return Err(io::ErrorKind::ConnectionAborted.into());
            }
            *active = Some(socket.try_clone()?);
        }
        socket.set_write_timeout(Some(Duration::from_secs(1)))?;
        if url.scheme() == "https" {
            socket.set_read_timeout(Some(Duration::from_millis(150)))?;
            // URL hosts bracket IPv6 literals; rustls expects the unbracketed IP.
            let host = url
                .host_str()
                .ok_or(io::ErrorKind::InvalidInput)?
                .trim_matches(['[', ']']);
            let name = rustls::pki_types::ServerName::try_from(host.to_owned())
                .map_err(io::Error::other)?;
            let conn =
                rustls::ClientConnection::new(tls.clone(), name).map_err(io::Error::other)?;
            Self::event_response(
                rustls::StreamOwned::new(conn, socket),
                url,
                user,
                auth.as_deref(),
            )
        } else {
            socket.set_read_timeout(Some(Duration::from_secs(1)))?;
            Self::event_response(socket, url, user, auth.as_deref())
        }
    }

    // Both transports expose raw HTTP bytes here, before any chunk decoding.
    fn event_response(
        mut stream: impl Read + Write + 'static,
        url: &url::Url,
        user: &str,
        auth: Option<&str>,
    ) -> io::Result<(u16, Box<dyn Read>)> {
        let authority = &url[url::Position::BeforeHost..url::Position::AfterPort];
        write!(stream, "GET {} HTTP/1.1\r\nHost: {authority}\r\nAccept: text/event-stream\r\nAccept-Encoding: identity\r\nX-Cito-User: {user}\r\nConnection: close\r\n", &url[url::Position::BeforePath..url::Position::AfterQuery])?;
        if let Some(value) = auth {
            write!(stream, "Authorization: {value}\r\n")?;
        }
        stream.write_all(b"\r\n")?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut headers = reader.by_ref().take(65_536);
        let mut line = String::new();
        headers.read_line(&mut line)?;
        let (version, rest) = line.split_once(' ').ok_or(io::ErrorKind::InvalidData)?;
        if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let status: u16 = rest
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or(io::ErrorKind::InvalidData)?;
        let mut length = None;
        let mut chunked = false;
        loop {
            line.clear();
            headers.read_line(&mut line)?;
            if line == "\r\n" {
                break;
            }
            let (name, value) = line
                .strip_suffix("\r\n")
                .and_then(|s| s.split_once(':'))
                .ok_or(io::ErrorKind::InvalidData)?;
            if name.eq_ignore_ascii_case("content-length") {
                let n = value
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| io::ErrorKind::InvalidData)?;
                if length.replace(n).is_some() {
                    return Err(io::ErrorKind::InvalidData.into());
                }
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                if chunked || !value.trim().eq_ignore_ascii_case("chunked") {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                chunked = true;
            }
        }
        let body: Box<dyn Read> = match (chunked, length) {
            (true, None) => Box::new(ChunkedEvents {
                reader,
                remaining: 0,
                terminator: false,
                line: Vec::new(),
            }),
            (false, Some(n)) => Box::new(reader.take(n)),
            (false, None) => Box::new(reader),
            (true, Some(_)) => return Err(io::ErrorKind::InvalidData.into()),
        };
        Ok((status, body))
    }
    pub fn start_events(&self) -> Option<thread::JoinHandle<()>> {
        self.start_events_with(Duration::from_secs(90), EVENT_TLS.clone())
    }
    fn start_events_with(
        &self,
        idle_timeout: Duration,
        tls: Arc<rustls::ClientConfig>,
    ) -> Option<thread::JoinHandle<()>> {
        if let Some(local) = &self.local {
            let mut subscription = local.subscribe(Some(self.identity.clone())).ok()?;
            if let Ok(mut wake) = self.local_event_wake.lock() {
                *wake = Some(subscription.wake.clone());
            }
            let client = self.clone();
            client.stream_live.store(true, Ordering::SeqCst);
            return Some(thread::spawn(move || {
                while !client.closed.load(Ordering::SeqCst) {
                    let mut fds = [libc::pollfd {
                        fd: subscription.wake.read_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    }];
                    let _ = crate::support::poll_until(&mut fds, None);
                    subscription.wake.drain();
                    let mut changed = false;
                    let disconnected = loop {
                        match subscription.try_recv() {
                            Ok(_) => changed = true,
                            Err(std::sync::mpsc::TryRecvError::Empty) => break false,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => break true,
                        }
                    };
                    if changed || disconnected {
                        client.mutation();
                    }
                    if disconnected && !client.closed.load(Ordering::SeqCst) {
                        let Some(local) = &client.local else { break };
                        let Ok(next) = local.subscribe(Some(client.identity.clone())) else {
                            break;
                        };
                        subscription = next;
                        if let Ok(mut wake) = client.local_event_wake.lock() {
                            *wake = Some(subscription.wake.clone());
                            if client.closed.load(Ordering::SeqCst) {
                                subscription.wake.notify();
                            }
                        }
                    }
                }
                client.stream_live.store(false, Ordering::SeqCst);
            }));
        }
        if let Ok(mut token) = self.event_credentials.lock() {
            *token = self.bearer.clone();
        }
        let url = url::Url::parse(&format!(
            "{}/v1/events",
            string(&self.config, "url").trim_end_matches('/')
        ))
        .ok()?;
        let client = self.clone();
        Some(thread::spawn(move || {
            let mut backoff = crate::support::RetryBackoff::default();
            while !client.closed.load(Ordering::SeqCst) {
                let bearer = client
                    .event_credentials
                    .lock()
                    .ok()
                    .and_then(|token| token.clone());
                let response = client.open_events(&url, bearer.as_deref(), &tls);
                let status = response.as_ref().ok().map(|(status, _)| *status);
                if let Some(body) = response
                    .ok()
                    .and_then(|(status, body)| (status == 200).then_some(body))
                {
                    client.stream_live.store(true, Ordering::SeqCst);
                    let mut reader = BufReader::new(body);
                    let connected = Instant::now();
                    let mut last_input = connected;
                    // Only the SSE field name matters. Consume each fragment before
                    // checking cancellation, even for an unterminated data line.
                    let mut prefix = Vec::with_capacity(5);
                    while !client.closed.load(Ordering::SeqCst)
                        && last_input.elapsed() < idle_timeout
                    {
                        let bytes = match reader.fill_buf() {
                            Ok([]) => break,
                            Ok(bytes) => bytes,
                            Err(e)
                                if matches!(
                                    e.kind(),
                                    io::ErrorKind::TimedOut
                                        | io::ErrorKind::WouldBlock
                                        | io::ErrorKind::Interrupted
                                ) =>
                            {
                                continue
                            }
                            Err(_) => break,
                        };
                        last_input = Instant::now();
                        let newline = bytes.iter().position(|b| *b == b'\n');
                        let n = newline.map_or(bytes.len(), |i| i + 1);
                        prefix.extend_from_slice(&bytes[..n.min(5 - prefix.len())]);
                        reader.consume(n);
                        if newline.is_some() {
                            if prefix == b"data:" {
                                backoff.reset();
                                client.mutation();
                            } else if connected.elapsed() >= Duration::from_secs(30) {
                                backoff.reset();
                            }
                            prefix.clear();
                        }
                    }
                }
                client
                    .event_socket
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                client.stream_live.store(false, Ordering::SeqCst);
                if bearer.is_some() && status == Some(401) {
                    client.reject_token(&bearer);
                }
                if matches!(status, Some(401 | 403 | 409)) {
                    let guard = client
                        .event_wait
                        .0
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _guard = client
                        .event_wait
                        .1
                        .wait_while(guard, |_| {
                            !client.closed.load(Ordering::SeqCst)
                                && client
                                    .event_credentials
                                    .lock()
                                    .is_ok_and(|token| *token == bearer)
                        })
                        .unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                if client.closed.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(guard) = client.event_wait.0.lock() {
                    let _ = client
                        .event_wait
                        .1
                        .wait_timeout_while(guard, backoff.failed(), |_| {
                            !client.closed.load(Ordering::SeqCst)
                        });
                }
            }
        }))
    }
    pub fn human_request(&self, op: &str, params: &Value) -> Result<Value, String> {
        let _update = self
            .directory_updates
            .lock()
            .map_err(|_| "STATE_ERROR: Directory update lock poisoned")?;
        let result = match self.rpc(op, params) {
            Ok(value) => value,
            Err(error) => {
                if let Some(state) = error
                    .strip_prefix("CONTROL_REVISION_CONFLICT: ")
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                {
                    self.cache_human(&state);
                }
                return Err(error);
            }
        };
        if !result["record"].is_object() || !result["record"]["listener_id"].is_string() {
            return Err("NATS_UNAVAILABLE: Invalid Human response".into());
        }
        self.cache_human(&result);
        Ok(result)
    }
    fn cache_human(&self, result: &Value) {
        if !result["record"]["listener_id"].is_string() {
            return;
        }
        self.observe_server_clock(result);
        if let Ok(mut observed) = self.human_observed_at.lock() {
            *observed = Some(Instant::now());
        }
        if result["record"].is_object() {
            let mut record = result["record"].clone();
            record["server_now_ms"] = result["server_now_ms"].clone();
            if let Ok(mut cache) = self.directory_cache.lock() {
                cache.retain(|row| row["listener_id"] != record["listener_id"]);
                cache.push(record);
                self.directory_revision.fetch_add(1, Ordering::Release);
            }
        }
    }
    pub fn kv_get(&self, id: &str) -> Result<Option<(Value, u64)>, String> {
        let value = self.rpc("kv_get", &json!({"key":id}))?;
        self.observe_server_clock(&value);
        if value.is_null() {
            return Ok(None);
        }
        if value["forbidden_scope"] == true {
            return Err("FORBIDDEN_TARGET_SCOPE: Target is private".into());
        }
        let bytes = STANDARD
            .decode(string(&value, "value_b64"))
            .map_err(|e| e.to_string())?;
        let record: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        Ok(Some((record, value["revision"].as_u64().unwrap_or(0))))
    }
    pub fn kv_save(&self, id: &str, value: &Value, revision: Option<u64>) -> Result<Value, String> {
        let mut value = value.clone();
        if let Some(fields) = value.as_object_mut() {
            fields.remove("server_now_ms");
        }
        let data = STANDARD.encode(serde_json::to_vec(&value).map_err(|e| e.to_string())?);
        self.rpc(
            if revision.is_some() {
                "kv_update"
            } else {
                "kv_create"
            },
            &json!({"key":id,"data_b64":data,"revision":revision.unwrap_or(0)}),
        )
    }
    pub fn directory(&self) -> Result<Vec<Value>, String> {
        let _update = self
            .directory_updates
            .lock()
            .map_err(|_| "STATE_ERROR: Directory update lock poisoned")?;
        let mut cursor: Option<String> = None;
        let mut rows = Vec::new();
        loop {
            let raw = self.rpc("kv_snapshot", &json!({"after_key":cursor}))?;
            if !raw["next_cursor"].is_null() && !raw["next_cursor"].is_string() {
                return Err("NATS_UNAVAILABLE: Invalid snapshot cursor".into());
            }
            let items = raw
                .as_array()
                .or_else(|| raw["entries"].as_array())
                .ok_or("NATS_UNAVAILABLE: Missing directory entries")?;
            for item in items {
                if let Some(id) = item["listener_id"].as_str() {
                    if !id.is_empty() {
                        rows.push(item.clone());
                    }
                    continue;
                }
                if raw.is_array() && item.get("value_b64").is_none() {
                    continue;
                }
                let data = STANDARD
                    .decode(
                        item["value_b64"]
                            .as_str()
                            .ok_or("NATS_UNAVAILABLE: Missing directory record")?,
                    )
                    .map_err(|e| e.to_string())?;
                // KV also carries arbitrary bytes and non-registration JSON.
                if let Ok(row) = serde_json::from_slice::<Value>(&data) {
                    if row["listener_id"].as_str().is_some_and(|id| !id.is_empty()) {
                        rows.push(row);
                    }
                }
            }
            let next = raw["next_cursor"].as_str().map(str::to_owned);
            if next.is_some() && next <= cursor {
                return Err("NATS_UNAVAILABLE: Snapshot cursor did not advance".into());
            }
            cursor = next;
            if cursor.is_none() {
                if let Some(now) = raw["server_now_ms"].as_u64() {
                    for row in &mut rows {
                        row["server_now_ms"] = json!(now);
                    }
                }
                self.observe_server_clock(&raw);
                break;
            }
        }
        if let Ok(mut cache) = self.directory_cache.lock() {
            if *cache != rows {
                *cache = rows.clone();
                self.directory_revision.fetch_add(1, Ordering::Release);
            }
        }
        let now = Instant::now();
        if let Ok(mut observed) = self.directory_observed_at.lock() {
            *observed = Some(now);
        }
        if let Ok(mut observed) = self.human_observed_at.lock() {
            *observed = Some(now);
        }
        Ok(rows)
    }
}
