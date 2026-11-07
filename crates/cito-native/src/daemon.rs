use crate::plugins::{
    ensure_daemon, expand_home_dir, http_post_mcp, now_ms, probe_daemon_health, query_sqlite_threads,
    random_base32_12, random_uuid_v4, LoadedPlugins, NativeConfig, PluginDescriptor, CITO_VERSION,
};
use crate::setup::{
    apply_plugin_setup, ensure_cli_shims_native, inspect_launch_at_login, inspect_plugin_setup,
    normalize_existing_config, remove_plugin_setup, resolve_package_root, set_launch_at_login,
    uninstall_setup_native,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

extern "C" fn handle_daemon_sig(_sig: libc::c_int) {
    unsafe {
        libc::_exit(0);
    }
}

#[derive(Clone, Debug)]
struct LocalListener {
    hub: String,
    listener_id: String,
    public_id: String,
    harness: String,
    session_id: String,
    endpoint_pid: Option<u32>,
    user: String,
    host_name: String,
    cwd: String,
    title: String,
    about: String,
    scope: String,
    created_iso: String,
}

#[derive(Clone, Debug)]
struct LocalSignal {
    hub: String,
    signal_id: String,
    seq: u64,
    from_user: String,
    from_host_name: String,
    from_harness: Option<String>,
    from_listener_id: Option<String>,
    from_about: Option<String>,
    message: String,
    in_reply_to: Option<String>,
    created_at: String,
    acknowledged: bool,
}

struct DaemonState {
    config: NativeConfig,
    plugins: LoadedPlugins,
    next_seq: u64,
    listeners: HashMap<String, LocalListener>,
    signals: Vec<LocalSignal>,
    ui_stdin: Option<Arc<Mutex<std::process::ChildStdin>>>,
}

impl DaemonState {
    fn build_settings_json(&self) -> Value {
        let mut apps = Vec::new();
        for p in self.plugins.agent_plugins() {
            let (installed, configured, paths) = inspect_plugin_setup(p);
            if paths.is_empty() && !p.is_builtin {
                continue;
            }
            apps.push(json!({
                "id": p.id,
                "display_name": p.display_name,
                "tier": p.tier,
                "installed": installed,
                "configured": configured,
                "status": if configured { "ready" } else { "not_configured" },
                "detail": if configured {
                    "Connected and ready for Cito notifications"
                } else if installed {
                    "Installed on this Mac · Click Set Up to connect"
                } else {
                    "Not detected yet · Click Set Up to pre-configure"
                },
                "paths": paths,
            }));
        }
        let raw_cfg = fs::read_to_string(&self.config.config_path).ok();
        let (def_hub, hubs_map, _) = normalize_existing_config(raw_cfg.as_deref());
        let mut hubs_list = Vec::new();
        for (name, h) in &hubs_map {
            let url = h.get("url").and_then(|v| v.as_str()).unwrap_or("local://");
            let auth = h.get("authMode").and_then(|v| v.as_str()).unwrap_or("none");
            let enabled = h.get("enabled").and_then(|v| v.as_bool()) != Some(false);
            hubs_list.push(json!({
                "name": name,
                "url": url,
                "auth_mode": auth,
                "enabled": enabled,
                "is_default": name == &def_hub,
                "status": if enabled { "connected" } else { "disconnected" },
                "user": self.config.user,
                "error": Value::Null,
            }));
        }
        json!({
            "apps": apps,
            "channels": [],
            "desktop_banner_enabled": true,
            "hubs": hubs_list,
            "host_hub": {
                "enabled": false,
                "running": false,
                "port": 4291,
                "endpoint_url": "http://127.0.0.1:4291",
                "token": "",
                "invite_command": "cito setup --hub lan --url http://127.0.0.1:4291 --default",
                "error": Value::Null,
            },
            "launch_at_login": inspect_launch_at_login(&self.config.home_dir),
        })
    }

    fn build_harnesses_json(&self) -> Vec<Value> {
        self.plugins
            .agent_plugins()
            .into_iter()
            .enumerate()
            .map(|(idx, p)| {
                json!({
                    "id": p.id,
                    "display_name": p.display_name,
                    "sf_symbol": p.sf_symbol,
                    "tint": p.tint,
                    "tier": p.tier,
                    "order": idx,
                })
            })
            .collect()
    }

    fn push_ui_state(&self) {
        let Some(ref ui_in) = self.ui_stdin else { return };
        let hubs = vec![json!({
            "name": self.config.default_hub,
            "status": "connected",
            "user": self.config.user,
            "auth_mode": "none",
            "error": Value::Null,
        })];
        let listeners: Vec<Value> = self
            .listeners
            .values()
            .map(|l| {
                json!({
                    "hub": l.hub,
                    "listener_id": l.listener_id,
                    "public_id": l.public_id,
                    "scoped_id": l.public_id,
                    "harness": l.harness,
                    "session_id": l.session_id,
                    "title": l.title,
                    "about": l.about,
                    "cwd": l.cwd,
                    "repo": Value::Null,
                    "branch": Value::Null,
                    "scope": l.scope,
                })
            })
            .collect();
        let frame = json!({
            "op": "state",
            "hubs": hubs,
            "listeners": listeners,
            "available": [],
            "my_remote_listeners": [],
            "directory_online_count": listeners.len(),
            "unread_count": self.signals.iter().filter(|s| !s.acknowledged).count(),
            "activity": [],
            "harnesses": self.build_harnesses_json(),
            "settings": self.build_settings_json(),
        });
        if let Ok(line) = serde_json::to_string(&frame) {
            if let Ok(mut guard) = ui_in.lock() {
                let _ = guard.write_all(format!("{line}\n").as_bytes());
                let _ = guard.flush();
            }
        }
    }
}

fn identify_socket_bridge(socket_path: &Path) -> Option<(String, String, String)> {
    let mut stream = UnixStream::connect(socket_path).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(600)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(600)));
    stream.write_all(b"{\"op\":\"identify\"}\n").ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let obj = serde_json::from_str::<Value>(&line).ok()?;
    let map = obj.as_object()?;
    if map.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let session_id = map.get("session_id").and_then(|v| v.as_str())?.to_string();
    let session_name = map
        .get("session_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cwd = map.get("cwd").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Some((session_id, session_name, cwd))
}

