use crate::plugins::{expand_home_dir, now_ms, validate_https_issuer};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

extern "C" fn handle_hub_sig(_sig: libc::c_int) {
    unsafe {
        libc::_exit(0);
    }
}

#[derive(Clone, Debug)]
struct KvRecord {
    key: String,
    revision: u64,
    created_ms: u64,
    data_b64: String,
    operation: String,
}

#[derive(Clone, Debug)]
struct SignalRecord {
    seq: u64,
    msg_id: Option<String>,
}

struct HubState {
    next_rev: u64,
    next_seq: u64,
    kv: HashMap<String, KvRecord>,
    signals: Vec<SignalRecord>,
    consumers: HashMap<String, u64>,
    sse_clients: Vec<TcpStream>,
}

impl HubState {
    fn new() -> Self {
        Self {
            next_rev: 1,
            next_seq: 1,
            kv: HashMap::new(),
            signals: Vec::new(),
            consumers: HashMap::new(),
            sse_clients: Vec::new(),
        }
    }

    fn broadcast_sse(&mut self, event: &Value) {
        let Ok(s) = serde_json::to_string(event) else { return };
        let frame = format!("data: {s}\n\n");
        self.sse_clients
            .retain_mut(|stream| stream.write_all(frame.as_bytes()).and_then(|_| stream.flush()).is_ok());
    }
}

fn check_auth(
    auth_mode: &str,
    expected_token: Option<&str>,
    headers: &HashMap<String, String>,
) -> bool {
    if auth_mode == "none" {
        return true;
    }
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .or_else(|| headers.get("x-cito-token").map(String::as_str).map(str::trim));
    let Some(tok) = bearer.filter(|s| !s.is_empty()) else {
        return false;
    };
    if auth_mode == "token" {
        return Some(tok) == expected_token;
    }
    // oidc mode: require 3-segment JWT
    tok.split('.').count() == 3
}

