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
fn api(config: &Value, method: &str, body: Value) -> Result<Value> {
    let token = str_at(config, "botToken").ok_or("Bot Token is required")?;
    let base = str_at(config, "apiBaseUrl")
        .unwrap_or("https://api.telegram.org")
        .trim_end_matches('/');
    let response = ureq::post(&format!("{base}/bot{token}/{method}"))
        .timeout(Duration::from_secs(if method == "getUpdates" {
            35
        } else {
            10
        }))
        .send_json(body)
        .map_err(|_| format!("Telegram {method} request failed"))?;
    let v: Value = response.into_json().map_err(|e| e.to_string())?;
    if v["ok"] != true {
        return Err(str_at(&v, "description")
            .unwrap_or("Telegram API returned an error")
            .into());
    }
    Ok(v["result"].clone())
}
fn send(config: &Value, chat: &str, text: &str) -> Result<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut ids = vec![];
    for chunk in chars.chunks(4096) {
        let text: String = chunk.iter().collect();
        let r = api(config, "sendMessage", json!({"chat_id":chat,"text":text}))?;
        ids.push(id(&r["message_id"]).ok_or("Telegram message_id is missing")?);
    }
    Ok(ids)
}
fn delivery(config: &Value, req: &Value) -> Value {
    let result = (|| {
        let chat = str_at(&req["config"], "chatId").ok_or("Chat ID is required")?;
        let s = &req["signal"];
        let from = str_at(&s["from"], "title")
            .or_else(|| str_at(&s["from"], "listener_id"))
            .unwrap_or("sender");
        let text = format!(
            "🔔 Cito · {from}\n\n{}",
            s["message"].as_str().unwrap_or("")
        );
        send(config, chat, &text)
    })();
    match result {
        Ok(ids) => {
            json!({"op":"delivered","delivery_id":req["delivery_id"],"ok":true,"external_msg_id":ids.last(),"external_msg_ids":ids,"error":null})
        }
        Err(e) => {
            json!({"op":"delivered","delivery_id":req["delivery_id"],"ok":false,"external_msg_id":null,"external_msg_ids":[],"error":e})
        }
    }
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
                json!({"pairing_command":format!("/start {}",str_at(req,"code").ok_or("Pairing code is required")?),"instruction":format!("Send \"/start {}\" to @{username}",str_at(req,"code").ok_or("Pairing code is required")?),"bot_username":username}),
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
    let (tx, rx) = mpsc::channel();
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
                    Ok(updates) => runtime.updates(updates),
                    Err(e) => {
                        eprintln!("Telegram polling: {e}");
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