fn deliver_socket_bridge(socket_path: &Path, session_id: &str, batch_id: &str, card: &str) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket_path) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(1500)));
    let req = json!({
        "op": "deliver",
        "session_id": session_id,
        "batch_id": batch_id,
        "wake": true,
        "content": card,
    });
    let Ok(s) = serde_json::to_string(&req) else { return false };
    if stream.write_all(format!("{s}\n").as_bytes()).is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return false;
    }
    serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|v| v.get("ok").and_then(|b| b.as_bool()))
        == Some(true)
}

fn resolve_caller_from_meta(
    meta: &Map<String, Value>,
    state: &DaemonState,
) -> (String, String, Option<u32>, String, String) {
    let cwd = meta
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("/tmp"))
                .to_string_lossy()
                .to_string()
        });

    for p in state.plugins.agent_plugins() {
        if let Some(mk) = p.native_str("metaKey") {
            if let Some(sid) = meta.get(mk).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                return (p.id.clone(), sid.to_string(), None, cwd.clone(), p.display_name.clone());
            }
        }
        if let Some(amk) = p.native_str("altMetaKey") {
            if let Some(sid) = meta.get(amk).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                return (p.id.clone(), sid.to_string(), None, cwd.clone(), p.display_name.clone());
            }
        }
    }

    if let (Some(eh), Some(es)) = (
        meta.get("expected_harness").and_then(|v| v.as_str()),
        meta.get("expected_session_id").and_then(|v| v.as_str()),
    ) {
        let title = meta
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or(eh)
            .to_string();
        return (eh.to_string(), es.to_string(), None, cwd, title);
    }

    let pid = meta
        .get("endpoint_pid")
        .or_else(|| meta.get("caller_pid"))
        .or_else(|| meta.get("_stdio").and_then(|s| s.get("endpoint_pid")))
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);

    if let Some(p) = pid {
        let sockets_dir = state.config.state_dir.join("sockets");
        for plugin in state.plugins.agent_plugins() {
            let sock = sockets_dir.join(format!("{}-pid-{p}.sock", plugin.id));
            if sock.exists() {
                if let Some((sid, sname, scwd)) = identify_socket_bridge(&sock) {
                    let t = if sname.is_empty() {
                        plugin.display_name.clone()
                    } else {
                        sname
                    };
                    let c = if scwd.is_empty() { cwd.clone() } else { scwd };
                    return (plugin.id.clone(), sid, Some(p), c, t);
                }
            }
        }
        let default_harness = state
            .plugins
            .agent_plugins()
            .first()
            .map(|pl| pl.id.clone())
            .unwrap_or_else(|| "agent".to_string());
        return (
            default_harness.clone(),
            format!("session-{p}"),
            Some(p),
            cwd,
            default_harness,
        );
    }

    ("human".to_string(), "default".to_string(), None, cwd, format!("@{}", state.config.user))
}

fn has_bound_caller(meta: &Map<String, Value>, state: &DaemonState) -> bool {
    for p in state.plugins.agent_plugins() {
        if let Some(mk) = p.native_str("metaKey") {
            if meta.get(mk).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).is_some() {
                return true;
            }
        }
        if let Some(amk) = p.native_str("altMetaKey") {
            if meta.get(amk).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).is_some() {
                return true;
            }
        }
    }
    if meta.get("expected_harness").and_then(|v| v.as_str()).is_some()
        && meta.get("expected_session_id").and_then(|v| v.as_str()).is_some()
    {
        return true;
    }
    let pid = meta
        .get("endpoint_pid")
        .or_else(|| meta.get("caller_pid"))
        .or_else(|| meta.get("_stdio").and_then(|s| s.get("endpoint_pid")))
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    if let Some(p) = pid {
        let sockets_dir = state.config.state_dir.join("sockets");
        for plugin in state.plugins.agent_plugins() {
            let sock = sockets_dir.join(format!("{}-pid-{p}.sock", plugin.id));
            if sock.exists() && identify_socket_bridge(&sock).is_some() {
                return true;
            }
        }
    }
    !state.listeners.is_empty()
}

