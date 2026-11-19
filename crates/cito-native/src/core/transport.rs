use super::hub::HubEngine;
use super::identity::{principal, string};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
    pub auth_rejected_hash: Option<String>,
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
    observed_at: Arc<Mutex<Option<Instant>>>,
    event_credentials: Arc<Mutex<Option<String>>>,
    pub closed: Arc<AtomicBool>,
    pub registration: Arc<Mutex<()>>,
    agent: ureq::Agent,
}
impl HubClient {
    pub fn new(name: String, config: Value, user: &str, local: Option<Arc<HubEngine>>) -> Self {
        let bearer = config["token"].as_str().map(str::to_string);
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
            auth_rejected_hash: None,
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
            observed_at: Arc::new(Mutex::new(None)),
            closed: Arc::new(AtomicBool::new(false)),
            registration: Arc::new(Mutex::new(())),
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
        if string(&self.config, "url").starts_with("http://") {
            return self.rpc_http(op, params);
        }
        let mut request = self
            .agent
            .post(&format!(
                "{}/v1/rpc",
                string(&self.config, "url").trim_end_matches('/')
            ))
            .timeout(Duration::from_secs(5))
            .set("X-Cito-User", string(&self.identity, "owner_user"));
        let bearer = self
            .event_credentials
            .lock()
            .map_err(|_| "STATE_ERROR: Credentials lock poisoned")?
            .clone();
        if let Some(token) = bearer.as_deref().filter(|s| !s.is_empty()) {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        let result = match request.send_json(json!({"op":op,"params":params})) {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(error) => {
                if let Ok(mut status) = self.status.lock() {
                    *status = "reconnecting".into();
                }
                return Err(format!("NATS_UNAVAILABLE: {error}"));
            }
        };
        let status = result.status();
        let mut bytes = Vec::new();
        result
            .into_reader()
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("Hub response exceeds frame limit".into());
        }
        self.parse_rpc_response(status, &bytes, &bearer)
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
            Err(error) => return Err(error.to_string()),
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
        Ok(response.get("result").cloned().unwrap_or(response))
    }
    fn rpc_http(&self, op: &str, params: &Value) -> Result<Value, String> {
        let url = url::Url::parse(&format!(
            "{}/v1/rpc",
            string(&self.config, "url").trim_end_matches('/')
        ))
        .map_err(|e| e.to_string())?;
        let host = url.host_str().ok_or("Invalid Hub URL")?;
        let port = url.port_or_known_default().ok_or("Invalid Hub port")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let addresses = url.socket_addrs(|| None).map_err(|e| e.to_string())?;
        let credential = self
            .event_credentials
            .lock()
            .map_err(|_| "STATE_ERROR: Credentials lock poisoned")?
            .clone();
        let result = (|| {
            let mut connected = None;
            let mut last_error = "Hub address unavailable".to_owned();
            for address in addresses {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match std::net::TcpStream::connect_timeout(
                    &address,
                    remaining.min(Duration::from_millis(500)),
                ) {
                    Ok(socket) => {
                        connected = Some(socket);
                        break;
                    }
                    Err(error) => last_error = error.to_string(),
                }
            }
            let socket = connected.ok_or_else(|| format!("NATS_UNAVAILABLE: {last_error}"))?;
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
            let bearer = credential
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(|v| format!("Authorization: Bearer {v}\r\n"))
                .unwrap_or_default();
            write!(stream,"POST {} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nX-Cito-User: {}\r\n{bearer}Connection: close\r\nContent-Length: {}\r\n\r\n{body}",url.path(),string(&self.identity,"owner_user"),body.len()).map_err(|e|format!("NATS_UNAVAILABLE: {e}"))?;
            crate::plugins::read_http_response(&mut stream)
                .map_err(|e| format!("NATS_UNAVAILABLE: {e}"))
        })();
        if result
            .as_ref()
            .is_err_and(|e| e.starts_with("NATS_UNAVAILABLE"))
        {
            if let Ok(mut status) = self.status.lock() {
                *status = "disconnected".into();
            }
        }
        let reply = result?;
        self.parse_rpc_response(reply.status, reply.body.as_bytes(), &credential)
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
        self.rejected_bearer = old.rejected_bearer.clone();
        self.observed_at = old.observed_at.clone();
        self.closed = old.closed.clone();
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
    fn observation_recent(&self) -> bool {
        self.local.is_some()
            || self
                .observed_at
                .lock()
                .is_ok_and(|at| at.is_some_and(|at| at.elapsed() < Duration::from_secs(60)))
    }
    pub fn observation_current(&self) -> bool {
        self.auth_valid() && self.observation_recent()
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
        let authenticated = self.auth_valid();
        let rpc = self
            .status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| "disconnected".into());
        let stream = self.stream_live.load(Ordering::SeqCst);
        let current = authenticated && self.observation_recent();
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
            self.event_wait.1.notify_all();
            if let Ok(wake) = self.local_event_wake.lock() {
                if let Some(wake) = &*wake {
                    wake.notify();
                }
            }
        }
    }
    pub fn start_events(&self) -> Option<thread::JoinHandle<()>> {
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
        let mut client = self.clone();
        client.agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(1))
            .build();
        Some(thread::spawn(move || {
            let mut backoff = crate::support::RetryBackoff::default();
            while !client.closed.load(Ordering::SeqCst) {
                let url = format!(
                    "{}/v1/events",
                    string(&client.config, "url").trim_end_matches('/')
                );
                let mut request = client
                    .agent
                    .get(&url)
                    .set("X-Cito-User", string(&client.identity, "owner_user"));
                let bearer = client
                    .event_credentials
                    .lock()
                    .ok()
                    .and_then(|token| token.clone());
                if let Some(token) = bearer.as_deref().filter(|s| !s.is_empty()) {
                    request = request.set("Authorization", &format!("Bearer {token}"));
                }
                let response = request.call();
                if bearer.is_some() && matches!(&response, Err(ureq::Error::Status(401, _))) {
                    client.reject_token(&bearer);
                }
                if matches!(&response, Err(ureq::Error::Status(401 | 403 | 409, _))) {
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
                if let Ok(response) = response {
                    client.stream_live.store(true, Ordering::SeqCst);
                    let mut reader = BufReader::new(response.into_reader());
                    let connected = Instant::now();
                    let mut line = Vec::new();
                    loop {
                        let remaining = 65_536usize.saturating_sub(line.len());
                        if remaining == 0 {
                            // The stream carries invalidation hints, not authoritative payloads.
                            // Drain an oversized hint without growing memory or reconnecting.
                            let mut chunk = [0u8; 8192];
                            let drained = loop {
                                if client.closed.load(Ordering::SeqCst) {
                                    break false;
                                }
                                match reader.fill_buf() {
                                    Ok([]) => break false,
                                    Ok(bytes) => {
                                        let count = bytes.len().min(chunk.len());
                                        chunk[..count].copy_from_slice(&bytes[..count]);
                                        let newline =
                                            chunk[..count].iter().position(|b| *b == b'\n');
                                        reader.consume(newline.map_or(count, |n| n + 1));
                                        if newline.is_some() {
                                            break true;
                                        }
                                    }
                                    Err(error)
                                        if matches!(
                                            error.kind(),
                                            io::ErrorKind::TimedOut
                                                | io::ErrorKind::WouldBlock
                                                | io::ErrorKind::Interrupted
                                        ) =>
                                    {
                                        continue
                                    }
                                    Err(_) => break false,
                                }
                            };
                            if !drained {
                                break;
                            }
                            if line.starts_with(b"data:") {
                                client.mutation();
                                backoff.reset();
                            }
                            line.clear();
                            continue;
                        }
                        let result = reader
                            .by_ref()
                            .take(remaining as u64)
                            .read_until(b'\n', &mut line);
                        if client.closed.load(Ordering::SeqCst) {
                            break;
                        }
                        if matches!(&result, Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted))
                        {
                            continue;
                        }
                        if matches!(result, Ok(n) if n > 0 && !line.ends_with(b"\n")) {
                            continue;
                        }
                        if !matches!(result, Ok(n) if n > 0) {
                            break;
                        }
                        if connected.elapsed() >= Duration::from_secs(30) {
                            backoff.reset();
                        }
                        if line.starts_with(b"data:") {
                            backoff.reset();
                            // Hints invalidate observations; only the durable consumer advances cursors.
                            client.mutation();
                        }
                        line.clear();
                    }
                }
                client.stream_live.store(false, Ordering::SeqCst);
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
        self.cache_human(&result);
        Ok(result)
    }
    fn cache_human(&self, result: &Value) {
        self.observe_server_clock(result);
        if let Ok(mut observed) = self.observed_at.lock() {
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
            self.observe_server_clock(&raw);
            let items = raw
                .as_array()
                .or_else(|| raw["entries"].as_array())
                .cloned()
                .unwrap_or_default();
            for item in items {
                if item.get("listener_id").is_some() {
                    rows.push(item);
                    continue;
                }
                let data = STANDARD
                    .decode(string(&item, "value_b64"))
                    .map_err(|e| e.to_string())?;
                let mut row: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
                row["server_now_ms"] = raw["server_now_ms"].clone();
                rows.push(row);
            }
            let next = raw["next_cursor"].as_str().map(str::to_owned);
            if next.is_some() && next <= cursor {
                return Err("NATS_UNAVAILABLE: Snapshot cursor did not advance".into());
            }
            cursor = next;
            if cursor.is_none() {
                break;
            }
        }
        if let Ok(mut cache) = self.directory_cache.lock() {
            if *cache != rows {
                *cache = rows.clone();
                self.directory_revision.fetch_add(1, Ordering::Release);
            }
        }
        if let Ok(mut observed) = self.observed_at.lock() {
            *observed = Some(Instant::now());
        }
        Ok(rows)
    }
}
impl Drop for HubClient {
    fn drop(&mut self) {
        // Event worker owns another clone; DaemonService explicitly sets closed before join.
    }
}
