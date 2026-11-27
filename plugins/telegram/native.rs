use crate::support::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
fn id(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_string)
        .or_else(|| v.as_i64().map(|n| n.to_string()))
        .or_else(|| v.as_u64().map(|n| n.to_string()))
}
fn bot_identity(config: &Value) -> Option<String> {
    str_at(config, "botToken").map(|token| {
        token
            .split_once(':')
            .map(|(id, _)| id)
            .unwrap_or(token)
            .to_owned()
    })
}
fn clean_inline(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn format_sender(from: &Value) -> String {
    if let Some(title) = str_at(from, "title") {
        let cleaned = clean_inline(title);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    let harness = str_at(from, "harness")
        .map(clean_inline)
        .filter(|s| !s.is_empty());
    let host = str_at(from, "host_name")
        .map(clean_inline)
        .filter(|s| !s.is_empty());
    let user = str_at(from, "user")
        .map(clean_inline)
        .filter(|s| !s.is_empty());
    match (harness, host, user) {
        (Some(h), Some(host), _) => format!("{h}@{host}"),
        (Some(h), None, Some(u)) => format!("{u} ({h})"),
        (Some(h), None, None) => h,
        (None, Some(host), Some(u)) => format!("{u}@{host}"),
        (None, None, Some(u)) => u,
        _ => str_at(from, "listener_id")
            .map(clean_inline)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "sender".into()),
    }
}
#[derive(Default)]
struct OutboundState {
    cooldowns: HashMap<(String, String), Instant>,
    in_flight: HashSet<(String, String)>,
    active_bot: Option<(String, String)>,
}
impl OutboundState {
    fn remaining_cooldown(&mut self, bot_id: &str, chat_id: &str) -> Option<u64> {
        let now = Instant::now();
        self.cooldowns.retain(|_, until| *until > now);
        self.cooldowns
            .get(&(bot_id.to_owned(), chat_id.to_owned()))
            .and_then(|until| {
                until
                    .checked_duration_since(now)
                    .map(|d| (d.as_millis() as u64).div_ceil(1000).max(1))
            })
    }
    fn record_cooldown(&mut self, bot_id: &str, chat_id: &str, retry_after_secs: u64) {
        if retry_after_secs == 0 {
            return;
        }
        let now = Instant::now();
        self.cooldowns.retain(|_, until| *until > now);
        let until = now + Duration::from_secs(retry_after_secs.min(u64::from(u32::MAX)));
        let entry = self
            .cooldowns
            .entry((bot_id.to_owned(), chat_id.to_owned()))
            .or_insert(until);
        if until > *entry {
            *entry = until;
        }
    }
    fn set_bot_username(&mut self, bot_id: String, username: String) -> bool {
        let switched = self
            .active_bot
            .as_ref()
            .is_some_and(|(_, previous)| !previous.eq_ignore_ascii_case(&username));
        self.active_bot = Some((bot_id, username));
        switched
    }
    fn bot_username(&self, bot_id: &str) -> Option<&str> {
        self.active_bot
            .as_ref()
            .and_then(|(id, name)| (id == bot_id).then_some(name.as_str()))
    }
}
#[derive(Debug)]
struct ApiFailure {
    message: String,
    status: Option<u16>,
    retryable: bool,
    effect_unknown: bool,
    retry_after: Option<u64>,
}
impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl From<String> for ApiFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            status: None,
            retryable: false,
            effect_unknown: false,
            retry_after: None,
        }
    }
}
impl From<&str> for ApiFailure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
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
        status: None,
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
            let is_5xx = (500..=599).contains(&status);
            return Err(ApiFailure {
                message: format!(
                    "Telegram {method} HTTP {status}: {}",
                    str_at(&v, "description").unwrap_or("Telegram rejected the request")
                ),
                status: Some(status),
                retryable: matches!(status, 408 | 429) || (method != "sendMessage" && is_5xx),
                effect_unknown: method == "sendMessage" && is_5xx,
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
                status: None,
                retryable: before_send,
                effect_unknown: !before_send && method == "sendMessage",
                retry_after: None,
            });
        }
    };
    let status = Some(response.status());
    let mut bytes = Vec::new();
    let read = response
        .into_reader()
        .take(1_048_577)
        .read_to_end(&mut bytes);
    if read.is_err() && bytes.len() <= 1_048_576 {
        return Err(ApiFailure {
            message: format!("Telegram {method} transport read was incomplete"),
            status,
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
                .ok_or_else(|| ApiFailure {
                    message: "Telegram getUpdates response has no parseable update_id".into(),
                    status: None,
                    retryable: true,
                    effect_unknown: false,
                    retry_after: None,
                })?;
            eprintln!(
                "Telegram getUpdates: skipping oversized or malformed single update {update}"
            );
            return Ok(json!([{"update_id":update}]));
        }
        None => {
            return Err(ApiFailure {
                message: format!("Telegram {method} response was incomplete"),
                status,
                retryable: false,
                effect_unknown: method == "sendMessage",
                retry_after: None,
            })
        }
    };
    api_result(method, v).map_err(|mut error| {
        error.status = error.status.or(status);
        error
    })
}