fn handle_tool_call(
    state: &mut DaemonState,
    tool_name: &str,
    args: &Map<String, Value>,
    meta: &Map<String, Value>,
) -> Result<Value, (&'static str, String)> {
    let hub = args
        .get("hub")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&state.config.default_hub)
        .to_string();

    match tool_name {
        "cito_listen" => {
            let about = args
                .get("about")
                .and_then(|v| v.as_str())
                .unwrap_or("Session")
                .to_string();
            let scope = args
                .get("scope")
                .and_then(|v| v.as_str())
                .unwrap_or("workspace")
                .to_string();
            let tag = args
                .get("tag")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("session");
            let (harness, session_id, endpoint_pid, cwd, title) = resolve_caller_from_meta(meta, state);
            let key = format!("{hub}:{harness}:{session_id}");
            let (listener_id, public_id) = if let Some(ex) = state.listeners.get(&key) {
                (ex.listener_id.clone(), ex.public_id.clone())
            } else {
                let suffix = random_base32_12();
                let clean_tag: String = tag
                    .to_ascii_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect();
                let slug = clean_tag.trim_matches('-');
                let slug = if slug.is_empty() { "session" } else { slug };
                (format!("c_{suffix}"), format!("{slug}-{suffix}"))
            };
            let row = LocalListener {
                hub: hub.clone(),
                listener_id: listener_id.clone(),
                public_id: public_id.clone(),
                harness,
                session_id,
                endpoint_pid,
                user: state.config.user.clone(),
                host_name: "localhost".to_string(),
                cwd,
                title,
                about: about.clone(),
                scope: scope.clone(),
                created_iso: format!("{}Z", now_ms()),
            };
            state.listeners.insert(key, row);
            state.push_ui_state();
            Ok(json!({
                "ok": true,
                "hub": hub,
                "listener_id": public_id,
                "canonical_listener_id": listener_id,
                "scope": scope,
                "about": about,
            }))
        }
        "cito_unlisten" => {
            let (harness, session_id, _, _, _) = resolve_caller_from_meta(meta, state);
            let key = format!("{hub}:{harness}:{session_id}");
            let removed = if state.listeners.remove(&key).is_some() { 1 } else { 0 };
            state.push_ui_state();
            Ok(json!({
                "ok": true,
                "status": if removed > 0 { "unlistened" } else { "not_listening" },
                "removed": removed,
            }))
        }
        "cito_find" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let list: Vec<Value> = state
                .listeners
                .values()
                .filter(|l| {
                    query.is_empty()
                        || l.about.to_ascii_lowercase().contains(&query)
                        || l.title.to_ascii_lowercase().contains(&query)
                        || l.public_id.to_ascii_lowercase().contains(&query)
                })
                .map(|l| {
                    json!({
                        "hub": l.hub,
                        "listener_id": l.public_id,
                        "user": l.user,
                        "host_id": "local",
                        "host_name": l.host_name,
                        "harness": l.harness,
                        "about": l.about,
                        "repo": Value::Null,
                        "branch": Value::Null,
                        "title": l.title,
                        "scope": l.scope,
                        "last_seen_at": l.created_iso,
                    })
                })
                .collect();
            Ok(json!({
                "ok": true,
                "listeners": list,
            }))
        }
        "cito_send" => {
            let to_raw = args.get("to").and_then(|v| v.as_str()).unwrap_or("");
            let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let reply_to = args.get("in_reply_to").and_then(|v| v.as_str()).map(String::from);
            let (from_harness, _, _, _, from_title) = resolve_caller_from_meta(meta, state);
            let target = state
                .listeners
                .values()
                .find(|l| {
                    l.public_id == to_raw
                        || l.listener_id == to_raw
                        || (to_raw.starts_with('@') && l.harness == "human")
                })
                .cloned();
            let seq = state.next_seq;
            state.next_seq += 1;
            let sig_id = format!("sig_{}", random_uuid_v4().replace('-', ""));
            let to_id = target
                .as_ref()
                .map(|t| t.public_id.clone())
                .unwrap_or_else(|| to_raw.to_string());
            state.signals.push(LocalSignal {
                hub: hub.clone(),
                signal_id: sig_id.clone(),
                seq,
                from_user: state.config.user.clone(),
                from_host_name: "localhost".to_string(),
                from_harness: Some(from_harness),
                from_listener_id: None,
                from_about: Some(from_title),
                message: message.to_string(),
                in_reply_to: reply_to,
                created_at: format!("{}Z", now_ms()),
                acknowledged: false,
            });

            if let Some(ref t) = target {
                if let Some(pid) = t.endpoint_pid {
                    let sock = state
                        .config
                        .state_dir
                        .join("sockets")
                        .join(format!("{}-pid-{pid}.sock", t.harness));
                    let card = format!("🔔 Cito · {}@localhost\n\n{}", state.config.user, message);
                    let _ = deliver_socket_bridge(&sock, &t.session_id, &sig_id, &card);
                }
            }
            state.push_ui_state();
            Ok(json!({
                "ok": true,
                "signal_id": sig_id,
                "seq": seq,
                "to": to_id,
                "status": "wake_accepted",
                "held_reason": Value::Null,
            }))
        }
        "cito_inbox" => {
            if !has_bound_caller(meta, state) {
                return Err(("BINDING_REQUIRED", "Caller session binding is required".to_string()));
            }
            let ack_all = args.get("ack_all").and_then(|v| v.as_bool()).unwrap_or(true);
            let ack_ids: Vec<String> = args
                .get("ack_ids")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let mut out_signals = Vec::new();
            let mut acked_count = 0;
            for sig in &mut state.signals {
                if !sig.acknowledged {
                    out_signals.push(json!({
                        "signal_id": sig.signal_id,
                        "hub": sig.hub,
                        "seq": sig.seq,
                        "from": {
                            "user": sig.from_user,
                            "host_name": sig.from_host_name,
                            "harness": sig.from_harness,
                            "listener_id": sig.from_listener_id,
                            "about": sig.from_about,
                        },
                        "message": sig.message,
                        "held_reason": Value::Null,
                        "in_reply_to": sig.in_reply_to,
                        "created_at": sig.created_at,
                    }));
                    if ack_all || ack_ids.contains(&sig.signal_id) {
                        sig.acknowledged = true;
                        acked_count += 1;
                    }
                }
            }
            state.push_ui_state();
            Ok(json!({
                "ok": true,
                "replay_gap_detected": false,
                "replay_gap_range": Value::Null,
                "signals": out_signals,
                "acknowledged_count": acked_count,
            }))
        }
        _ => Err(("INVALID_INPUT", format!("Unknown tool: {tool_name}"))),
    }
}

