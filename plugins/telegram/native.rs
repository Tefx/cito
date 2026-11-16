use crate::support::*;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::time::Duration;
fn id(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_string)
        .or_else(|| v.as_i64().map(|n| n.to_string()))
        .or_else(|| v.as_u64().map(|n| n.to_string()))
}
#[derive(Debug)]
struct ApiFailure {
    message: String,
    retryable: bool,
    effect_unknown: bool,
    retry_after: Option<u64>,
}
impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl From<ApiFailure> for String {
    fn from(e: ApiFailure) -> Self {
        e.message
    }
}
fn agent(poll: bool) -> &'static ureq::Agent {
    static SEND: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    static POLL: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    (if poll { &POLL } else { &SEND }).get_or_init(|| {
        ureq::AgentBuilder::new()
            .redirects(0)
            .timeout_connect(Duration::from_secs(3))
            .timeout_read(Duration::from_secs(if poll { 35 } else { 25 }))
            .build()
    })
}
fn api(config: &Value, method: &str, body: Value) -> std::result::Result<Value, ApiFailure> {
    api_with_timeout(
        config,
        method,
        body,
        Duration::from_secs(if method == "getUpdates" { 35 } else { 25 }),
    )
}
fn api_with_timeout(
    config: &Value,
    method: &str,
    body: Value,
    timeout: Duration,
) -> std::result::Result<Value, ApiFailure> {
    use std::io::Read;
    let token = str_at(config, "botToken").ok_or_else(|| ApiFailure {
        message: "Bot Token is required".into(),
        retryable: false,
        effect_unknown: false,
        retry_after: None,
    })?;
    let base = str_at(config, "apiBaseUrl")
        .unwrap_or("https://api.telegram.org")
        .trim_end_matches('/');
    let response = match agent(method == "getUpdates")
        .post(&format!("{base}/bot{token}/{method}"))
        .timeout(timeout)
        .send_json(body.clone())
    {
        Ok(r) => r,
        Err(ureq::Error::Status(status, response)) => {
            let v: Value =
                serde_json::from_reader(response.into_reader().take(65_536)).unwrap_or(Value::Null);
            return Err(ApiFailure {
                message: format!(
                    "Telegram {method} HTTP {status}: {}",
                    str_at(&v, "description").unwrap_or("Telegram rejected the request")
                ),
                retryable: matches!(status, 408 | 429 | 500..=599),
                effect_unknown: false,
                retry_after: (status == 429)
                    .then(|| v["parameters"]["retry_after"].as_u64())
                    .flatten(),
            });
        }
        Err(ureq::Error::Transport(e)) => {
            // ureq reports ConnectionFailed/Dns before writing the request. Other transport
            // errors cannot prove that sendMessage was rejected, so never retry them blindly.
            let before_send = matches!(
                e.kind(),
                ureq::ErrorKind::ConnectionFailed | ureq::ErrorKind::Dns
            ) || std::error::Error::source(&e)
                .and_then(|e| e.downcast_ref::<std::io::Error>())
                .is_some_and(|e| {
                    matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::AddrNotAvailable
                    )
                });
            return Err(ApiFailure {
                message: format!("Telegram {method} request failed"),
                retryable: before_send,
                effect_unknown: !before_send && method == "sendMessage",
                retry_after: None,
            });
        }
    };
    let mut bytes = Vec::new();
    let read = response
        .into_reader()
        .take(1_048_577)
        .read_to_end(&mut bytes);
    if read.is_err() && bytes.len() <= 1_048_576 {
        return Err(ApiFailure {
            message: format!("Telegram {method} transport read was incomplete"),
            retryable: method == "getUpdates",
            effect_unknown: method == "sendMessage",
            retry_after: None,
        });
    }
    let parsed = if bytes.len() <= 1_048_576 {
        serde_json::from_slice::<Value>(&bytes).ok()
    } else {
        None
    };
    let v = match parsed {
        Some(value) => value,
        None if method == "getUpdates" => {
            if body["limit"].as_u64().unwrap_or(25) > 1 {
                let mut single = body;
                single["limit"] = json!(1);
                return api(config, method, single);
            }
            // The bounded prefix normally contains update_id before the large message.
            let prefix = String::from_utf8_lossy(&bytes);
            let update = prefix
                .split_once("\"update_id\"")
                .and_then(|(_, rest)| rest.split_once(':'))
                .map(|(_, rest)| {
                    rest.trim_start()
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>()
                })
                .and_then(|id| id.parse::<u64>().ok())
                .unwrap_or_else(|| body["offset"].as_u64().unwrap_or(0));
            eprintln!(
                "Telegram getUpdates: skipping oversized or malformed single update {update}"
            );
            return Ok(json!([{"update_id":update}]));
        }
        None => {
            return Err(ApiFailure {
                message: format!("Telegram {method} response was incomplete"),
                retryable: false,
                effect_unknown: method == "sendMessage",
                retry_after: None,
            })
        }
    };
    if v["ok"] != true {
        let status = v["error_code"].as_u64().unwrap_or(400);
        return Err(ApiFailure {
            message: format!(
                "Telegram {method} API {status}: {}",
                str_at(&v, "description").unwrap_or("Telegram API returned an error")
            ),
            retryable: matches!(status, 408 | 429 | 500..=599),
            effect_unknown: false,
            retry_after: (status == 429)
                .then(|| v["parameters"]["retry_after"].as_u64())
                .flatten(),
        });
    }
    Ok(v["result"].clone())
}
fn chunks(text: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let (mut start, mut units) = (0, 0);
    for (offset, character) in text.char_indices() {
        if units + character.len_utf16() > 4096 {
            chunks.push(&text[start..offset]);
            start = offset;
            units = 0;
        }
        units += character.len_utf16();
    }
    if start < text.len() {
        chunks.push(&text[start..]);
    }
    chunks
}
fn control_budget(deadline: u64) -> Result<Duration> {
    let remaining = deadline.saturating_sub(now_ms());
    if remaining == 0 {
        return Err("Control request deadline exceeded".into());
    }
    Ok(Duration::from_millis(remaining))
}
fn send(config: &Value, chat: &str, text: &str, deadline: u64) -> Result<Vec<String>> {
    let mut ids = vec![];
    for text in chunks(text) {
        let r = api_with_timeout(
            config,
            "sendMessage",
            json!({"chat_id":chat,"text":text}),
            control_budget(deadline)?,
        )?;
        ids.push(id(&r["message_id"]).ok_or("Telegram message_id is missing")?);
    }
    Ok(ids)
}
fn delivery(config: &Value, req: &Value) -> Value {
    let mut ids: Vec<String> = req["already_sent_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(id)
        .collect();
    let result = (|| -> std::result::Result<(), ApiFailure> {
        let chat = str_at(&req["config"], "chatId").ok_or_else(|| ApiFailure {
            message: "Chat ID is required".into(),
            retryable: false,
            effect_unknown: false,
            retry_after: None,
        })?;
        let s = &req["signal"];
        let from = str_at(&s["from"], "title")
            .or_else(|| str_at(&s["from"], "listener_id"))
            .unwrap_or("sender");
        let text = format!("Cito · {from}\n\n{}", s["message"].as_str().unwrap_or(""));
        let claim_until = req["claim_until_ms"].as_u64().ok_or_else(|| ApiFailure {
            message: "Delivery reservation deadline is required".into(),
            retryable: false,
            effect_unknown: false,
            retry_after: None,
        })?;
        for text in chunks(&text).into_iter().skip(ids.len()) {
            let budget = claim_until.saturating_sub(now_ms()).saturating_sub(500);
            if budget == 0 {
                return Err(ApiFailure {
                    message: "Delivery reservation expired before send".into(),
                    retryable: true,
                    effect_unknown: false,
                    retry_after: None,
                });
            }
            let r = api_with_timeout(
                config,
                "sendMessage",
                json!({"chat_id":chat,"text":text}),
                Duration::from_millis(budget.min(25_000)),
            )?;
            ids.push(id(&r["message_id"]).ok_or_else(|| ApiFailure {
                message: "Telegram message_id is missing".into(),
                retryable: false,
                effect_unknown: true,
                retry_after: None,
            })?);
            emit(
                &json!({"op":"delivered","delivery_id":req["delivery_id"],"progress":true,"external_msg_ids":ids}),
            );
        }
        Ok(())
    })();
    let (ok, retryable, effect_unknown, error, retry_after) = match result {
        Ok(()) => (true, false, false, None, None),
        Err(e) => (
            false,
            e.retryable,
            e.effect_unknown,
            Some(e.message),
            e.retry_after,
        ),
    };
    json!({"op":"delivered","delivery_id":req["delivery_id"],"ok":ok,"retryable":retryable,"effect_unknown":effect_unknown,"external_msg_id":ids.last(),"external_msg_ids":ids,"error":error,"retry_after":retry_after})
}
fn rpc(config: &Value, req: &Value) -> Value {
    let c = if req["hub_config"].is_object() {
        &req["hub_config"]
    } else {
        config
    };
    let result = (|| -> Result<Value> {
        let deadline = req["control_deadline_ms"]
            .as_u64()
            .unwrap_or_else(|| now_ms() + 8000);
        let me = api_with_timeout(c, "getMe", json!({}), control_budget(deadline)?)?;
        let username = str_at(&me, "username").ok_or("Bot username is unavailable")?;
        if req["op"] == "format_pair" {
            return Ok(
                json!({"pairing_command":format!("/start {}",str_at(req,"code").ok_or("Pairing code is required")?),"instruction":format!("Send \"/start {}\" to @{username}",str_at(req,"code").ok_or("Pairing code is required")?),"bot_username":username,"pairing_url":format!("https://t.me/{username}?start={}",str_at(req,"code").ok_or("Pairing code is required")?)}),
            );
        }
        let detail = if let Some(chat) = str_at(&req["user_config"], "chatId") {
            send(
                c,
                chat,
                &format!(
                    "Cito channel connected for @{}",
                    str_at(req, "user").unwrap_or("user")
                ),
                deadline,
            )?;
            format!("Sent test message to chat {chat}")
        } else {
            format!("Bot @{username} verified")
        };
        Ok(json!({"detail":detail,"bot_username":username}))
    })();
    match result {
        Ok(mut v) => {
            v["op"] = json!("rpc_result");
            v["req_id"] = req["req_id"].clone();
            v["ok"] = json!(true);
            v["error"] = Value::Null;
            v
        }
        Err(e) => json!({"op":"rpc_result","req_id":req["req_id"],"ok":false,"error":e}),
    }
}
#[cfg(test)]
mod control_tests {
    use super::*;
    #[test]
    fn rpc_shares_one_deadline_and_skips_expired_queue_work() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = json!({"botToken":"test-token","apiBaseUrl":format!("http://{}",listener.local_addr().unwrap())});
        let worker = std::thread::spawn(move || {
            use std::os::fd::AsRawFd;
            for method in ["getMe", "sendMessage"] {
                assert!(crate::support::poll_until(
                    &mut [libc::pollfd {
                        fd: listener.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0
                    }],
                    Some(std::time::Instant::now() + Duration::from_secs(2))
                )
                .unwrap());
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.contains(method));
                let mut length = 0;
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse::<usize>().unwrap();
                        }
                    }
                }
                reader.read_exact(&mut vec![0; length]).unwrap();
                // Each call fits the original budget on its own; together they
                // exceed it. A per-call reset would incorrectly report success.
                std::thread::sleep(Duration::from_millis(if method == "getMe" {
                    350
                } else {
                    300
                }));
                let body = if method == "getMe" {
                    json!({"ok":true,"result":{"username":"sample_bot"}})
                } else {
                    json!({"ok":true,"result":{"message_id":42}})
                }
                .to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        let started = std::time::Instant::now();
        let reply = rpc(
            &config,
            &json!({"op":"test","req_id":"bounded","user_config":{"chatId":"123"},"control_deadline_ms":now_ms()+500}),
        );
        worker.join().unwrap();
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["req_id"], "bounded");
        assert!(started.elapsed() < Duration::from_secs(1));
        let expired = rpc(
            &config,
            &json!({"op":"test","control_deadline_ms":now_ms()-1}),
        );
        assert_eq!(expired["ok"], false);
        assert_eq!(expired["error"], "Control request deadline exceeded");
    }
}