fn handle_hub_rpc(state: &mut HubState, op: &str, params: &Map<String, Value>) -> Result<Value, (u16, &'static str, String)> {
    let now = now_ms();
    match op {
        "account_info" => {
            let streams = if state.kv.is_empty() && state.signals.is_empty() { 0 } else { 2 };
            Ok(json!({
                "streams": streams,
                "consumers": state.consumers.len(),
                "memory": 0,
                "storage": 0,
            }))
        }
        "stream_info" | "stream_add" | "stream_update" => {
            let name = params
                .get("stream")
                .and_then(|v| v.as_str())
                .or_else(|| params.get("config").and_then(|c| c.get("name")).and_then(|v| v.as_str()))
                .unwrap_or("KV_cito_listeners");
            let cfg = if name == "CITO_SIGNALS" {
                json!({
                    "name": "CITO_SIGNALS",
                    "subjects": ["cito.signal.*"],
                    "retention": "limits",
                    "max_age": 604800000000000u64,
                    "duplicate_window": 604800000000000u64,
                    "storage": "file",
                    "discard": "old",
                })
            } else {
                json!({
                    "name": "KV_cito_listeners",
                    "subjects": ["$KV.cito_listeners.>"],
                    "max_msgs_per_subject": 1,
                    "max_age": 0,
                    "retention": "limits",
                    "storage": "file",
                    "discard": "old",
                })
            };
            Ok(json!({
                "config": cfg,
                "state": {
                    "messages": state.signals.len(),
                    "bytes": 0,
                    "first_seq": if state.signals.is_empty() { 0 } else { 1 },
                    "last_seq": state.signals.len(),
                    "consumer_count": state.consumers.len(),
                },
                "ts_ms": now,
            }))
        }
        "kv_get" => {
            let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(rec) = state.kv.get(key) {
                Ok(json!({
                    "bucket": "cito_listeners",
                    "key": rec.key,
                    "value_b64": rec.data_b64,
                    "revision": rec.revision,
                    "created_ms": rec.created_ms,
                    "operation": rec.operation,
                }))
            } else {
                Ok(Value::Null)
            }
        }
        "kv_put" | "kv_create" | "kv_update" => {
            let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let data_b64 = params.get("data_b64").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let rev = state.next_rev;
            state.next_rev += 1;
            let rec = KvRecord {
                key: key.clone(),
                revision: rev,
                created_ms: now,
                data_b64: data_b64.clone(),
                operation: "PUT".to_string(),
            };
            state.kv.insert(key.clone(), rec);
            state.broadcast_sse(&json!({
                "type": "kv",
                "key": key,
                "revision": rev,
                "created_ms": now,
                "value_b64": data_b64,
                "operation": "PUT",
            }));
            Ok(json!({ "revision": rev }))
        }
        "kv_delete" => {
            let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let rev = state.next_rev;
            state.next_rev += 1;
            state.kv.remove(&key);
            state.broadcast_sse(&json!({
                "type": "kv",
                "key": key,
                "revision": rev,
                "created_ms": now,
                "value_b64": "",
                "operation": "DEL",
            }));
            Ok(json!({ "revision": rev }))
        }
        "kv_keys" => {
            let keys: Vec<String> = state.kv.keys().cloned().collect();
            Ok(json!({ "keys": keys }))
        }
        "kv_snapshot" => {
            let entries: Vec<Value> = state
                .kv
                .values()
                .map(|r| {
                    json!({
                        "bucket": "cito_listeners",
                        "key": r.key,
                        "value_b64": r.data_b64,
                        "revision": r.revision,
                        "created_ms": r.created_ms,
                        "operation": r.operation,
                        "delta": 0,
                    })
                })
                .collect();
            Ok(json!({ "entries": entries }))
        }
        "js_publish" => {
            let subject = params.get("subject").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let data_b64 = params.get("data_b64").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let msg_id = params
                .get("opts")
                .and_then(|v| v.get("msgID"))
                .and_then(|v| v.as_str())
                .map(String::from);
            if let Some(ref mid) = msg_id {
                if let Some(existing) = state.signals.iter().find(|s| s.msg_id.as_deref() == Some(mid.as_str())) {
                    return Ok(json!({
                        "stream": "CITO_SIGNALS",
                        "seq": existing.seq,
                        "duplicate": true,
                    }));
                }
            }
            let seq = state.next_seq;
            state.next_seq += 1;
            state.signals.push(SignalRecord {
                seq,
                msg_id,
            });
            state.broadcast_sse(&json!({
                "type": "signal",
                "seq": seq,
                "subject": subject,
                "created_ms": now,
                "data_b64": data_b64,
            }));
            Ok(json!({
                "stream": "CITO_SIGNALS",
                "seq": seq,
                "duplicate": false,
            }))
        }
        "nc_publish" => {
            let subject = params.get("subject").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let data_b64 = params.get("data_b64").and_then(|v| v.as_str()).unwrap_or("").to_string();
            state.broadcast_sse(&json!({
                "type": "nc",
                "subject": subject,
                "data_b64": data_b64,
            }));
            Ok(json!({ "ok": true }))
        }
        "nc_subscribe_check" => Ok(json!({ "ok": true })),
        "consumer_add" => {
            let name = params
                .get("config")
                .and_then(|c| c.get("durable_name").or_else(|| c.get("name")))
                .and_then(|v| v.as_str())
                .unwrap_or("consumer")
                .to_string();
            state.consumers.entry(name.clone()).or_insert(0);
            Ok(json!({ "name": name }))
        }
        "consumer_backlog" => Ok(json!({ "messages": Vec::<Value>::new() })),
        "consumer_next" => Ok(Value::Null),
        "consumer_ack" | "consumer_delete" => Ok(json!({ "ok": true })),
        "signal_claim_dispatch" => {
            let ids = params
                .get("signal_ids")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            Ok(json!({ "claimed": ids, "already_acked": [] }))
        }
        "signal_ack" => Ok(json!({ "acked": 0 })),
        "human_channels_list" => Ok(json!({ "channels": [] })),
        "human_channels_configure" | "human_channels_test" | "human_channel_pair_start" | "human_channel_poll" => {
            Ok(json!({ "ok": true, "detail": "OK", "code": "cito-000000", "instruction": "" }))
        }
        _ => Err((400, "INVALID_INPUT", format!("Unknown hub RPC op: {op}"))),
    }
}