fn mcp_tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "cito_listen",
                "description": "Enroll this session to receive notifications.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "about": { "type": "string" },
                        "tag": { "type": "string" },
                        "hub": { "type": "string" },
                        "scope": { "type": "string", "enum": ["self", "workspace"] }
                    },
                    "required": ["about"]
                }
            },
            {
                "name": "cito_unlisten",
                "description": "Revoke this session on one hub or every active hub.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "hub": { "type": "string" } }
                }
            },
            {
                "name": "cito_find",
                "description": "Find active opted-in sessions across configured hubs.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "hub": { "type": "string" },
                        "user": { "type": "string" },
                        "host": { "type": "string" },
                        "harness": { "type": "string" },
                        "repo": { "type": "string" },
                        "limit": { "type": "integer" }
                    }
                }
            },
            {
                "name": "cito_send",
                "description": "Notify an exact listener, optionally prefixed with its hub.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "to": { "type": "string" },
                        "hub": { "type": "string" },
                        "message": { "type": "string" },
                        "in_reply_to": { "type": "string" },
                        "idempotency_key": { "type": "string" }
                    },
                    "required": ["to", "message"]
                }
            },
            {
                "name": "cito_inbox",
                "description": "Read and drain this session's local inbox by default.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "hub": { "type": "string" },
                        "ack_ids": { "type": "array", "items": { "type": "string" } },
                        "ack_all": { "type": "boolean" }
                    }
                }
            }
        ]
    })
}

fn handle_daemon_http(mut stream: TcpStream, state: Arc<Mutex<DaemonState>>) {
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
    let path = parts.next().unwrap_or("/").split('?').next().unwrap_or("/");

    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse::<usize>().unwrap_or(0);
            }
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body);
    }

    if path == "/health" && method == "GET" {
        let state_dir_str = {
            let guard = state.lock().unwrap();
            guard.config.state_dir.to_string_lossy().to_string()
        };
        let payload = serde_json::to_string(&json!({
            "ok": true,
            "pid": std::process::id(),
            "stateDir": state_dir_str,
            "version": CITO_VERSION,
        }))
        .unwrap();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        return;
    }

    if path == "/mcp" && method == "POST" {
        let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
            let resp = "HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes());
            return;
        };
        let Some(obj) = parsed.as_object() else { return };
        let req_id = obj.get("id").cloned();
        let rpc_method = obj.get("method").and_then(|v| v.as_str()).unwrap_or("");
        if req_id.is_none() || rpc_method.starts_with("notifications/") {
            let resp = "HTTP/1.1 202 Accepted\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
            return;
        }
        let id_val = req_id.unwrap_or(Value::Null);
        let rpc_resp = match rpc_method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id_val,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "cito", "version": CITO_VERSION }
                }
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id_val,
                "result": mcp_tools_list()
            }),
            "tools/call" => {
                let params = obj.get("params").and_then(|v| v.as_object());
                let name = params
                    .and_then(|p| p.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = params
                    .and_then(|p| p.get("arguments"))
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                let meta = params
                    .and_then(|p| p.get("_meta"))
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                let res = {
                    let mut guard = state.lock().unwrap();
                    handle_tool_call(&mut guard, name, &args, &meta)
                };
                match res {
                    Ok(payload) => json!({
                        "jsonrpc": "2.0",
                        "id": id_val,
                        "result": {
                            "content": [{ "type": "text", "text": serde_json::to_string(&payload).unwrap() }]
                        }
                    }),
                    Err((err_code, err_msg)) => json!({
                        "jsonrpc": "2.0",
                        "id": id_val,
                        "result": {
                            "isError": true,
                            "content": [{ "type": "text", "text": serde_json::to_string(&json!({ "ok": false, "error": { "code": err_code, "message": err_msg } })).unwrap() }]
                        }
                    }),
                }
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id_val,
                "result": {}
            }),
        };
        let payload = serde_json::to_string(&rpc_resp).unwrap();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
    }
}

