use super::hub::HubEngine;
use super::identity::{principal, string};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

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
    pub local: Option<Arc<HubEngine>>,
    pub status: Arc<Mutex<String>>,
    pub closed: Arc<AtomicBool>,
    pub registration: Arc<Mutex<()>>,
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
            bearer,
            expires_at: u64::MAX,
            auth_retry_at: 0,
            auth_rejected_hash: None,
            rejected_bearer: Arc::new(Mutex::new(None)),
            directory_cache: Arc::new(Mutex::new(Vec::new())),
            local,
            status: Arc::new(Mutex::new(status.into())),
            closed: Arc::new(AtomicBool::new(false)),
            registration: Arc::new(Mutex::new(())),
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
        let mut request = ureq::post(&format!(
            "{}/v1/rpc",
            string(&self.config, "url").trim_end_matches('/')
        ))
        .timeout(Duration::from_secs(5))
        .set("X-Cito-User", string(&self.identity, "owner_user"));
        if let Some(token) = self.bearer.as_deref().filter(|s| !s.is_empty()) {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        let result = match request.send_json(json!({"op":op,"params":params})) {
            Ok(response) => response,
            Err(ureq::Error::Status(status, response)) => {
                if status == 401 {
                    self.reject_bearer();
                }
                if let Ok(mut state) = self.status.lock() {
                    *state = if status == 401 {
                        "auth_required".into()
                    } else {
                        "connected".into()
                    };
                }
                let body: Value = response.into_json().unwrap_or(Value::Null);
                let code = body["error"]["code"].as_str().unwrap_or(if status == 401 {
                    "UNAUTHORIZED"
                } else if status == 403 {
                    "FORBIDDEN"
                } else {
                    "INVALID_INPUT"
                });
                return Err(format!(
                    "{code}: {}",
                    body["error"]["message"]
                        .as_str()
                        .unwrap_or("Hub rejected request")
                ));
            }
            Err(error) => {
                if let Ok(mut status) = self.status.lock() {
                    *status = "reconnecting".into();
                }
                return Err(format!("NATS_UNAVAILABLE: {error}"));
            }
        };
        let mut bytes = Vec::new();
        result
            .into_reader()
            .take(262_145)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 262_144 {
            return Err("Hub response exceeds frame limit".into());
        }
        let response: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if response["ok"] == false || response.get("error").is_some() {
            return Err(response.to_string());
        }
        if let Ok(mut status) = self.status.lock() {
            *status = "connected".into();
        }
        Ok(response.get("result").cloned().unwrap_or(response))
    }
    fn rpc_http(&self, op: &str, params: &Value) -> Result<Value, String> {
        use std::net::ToSocketAddrs;
        let url = url::Url::parse(&format!(
            "{}/v1/rpc",
            string(&self.config, "url").trim_end_matches('/')
        ))
        .map_err(|e| e.to_string())?;
        let host = url.host_str().ok_or("Invalid Hub URL")?;
        let port = url.port_or_known_default().ok_or("Invalid Hub port")?;
        let address = (host, port)
            .to_socket_addrs()
            .map_err(|e| e.to_string())?
            .next()
            .ok_or("Hub address unavailable")?;
        let result = (|| -> Result<Value, String> {
            let socket = std::net::TcpStream::connect_timeout(&address, Duration::from_millis(500))
                .map_err(|e| format!("NATS_UNAVAILABLE: {e}"))?;
            socket
                .set_read_timeout(Some(Duration::from_millis(100)))
                .map_err(|e| e.to_string())?;
            socket
                .set_write_timeout(Some(Duration::from_millis(100)))
                .map_err(|e| e.to_string())?;
            let mut stream = InterruptibleStream {
                socket,
                client: self,
                deadline: Instant::now() + Duration::from_secs(10),
            };
            let body = json!({"op":op,"params":params}).to_string();
            let bearer = self
                .bearer
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(|v| format!("Authorization: Bearer {v}\r\n"))
                .unwrap_or_default();
            write!(stream,"POST {} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nX-Cito-User: {}\r\n{bearer}Connection: close\r\nContent-Length: {}\r\n\r\n{body}",url.path(),string(&self.identity,"owner_user"),body.len()).map_err(|e|format!("NATS_UNAVAILABLE: {e}"))?;
            let reply = crate::plugins::read_http_response(&mut stream)
                .map_err(|e| format!("NATS_UNAVAILABLE: {e}"))?;
            let response: Value =
                serde_json::from_str(&reply.body).map_err(|e| format!("NATS_UNAVAILABLE: {e}"))?;
            if reply.status != 200 || response["ok"] == false {
                if reply.status == 401 {
                    self.reject_bearer();
                }
                let code = response["error"]["code"]
                    .as_str()
                    .unwrap_or("NATS_UNAVAILABLE");
                if let Ok(mut status) = self.status.lock() {
                    *status = if reply.status == 401 {
                        "auth_required"
                    } else {
                        "connected"
                    }
                    .into();
                }
                return Err(format!("{code}: {}", string(&response["error"], "message")));
            }
            if let Ok(mut status) = self.status.lock() {
                *status = "connected".into();
            }
            Ok(response.get("result").cloned().unwrap_or(response))
        })();
        if result
            .as_ref()
            .is_err_and(|e| e.starts_with("NATS_UNAVAILABLE"))
        {
            if let Ok(mut status) = self.status.lock() {
                *status = "disconnected".into();
            }
        }
        result
    }
    fn reject_bearer(&self) {
        if let Ok(mut rejected) = self.rejected_bearer.lock() {
            *rejected = Some(super::identity::hash(self.bearer.as_deref().unwrap_or("")));
        }
    }
    pub fn connection_status(&self) -> String {
        if self
            .rejected_bearer
            .lock()
            .is_ok_and(|rejected| rejected.is_some())
        {
            return "auth_required".into();
        }
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| "disconnected".into())
    }
    pub fn cached_directory(&self) -> Vec<Value> {
        self.directory_cache
            .lock()
            .map(|rows| rows.clone())
            .unwrap_or_default()
    }
    pub fn start_events(&self) -> Option<thread::JoinHandle<()>> {
        if self.local.is_some() {
            return None;
        }
        let client = self.clone();
        Some(thread::spawn(move || {
            let mut delay = 250;
            while !client.closed.load(Ordering::SeqCst) {
                let url = format!(
                    "{}/v1/events",
                    string(&client.config, "url").trim_end_matches('/')
                );
                let mut request = ureq::get(&url)
                    .timeout(Duration::from_millis(500))
                    .set("X-Cito-User", string(&client.identity, "owner_user"));
                if let Some(token) = client.bearer.as_deref().filter(|s| !s.is_empty()) {
                    request = request.set("Authorization", &format!("Bearer {token}"));
                }
                if let Ok(response) = request.call() {
                    if let Ok(mut s) = client.status.lock() {
                        *s = "connected".into();
                    }
                    delay = 250;
                    let reader = BufReader::new(response.into_reader());
                    for line in reader.lines() {
                        if client.closed.load(Ordering::SeqCst) {
                            break;
                        }
                        if line.is_err() {
                            break;
                        }
                        // The durable consumer backlog is authoritative; events only reduce
                        // reconnection latency and never advance the persisted cursor.
                    }
                }
                if client.closed.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(mut s) = client.status.lock() {
                    *s = "reconnecting".into();
                }
                for _ in 0..delay / 25 {
                    if client.closed.load(Ordering::SeqCst) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                delay = (delay * 2).min(2000);
            }
        }))
    }
    pub fn kv_get(&self, id: &str) -> Result<Option<(Value, u64)>, String> {
        let value = self.rpc("kv_get", &json!({"key":id}))?;
        if value.is_null() {
            return Ok(None);
        }
        if value["forbidden_scope"] == true {
            return Err("FORBIDDEN_TARGET_SCOPE: Target is private".into());
        }
        let bytes = STANDARD
            .decode(string(&value, "value_b64"))
            .map_err(|e| e.to_string())?;
        Ok(Some((
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
            value["revision"].as_u64().unwrap_or(0),
        )))
    }
    pub fn kv_save(&self, id: &str, value: &Value, revision: Option<u64>) -> Result<Value, String> {
        let data = STANDARD.encode(serde_json::to_vec(value).map_err(|e| e.to_string())?);
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
        let raw = self.rpc("kv_snapshot", &json!({}))?;
        let items = raw
            .as_array()
            .or_else(|| raw["entries"].as_array())
            .cloned()
            .unwrap_or_default();
        let mut rows = Vec::new();
        for item in items {
            if item.get("listener_id").is_some() {
                rows.push(item);
                continue;
            }
            if let Ok(data) = STANDARD.decode(string(&item, "value_b64")) {
                if let Ok(row) = serde_json::from_slice::<Value>(&data) {
                    rows.push(row);
                }
            }
        }
        if let Ok(mut cache) = self.directory_cache.lock() {
            *cache = rows.clone();
        }
        Ok(rows)
    }
}
impl Drop for HubClient {
    fn drop(&mut self) {
        // Event worker owns another clone; DaemonService explicitly sets closed before join.
    }
}
