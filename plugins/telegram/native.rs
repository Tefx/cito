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
    use std::io::Read;
    let token = str_at(config, "botToken").ok_or_else(|| ApiFailure {
        message: "Bot Token is required".into(),
        retryable: false,
        effect_unknown: false,
    })?;
    let base = str_at(config, "apiBaseUrl")
        .unwrap_or("https://api.telegram.org")
        .trim_end_matches('/');
    let response = match agent(method == "getUpdates")
        .post(&format!("{base}/bot{token}/{method}"))
        .send_json(body)
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
            });
        }
    };
    let v: Value =
        serde_json::from_reader(response.into_reader().take(1_048_576)).map_err(|_| {
            ApiFailure {
                message: format!("Telegram {method} response was incomplete"),
                retryable: false,
                effect_unknown: method == "sendMessage",
            }
        })?;
    if v["ok"] != true {
        let status = v["error_code"].as_u64().unwrap_or(400);
        return Err(ApiFailure {
            message: format!(
                "Telegram {method} API {status}: {}",
                str_at(&v, "description").unwrap_or("Telegram API returned an error")
            ),
            retryable: matches!(status, 408 | 429 | 500..=599),
            effect_unknown: false,
        });
    }
    Ok(v["result"].clone())
}
fn send(config: &Value, chat: &str, text: &str) -> Result<Vec<String>> {
    let mut ids = vec![];
    for chunk in text.chars().collect::<Vec<_>>().chunks(4096) {
        let text: String = chunk.iter().collect();
        let r = api(config, "sendMessage", json!({"chat_id":chat,"text":text}))?;
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
        })?;
        let s = &req["signal"];
        let from = str_at(&s["from"], "title")
            .or_else(|| str_at(&s["from"], "listener_id"))
            .unwrap_or("sender");
        let text = format!(
            "🔔 Cito · {from}\n\n{}",
            s["message"].as_str().unwrap_or("")
        );
        let chars: Vec<char> = text.chars().collect();
        for chunk in chars.chunks(4096).skip(ids.len()) {
            let text: String = chunk.iter().collect();
            let r = api(config, "sendMessage", json!({"chat_id":chat,"text":text}))?;
            ids.push(id(&r["message_id"]).ok_or_else(|| ApiFailure {
                message: "Telegram message_id is missing".into(),
                retryable: false,
                effect_unknown: true,
            })?);
            emit(
                &json!({"op":"delivered","delivery_id":req["delivery_id"],"progress":true,"external_msg_ids":ids}),
            );
        }
        Ok(())
    })();
    let (ok, retryable, effect_unknown, error) = match result {
        Ok(()) => (true, false, false, None),
        Err(e) => (false, e.retryable, e.effect_unknown, Some(e.message)),
    };
    json!({"op":"delivered","delivery_id":req["delivery_id"],"ok":ok,"retryable":retryable,"effect_unknown":effect_unknown,"external_msg_id":ids.last(),"external_msg_ids":ids,"error":error})
}
fn rpc(config: &Value, req: &Value) -> Value {
    let c = if req["hub_config"].is_object() {
        &req["hub_config"]
    } else {
        config
    };
    let result = (|| -> Result<Value> {
        let me = api(c, "getMe", json!({}))?;
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
struct Pending {
    cursor: u64,
    key: Option<String>,
    done: bool,
}
struct Runtime {
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
                } else if self.pending.is_empty() {
                    self.cursor = id(&r["cursor"]).and_then(|s| s.parse().ok()).unwrap_or(0);
                }
            }
            "format_pair" | "test" => emit(&rpc(&self.config, &r)),
            "deliver" => emit(&delivery(&self.config, &r)),
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
                        if let Err(e) = send(&self.config, &chat, &text) {
                            eprintln!("Telegram reply confirmation: {e}");
                        }
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
                        if let Err(e) = send(&self.config, &chat, &text) {
                            eprintln!("Telegram pairing confirmation: {e}");
                        }
                        self.done(&key);
                    }
                }
            }
            "on_acked" => {
                if let (Some(chat), Some(ids)) = (
                    str_at(&r["user_config"], "chatId"),
                    r["external_msg_ids"].as_array(),
                ) {
                    for msg in ids {
                        if let Some(mid) = id(msg) {
                            let _ = api(
                                &self.config,
                                "editMessageReplyMarkup",
                                json!({"chat_id":chat,"message_id":mid.parse::<u64>().ok(),"reply_markup":{"inline_keyboard":[]}}),
                            );
                        }
                    }
                }
            }
            _ => eprintln!("Unknown Telegram plugin operation"),
        };
        true
    }
}
pub fn main() {
    let input = stdin_watchdog();
    let (tx, rx) = mpsc::channel::<(Value, std::result::Result<Value, ApiFailure>)>();
    let mut runtime = Runtime {
        config: json!({}),
        cursor: 0,
        pending: VecDeque::new(),
        reply_chats: HashMap::new(),
        pair_chats: HashMap::new(),
    };
    let mut polling = false;
    let mut next_poll = std::time::Instant::now();
    loop {
        while let Ok(req) = input.try_recv() {
            if !runtime.request(req) {
                return;
            }
        }
        if let Ok((config, result)) = rx.try_recv() {
            polling = false;
            if config == runtime.config {
                match result {
                    Ok(updates) => {
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
                        emit(
                            &json!({"op":"health","error":format!("Cannot receive replies or pair accounts: {e}. {guidance}")}),
                        );
                        next_poll = std::time::Instant::now() + Duration::from_secs(1);
                    }
                }
            }
        }
        if !polling
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
                    json!({"offset":cursor,"timeout":25,"allowed_updates":["message"]}),
                );
                let _ = tx.send((config, r));
            });
            next_poll = std::time::Instant::now() + Duration::from_millis(100);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