fn spawn_ui_bridge(state: Arc<Mutex<DaemonState>>) -> Option<Child> {
    let pkg_root = resolve_package_root();
    let state_dir = { state.lock().unwrap().config.state_dir.clone() };
    let candidates = [
        pkg_root.join("Helpers/cito-ui"),
        pkg_root.join("bin/cito-ui-darwin-arm64"),
        state_dir.join("bin/cito-ui"),
    ];
    let ui_bin = candidates.into_iter().find(|p| p.is_file())?;
    let mut child = Command::new(ui_bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    {
        let mut guard = state.lock().unwrap();
        guard.ui_stdin = Some(Arc::new(Mutex::new(stdin)));
        guard.push_ui_state();
    }
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().flatten() {
            let Ok(Value::Object(action)) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let op = action.get("op").and_then(|v| v.as_str()).unwrap_or("");
            if op == "quit" {
                unsafe { libc::_exit(0) };
            }
            let guard = state.lock().unwrap();
            match op {
                "setup_apply" => {
                    if let Some(h) = action.get("harness").and_then(|v| v.as_str()) {
                        if let Some(p) = guard.plugins.find_by_id(h).cloned() {
                            let _ = apply_plugin_setup(&p, true, false);
                        }
                    }
                    guard.push_ui_state();
                }
                "setup_remove_app" => {
                    if let Some(h) = action.get("harness").and_then(|v| v.as_str()) {
                        if let Some(p) = guard.plugins.find_by_id(h).cloned() {
                            let _ = remove_plugin_setup(&p);
                        }
                    }
                    guard.push_ui_state();
                }
                "setup_uninstall_all" => {
                    let purge = action.get("purge_data").and_then(|v| v.as_bool()).unwrap_or(false);
                    let sd = guard.config.state_dir.clone();
                    let cp = guard.config.config_path.clone();
                    let hd = guard.config.home_dir.clone();
                    let pl = guard.plugins.clone();
                    let _ = uninstall_setup_native(&sd, &cp, &hd, &pl, purge);
                    if !purge {
                        guard.push_ui_state();
                    }
                }
                "set_launch_at_login" => {
                    let en = action.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                    let sd = guard.config.state_dir.clone();
                    let hd = guard.config.home_dir.clone();
                    let pl = guard.plugins.clone();
                    let _ = set_launch_at_login(en, &sd, &hd, &pl);
                    guard.push_ui_state();
                }
                "open_settings" | "refresh_available" => {
                    guard.push_ui_state();
                }
                _ => {}
            }
        }
    });
    Some(child)
}

pub fn run_daemon(config: &NativeConfig, plugins: &LoadedPlugins) {
    if let Some(existing) = probe_daemon_health(config) {
        if existing.pid > 0 {
            return;
        }
    }
    let _ = fs::create_dir_all(&config.state_dir);
    let _ = fs::set_permissions(&config.state_dir, fs::Permissions::from_mode(0o700));
    let sqlite_path = config.state_dir.join("state.sqlite");
    if !sqlite_path.exists() {
        let _ = fs::write(&sqlite_path, b"");
    }
    let _ = fs::set_permissions(&sqlite_path, fs::Permissions::from_mode(0o600));
    let _ = ensure_cli_shims_native(&config.state_dir, plugins);

    unsafe {
        libc::signal(libc::SIGINT, handle_daemon_sig as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle_daemon_sig as *const () as libc::sighandler_t);
    }

    let listener = match TcpListener::bind(("127.0.0.1", config.daemon_port)) {
        Ok(l) => l,
        Err(e) => {
            if probe_daemon_health(config).is_some() {
                return;
            }
            eprintln!("cito daemon: {e}");
            std::process::exit(1);
        }
    };

    let state = Arc::new(Mutex::new(DaemonState {
        config: config.clone(),
        plugins: plugins.clone(),
        next_seq: 1,
        listeners: HashMap::new(),
        signals: Vec::new(),
        ui_stdin: None,
    }));

    let _ui_child = spawn_ui_bridge(state.clone());

    for stream in listener.incoming().flatten() {
        let st = state.clone();
        thread::spawn(move || {
            handle_daemon_http(stream, st);
        });
    }
}