fn api_result(method: &str, mut v: Value) -> std::result::Result<Value, ApiFailure> {
    if v["ok"] != true {
        let Some(code) = v["error_code"].as_u64().filter(|_| v["ok"] == false) else {
            return Err(ApiFailure {
                message: format!("Telegram {method} response is missing a valid outcome"),
                status: None,
                retryable: method == "getUpdates",
                effect_unknown: method == "sendMessage",
                retry_after: None,
            });
        };
        let status = u16::try_from(code).ok();
        let is_5xx = (500..=599).contains(&code);
        return Err(ApiFailure {
            message: format!(
                "Telegram {method} API {code}: {}",
                str_at(&v, "description").unwrap_or("Telegram API returned an error")
            ),
            status,
            retryable: matches!(code, 408 | 429) || (method != "sendMessage" && is_5xx),
            effect_unknown: method == "sendMessage" && is_5xx,
            retry_after: (code == 429)
                .then(|| v["parameters"]["retry_after"].as_u64())
                .flatten(),
        });
    }
    if method == "getUpdates" && !v["result"].is_array() {
        return Err(ApiFailure {
            message: "Telegram getUpdates result must be an array".into(),
            status: None,
            retryable: true,
            effect_unknown: false,
            retry_after: None,
        });
    }
    if method == "sendMessage" && v["result"]["message_id"].as_u64().is_none_or(|id| id == 0) {
        return Err(ApiFailure {
            message: "Telegram message_id is missing".into(),
            status: None,
            retryable: false,
            effect_unknown: true,
            retry_after: None,
        });
    }
    Ok(v["result"].take())
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
fn send_message(
    config: &Value,
    chat: &str,
    text: &str,
    budget: Duration,
    state: Option<&Arc<Mutex<OutboundState>>>,
) -> std::result::Result<String, ApiFailure> {
    let destination = state.zip(bot_identity(config).map(|bot| (bot, chat.to_owned())));
    if let Some((state, key)) = &destination {
        let mut s = state.lock().map_err(|_| "Outbound state unavailable")?;
        let cooldown = s.remaining_cooldown(&key.0, chat);
        if cooldown.is_some() || !s.in_flight.insert(key.clone()) {
            return Err(ApiFailure {
                message: "Telegram sendMessage waiting for destination cooldown or active send"
                    .into(),
                status: cooldown.map(|_| 429),
                retryable: true,
                effect_unknown: false,
                retry_after: Some(cooldown.unwrap_or(1)),
            });
        }
    }
    let result = api_with_timeout(
        config,
        "sendMessage",
        json!({"chat_id":chat,"text":text}),
        budget,
    );
    if let Some((state, key)) = destination {
        if let Ok(mut s) = state.lock() {
            if let Some(wait) = result.as_ref().err().and_then(|e| e.retry_after) {
                s.record_cooldown(&key.0, chat, wait);
            }
            s.in_flight.remove(&key);
        }
    }
    result.map(|r| r["message_id"].to_string())
}
fn send(
    config: &Value,
    chat: &str,
    text: &str,
    deadline: u64,
    state: Option<&Arc<Mutex<OutboundState>>>,
) -> std::result::Result<(), ApiFailure> {
    for text in chunks(text) {
        send_message(config, chat, text, control_budget(deadline)?, state)?;
    }
    Ok(())
}
fn unstarted_chat_error(error: &ApiFailure) -> bool {
    if !matches!(error.status, Some(400 | 403)) {
        return false;
    }
    let lower = error.message.to_ascii_lowercase();
    [
        "chat not found",
        "can't initiate",
        "blocked by the user",
        "user is deactivated",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}
fn delivery(config: &Value, req: &Value, state: Option<&Arc<Mutex<OutboundState>>>) -> Value {
    let mut ids: Vec<String> = req["already_sent_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(id)
        .collect();
    let bot_id = bot_identity(config);
    let chat_opt = str_at(&req["config"], "chatId").map(str::to_owned);
    let mut delivery_unverified = false;
    let result = (|| -> std::result::Result<(), ApiFailure> {
        let chat = chat_opt.as_deref().ok_or("Chat ID is required")?;
        let s = &req["signal"];
        let from = format_sender(&s["from"]);
        let text = format!("Cito · {from}\n\n{}", s["message"].as_str().unwrap_or(""));
        let claim_until = req["claim_until_ms"].as_u64().ok_or_else(|| ApiFailure {
            message: "Delivery reservation deadline is required".into(),
            status: None,
            retryable: false,
            effect_unknown: false,
            retry_after: None,
        })?;
        for text in chunks(&text).into_iter().skip(ids.len()) {
            let budget = claim_until.saturating_sub(now_ms()).saturating_sub(500);
            if budget == 0 {
                return Err(ApiFailure {
                    message: "Delivery reservation expired before send".into(),
                    status: None,
                    retryable: true,
                    effect_unknown: false,
                    retry_after: None,
                });
            }
            let r = match send_message(
                config,
                chat,
                text,
                Duration::from_millis(budget.min(25_000)),
                state,
            ) {
                Ok(r) => r,
                Err(mut e) => {
                    if unstarted_chat_error(&e) {
                        delivery_unverified = true;
                        let target = state
                            .zip(bot_id.as_deref())
                            .and_then(|(state, bot)| {
                                state
                                    .lock()
                                    .ok()?
                                    .bot_username(bot)
                                    .map(|name| format!("@{name}"))
                            })
                            .unwrap_or_else(|| "the bot".into());
                        e.message.push_str(&format!(" Delivery unverified: send /start to {target} or re-pair your account."));
                    }
                    return Err(e);
                }
            };
            ids.push(r);
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
    let mut frame = json!({"op":"delivered","delivery_id":req["delivery_id"],"ok":ok,"retryable":retryable,"effect_unknown":effect_unknown,"external_msg_id":ids.last(),"external_msg_ids":ids,"error":error,"retry_after":retry_after});
    if delivery_unverified {
        frame["delivery_unverified"] = json!(true);
    }
    frame
}
fn rpc(config: &Value, req: &Value, state: Option<&Arc<Mutex<OutboundState>>>) -> Value {
    let c = if req["hub_config"].is_object() {
        &req["hub_config"]
    } else {
        config
    };
    let result = (|| -> std::result::Result<Value, ApiFailure> {
        let deadline = req["control_deadline_ms"]
            .as_u64()
            .unwrap_or_else(|| now_ms() + 8000);
        let me = api_with_timeout(c, "getMe", json!({}), control_budget(deadline)?)?;
        let username = str_at(&me, "username").ok_or("Bot username is unavailable")?;
        if req["op"] == "format_pair" {
            return Ok(
                json!({"pairing_command":format!("/start {}",str_at(req,"code").ok_or("Pairing code is required")?),"instruction":format!("Send \"/start {}\" to @{username}",str_at(req,"code").ok_or("Pairing code is required")?),"bot_username":username,"service_label":format!("@{username}"),"pairing_url":format!("https://t.me/{username}?start={}",str_at(req,"code").ok_or("Pairing code is required")?)}),
            );
        }
        let detail = if let Some(chat) = str_at(&req["user_config"], "chatId") {
            let user = str_at(req, "user")
                .map(|u| u.trim_start_matches('@'))
                .filter(|u| !u.is_empty())
                .unwrap_or("user");
            send(
                c,
                chat,
                &format!("Cito channel connected for @{user}"),
                deadline,
                state,
            )?;
            format!("Sent test message to chat {chat}")
        } else {
            format!("Bot @{username} verified")
        };
        Ok(json!({"detail":detail,"bot_username":username,"service_label":format!("@{username}")}))
    })();
    match result {
        Ok(mut v) => {
            v["op"] = json!("rpc_result");
            v["req_id"] = req["req_id"].clone();
            v["ok"] = json!(true);
            v["error"] = Value::Null;
            v
        }
        Err(e) => {
            json!({"op":"rpc_result","req_id":req["req_id"],"ok":false,"error":e.message,"effect_unknown":e.effect_unknown,"retry_after":e.retry_after})
        }
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
            None,
        );
        worker.join().unwrap();
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["req_id"], "bounded");
        assert!(started.elapsed() < Duration::from_secs(1));
        let expired = rpc(
            &config,
            &json!({"op":"test","control_deadline_ms":now_ms()-1}),
            None,
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
    state: Arc<Mutex<OutboundState>>,
}
impl Outbound {
    fn new() -> Self {
        let state = Arc::new(Mutex::new(OutboundState::default()));
        fn worker(state: Arc<Mutex<OutboundState>>) -> mpsc::SyncSender<(Value, Value)> {
            let (tx, rx) = mpsc::sync_channel::<(Value, Value)>(64);
            std::thread::spawn(move || {
                while let Ok((config, request)) = rx.recv() {
                    match str_at(&request, "op").unwrap_or("") {
                        "deliver" => emit(&delivery(&config, &request, Some(&state))),
                        "_send" => {
                            if let Err(error) = send(
                                &config,
                                str_at(&request, "chat").unwrap_or(""),
                                str_at(&request, "text").unwrap_or(""),
                                request["control_deadline_ms"].as_u64().unwrap_or(0),
                                Some(&state),
                            ) {
                                eprintln!("Telegram confirmation: {error}");
                            }
                        }
                        _ => emit(&rpc(&config, &request, Some(&state))),
                    }
                }
            });
            tx
        }
        Self {
            deliveries: worker(Arc::clone(&state)),
            control: worker(Arc::clone(&state)),
            state,
        }
    }
    fn submit(&self, config: &Value, mut request: Value) {
        if request["op"] == "deliver" {
            if let (Some(bot), Some(chat)) =
                (bot_identity(config), str_at(&request["config"], "chatId"))
            {
                if let Some(wait_secs) = self
                    .state
                    .lock()
                    .ok()
                    .and_then(|mut s| s.remaining_cooldown(&bot, chat))
                {
                    let ids: Vec<String> = request["already_sent_ids"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(id)
                        .collect();
                    emit(&json!({
                        "op":"delivered",
                        "delivery_id":request["delivery_id"],
                        "ok":false,
                        "retryable":true,
                        "effect_unknown":false,
                        "external_msg_id":ids.last(),
                        "external_msg_ids":ids,
                        "error":"Telegram sendMessage rate limited; waiting for retry_after cooldown",
                        "retry_after":wait_secs
                    }));
                    return;
                }
            }
        }
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
    inbound_mode: String,
    cursor: u64,
    last_update_ms: u64,
    pending: VecDeque<Pending>,
    reply_chats: HashMap<String, String>,
    pair_chats: HashMap<String, (String, String)>,
}
impl Runtime {
    fn can_poll(&self) -> bool {
        self.inbound_mode != "outbound_only"
            && self.pending.is_empty()
            && str_at(&self.config, "botToken").is_some()
    }

    // Stop sending the old offset before a long poll can cross Telegram's
    // one-week boundary and silently acknowledge an unrelated new ID range.
    fn rollover_probe(&self, now: u64) -> bool {
        self.cursor > 0
            && self.last_update_ms > 0
            && now.saturating_sub(self.last_update_ms) >= 604_800_000 - 35_000
    }
    fn save(&mut self) {
        self.save_with_rollover(false);
    }
    fn save_with_rollover(&mut self, rollover: bool) {
        let mut cursor = self.cursor;
        while self.pending.front().map(|p| p.done).unwrap_or(false) {
            if let Some(p) = self.pending.pop_front() {
                cursor = cursor.max(p.cursor);
            }
        }
        if cursor > self.cursor || rollover {
            self.cursor = cursor;
            let mut frame = json!({"op":"cursor_save","cursor":cursor.to_string(),"last_update_ms":self.last_update_ms});
            if rollover {
                frame["rollover"] = json!(true);
            }
            emit(&frame);
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
    fn updates(&mut self, updates: Value, rollover_probe: bool) {
        let Some(mut updates) = updates.as_array().cloned() else {
            return;
        };
        updates.sort_by_key(|u| u["update_id"].as_u64().unwrap_or(0));
        let max_id = updates.iter().filter_map(|u| u["update_id"].as_u64()).max();
        let rollover = rollover_probe
            && self.cursor > 0
            && self.pending.is_empty()
            && max_id.is_some_and(|m| m < self.cursor)
            && updates.iter().any(|u| {
                let date = u["message"]["date"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_mul(1000);
                date > self.last_update_ms
            });
        if rollover {
            // Telegram resets update_id after ~1 week of inactivity.
            self.cursor = 0;
            self.reply_chats.clear();
            self.pair_chats.clear();
            self.last_update_ms = updates
                .iter()
                .filter_map(|u| u["message"]["date"].as_u64())
                .map(|date| date.saturating_mul(1000).min(now_ms()))
                .max()
                .unwrap_or_else(now_ms);
            // Reset the Hub deduplication floor before any reply from this batch.
            self.save_with_rollover(true);
        }
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
            let date = u["message"]["date"]
                .as_u64()
                .unwrap_or(0)
                .saturating_mul(1000);
            self.last_update_ms = self.last_update_ms.max(if date == 0 {
                now_ms()
            } else {
                date.min(now_ms())
            });
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
            let display_label = str_at(&m["from"], "username")
                .map(|s| format!("@{}", s.trim_start_matches('@')))
                .or_else(|| {
                    let first = str_at(&m["from"], "first_name")
                        .map(clean_inline)
                        .unwrap_or_default();
                    let last = str_at(&m["from"], "last_name")
                        .map(clean_inline)
                        .unwrap_or_default();
                    let full = format!("{first} {last}").trim().to_owned();
                    (!full.is_empty()).then_some(full)
                })
                .unwrap_or_else(|| format!("Chat {chat}"));
            emit(
                &json!({"op":"pair_redeem","code":code,"external_chat_id":chat,"external_user_id":user,"is_private_chat":true,"display_label":display_label,"user_config_patch":{"chatId":chat}}),
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
                let old_identity = bot_identity(&self.config);
                let token_changed =
                    old_identity.is_some() && old_identity != bot_identity(&r["hub_config"]);
                self.config = r["hub_config"].clone();
                let old_mode = std::mem::replace(
                    &mut self.inbound_mode,
                    r["inbound_mode"].as_str().unwrap_or("primary").into(),
                );
                if token_changed {
                    self.cursor = 0;
                    self.last_update_ms = 0;
                } else if let Some(cursor) = id(&r["cursor"]).and_then(|s| s.parse().ok()) {
                    let update_ms = r["cursor_last_update_ms"].as_u64();
                    if cursor >= self.cursor
                        && update_ms.is_none_or(|seen| seen >= self.last_update_ms)
                    {
                        self.cursor = cursor;
                        self.last_update_ms = update_ms.unwrap_or(self.last_update_ms);
                    }
                }
                if token_changed || old_mode != self.inbound_mode {
                    self.pending.clear();
                    self.reply_chats.clear();
                    self.pair_chats.clear();
                }
            }
            "format_pair" | "test" | "deliver" => self.outbound.submit(&self.config, r.clone()),
            "reply_ack" => {
                if let Some(key) = id(&r["update_id"]) {
                    if let Some(chat) = self.reply_chats.remove(&key) {
                        if r["duplicate"] != true {
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
                            self.outbound.submit(
                                &self.config,
                                json!({"op":"_send","chat":chat,"text":text}),
                            );
                        }
                        self.done(&key);
                    }
                }
            }
            "pair_ack" => {
                if let Some(code) = str_at(&r, "code") {
                    if let Some((chat, key)) = self.pair_chats.remove(code) {
                        if r["duplicate"] != true {
                            let text = if r["ok"] == true {
                                let owner = str_at(&r, "owner_user")
                                    .map(|u| u.trim_start_matches('@'))
                                    .filter(|u| !u.is_empty())
                                    .unwrap_or("user");
                                format!("Paired with Cito as @{owner}")
                            } else {
                                format!(
                                    "Pairing failed: {}",
                                    str_at(&r, "error").unwrap_or("Invalid or expired code")
                                )
                            };
                            self.outbound.submit(
                                &self.config,
                                json!({"op":"_send","chat":chat,"text":text}),
                            );
                        }
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
fn permanent_poll_error(error: &ApiFailure) -> bool {
    matches!(error.status, Some(401 | 403))
}
#[derive(Default)]
struct PollBackoff {
    failures: u32,
}
impl PollBackoff {
    fn failed(&mut self, error: &ApiFailure, jitter_ms: u64) -> (bool, Duration) {
        self.failures = self.failures.saturating_add(1);
        let delay = 500u64.saturating_mul(1 << self.failures.saturating_sub(1).min(6));
        (
            self.failures >= 3,
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
    Poll(u64, bool, std::result::Result<Value, ApiFailure>),
    ServiceLabel(Value, Option<String>),
}
pub fn main() {
    let (tx, rx) = mpsc::channel::<Event>();
    stdin_events(tx.clone(), Event::Request);
    let mut runtime = Runtime {
        outbound: Outbound::new(),
        config: json!({}),
        inbound_mode: "primary".into(),
        cursor: 0,
        last_update_ms: 0,
        pending: VecDeque::new(),
        reply_chats: HashMap::new(),
        pair_chats: HashMap::new(),
    };
    let mut polling = false;
    let mut backoff = PollBackoff::default();
    let mut next_poll = std::time::Instant::now();
    let mut healthy_reported = false;
    let mut paused = false;
    let mut poll_generation = 0u64;
    let mut label_emitted = false;
    let mut label_pending = false;
    let mut label_requested = false;
    loop {
        let can_poll = !polling && !paused && runtime.can_poll();
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
        match event {
            Some(Event::Request(req)) => {
                let configured = req["op"] == "configure";
                let config_changed = configured && req["hub_config"] != runtime.config;
                let mode_changed = configured
                    && req["inbound_mode"].as_str().unwrap_or("primary") != runtime.inbound_mode;
                if !runtime.request(req) {
                    return;
                }
                if config_changed || mode_changed || (configured && paused) {
                    poll_generation = poll_generation.wrapping_add(1);
                    healthy_reported = false;
                    paused = false;
                    backoff.succeeded();
                    next_poll = std::time::Instant::now();
                }
                if config_changed {
                    label_emitted = false;
                }
                if configured {
                    label_requested = str_at(&runtime.config, "botToken").is_some()
                        && (!label_emitted || runtime.inbound_mode == "outbound_only");
                }
            }
            Some(Event::ServiceLabel(config, label)) => {
                label_pending = false;
                if config == runtime.config {
                    if let Some(label) = label {
                        let username = label.trim_start_matches('@').to_owned();
                        let switched = bot_identity(&config).is_some_and(|bot| {
                            runtime.outbound.state.lock().is_ok_and(|mut state| {
                                state.set_bot_username(bot, username.clone())
                            })
                        });
                        let mut health =
                            json!({"op":"health","service_label":label,"bot_username":username});
                        if switched {
                            health["delivery_unverified"] = json!(true);
                            health["delivery_guidance"] = json!(format!(
                                "Switched to @{username}. Send /start to @{username} or re-pair your account to verify delivery."
                            ));
                        }
                        // Credential lookup cannot prove that inbound polling is healthy.
                        if runtime.inbound_mode == "outbound_only" {
                            health["error"] = Value::Null;
                        }
                        emit(&health);
                        label_emitted = true;
                        label_requested = false;
                    }
                }
            }
            Some(Event::Poll(generation, rollover_probe, result)) => {
                polling = false;
                if generation == poll_generation && runtime.inbound_mode != "outbound_only" {
                    match result {
                        Ok(updates) => {
                            backoff.succeeded();
                            if !healthy_reported {
                                emit(&json!({"op":"health","error":null}));
                                healthy_reported = true;
                            }
                            runtime.updates(updates, rollover_probe);
                        }
                        Err(e) => {
                            eprintln!("Telegram polling: {e}");
                            let conflict = e.status == Some(409);
                            let guidance = if conflict {
                                "Inbound polling conflict: this bot is active on another Hub or webhook. Outbound alerts still work; switch to Outbound-Only or use a dedicated bot token."
                            } else if e.status == Some(401) {
                                "Check or replace the Bot Token in Human Channels Settings."
                            } else {
                                "Check the network, Bot Token, and bot permissions."
                            };
                            let (report, delay) = backoff.failed(&e, now_ms() % 500);
                            paused = permanent_poll_error(&e);
                            if paused || report || conflict {
                                healthy_reported = false;
                                emit(
                                    &json!({"op":"health","error":format!("Cannot receive replies or pair accounts: {e}. {guidance}")}),
                                );
                            }
                            next_poll = std::time::Instant::now() + delay;
                        }
                    }
                }
            }
            None => {}
        }
        // Single-flight identity lookup; configure frames remain responsive even
        // when the network stalls or credentials change during a lookup.
        if label_requested && !label_pending {
            label_requested = false;
            label_pending = true;
            let config = runtime.config.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                let label = api_with_timeout(&config, "getMe", json!({}), Duration::from_secs(8))
                    .ok()
                    .and_then(|me| str_at(&me, "username").map(|name| format!("@{name}")));
                let _ = tx.send(Event::ServiceLabel(config, label));
            });
        }
        if !polling && !paused && runtime.can_poll() && std::time::Instant::now() >= next_poll {
            polling = true;
            let generation = poll_generation;
            let config = runtime.config.clone();
            let rollover_probe = runtime.rollover_probe(now_ms());
            let mut params = json!({"timeout":25,"limit":25,"allowed_updates":["message"]});
            if !rollover_probe {
                params["offset"] = json!(runtime.cursor);
            }
            let tx = tx.clone();
            std::thread::spawn(move || {
                let r = api(&config, "getUpdates", params);
                let _ = tx.send(Event::Poll(generation, rollover_probe, r));
            });
            next_poll = std::time::Instant::now() + Duration::from_millis(100);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn poll_success_requires_an_updates_array() {
        for reply in [
            json!({"ok":true}),
            json!({"ok":true,"result":null}),
            json!({"ok":true,"result":{"update_id":42}}),
        ] {
            let error = api_result("getUpdates", reply).unwrap_err();
            assert!(error.retryable);
            assert!(!permanent_poll_error(&error));
        }
        assert_eq!(
            api_result("getUpdates", json!({"ok":true,"result":[]})).unwrap(),
            json!([])
        );
        let bot = json!({"username":"sample_bot"});
        assert_eq!(
            api_result("getMe", json!({"ok":true,"result":bot})).unwrap(),
            bot
        );
    }

    #[test]
    fn polling_hysteresis_backoff_and_permanent_auth_errors() {
        let failure = |status: Option<u16>, message: &str| ApiFailure {
            message: message.into(),
            status,
            retryable: false,
            effect_unknown: false,
            retry_after: None,
        };
        for (status, message) in [
            (None, "Telegram getUpdates request failed"),
            (Some(502), "Telegram getUpdates HTTP 502: Bad gateway"),
            (Some(409), "Telegram getUpdates HTTP 409: Conflict"),
            (Some(409), "Telegram getUpdates API 409: Conflict"),
        ] {
            let err = failure(status, message);
            assert!(!permanent_poll_error(&err));
            let mut backoff = PollBackoff::default();
            assert_eq!(backoff.failed(&err, 0), (false, Duration::from_millis(500)));
            assert_eq!(
                backoff.failed(&err, 100),
                (false, Duration::from_millis(1100))
            );
            assert_eq!(
                backoff.failed(&err, 200),
                (true, Duration::from_millis(2200))
            );
            for _ in 0..100 {
                assert!(backoff.failed(&err, 499).1 <= Duration::from_millis(32499));
            }
            backoff.succeeded();
            assert!(!backoff.failed(&err, 0).0);
        }
        for (status, message) in [
            (Some(401), "Telegram getUpdates HTTP 401: Unauthorized"),
            (Some(403), "Telegram getUpdates API 403: Forbidden"),
        ] {
            assert!(permanent_poll_error(&failure(status, message)));
        }
    }
    #[test]
    fn send_message_5xx_and_incomplete_classify_as_effect_unknown() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        let err_502 = api_result(
            "sendMessage",
            json!({"ok":false,"error_code":502,"description":"Bad gateway"}),
        )
        .unwrap_err();
        assert_eq!(err_502.status, Some(502));
        assert!(err_502.effect_unknown);
        assert!(!err_502.retryable);
        let poll_502 = api_result(
            "getUpdates",
            json!({"ok":false,"error_code":502,"description":"Bad gateway"}),
        )
        .unwrap_err();
        assert_eq!(poll_502.status, Some(502));
        assert!(!poll_502.effect_unknown);
        assert!(poll_502.retryable);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = json!({"botToken":"111:test","apiBaseUrl":format!("http://{}",listener.local_addr().unwrap())});
        let worker = std::thread::spawn(move || {
            for mode in ["500", "incomplete"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                let mut len = 0usize;
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                reader.read_exact(&mut vec![0; len]).unwrap();
                if mode == "500" {
                    let body =
                        json!({"ok":false,"error_code":500,"description":"Internal"}).to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                } else {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: 50\r\nConnection: close\r\n\r\n{{\"ok\":true"
                    );
                }
            }
        });
        for id in ["d1", "d2"] {
            let res = delivery(
                &config,
                &json!({"delivery_id":id,"claim_until_ms":now_ms()+10_000,"config":{"chatId":"9"},"signal":{"message":"hi"}}),
                None,
            );
            assert_eq!(res["ok"], false);
            assert_eq!(res["effect_unknown"], true);
            assert_eq!(res["retryable"], false);
        }
        worker.join().unwrap();
    }
    #[test]
    fn shared_429_cooldown_blocks_sibling_sends_and_survives_noop_configure() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_srv = Arc::clone(&calls);
        let config = json!({"botToken":"111:test","apiBaseUrl":format!("http://{}",listener.local_addr().unwrap())});
        let worker = std::thread::spawn(move || {
            for i in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                calls_srv.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                let mut len = 0usize;
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                reader.read_exact(&mut vec![0; len]).unwrap();
                if i == 0 {
                    let body = json!({"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":30}}).to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                } else {
                    let body = json!({"ok":true,"result":{"message_id":77}}).to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            }
        });
        let state = Arc::new(Mutex::new(OutboundState::default()));
        let first = delivery(
            &config,
            &json!({"delivery_id":"d1","claim_until_ms":now_ms()+10_000,"config":{"chatId":"chatA"},"signal":{"message":"one"}}),
            Some(&state),
        );
        assert_eq!(first["ok"], false);
        assert_eq!(first["retryable"], true);
        assert_eq!(first["retry_after"], 30);
        // Sibling send to same bot+chat must not hit HTTP.
        let sibling = delivery(
            &config,
            &json!({"delivery_id":"d2","claim_until_ms":now_ms()+10_000,"config":{"chatId":"chatA"},"signal":{"message":"two"}}),
            Some(&state),
        );
        assert_eq!(sibling["ok"], false);
        assert_eq!(sibling["retryable"], true);
        assert!(sibling["retry_after"].as_u64().is_some_and(|s| s >= 29));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Different chat on same bot is not blocked.
        let other = delivery(
            &config,
            &json!({"delivery_id":"d3","claim_until_ms":now_ms()+10_000,"config":{"chatId":"chatB"},"signal":{"message":"three"}}),
            Some(&state),
        );
        assert_eq!(other["ok"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        worker.join().unwrap();
    }
    #[test]
    fn idempotent_pairing_receipt_and_one_week_update_id_rollover() {
        let mut runtime = Runtime {
            outbound: Outbound::new(),
            config: json!({"botToken":"111:first"}),
            inbound_mode: "primary".into(),
            cursor: 500_000_000,
            last_update_ms: now_ms() - 604_800_000,
            pending: VecDeque::new(),
            reply_chats: HashMap::new(),
            pair_chats: HashMap::new(),
        };
        // A low ID is ignored until polling without the old offset after inactivity.
        let batch = json!([
            {"update_id":100,"message":{"date":now_ms() / 1000,"chat":{"id":9,"type":"private"},"from":{"id":9,"username":"alice"},"text":"/start cito-rollover"}}
        ]);
        assert!(!runtime.rollover_probe(runtime.last_update_ms + 60_000));
        runtime.updates(batch.clone(), false);
        assert_eq!(runtime.cursor, 500_000_000);
        assert!(runtime.rollover_probe(now_ms()));
        runtime.updates(batch, true);
        assert_eq!(runtime.cursor, 0);
        assert!(runtime.pair_chats.contains_key("cito-rollover"));
        // Replay idempotent pairing receipt (duplicate: true) advances cursor without error.
        runtime.request(json!({"op":"pair_ack","code":"cito-rollover","ok":true,"duplicate":true,"owner_user":"alice"}));
        assert!(!runtime.pair_chats.contains_key("cito-rollover"));
        assert_eq!(runtime.cursor, 101);
    }
    #[test]
    fn sender_identity_and_bot_switch_unverified_guidance() {
        assert_eq!(format_sender(&json!({"title":"work"})), "work");
        assert_eq!(
            format_sender(&json!({"title":"  alice\nspoof  "})),
            "alice spoof"
        );
        assert_eq!(
            format_sender(
                &json!({"harness":"pi","host_name":"mac","listener_id":"none#alice:pi:mac:1"})
            ),
            "pi@mac"
        );
        assert_eq!(format_sender(&json!({})), "sender");

        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = json!({"botToken":"222:new","apiBaseUrl":format!("http://{}",listener.local_addr().unwrap())});
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            let mut len = 0usize;
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            reader.read_exact(&mut vec![0; len]).unwrap();
            let body = json!({"ok":false,"error_code":403,"description":"Forbidden: bot can't initiate conversation with a user"}).to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        let state = Arc::new(Mutex::new(OutboundState::default()));
        state
            .lock()
            .unwrap()
            .set_bot_username("222".into(), "new_bot".into());
        let res = delivery(
            &config,
            &json!({"delivery_id":"d-switch","claim_until_ms":now_ms()+10_000,"config":{"chatId":"9"},"signal":{"message":"hello"}}),
            Some(&state),
        );
        worker.join().unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["delivery_unverified"], true);
        let err = res["error"].as_str().unwrap();
        assert!(err.contains("/start"));
        assert!(err.contains("@new_bot"));
        assert!(err.contains("re-pair"));
    }
    #[test]
    fn configure_retains_monotonic_cursor_and_ack_without_keyboard_is_noop() {
        let mut runtime = Runtime {
            outbound: Outbound::new(),
            config: json!({"botToken":"first"}),
            inbound_mode: "primary".into(),
            cursor: 20,
            last_update_ms: 0,
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
        assert!(runtime.can_poll());
        runtime.request(json!({"op":"configure","hub_config":{"botToken":"second"},"cursor":30,"inbound_mode":"outbound_only"}));
        assert!(!runtime.can_poll());
        assert_eq!(runtime.cursor, 30);
        runtime.request(json!({"op":"configure","hub_config":{"botToken":"second"},"cursor":10}));
        assert!(runtime.can_poll());
        assert_eq!(runtime.cursor, 30);
    }
}