fn handle_hub_connection(
    mut stream: TcpStream,
    state: Arc<Mutex<HubState>>,
    auth_mode: String,
    token: Option<String>,
) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut req_line = String::new();
    if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");
    let clean_path = path.split('?').next().unwrap_or("/");

    let mut headers = HashMap::new();
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key == "content-length" {
                content_length = val.parse::<usize>().unwrap_or(0);
            }
            headers.insert(key, val);
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body);
    }

    if clean_path == "/health" && method == "GET" {
        let payload = serde_json::to_string(&json!({ "ok": true, "auth_mode": auth_mode })).unwrap();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        return;
    }

    if !check_auth(&auth_mode, token.as_deref(), &headers) {
        let payload = serde_json::to_string(&json!({
            "ok": false,
            "error": { "code": "AUTHORIZATION_VIOLATION", "message": "Authorization Violation" }
        }))
        .unwrap();
        let resp = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        return;
    }

    if clean_path == "/v1/events" && method == "GET" {
        let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache, no-transform\r\nConnection: keep-alive\r\n\r\n:ok\n\n";
        if stream.write_all(resp.as_bytes()).and_then(|_| stream.flush()).is_ok() {
            if let Ok(mut guard) = state.lock() {
                guard.sse_clients.push(stream);
            }
        }
        return;
    }

    if clean_path == "/v1/rpc" && method == "POST" {
        let parsed: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => {
                let err = "{\"ok\":false,\"error\":{\"code\":\"INVALID_INPUT\",\"message\":\"Invalid JSON body\"}}";
                let resp = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    err.len(),
                    err
                );
                let _ = stream.write_all(resp.as_bytes());
                return;
            }
        };
        let Some(obj) = parsed.as_object() else {
            return;
        };
        let op = obj.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let res = {
            let mut guard = state.lock().unwrap();
            handle_hub_rpc(&mut guard, op, obj)
        };
        match res {
            Ok(val) => {
                let payload = serde_json::to_string(&json!({ "ok": true, "result": val })).unwrap();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
            Err((status, code, msg)) => {
                let payload = serde_json::to_string(&json!({
                    "ok": false,
                    "error": { "code": code, "message": msg }
                }))
                .unwrap();
                let resp = format!(
                    "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        }
        return;
    }

    let not_found = "{\"ok\":false,\"error\":{\"code\":\"NOT_FOUND\",\"message\":\"Not found\"}}";
    let resp = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        not_found.len(),
        not_found
    );
    let _ = stream.write_all(resp.as_bytes());
}

pub fn run_hub_cli(args: &[String]) {
    let parse = || -> Result<(u16, String, String, Option<String>, Option<String>, Option<String>), String> {
        let mut raw_port: Option<String> = None;
        let mut raw_bind: Option<String> = None;
        let mut raw_auth: Option<String> = None;
        let mut raw_token: Option<String> = None;
        let mut raw_issuer: Option<String> = None;
        let mut raw_client_id: Option<String> = None;
        let mut raw_data_dir: Option<String> = None;

        let read_val = |arg: &str, idx: usize, flag: &str| -> Result<(String, usize), String> {
            if let Some(rest) = arg.strip_prefix(&format!("{flag}=")) {
                return Ok((rest.to_string(), idx));
            }
            let next = args
                .get(idx + 1)
                .ok_or_else(|| format!("Missing value for {flag}"))?;
            if next.starts_with('-') {
                return Err(format!("Missing value for {flag}"));
            }
            Ok((next.clone(), idx + 1))
        };

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == "--port" || arg.starts_with("--port=") {
                let (v, ni) = read_val(arg, i, "--port")?;
                raw_port = Some(v);
                i = ni;
            } else if arg == "--bind" || arg.starts_with("--bind=") {
                let (v, ni) = read_val(arg, i, "--bind")?;
                raw_bind = Some(v);
                i = ni;
            } else if arg == "--auth" || arg.starts_with("--auth=") {
                let (v, ni) = read_val(arg, i, "--auth")?;
                raw_auth = Some(v);
                i = ni;
            } else if arg == "--token" || arg.starts_with("--token=") {
                let (v, ni) = read_val(arg, i, "--token")?;
                raw_token = Some(v);
                i = ni;
            } else if arg == "--issuer" || arg.starts_with("--issuer=") {
                let (v, ni) = read_val(arg, i, "--issuer")?;
                raw_issuer = Some(v);
                i = ni;
            } else if arg == "--client-id" || arg.starts_with("--client-id=") {
                let (v, ni) = read_val(arg, i, "--client-id")?;
                raw_client_id = Some(v);
                i = ni;
            } else if arg == "--data-dir" || arg.starts_with("--data-dir=") {
                let (v, ni) = read_val(arg, i, "--data-dir")?;
                raw_data_dir = Some(v);
                i = ni;
            } else if arg.starts_with('-') {
                return Err(format!("Unknown flag: {arg}"));
            } else {
                return Err(format!("Unexpected argument: {arg}"));
            }
            i += 1;
        }

        let auth_str = raw_auth
            .or_else(|| env::var("CITO_AUTH_MODE").ok())
            .unwrap_or_else(|| "none".to_string());
        let auth_mode = auth_str.trim().to_string();
        if !matches!(auth_mode.as_str(), "none" | "token" | "oidc") {
            return Err("CITO_AUTH_MODE must be none, token, or oidc".to_string());
        }

        let port_str = raw_port
            .or_else(|| env::var("CITO_HUB_PORT").ok())
            .unwrap_or_else(|| "4291".to_string());
        if port_str.trim().is_empty() {
            return Err("CITO_HUB_PORT must be an integer in 0..65535".to_string());
        }
        let port_num = port_str
            .trim()
            .parse::<u32>()
            .map_err(|_| "CITO_HUB_PORT must be an integer in 0..65535".to_string())?;
        if port_num > 65535 {
            return Err("CITO_HUB_PORT must be an integer in 0..65535".to_string());
        }

        let default_bind = if auth_mode == "none" { "127.0.0.1" } else { "0.0.0.0" };
        let bind_raw = raw_bind
            .or_else(|| env::var("CITO_HUB_BIND").ok())
            .unwrap_or_else(|| default_bind.to_string());
        let bind = bind_raw.trim().to_string();
        if bind.is_empty() {
            return Err("CITO_HUB_BIND must not be empty".to_string());
        }

        let tok_opt = raw_token.or_else(|| env::var("CITO_HUB_TOKEN").ok());
        if let Some(ref t) = tok_opt {
            if t.trim().is_empty() {
                return Err("CITO_HUB_TOKEN must not be empty".to_string());
            }
        }
        if auth_mode == "token" && tok_opt.as_deref().unwrap_or("").trim().is_empty() {
            return Err("CITO_HUB_TOKEN is required in token mode".to_string());
        }

        let iss_opt = raw_issuer.or_else(|| env::var("CITO_OIDC_ISSUER").ok());
        let cid_opt = raw_client_id.or_else(|| env::var("CITO_OIDC_CLIENT_ID").ok());
        let mut oidc_issuer = None;
        let mut oidc_client_id = None;
        if auth_mode == "oidc" || iss_opt.is_some() {
            let iss_raw = iss_opt.as_deref().unwrap_or("").trim();
            if iss_raw.is_empty() {
                return Err("CITO_OIDC_ISSUER is required in oidc mode".to_string());
            }
            oidc_issuer = Some(validate_https_issuer(iss_raw)?);
            let cid_raw = cid_opt.as_deref().unwrap_or("").trim();
            if cid_raw.is_empty() {
                return Err("CITO_OIDC_CLIENT_ID is required in oidc mode".to_string());
            }
            oidc_client_id = Some(cid_raw.to_string());
        }

        if let Some(dd) = raw_data_dir.or_else(|| env::var("CITO_HUB_DATA_DIR").ok()) {
            let home = env::var("HOME").ok();
            let _ = expand_home_dir(&dd, "CITO_HUB_DATA_DIR", home.as_deref())?;
        }

        Ok((
            port_num as u16,
            bind,
            auth_mode,
            tok_opt.map(|s| s.trim().to_string()),
            oidc_issuer,
            oidc_client_id,
        ))
    };

    let (port, bind, auth_mode, token, oidc_issuer, oidc_client_id) = match parse() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("cito hub: {err}");
            std::process::exit(1);
        }
    };

    unsafe {
        libc::signal(libc::SIGINT, handle_hub_sig as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle_hub_sig as *const () as libc::sighandler_t);
    }

    let listener = match TcpListener::bind(format!("{bind}:{port}")) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cito hub: {e}");
            std::process::exit(1);
        }
    };
    let actual_port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let display_host = if bind == "0.0.0.0" || bind == "::" {
        "<host>"
    } else {
        bind.as_str()
    };
    let join_url = format!("http://{display_host}:{actual_port}");
    let join_cmd = match auth_mode.as_str() {
        "oidc" => format!(
            "cito setup --hub team --url {join_url} --auth oidc --issuer {} --client-id {}",
            oidc_issuer.as_deref().unwrap_or(""),
            oidc_client_id.as_deref().unwrap_or("")
        ),
        "token" => format!("cito setup --hub tailnet --url {join_url} --auth token --token <token>"),
        _ => format!("cito setup --hub tailnet --url {join_url}"),
    };

    eprintln!("cito hub: ready on http://{bind}:{actual_port} (auth={auth_mode})");
    eprintln!("cito hub: join with: {join_cmd}");

    let state = Arc::new(Mutex::new(HubState::new()));
    for stream in listener.incoming().flatten() {
        let st = state.clone();
        let am = auth_mode.clone();
        let tk = token.clone();
        thread::spawn(move || {
            handle_hub_connection(stream, st, am, tk);
        });
    }
}