struct Pending {
    cursor: u64,
    key: Option<String>,
    done: bool,
}
struct Outbound {
    deliveries: mpsc::SyncSender<(Value, Value)>,
    control: mpsc::SyncSender<(Value, Value)>,
}
impl Outbound {
    fn new() -> Self {
        fn worker() -> mpsc::SyncSender<(Value, Value)> {
            let (tx, rx) = mpsc::sync_channel::<(Value, Value)>(64);
            std::thread::spawn(move || {
                while let Ok((config, request)) = rx.recv() {
                    match str_at(&request, "op").unwrap_or("") {
                        "deliver" => emit(&delivery(&config, &request)),
                        "_send" => {
                            if let Err(error) = send(
                                &config,
                                str_at(&request, "chat").unwrap_or(""),
                                str_at(&request, "text").unwrap_or(""),
                                request["control_deadline_ms"].as_u64().unwrap_or(0),
                            ) {
                                eprintln!("Telegram confirmation: {error}");
                            }
                        }
                        _ => emit(&rpc(&config, &request)),
                    }
                }
            });
            tx
        }
        Self {
            deliveries: worker(),
            control: worker(),
        }
    }
    fn submit(&self, config: &Value, mut request: Value) {
        request["control_deadline_ms"] = json!(now_ms() + 8000);
        let tx = if request["op"] == "deliver" {
            &self.deliveries
        } else {
            &self.control
        };
        if tx.try_send((config.clone(), request.clone())).is_err() {
            if request["op"] == "deliver" {
                emit(
                    &json!({"op":"delivered","delivery_id":request["delivery_id"],"ok":false,"retryable":true,"effect_unknown":false,"error":"Outbound queue is full"}),
                );
            } else if request["op"] != "_send" {
                emit(
                    &json!({"op":"rpc_result","req_id":request["req_id"],"ok":false,"error":"Outbound queue is full"}),
                );
            }
        }
    }
}
struct Runtime {
    outbound: Outbound,
    config: Value,
    cursor: u64,
    pending: VecDeque<Pending>,
    reply_chats: HashMap<String, String>,
    pair_chats: HashMap<String, (String, String)>,
}
impl Runtime {
    fn save(&mut self) {
        let mut cursor = self.cursor;
        while self.pending.front().map(|p| p.done).unwrap_or(false) {
            if let Some(p) = self.pending.pop_front() {
                cursor = cursor.max(p.cursor);
            }
        }
        if cursor > self.cursor {
            self.cursor = cursor;
            emit(&json!({"op":"cursor_save","cursor":cursor.to_string()}));
        }
    }
    fn done(&mut self, key: &str) {
        for p in &mut self.pending {
            if p.key.as_deref() == Some(key) {
                p.done = true;
            }
        }
        self.save();
    }
    fn updates(&mut self, updates: Value) {
        let Some(mut updates) = updates.as_array().cloned() else {
            return;
        };
        updates.sort_by_key(|u| u["update_id"].as_u64().unwrap_or(0));
        for u in updates {
            let Some(n) = u["update_id"].as_u64() else {
                eprintln!("Ignoring update without update_id");
                continue;
            };
            let Some(next) = n.checked_add(1) else {
                eprintln!("Ignoring update with overflowing update_id");
                continue;
            };
            if n < self.cursor || self.pending.iter().any(|p| p.cursor == next) {
                continue;
            }
            self.pending.push_back(Pending {
                cursor: next,
                key: None,
                done: true,
            });
            let result = self.update(&u, n);
            match result {
                Ok(Some(key)) => {
                    if let Some(p) = self.pending.back_mut() {
                        p.key = Some(key);
                        p.done = false;
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("Telegram update {n}: {e}"),
            }
        }
        self.save();
    }
    fn update(&mut self, u: &Value, n: u64) -> Result<Option<String>> {
        let m = &u["message"];
        let Some(text) = str_at(m, "text") else {
            return Ok(None);
        };
        let chat = id(&m["chat"]["id"]).ok_or("Missing chat identity")?;
        let user = id(&m["from"]["id"]).ok_or("Missing sender identity")?;
        if m["chat"]["type"] != "private" || chat != user {
            return Ok(None);
        }
        let mut words = text.split_whitespace();
        let first = words.next().unwrap_or("");
        if first.split('@').next() == Some("/start") {
            let Some(code) = words.next() else {
                return Ok(None);
            };
            if !code.starts_with("cito-") || words.next().is_some() {
                return Ok(None);
            }
            if self.pair_chats.contains_key(code) {
                return Ok(None);
            }
            let key = format!("pair:{n}");
            self.pair_chats
                .insert(code.into(), (chat.clone(), key.clone()));
            emit(
                &json!({"op":"pair_redeem","code":code,"external_chat_id":chat,"external_user_id":user,"is_private_chat":true,"display_label":str_at(&m["from"],"username").map(|s|format!("@{s}")).unwrap_or_else(||format!("Chat {chat}")),"user_config_patch":{"chatId":chat}}),
            );
            return Ok(Some(key));
        }
        let key = n.to_string();
        self.reply_chats.insert(key.clone(), chat.clone());
        emit(
            &json!({"op":"inbound_reply","update_id":key,"external_chat_id":chat,"external_user_id":user,"reply_to_external_msg_id":id(&m["reply_to_message"]["message_id"]),"text":text}),
        );
        Ok(Some(key))
    }
    fn request(&mut self, r: Value) -> bool {
        match str_at(&r, "op").unwrap_or("") {
            "shutdown" => return false,
            "configure" => {
                let old_token = str_at(&self.config, "botToken").unwrap_or("");
                let token_changed = !old_token.is_empty()
                    && Some(old_token) != str_at(&r["hub_config"], "botToken");
                self.config = r["hub_config"].clone();
                if token_changed {
                    self.cursor = 0;
                    self.pending.clear();
                    self.reply_chats.clear();
                    self.pair_chats.clear();
                } else {
                    self.cursor = self
                        .cursor
                        .max(id(&r["cursor"]).and_then(|s| s.parse().ok()).unwrap_or(0));
                }
            }
            "format_pair" | "test" | "deliver" => self.outbound.submit(&self.config, r.clone()),
            "reply_ack" => {
                if let Some(key) = id(&r["update_id"]) {
                    if let Some(chat) = self.reply_chats.remove(&key) {
                        let text = if r["ok"] == true {
                            format!(
                                "↩ Replied to {}",
                                str_at(&r, "target_display").unwrap_or("sender")
                            )
                        } else {
                            format!(
                                "Reply failed: {}",
                                str_at(&r, "error").unwrap_or("Target unavailable")
                            )
                        };
                        self.outbound
                            .submit(&self.config, json!({"op":"_send","chat":chat,"text":text}));
                        self.done(&key);
                    }
                }
            }
            "pair_ack" => {
                if let Some(code) = str_at(&r, "code") {
                    if let Some((chat, key)) = self.pair_chats.remove(code) {
                        let text = if r["ok"] == true {
                            format!(
                                "Paired with Cito as @{}",
                                str_at(&r, "owner_user").unwrap_or("user")
                            )
                        } else {
                            format!(
                                "Pairing failed: {}",
                                str_at(&r, "error").unwrap_or("Invalid or expired code")
                            )
                        };
                        self.outbound
                            .submit(&self.config, json!({"op":"_send","chat":chat,"text":text}));
                        self.done(&key);
                    }
                }
            }
            // Deliveries contain no inline keyboard, so acknowledgment requires
            // no remote markup edit. Repeated ACKs remain side-effect free.
            "on_acked" => {}
            _ => eprintln!("Unknown Telegram plugin operation"),
        };
        true
    }
}
#[derive(Default)]
struct PollBackoff {
    failures: u32,
}
impl PollBackoff {
    fn failed(&mut self, error: &ApiFailure, jitter_ms: u64) -> (bool, Duration) {
        self.failures = self.failures.saturating_add(1);
        let permanent = [
            "HTTP 401:",
            "HTTP 403:",
            "HTTP 409:",
            "API 401:",
            "API 403:",
            "API 409:",
        ]
        .iter()
        .any(|code| error.message.contains(code));
        let delay = 500u64.saturating_mul(1 << self.failures.saturating_sub(1).min(6));
        (
            permanent || self.failures >= 3,
            Duration::from_millis(delay + jitter_ms.min(499))
                .max(Duration::from_secs(error.retry_after.unwrap_or(0))),
        )
    }
    fn succeeded(&mut self) {
        self.failures = 0;
    }
}

enum Event {
    Request(Value),
    Poll(Value, std::result::Result<Value, ApiFailure>),
}
pub fn main() {
    let (tx, rx) = mpsc::channel::<Event>();
    stdin_events(tx.clone(), Event::Request);
    let mut runtime = Runtime {
        outbound: Outbound::new(),
        config: json!({}),
        cursor: 0,
        pending: VecDeque::new(),
        reply_chats: HashMap::new(),
        pair_chats: HashMap::new(),
    };
    let mut polling = false;
    let mut backoff = PollBackoff::default();
    let mut next_poll = std::time::Instant::now();
    let mut paused = false;
    loop {
        let can_poll = !polling
            && !paused
            && runtime.pending.is_empty()
            && str_at(&runtime.config, "botToken").is_some();
        let event = if can_poll {
            match rx.recv_timeout(next_poll.saturating_duration_since(std::time::Instant::now())) {
                Ok(event) => Some(event),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => return,
            }
        } else {
            match rx.recv() {
                Ok(event) => Some(event),
                Err(_) => return,
            }
        };
        if let Some(Event::Request(req)) = event.as_ref() {
            let configured = req["op"] == "configure";
            if !runtime.request(req.clone()) {
                return;
            }
            if configured {
                paused = false;
                backoff.succeeded();
                next_poll = std::time::Instant::now();
            }
        }
        if let Some(Event::Poll(config, result)) = event {
            polling = false;
            if config == runtime.config {
                match result {
                    Ok(updates) => {
                        backoff.succeeded();
                        emit(&json!({"op":"health","error":null}));
                        runtime.updates(updates);
                    }
                    Err(e) => {
                        eprintln!("Telegram polling: {e}");
                        let guidance = if e.message.contains("409") {
                            "Remove the external webhook yourself or use a dedicated bot token; stop other pollers."
                        } else if e.message.contains("401") {
                            "Check or replace the Bot Token in Human Channels Settings."
                        } else {
                            "Check the network, Bot Token, and bot permissions."
                        };
                        let jitter = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .subsec_millis() as u64
                            % 500;
                        let (report, delay) = backoff.failed(&e, jitter);
                        if report {
                            emit(
                                &json!({"op":"health","error":format!("Cannot receive replies or pair accounts: {e}. {guidance}")}),
                            );
                        }
                        paused = [
                            "HTTP 401:",
                            "HTTP 403:",
                            "HTTP 409:",
                            "API 401:",
                            "API 403:",
                            "API 409:",
                        ]
                        .iter()
                        .any(|code| e.message.contains(code));
                        next_poll = std::time::Instant::now() + delay;
                    }
                }
            }
        }
        if !polling
            && !paused
            && runtime.pending.is_empty()
            && str_at(&runtime.config, "botToken").is_some()
            && std::time::Instant::now() >= next_poll
        {
            polling = true;
            let config = runtime.config.clone();
            let cursor = runtime.cursor;
            let tx = tx.clone();
            std::thread::spawn(move || {
                let r = api(
                    &config,
                    "getUpdates",
                    json!({"offset":cursor,"timeout":25,"limit":25,"allowed_updates":["message"]}),
                );
                let _ = tx.send(Event::Poll(config, r));
            });
            next_poll = std::time::Instant::now() + Duration::from_millis(100);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn polling_hysteresis_backoff_and_permanent_auth_errors() {
        let failure = |message: &str| ApiFailure {
            message: message.into(),
            retryable: false,
            effect_unknown: false,
            retry_after: None,
        };
        for message in [
            "Telegram getUpdates request failed",
            "Telegram getUpdates HTTP 502: Bad gateway",
        ] {
            let mut backoff = PollBackoff::default();
            assert_eq!(
                backoff.failed(&failure(message), 0),
                (false, Duration::from_millis(500))
            );
            assert_eq!(
                backoff.failed(&failure(message), 100),
                (false, Duration::from_millis(1100))
            );
            assert_eq!(
                backoff.failed(&failure(message), 200),
                (true, Duration::from_millis(2200))
            );
            for _ in 0..100 {
                assert!(backoff.failed(&failure(message), 499).1 <= Duration::from_millis(32499));
            }
            backoff.succeeded();
            assert!(!backoff.failed(&failure(message), 0).0);
        }
        for message in [
            "Telegram getUpdates HTTP 401: Unauthorized",
            "Telegram getUpdates API 403: Forbidden",
            "Telegram getUpdates HTTP 409: Conflict",
        ] {
            assert!(PollBackoff::default().failed(&failure(message), 0).0);
        }
    }
    #[test]
    fn configure_retains_monotonic_cursor_and_ack_without_keyboard_is_noop() {
        let mut runtime = Runtime {
            outbound: Outbound::new(),
            config: json!({"botToken":"first"}),
            cursor: 20,
            pending: VecDeque::new(),
            reply_chats: HashMap::new(),
            pair_chats: HashMap::new(),
        };
        runtime.request(json!({"op":"configure","hub_config":{"botToken":"first"},"cursor":"10"}));
        assert_eq!(runtime.cursor, 20);
        runtime.pending.push_back(Pending {
            cursor: 21,
            key: Some("pending".into()),
            done: false,
        });
        runtime.request(json!({"op":"configure","hub_config":{"botToken":"first"},"cursor":"30"}));
        assert_eq!(runtime.cursor, 30);
        assert_eq!(runtime.pending.len(), 1);
        runtime
            .request(json!({"op":"configure","hub_config":{"botToken":"second"},"cursor":"999"}));
        assert_eq!(runtime.cursor, 0);
        assert!(runtime.pending.is_empty());
        assert!(runtime.request(
            json!({"op":"on_acked","user_config":{"chatId":"1"},"external_msg_ids":["7"]})
        ));
        assert_eq!(runtime.cursor, 0);
    }
}