fn resolve_custom_plugin_cli(plugin: &PluginDescriptor, cmd: &str, args: &[String]) {
    if let Some(ref drv) = plugin.driver {
        let drv_path = plugin.plugin_dir.join(drv);
        let status = Command::new(drv_path).arg(cmd).args(args).status();
        std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
    }
    if plugin.has_script_entry {
        let pkg_root = resolve_package_root();
        let mcp_js = pkg_root.join("bin/cito-mcp");
        let node_bin = env::var("CITO_NODE_BIN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "node".to_string());
        let status = Command::new(node_bin)
            .env("CITO_DISABLE_NATIVE", "1")
            .arg(mcp_js)
            .arg(cmd)
            .args(args)
            .status();
        std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
    }
}

pub fn run_cli_command(cmd: &str, args: &[String], config: &NativeConfig, plugins: &LoadedPlugins) {
    let run = || -> Result<(), String> {
        let read_val = |arg: &str, idx: usize, flag: &str| -> Result<(String, usize), String> {
            if let Some(rest) = arg.strip_prefix(&format!("{flag}=")) {
                let v = rest.trim();
                if v.is_empty() {
                    return Err(format!("Missing value for {flag}"));
                }
                return Ok((v.to_string(), idx));
            }
            let next = args
                .get(idx + 1)
                .ok_or_else(|| format!("Missing value for {flag}"))?;
            if next.starts_with('-') {
                return Err(format!("Missing value for {flag}"));
            }
            Ok((next.clone(), idx + 1))
        };

        if cmd == "send" {
            let mut to: Option<String> = None;
            let mut hub: Option<String> = None;
            let mut reply_to: Option<String> = None;
            let mut msg_parts: Vec<String> = Vec::new();
            let mut i = 0;
            while i < args.len() {
                let a = &args[i];
                if a == "--to" || a == "-t" || a.starts_with("--to=") {
                    let (v, ni) = read_val(a, i, if a == "-t" { "-t" } else { "--to" })?;
                    to = Some(v);
                    i = ni;
                } else if a == "--hub" || a.starts_with("--hub=") {
                    let (v, ni) = read_val(a, i, "--hub")?;
                    hub = Some(v);
                    i = ni;
                } else if a == "--reply-to" || a.starts_with("--reply-to=") {
                    let (v, ni) = read_val(a, i, "--reply-to")?;
                    reply_to = Some(v);
                    i = ni;
                } else if a == "-m" || a == "--message" || a.starts_with("--message=") {
                    let (v, ni) = read_val(a, i, if a == "-m" { "-m" } else { "--message" })?;
                    msg_parts.push(v);
                    i = ni;
                } else {
                    msg_parts.push(a.clone());
                }
                i += 1;
            }
            let message = msg_parts.join(" ").trim().to_string();
            let Some(to_val) = to.filter(|s| !s.is_empty()) else {
                return Err("Usage: cito send --to <target> [-m <message>] [--hub <hub>]".to_string());
            };
            if message.is_empty() {
                return Err("Usage: cito send --to <target> [-m <message>] [--hub <hub>]".to_string());
            }
            let mut tool_args = Map::new();
            tool_args.insert("to".to_string(), json!(to_val));
            tool_args.insert("message".to_string(), json!(message));
            if let Some(h) = hub {
                tool_args.insert("hub".to_string(), json!(h));
            }
            if let Some(r) = reply_to {
                tool_args.insert("in_reply_to".to_string(), json!(r));
            }
            let payload = call_daemon_tool(
                config,
                "cito_send",
                Value::Object(tool_args),
                json!({ "http_transport": true }),
            )?;
            println!(
                "Sent {} ({}) to {}",
                payload.get("signal_id").and_then(|v| v.as_str()).unwrap_or(""),
                payload.get("status").and_then(|v| v.as_str()).unwrap_or(""),
                payload.get("to").and_then(|v| v.as_str()).unwrap_or("")
            );
            return Ok(());
        }

        if cmd == "find" {
            let mut hub: Option<String> = None;
            let mut query_parts: Vec<String> = Vec::new();
            let mut i = 0;
            while i < args.len() {
                let a = &args[i];
                if a == "--hub" || a.starts_with("--hub=") {
                    let (v, ni) = read_val(a, i, "--hub")?;
                    hub = Some(v);
                    i = ni;
                } else if a == "--query" || a == "-q" || a.starts_with("--query=") {
                    let (v, ni) = read_val(a, i, if a == "-q" { "-q" } else { "--query" })?;
                    query_parts.push(v);
                    i = ni;
                } else if !a.starts_with('-') {
                    query_parts.push(a.clone());
                }
                i += 1;
            }
            let mut tool_args = Map::new();
            let q = query_parts.join(" ").trim().to_string();
            if !q.is_empty() {
                tool_args.insert("query".to_string(), json!(q));
            }
            if let Some(h) = hub {
                tool_args.insert("hub".to_string(), json!(h));
            }
            let payload = call_daemon_tool(
                config,
                "cito_find",
                Value::Object(tool_args),
                json!({ "http_transport": true }),
            )?;
            println!("{}", serde_json::to_string_pretty(&payload).unwrap());
            return Ok(());
        }

        if cmd == "inbox" {
            let mut hub: Option<String> = None;
            let mut ack_all: Option<bool> = None;
            for a in args {
                if a == "--peek" || a == "--no-ack" {
                    ack_all = Some(false);
                } else if a == "--ack-all" {
                    ack_all = Some(true);
                } else if let Some(h) = a.strip_prefix("--hub=") {
                    hub = Some(h.to_string());
                }
            }
            let mut tool_args = Map::new();
            if let Some(h) = hub {
                tool_args.insert("hub".to_string(), json!(h));
            }
            if let Some(a) = ack_all {
                tool_args.insert("ack_all".to_string(), json!(a));
            }
            let ppid = unsafe { libc::getppid() as u32 };
            let payload = call_daemon_tool(
                config,
                "cito_inbox",
                Value::Object(tool_args),
                json!({ "caller_pid": ppid }),
            )?;
            println!("{}", serde_json::to_string_pretty(&payload).unwrap());
            return Ok(());
        }

        // listen or unlisten
        let mut explicit_scope: Option<String> = None;
        let mut team = false;
        let mut hub: Option<String> = None;
        let mut tag: Option<String> = None;
        let mut selected_harness: Option<String> = None;
        let mut selected_session: Option<String> = None;
        let mut positionals: Vec<String> = Vec::new();
        let cwd = env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .to_string_lossy()
            .to_string();

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            let mut matched_plugin_flag = false;
            for p in plugins.agent_plugins() {
                if !p.cli_flag {
                    continue;
                }
                let flag = format!("--{}", p.id);
                if arg == &flag || arg.starts_with(&format!("{flag}=")) {
                    matched_plugin_flag = true;
                    if let Some(ref sh) = selected_harness {
                        if sh != &p.id {
                            return Err("Conflicting harness flags".to_string());
                        }
                    }
                    selected_harness = Some(p.id.clone());
                    if let Some(rest) = arg.strip_prefix(&format!("{flag}=")) {
                        let v = rest.trim();
                        if v.is_empty() {
                            return Err(format!("Missing value for {flag}"));
                        }
                        if let Some(ref ss) = selected_session {
                            if ss != v {
                                return Err("Conflicting session flags".to_string());
                            }
                        }
                        selected_session = Some(v.to_string());
                    } else if let Some(next) = args.get(i + 1) {
                        if !next.starts_with('-') && (cmd == "unlisten" || crate::is_valid_transcript_uuid(next)) {
                            if let Some(ref ss) = selected_session {
                                if ss != next {
                                    return Err("Conflicting session flags".to_string());
                                }
                            }
                            selected_session = Some(next.clone());
                            i += 1;
                        }
                    }
                    break;
                }
            }
            if matched_plugin_flag {
                i += 1;
                continue;
            }

            if arg == "--harness" || arg.starts_with("--harness=") {
                let (val, ni) = read_val(arg, i, "--harness")?;
                if let Some(ref sh) = selected_harness {
                    if sh != &val {
                        return Err("Conflicting harness flags".to_string());
                    }
                }
                selected_harness = Some(val);
                i = ni;
            } else if arg == "--session" || arg.starts_with("--session=") {
                let (val, ni) = read_val(arg, i, "--session")?;
                if let Some(ref ss) = selected_session {
                    if ss != &val {
                        return Err("Conflicting session flags".to_string());
                    }
                }
                selected_session = Some(val);
                i = ni;
            } else if arg == "--hub" || arg.starts_with("--hub=") {
                let (val, ni) = read_val(arg, i, "--hub")?;
                hub = Some(val);
                i = ni;
            } else if cmd == "listen" && arg == "--team" {
                team = true;
            } else if cmd == "listen" && (arg == "--scope" || arg.starts_with("--scope=")) {
                let (val, ni) = read_val(arg, i, "--scope")?;
                if val != "self" && val != "workspace" {
                    return Err(format!("Invalid --scope: {val}"));
                }
                if let Some(ref es) = explicit_scope {
                    if es != &val {
                        return Err("Conflicting --scope values".to_string());
                    }
                }
                explicit_scope = Some(val);
                i = ni;
            } else if cmd == "listen" && (arg == "--tag" || arg.starts_with("--tag=")) {
                let (val, ni) = read_val(arg, i, "--tag")?;
                tag = Some(val);
                i = ni;
            } else if arg.starts_with('-') {
                return Err(format!("Unknown flag: {arg}"));
            } else {
                positionals.push(arg.clone());
            }
            i += 1;
        }

        if cmd == "unlisten" && !positionals.is_empty() {
            return Err(format!("Unexpected argument: {}", positionals[0]));
        }
        if team && explicit_scope.as_deref() == Some("self") {
            return Err("Conflicting --scope self and --team".to_string());
        }
        if selected_session.is_some() && selected_harness.is_none() {
            return Err("--session requires --harness".to_string());
        }
        if let Some(ref sh) = selected_harness {
            if plugins.rejected_ids.contains(sh) || plugins.find_by_id(sh).is_none() {
                return Err(format!("Unknown harness: {sh}"));
            }
            if let Some(plugin) = plugins.find_by_id(sh) {
                if !plugin.is_builtin && (plugin.driver.is_some() || plugin.has_script_entry) {
                    resolve_custom_plugin_cli(plugin, cmd, args);
                }
            }
        }

        let meta = resolve_cli_meta(cmd, selected_harness.as_deref(), selected_session.as_deref(), &cwd, config, plugins)?;

        if cmd == "listen" {
            let about = if positionals.is_empty() {
                Path::new(&cwd)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("CLI session")
                    .to_string()
            } else {
                positionals.join(" ")
            };
            let scope = if team {
                "workspace".to_string()
            } else {
                explicit_scope.unwrap_or_else(|| "self".to_string())
            };
            let mut tool_args = Map::new();
            tool_args.insert("about".to_string(), json!(about));
            tool_args.insert("scope".to_string(), json!(scope));
            if let Some(h) = hub {
                tool_args.insert("hub".to_string(), json!(h));
            }
            if let Some(t) = tag {
                tool_args.insert("tag".to_string(), json!(t));
            }
            let payload = call_daemon_tool(config, "cito_listen", Value::Object(tool_args), meta)?;
            println!(
                "Listening as {} ({}): {}",
                payload.get("listener_id").and_then(|v| v.as_str()).unwrap_or(""),
                payload.get("scope").and_then(|v| v.as_str()).unwrap_or(""),
                payload.get("about").and_then(|v| v.as_str()).unwrap_or("")
            );
        } else {
            let mut tool_args = Map::new();
            if let Some(h) = hub {
                tool_args.insert("hub".to_string(), json!(h));
            }
            let payload = call_daemon_tool(config, "cito_unlisten", Value::Object(tool_args), meta)?;
            let removed = payload
                .get("removed")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| {
                    if payload.get("status").and_then(|v| v.as_str()) == Some("unlistened") {
                        1
                    } else {
                        0
                    }
                });
            println!("Unlistened ({removed} removed)");
        }
        Ok(())
    };

    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn resolve_cli_meta(
    cmd: &str,
    harness: Option<&str>,
    explicit_id: Option<&str>,
    cwd: &str,
    config: &NativeConfig,
    plugins: &LoadedPlugins,
) -> Result<Value, String> {
    let ppid = unsafe { libc::getppid() as u32 };
    let Some(h) = harness else {
        return Ok(json!({ "caller_pid": ppid }));
    };
    let Some(plugin) = plugins.find_by_id(h) else {
        return Err(format!("Unknown harness: {h}"));
    };
    match plugin.protocol.as_str() {
        "sqlite-cli" => {
            let meta_key = plugin.native_str("metaKey").unwrap_or("threadId");
            if let Some(id) = explicit_id {
                return Ok(json!({ meta_key: id, "cwd": cwd }));
            }
            let home_env = env::var("HOME").ok();
            let env_key = plugin.native_str("homeEnv").unwrap_or("HOME");
            let def_home = plugin.native_str("defaultHome").unwrap_or("~");
            let raw_home = env::var(env_key).unwrap_or_else(|_| def_home.to_string());
            let home_dir = expand_home_dir(&raw_home, env_key, home_env.as_deref())?;
            let db_rel = plugin.native_str("sqliteRelPath").unwrap_or("state.sqlite");
            let rows = query_sqlite_threads(&home_dir.join(db_rel));
            let allow_archived = cmd == "unlisten";
            let matched = rows
                .into_iter()
                .find(|r| r.cwd == cwd && (allow_archived || !r.archived));
            let Some(row) = matched else {
                return Err(format!("{} thread metadata is required", plugin.display_name));
            };
            Ok(json!({ meta_key: row.id, "cwd": cwd }))
        }
        "transcript-hook" => {
            let meta_key = plugin.native_str("metaKey").unwrap_or("sessionId");
            let sess_env = plugin.native_str("sessionEnv").unwrap_or("SESSION_ID");
            let sid = explicit_id
                .map(String::from)
                .or_else(|| env::var(sess_env).ok().filter(|s| !s.trim().is_empty()))
                .or_else(|| crate::resolve_transcript_session_id(cwd, &config.transcript_home));
            let Some(sid) = sid else {
                return Err(format!("{} session metadata is required", plugin.display_name));
            };
            Ok(json!({ meta_key: sid, "cwd": cwd }))
        }
        _ => {
            let mut m = Map::new();
            m.insert("caller_pid".to_string(), json!(ppid));
            m.insert("expected_harness".to_string(), json!(h));
            if let Some(id) = explicit_id {
                m.insert("expected_session_id".to_string(), json!(id));
            }
            m.insert("cwd".to_string(), json!(cwd));
            Ok(Value::Object(m))
        }
    }
}

fn call_daemon_tool(
    config: &NativeConfig,
    name: &str,
    arguments: Value,
    meta: Value,
) -> Result<Map<String, Value>, String> {
    ensure_daemon(config)?;
    let body = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments,
            "_meta": meta,
        }
    }))
    .map_err(|e| e.to_string())?;
    let resp = http_post_mcp(config.daemon_port, &body, Duration::from_secs(10))?;
    let rpc = crate::extract_jsonrpc_from_http_body(&resp.body)
        .ok_or_else(|| format!("HTTP {}", resp.status))?;
    if let Some(err) = rpc.get("error") {
        let msg = err
            .get("message")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| err.to_string());
        return Err(msg);
    }
    let result = rpc.get("result").and_then(|v| v.as_object());
    let is_error = result
        .and_then(|r| r.get("isError"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let text = result
        .and_then(|r| r.get("content"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let parsed: Map<String, Value> = serde_json::from_str(text).unwrap_or_default();
    if is_error || parsed.get("ok").and_then(|v| v.as_bool()) == Some(false) || resp.status >= 400 {
        let msg = parsed
            .get("error")
            .and_then(|e| e.get("message").and_then(|v| v.as_str()).or_else(|| e.as_str()))
            .unwrap_or(text)
            .to_string();
        return Err(msg);
    }
    Ok(parsed)
}
