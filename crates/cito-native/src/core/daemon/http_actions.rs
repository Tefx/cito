use super::{
    channels::{channel_action_kind, dispatch_channel_action},
    host_hub::{
        advertised_hub_host, host_hub_auth_mode, host_hub_server_config, hub_name_from_url,
        lan_advertised_host, parse_hub_invite, replace_host_hub_server, tailscale_advertised_host,
    },
    mcp::{tools_list, validate_tool_input, wait_send_receipt},
    request_shutdown, shutdown_requested,
    ui_ipc::{bounded_frame, REQUEST_LIMIT},
    wake_daemon, DaemonService, SHUTDOWN_REQUESTED,
};
use crate::core::{
    config::repair_default_hub,
    identity::{listener_id, local_host_name, normalize_slug, string},
};
use crate::plugins::{now_ms, CITO_VERSION};
use crate::setup::{
    apply_plugin_setup, ensure_single_plugin_staged, remove_plugin_setup, set_launch_at_login,
};
use serde_json::{json, Map, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::MetadataExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct InteractiveLogin {
    pub(super) cancelled: AtomicBool,
    serial: Mutex<()>,
}

fn login_cache_snapshot(path: &std::path::Path) -> Option<(Value, Value)> {
    let file = fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    let cache = serde_json::from_reader(file).ok()?;
    // Atomic cache publication replaces the inode even when an issuer returns
    // the same bearer bytes. Bind queued login intent to that publication.
    let generation = json!([
        metadata.dev(),
        metadata.ino(),
        metadata.mtime(),
        metadata.mtime_nsec()
    ]);
    Some((cache, generation))
}

fn respond_bytes(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let header=format!("HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",body.len());
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}

pub(super) fn respond(stream: &mut TcpStream, status: u16, value: &Value) {
    if let Ok(body) = serde_json::to_vec(value) {
        respond_bytes(stream, status, &body);
    }
}

fn http_client_pid(stream: &TcpStream) -> Option<u32> {
    let remote = stream.peer_addr().ok()?.port();
    let local = stream.local_addr().ok()?.port();
    let mut command = Command::new("/usr/sbin/lsof");
    command
        .args([
            "-nP",
            &format!("-iTCP:{remote}"),
            "-sTCP:ESTABLISHED",
            "-Fpn",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let output =
        crate::support::run_command_with_timeout(command, Duration::from_millis(250)).ok()??;
    let expected = format!("n127.0.0.1:{remote}->127.0.0.1:{local}");
    let mut pid = None;
    let mut matches = std::collections::HashSet::new();
    for line in std::str::from_utf8(&output.stdout).ok()?.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse::<u32>().ok();
        }
        if line == expected {
            if let Some(pid) = pid {
                matches.insert(pid);
            }
        }
    }
    matches.remove(&std::process::id());
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

pub(super) fn interactive_login(action: &Value) -> bool {
    action["op"] == "login_hub"
        || (action["op"] == "hub_action"
            && matches!(action["action"].as_str(), Some("login" | "switch_account")))
}

pub(super) fn dispatch_action(
    service: &Arc<Mutex<DaemonService>>,
    action: &Value,
    ui_generation: Option<u64>,
) -> Result<Value, String> {
    let mut result = dispatch_action_inner(service, action, ui_generation);
    let refresh_setup = matches!(
        string(action, "op"),
        "setup_apply" | "setup_remove_app" | "setup_disconnect_all"
    );
    // Await invalidated inspections without holding service. Retain the final
    // guard so another mutation cannot invalidate them before receipt publication.
    let completion = service
        .lock()
        .map_err(|_| "Daemon lock poisoned")?
        .setup_completion
        .clone();
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut s = loop {
        let before = *completion.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        s.validate_ui_generation(ui_generation)?;
        if !refresh_setup {
            break s;
        }
        if s.closing || shutdown_requested() {
            result = Err("Shutting down".into());
            break s;
        }
        s.refresh_setup_inspections();
        if s.setup_inspections
            .values()
            .all(|entry| entry.worker.is_none())
        {
            break s;
        }
        if Instant::now() >= deadline {
            result = Err("Setup inspection timed out".into());
            break s;
        }
        drop(s);
        let revision = completion.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = completion
            .1
            .wait_timeout_while(
                revision,
                deadline.saturating_duration_since(Instant::now()),
                |revision| *revision == before,
            )
            .unwrap_or_else(|e| e.into_inner());
    };
    if interactive_login(action)
        || (result.is_err() && matches!(string(action, "op"), "setup_save_hub" | "save_hub"))
    {
        s.record_hub_result(action, &result, None);
    }
    if let Err(error) = &result {
        s.emit(
            json!({"op":"error","error":error,"source_op":action["op"],"request_id":action["request_id"],"hub":action["hub"]}),
        );
    }
    if refresh_setup && result.is_ok() {
        s.emit(json!({"op":"settings","ok":true,"committed":true,"source_op":action["op"],"request_id":action["request_id"],"settings":s.settings()}));
        s.push_ui_result(Some(action));
    } else {
        s.push_ui();
    }
    drop(s);
    wake_daemon();
    result
}

fn dispatch_action_inner(
    service: &Arc<Mutex<DaemonService>>,
    action: &Value,
    ui_generation: Option<u64>,
) -> Result<Value, String> {
    if let Some(kind) = channel_action_kind(action) {
        return dispatch_channel_action(service, action, kind, ui_generation);
    }
    if matches!(
        string(action, "op"),
        "setup_save_hub" | "save_hub" | "paste_hub_invite"
    ) && action["parse_only"] != true
    {
        let (before, default_hub) = {
            let s = service.lock().map_err(|_| "Daemon lock poisoned")?;
            s.validate_ui_generation(ui_generation)?;
            if action["op"] == "paste_hub_invite" {
                s.reject_own_hub_invite(&parse_hub_invite(string(action, "invite"))?)?;
            }
            (s.raw_config()?, s.config.default_hub.clone())
        };
        let mut prepared = action.clone();
        let mut submitted = if action["op"] == "paste_hub_invite" {
            parse_hub_invite(string(action, "invite"))?
        } else {
            action.clone()
        };
        let patch = submitted
            .get("config")
            .filter(|v| v.is_object())
            .unwrap_or(&submitted);
        let original = submitted["original_name"]
            .as_str()
            .or_else(|| submitted["hub"].as_str())
            .unwrap_or(&default_hub);
        let base = if submitted["edit"] == true {
            &before["hubs"][original]
        } else {
            &Value::Null
        };
        let config = crate::core::config::discover_hub_config(
            crate::core::config::merge_hub_config(base, patch),
        )?;
        if action["op"] == "paste_hub_invite" {
            submitted = config;
            prepared["invite"] = json!(submitted.to_string());
        } else {
            prepared["config"] = config;
        }
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        s.validate_ui_generation(ui_generation)?;
        if s.closing || shutdown_requested() {
            return Err("Shutting down".into());
        }
        return s.action_with_precondition(&prepared, Some(&before));
    }
    let (hub, config, state_dir, attempt, snapshot) = {
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        s.validate_ui_generation(ui_generation)?;
        if s.closing || shutdown_requested() {
            return Err("Shutting down".into());
        }
        if !interactive_login(action) {
            return s.action(action);
        }
        s.reload_slots()?;
        let hub = action["hub"]
            .as_str()
            .unwrap_or(&s.config.default_hub)
            .to_string();
        s.mutate_config(|v| {
            if !v["hubs"].is_object() || !v["hubs"][&hub].is_object() {
                return Err("INVALID_INPUT: Unknown hub".into());
            }
            v["hubs"][&hub]["enabled"] = json!(true);
            if let Some(m) = v["hubs"][&hub].as_object_mut() {
                m.remove("disabled");
            }
            Ok(())
        })?;
        s.reload_slots()?;
        let config = s.slot(&hub)?.config.clone();
        let snapshot =
            login_cache_snapshot(&s.config.state_dir.join(format!("oidc-token-{hub}.json")));
        let attempt = s.logins.entry(hub.clone()).or_default().clone();
        (hub, config, s.config.state_dir.clone(), attempt, snapshot)
    };
    // Browser callbacks and cache-lock waits never hold the daemon's global mutex.
    // Same-hub UI logins share publication, even when the provider returns
    // identical bearer bytes. Neither this queue nor the browser holds service.
    let _serial = attempt.serial.lock().map_err(|_| "Login lock poisoned")?;
    let result = {
        let current = login_cache_snapshot(&state_dir.join(format!("oidc-token-{hub}.json")));
        let changed = snapshot.as_ref().map(|(_, generation)| generation)
            != current.as_ref().map(|(_, generation)| generation);
        let rejected = if changed {
            None
        } else {
            snapshot
                .as_ref()
                .and_then(|(cache, _)| cache["token_sha256"].as_str())
        };
        crate::core::login::recover_cancellable_with_options(
            &state_dir,
            &hub,
            &config,
            rejected,
            true,
            action["action"] == "switch_account" || action["force_interactive"] == true,
            &|| shutdown_requested() || attempt.cancelled.load(Ordering::SeqCst),
        )
    };
    // Release per-Hub publication before re-entering the service mutex. Purge
    // may hold service while cancelling and waiting for this publication lock.
    drop(_serial);
    let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
    let current = s.logins.get(&hub).is_some_and(|c| Arc::ptr_eq(c, &attempt));
    if current && Arc::strong_count(&attempt) == 2 {
        s.logins.remove(&hub);
    }
    if !current || attempt.cancelled.load(Ordering::SeqCst) || s.closing || shutdown_requested() {
        return Err("OIDC_LOGIN_TIMEOUT: OIDC login cancelled".into());
    }
    s.reload_slots()?;
    if !s.slots.get(&hub).is_some_and(|slot| slot.config == config) {
        return Err("OIDC_LOGIN_TIMEOUT: Hub changed during login".into());
    }
    let (token, identity) = result?;
    if let Some(old) = s.slots.remove(&hub) {
        old.close();
    }
    for row in s.state.active()?.into_iter().filter(|row| {
        row["hub"] == hub
            && !crate::core::auth::principal_ids_equal(
                string(row, "principal_id"),
                string(&identity, "principal_id"),
            )
    }) {
        s.state.fence_listener_for(&row, "fenced_before_dispatch")?;
    }
    s.reload_slots()?;
    s.push_ui();
    Ok(json!({"ok":true,"user":identity["owner_user"],"authenticated":!token.is_empty()}))
}

pub(super) fn handle_http(
    mut stream: TcpStream,
    service: Arc<Mutex<DaemonService>>,
    health: Arc<Value>,
) {
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(copy) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(copy);
    let mut request = String::new();
    if reader
        .by_ref()
        .take(8193)
        .read_line(&mut request)
        .unwrap_or(0)
        > 8192
    {
        return;
    }
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");
    let mut length = 0;
    let mut headers = Map::new();
    let mut header_size = 0;
    loop {
        let mut line = String::new();
        let n = reader.by_ref().take(8193).read_line(&mut line).unwrap_or(0);
        header_size += n;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if header_size > 16_384 {
            respond(&mut stream, 431, &json!({"error":"Headers too large"}));
            return;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            if k == "content-length" {
                let Ok(n) = v.trim().parse::<usize>() else {
                    respond(&mut stream, 400, &json!({"error":"Invalid Content-Length"}));
                    return;
                };
                length = n;
            }
            if headers.insert(k, json!(v.trim())).is_some() {
                respond(&mut stream, 400, &json!({"error":"Duplicate header"}));
                return;
            }
        }
    }
    let Ok(bound) = stream.local_addr() else {
        return;
    };
    if let Err((status, error)) = crate::core::http_boundary::validate(
        bound,
        method,
        headers.get("host").and_then(Value::as_str),
        headers.get("origin").and_then(Value::as_str),
        headers.get("content-type").and_then(Value::as_str),
    ) {
        respond(&mut stream, status, &json!({"error":error}));
        return;
    }
    if length > REQUEST_LIMIT {
        respond(
            &mut stream,
            413,
            &json!({"error":"Request body exceeds 1 MiB"}),
        );
        return;
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    if path == "/health" && method == "GET" {
        if shutdown_requested() {
            respond(
                &mut stream,
                503,
                &json!({"ok":false,"error":"Shutting down"}),
            );
        } else {
            respond(&mut stream, 200, &health);
        }
        return;
    }
    if method != "POST" {
        respond(&mut stream, 404, &json!({"error":"Not found"}));
        return;
    }
    let Ok(mut parsed) = serde_json::from_slice::<Value>(&body) else {
        respond(&mut stream, 400, &json!({"error":"Invalid JSON"}));
        return;
    };
    if matches!(path, "/ui" | "/action" | "/control") {
        match dispatch_action(&service, &parsed, None) {
            Ok(v) => match bounded_frame(v) {
                Ok(bytes) => respond_bytes(&mut stream, 200, &bytes),
                Err(error) => respond(&mut stream, 500, &json!({"ok":false,"error":error})),
            },
            Err(e) => respond(&mut stream, 400, &json!({"ok":false,"error":e})),
        }
        return;
    }
    let Ok(mut s) = service.lock() else {
        return;
    };
    if s.closing || SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
        respond(&mut stream, 503, &json!({"error":"Shutting down"}));
        return;
    }
    if path != "/mcp" {
        respond(&mut stream, 404, &json!({"error":"Not found"}));
        return;
    }
    let id = parsed["id"].clone();
    if !parsed.is_object() || parsed["jsonrpc"] != "2.0" || !parsed["method"].is_string() {
        respond(
            &mut stream,
            400,
            &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32600,"message":"Invalid Request"}}),
        );
        return;
    }
    let method = string(&parsed, "method").to_string();
    if id.is_null() || method.starts_with("notifications/") {
        respond(&mut stream, 202, &Value::Null);
        return;
    }
    let result = match method.as_str() {
        "initialize" => {
            json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"cito","version":CITO_VERSION}})
        }
        "tools/list" => tools_list(),
        "ping" => json!({}),
        "tools/call" => {
            if !parsed["params"].is_object() {
                parsed["params"] = json!({});
            }
            let name = string(&parsed["params"], "name").to_string();
            if !parsed["params"]["_meta"].is_object() {
                parsed["params"]["_meta"] = json!({});
            }
            parsed["params"]["_meta"]["headers"] = Value::Object(headers);
            if let Some(pid) = http_client_pid(&stream) {
                parsed["params"]["_meta"]["_http_client"] = json!(pid);
            }
            let args = parsed["params"]["arguments"].clone();
            let tool = s.tool(&name, &args, &parsed["params"]["_meta"]);
            drop(s);
            match tool {
                Ok(mut payload) => {
                    if name == "cito_send" {
                        wait_send_receipt(&service, &mut payload);
                    }
                    json!({"isError":false,"content":[{"type":"text","text":payload.to_string()}]})
                }
                Err(error) => {
                    let (code, message) = error
                        .split_once(':')
                        .unwrap_or(("INTERNAL_ERROR", error.as_str()));
                    json!({"isError":true,"content":[{"type":"text","text":json!({"ok":false,"error":{"code":code,"message":message.trim(),"retryable":matches!(code,"NATS_UNAVAILABLE"|"AUTH_UNAVAILABLE"|"STALE_GENERATION")}}).to_string()}]})
                }
            }
        }
        _ => {
            respond(
                &mut stream,
                200,
                &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
            );
            return;
        }
    };
    respond(
        &mut stream,
        200,
        &json!({"jsonrpc":"2.0","id":id,"result":result}),
    );
}

pub(super) fn mutating_action(action: &Value) -> bool {
    matches!(
        string(action, "op"),
        "setup_save_hub"
            | "save_hub"
            | "set_host_hub"
            | "setup_apply"
            | "setup_remove_app"
            | "setup_disconnect_all"
            | "setup_remove_hub"
            | "remove_hub"
            | "setup_toggle_hub"
            | "hub_action"
            | "set_user"
            | "switch_hub"
            | "set_launch_at_login"
            | "set_auto_listen"
            | "set_scope"
            | "set_listen_mode"
            | "unlisten_session"
            | "unlisten_all"
            | "ack_signals"
            | "mark_activity_read"
            | "mark_all_activity_read"
            | "clear_activity_history"
    )
}

impl DaemonService {
    pub(super) fn purge_hub_state(&mut self, hub: &str) -> Result<(), String> {
        if let Some(slot) = self.slots.remove(hub) {
            slot.close();
        }
        if let Some(pending) = self.oidc_recoveries.remove(hub) {
            pending.cancelled.store(true, Ordering::SeqCst);
            let _ = pending.worker.join();
        }
        let belongs = |key: &str| {
            key.starts_with(&format!("{hub}:"))
                || ["directory", "channels", "auto"]
                    .iter()
                    .any(|prefix| key == format!("{prefix}:{hub}"))
        };
        let workers: Vec<_> = self
            .receivers
            .keys()
            .filter(|key| belongs(key))
            .cloned()
            .collect();
        for key in workers {
            if let Some(worker) = self.receivers.remove(&key) {
                let _ = worker.join();
            }
        }
        self.state.db.transaction(|db| {
            for table in [
                "local_signals",
                "local_listeners",
                "pending_hub_acks",
                "pending_hub_detaches",
                "local_sent_idempotency",
                "wake_retries",
            ] {
                db.execute(&format!("DELETE FROM {table} WHERE hub=?"), &[json!(hub)])?;
            }
            Ok(())
        })?;
        self.last_hub_errors.remove(hub);
        self.auth_backoff.remove(hub);
        self.channel_inventories.remove(hub);
        self.observations.retain(|key, _| !belongs(key));
        if let Ok(mut retries) = self.ack_backoff.lock() {
            retries.retain(|key, _| {
                serde_json::from_str::<Value>(key).is_ok_and(|value| value[0] != hub)
            });
        }
        let token = self.config.state_dir.join(format!("oidc-token-{hub}.json"));
        match fs::remove_file(token) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn reject_own_hub_invite(&self, invite: &Value) -> Result<(), String> {
        let Some(server) = &self.host_server else {
            return Ok(());
        };
        let url =
            url::Url::parse(string(invite, "url")).map_err(|_| "INVALID_INPUT: Invalid Hub URL")?;
        let raw = self.raw_config()?;
        let host = raw
            .get("host_hub")
            .or_else(|| raw.get("hostHub"))
            .unwrap_or(&Value::Null);
        let bind = string(host, "bind");
        let hostname = local_host_name();
        let mut local_hosts = vec![
            "127.0.0.1".into(),
            "localhost".into(),
            "::1".into(),
            hostname.clone(),
            format!("{}.local", hostname.trim_end_matches(".local")),
            advertised_hub_host(bind),
            lan_advertised_host(bind),
        ];
        if matches!(bind, "" | "0.0.0.0" | "::") {
            local_hosts.extend(tailscale_advertised_host());
        }
        let invite_host = url
            .host_str()
            .unwrap_or("")
            .trim_matches(['[', ']'])
            .trim_end_matches('.');
        if url.port_or_known_default() == Some(server.port())
            && local_hosts.iter().any(|local| {
                local
                    .trim_matches(['[', ']'])
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(invite_host)
            })
        {
            return Err("INVALID_INPUT: This invite code belongs to this Mac's own shared Hub (already active as 'local'). Paste it in Cito on your other Mac to connect.".into());
        }
        Ok(())
    }
    pub fn action(&mut self, action: &Value) -> Result<Value, String> {
        self.action_with_precondition(action, None)
    }

    fn action_with_precondition(
        &mut self,
        action: &Value,
        precondition: Option<&Value>,
    ) -> Result<Value, String> {
        let mut resolved_hub = None;
        let result = self
            .action_inner(action, &mut resolved_hub, precondition)
            .map(|mut value| {
                if mutating_action(action) && value["ok"] == true {
                    value["committed"] = json!(true);
                }
                if let Some(name) = &resolved_hub {
                    value["redirect_port"] = self
                        .slots
                        .get(name)
                        .map(|slot| slot.config["redirectPort"].clone())
                        .unwrap_or(Value::Null);
                }
                value
            });
        self.record_hub_result(action, &result, resolved_hub.as_deref());
        if result.is_ok() && matches!(string(action, "op"), "setup_save_hub" | "save_hub") {
            self.emit(json!({"op":"settings","ok":true,"committed":true,"source_op":action["op"],"request_id":action["request_id"],"settings":self.settings()}));
        }
        if result.is_ok()
            && matches!(
                string(action, "op"),
                "clear_activity_history"
                    | "unlisten_all"
                    | "ack_signals"
                    | "mark_activity_read"
                    | "mark_all_activity_read"
            )
        {
            self.push_ui_result(Some(action));
        } else {
            self.push_ui();
        }
        result
    }

    fn record_hub_result(
        &mut self,
        action: &Value,
        result: &Result<Value, String>,
        resolved_hub: Option<&str>,
    ) {
        if matches!(
            string(action, "op"),
            "login_hub"
                | "hub_action"
                | "test_hub"
                | "setup_save_hub"
                | "save_hub"
                | "setup_toggle_hub"
        ) {
            let saving = matches!(string(action, "op"), "setup_save_hub" | "save_hub");
            let mut hubs = Vec::new();
            if let Some(hub) = action["hub"]
                .as_str()
                .or_else(|| action["name"].as_str())
                .filter(|hub| !hub.is_empty())
            {
                hubs.push(hub.to_owned());
            } else if !saving {
                hubs.push(self.config.default_hub.clone());
            }
            if saving {
                if let Some(hub) = resolved_hub {
                    hubs.push(hub.to_owned());
                }
                let original = string(action, "original_name");
                if !original.is_empty() {
                    hubs.push(original.to_owned());
                }
                let requested = string(action, "name").trim();
                if requested.chars().any(|c| c.is_ascii_alphanumeric()) {
                    hubs.push(normalize_slug(requested));
                }
            }
            hubs.sort_unstable();
            hubs.dedup();
            for hub in hubs {
                match result {
                    Ok(_) => {
                        self.last_hub_errors.remove(&hub);
                    }
                    Err(error) => {
                        let transient = ["NATS_UNAVAILABLE:", "AUTH_UNAVAILABLE:", "UNAUTHORIZED:"]
                            .iter()
                            .any(|prefix| error.starts_with(prefix));
                        self.last_hub_errors.insert(hub, (error.clone(), transient));
                    }
                }
            }
        }
    }

    fn action_inner(
        &mut self,
        action: &Value,
        resolved_hub: &mut Option<String>,
        precondition: Option<&Value>,
    ) -> Result<Value, String> {
        if !action.is_object() {
            return Err("INVALID_INPUT: Action must be an object".into());
        }
        let op = string(action, "op");
        let hub = action["hub"]
            .as_str()
            .unwrap_or(&self.config.default_hub)
            .to_string();
        if matches!(
            op,
            "setup_remove_hub" | "remove_hub" | "save_hub" | "setup_save_hub"
        ) || (op == "hub_action" && action["action"] == "disconnect")
            || (op == "setup_toggle_hub" && action["enabled"] == false)
        {
            if self.active_settings_hub.as_deref() == Some(&hub) {
                self.active_settings_hub = None;
            }
            if let Some(cancelled) = self.logins.remove(&hub) {
                cancelled.cancelled.store(true, Ordering::SeqCst);
            }
        }
        self.reload_slots()?;
        match op {
            "get_snapshot" => {
                self.refresh_available()?;
                self.observations
                    .retain(|key, _| !key.starts_with("directory:"));
                wake_daemon();
                return self.snapshot();
            }
            "open_menu" => {
                self.menu_open = true;
                self.refresh_available()?;
            }
            "close_menu" => {
                self.menu_open = false;
            }
            "cli_human_device" => {
                if action["attach"] == true {
                    let slot = self.slot(&hub)?;
                    return self.listen(&hub,&json!({"about":"Human"}),&json!({"harness":"human","session_id":"default","cwd":"","title":"Human","principal_id":slot.identity["principal_id"]}));
                }
                let id = crate::core::registration::local_node_id(
                    &self.state,
                    self.slot(&hub)?,
                    "human",
                    "default",
                )?;
                let removed = self.unlisten_id(&hub, &id)?;
                return Ok(json!({"ok":true,"removed":u8::from(removed)}));
            }
            "human_inbox_update" => {
                let slot = self.slot(&hub)?;
                let registration = self.state.registration_lock(&hub)?;
                let _registration = registration
                    .lock()
                    .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
                let id = slot
                    .cached_directory()
                    .into_iter()
                    .find(|row| {
                        row["harness"] == "human"
                            && crate::core::auth::principal_ids_equal(
                                string(row, "principal_id"),
                                string(&slot.identity, "principal_id"),
                            )
                    })
                    .map(|row| string(&row, "listener_id").to_owned())
                    .unwrap_or_else(|| {
                        listener_id(string(&slot.identity, "principal_id"), "human", "default")
                    });
                let mut params = action.clone();
                params["node_id"] = json!(id);
                if params.get("expected_control_revision").is_none() {
                    params["expected_control_revision"] = slot
                        .kv_get(&id)?
                        .map(|(row, _)| row["human_control_revision"].clone())
                        .filter(|v| v.is_number())
                        .unwrap_or(json!(1));
                }
                let result = slot.human_request("human_inbox_update", &params)?;
                Self::reconcile_human_scope(&self.state, slot, &self.host_id, &result["record"])?;
                self.push_ui_result(Some(action));
                return Ok(result);
            }
            "cli_rename" => {
                let caller = self
                    .caller(&action["meta"], true)?
                    .ok_or("BINDING_REQUIRED: Caller required")?;
                return self.rename_session(
                    string(&caller, "harness"),
                    string(&caller, "session_id"),
                    string(action, "title"),
                );
            }
            "rename_session" => {
                let row = self
                    .state
                    .active()?
                    .into_iter()
                    .find(|r| {
                        (action["harness"].is_null() || action["harness"] == r["harness"])
                            && (action["session_id"].is_null()
                                || action["session_id"] == r["session_id"])
                            && (action["listener_id"].is_null()
                                || action["listener_id"] == r["listener_id"])
                            && ((action["harness"].is_string() && action["session_id"].is_string())
                                || action["listener_id"].is_string())
                    })
                    .ok_or("LISTENER_NOT_FOUND: Session is not active")?;
                return self.rename_session(
                    string(&row, "harness"),
                    string(&row, "session_id"),
                    string(action, "title"),
                );
            }
            "unlisten_session" => {
                let mut detach_errors = Vec::new();
                if action["harness"] != "human" {
                    let targets: Vec<String> = self
                        .session_hubs(string(action, "harness"), string(action, "session_id"))?
                        .into_iter()
                        .filter(|name| action["hub"].is_null() || *name == hub)
                        .collect();
                    for name in targets {
                        self.ensure_agent_stop_row(
                            &name,
                            string(action, "harness"),
                            string(action, "session_id"),
                        )?;
                        let slot = self.slots.get(&name);
                        let rows=self.state.db.query("SELECT * FROM local_listeners WHERE hub=? AND harness=? AND session_id=? AND status!='FENCED_INACTIVE'",&[json!(name),action["harness"].clone(),action["session_id"].clone()])?;
                        for row in rows.into_iter().filter(|row| {
                            slot.is_none_or(|slot| {
                                crate::core::auth::principal_ids_equal(
                                    string(row, "principal_id"),
                                    string(&slot.identity, "principal_id"),
                                )
                            })
                        }) {
                            if let Err(error) = self.unlisten_id(&name, string(&row, "listener_id"))
                            {
                                detach_errors.push(format!("{name}: {error}"));
                            }
                        }
                    }
                }
                if action["harness"] == "human" && action["session_id"] == "default" {
                    // Fence automatic enrollment that has not committed a local row yet.
                    for (name, slot) in &self.slots {
                        if !action["hub"].is_null() && name != &hub {
                            continue;
                        }
                        let id = crate::core::registration::local_node_id(
                            &self.state,
                            slot,
                            "human",
                            "default",
                        )?;
                        if let Err(error) = self.unlisten_id(name, &id) {
                            detach_errors.push(format!("{name}: {error}"));
                        }
                    }
                }
                if !detach_errors.is_empty() {
                    return Err(format!("DETACH_PENDING: {}", detach_errors.join("; ")));
                }
            }
            "set_user" => {
                let user = string(action, "user");
                if !crate::plugins::validate_user(user) {
                    return Err("INVALID_INPUT: Invalid user".into());
                }
                self.mutate_config(|v| {
                    v["user"] = json!(user);
                    Ok(())
                })?;
            }
            "setup_apply" => {
                let id = action["harness"]
                    .as_str()
                    .or_else(|| action["app"].as_str())
                    .ok_or("INVALID_INPUT: harness required")?;
                let p = self
                    .plugins
                    .find_by_id(id)
                    .ok_or("UNSUPPORTED_HARNESS: Unknown harness")?;
                ensure_single_plugin_staged(&self.config.state_dir, p)?;
                let result = apply_plugin_setup(p, true, false);
                self.invalidate_setup(id);
                result?;
                self.last_app_action.insert(id.to_owned(), "setup".into());
                self.refresh_available()?;
            }
            "setup_remove_app" => {
                let id = action["harness"]
                    .as_str()
                    .or_else(|| action["app"].as_str())
                    .ok_or("INVALID_INPUT: harness required")?;
                let p = self
                    .plugins
                    .find_by_id(id)
                    .ok_or("UNSUPPORTED_HARNESS: Unknown harness")?;
                let result = remove_plugin_setup(p);
                let notice = p
                    .post_disconnect_hint
                    .clone()
                    .unwrap_or("Disconnected · Active listeners stopped".into());
                self.invalidate_setup(id);
                result?;
                let mut detach_errors = Vec::new();
                for row in self
                    .state
                    .active()?
                    .into_iter()
                    .filter(|r| r["harness"] == id)
                {
                    if let Err(error) = self.retire_id(
                        string(&row, "hub"),
                        string(&row, "listener_id"),
                        "disconnected",
                    ) {
                        detach_errors.push(error);
                    }
                }
                self.caller_cache
                    .lock()
                    .map_err(|_| "Caller cache lock poisoned")?
                    .retain(|_, v| v["harness"] != id);
                self.last_app_action
                    .insert(id.to_owned(), "disconnect".into());
                if let Some(activity) = self.session_activity.as_mut() {
                    activity.remove_candidates(id);
                }
                return Ok(
                    json!({"ok":true,"notice":notice,"detach_status":if detach_errors.is_empty(){"complete"}else{"pending"},"warnings":detach_errors}),
                );
            }
            "setup_save_hub" | "save_hub" => {
                let requested_name = string(action, "name").trim();
                if !requested_name.is_empty()
                    && !requested_name.chars().any(|c| c.is_ascii_alphanumeric())
                {
                    return Err("INVALID_INPUT: Invalid hub name".into());
                }
                let mut cfg = action["config"]
                    .as_object()
                    .cloned()
                    .map(Value::Object)
                    .unwrap_or_else(|| action.clone());
                // Normalize submitted aliases before merging with existing canonical keys.
                for (canonical, aliases) in [
                    ("authMode", &["auth_mode", "auth"][..]),
                    ("clientId", &["client_id", "oidcClientId"][..]),
                    ("issuer", &["oidcIssuer"][..]),
                    ("redirectPort", &["redirect_port", "oidcRedirectPort"][..]),
                    ("scopes", &["oidcScopes"][..]),
                ] {
                    if cfg.get(canonical).is_none() {
                        if let Some(value) = aliases.iter().find_map(|key| cfg.get(*key)).cloned() {
                            cfg[canonical] = value;
                        }
                    }
                }
                let editing = action["edit"] == true;
                let original = action["original_name"].as_str().unwrap_or(&hub).to_string();
                let mut rename_before = Value::Null;
                let mut renamed_config = Value::Null;
                self.mutate_config(|v| {
                    if precondition.is_some_and(|before| before != v) {
                        return Err(
                            "INVALID_INPUT: Configuration changed during discovery; retry".into(),
                        );
                    }
                    rename_before = v.clone();
                    // Materialize the implicit local Hub before saving/editing a
                    // first explicit Hub, so first-run edits cannot discard it.
                    if v["hubs"].is_null() {
                        v["hubs"] = json!({"local":{"url":"local://"}});
                    }
                    if !v["hubs"].is_object() {
                        return Err("INVALID_INPUT: Hubs must be an object".into());
                    }
                    if editing {
                        let mut base = v["hubs"][&original].clone();
                        if !base.is_object() {
                            return Err("INVALID_INPUT: Unknown hub to edit".into());
                        }
                        if precondition.is_none() {
                            base = crate::core::config::merge_hub_config(&base, &cfg);
                            cfg = base;
                        }
                    }
                    cfg["url"] = json!(crate::plugins::validate_nats_url(
                        string(&cfg, "url").trim()
                    )
                    .map_err(|error| format!("INVALID_INPUT: {error}"))?);
                    if cfg["authMode"] == "oidc" {
                        super::super::login::Settings::new(&cfg)?;
                    }
                    let base_name = if requested_name.is_empty() {
                        hub_name_from_url(string(&cfg, "url"))
                    } else {
                        normalize_slug(requested_name)
                    };
                    let mut name = base_name.clone();
                    if requested_name.is_empty() {
                        let mut suffix = 2;
                        while name == "local"
                            || v["hubs"].get(&name).is_some_and(|existing| {
                                (!editing || name != original) && existing["url"] != cfg["url"]
                            })
                        {
                            let ending = format!("-{suffix}");
                            name = format!(
                                "{}{}",
                                base_name
                                    .chars()
                                    .take(32 - ending.len())
                                    .collect::<String>()
                                    .trim_end_matches('-'),
                                ending
                            );
                            suffix += 1;
                        }
                    }
                    if !crate::plugins::validate_hub_name(&name) {
                        return Err("INVALID_INPUT: Invalid hub name".into());
                    }
                    if editing && original == "local" && name != "local" {
                        return Err("INVALID_INPUT: Cannot rename the local hub".into());
                    }
                    if !editing
                        && (name == "local"
                            || v["hubs"]
                                .get(&name)
                                .is_some_and(|existing| existing["url"] != cfg["url"]))
                    {
                        return Err("INVALID_INPUT: Hub name already exists".into());
                    }
                    let auth = cfg
                        .get("authMode")
                        .or_else(|| cfg.get("auth_mode"))
                        .or_else(|| cfg.get("auth"))
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| {
                            if !string(&cfg, "token").is_empty() {
                                "token"
                            } else if !string(&cfg, "issuer").is_empty()
                                || !string(&cfg, "clientId").is_empty()
                            {
                                "oidc"
                            } else {
                                "none"
                            }
                        })
                        .to_ascii_lowercase();
                    if !matches!(auth.as_str(), "none" | "token" | "oidc") {
                        return Err("INVALID_INPUT: Invalid authMode".into());
                    }
                    cfg["authMode"] = json!(auth);
                    if auth == "token"
                        && cfg["token"]
                            .as_str()
                            .is_none_or(|token| token.trim().is_empty())
                    {
                        return Err("INVALID_INPUT: Hub token is required in token mode".into());
                    }
                    if auth == "oidc" {
                        crate::core::login::Settings::new(&cfg)?;
                    }
                    if cfg["user"].is_null() {
                        if let Some(m) = cfg.as_object_mut() {
                            m.remove("user");
                        }
                    }
                    let set_default = cfg["set_default"].as_bool().unwrap_or(false);
                    if let Some(m) = cfg.as_object_mut() {
                        if let Some(disabled) = m.remove("disabled") {
                            let enabled = m.get("enabled").and_then(Value::as_bool) != Some(false)
                                && disabled.as_bool() != Some(true);
                            m.insert("enabled".to_string(), json!(enabled));
                        }
                        for k in [
                            "op",
                            "name",
                            "hub",
                            "auth",
                            "auth_mode",
                            "set_default",
                            "edit",
                            "original_name",
                            "request_id",
                            "client_id",
                            "oidcIssuer",
                            "oidcClientId",
                            "redirect_port",
                            "oidcRedirectPort",
                            "oidcScopes",
                        ] {
                            m.remove(k);
                        }
                    }
                    if editing && original != name {
                        if self
                            .config
                            .state_dir
                            .join(format!("oidc-token-{name}.json"))
                            .exists()
                        {
                            return Err(
                                "INVALID_INPUT: Destination Hub credentials already exist".into()
                            );
                        }
                        if !v["hubs"][&name].is_null() {
                            return Err("INVALID_INPUT: Hub name already exists".into());
                        }
                        for table in [
                            "local_listeners",
                            "local_signals",
                            "pending_hub_acks",
                            "pending_hub_detaches",
                            "local_sent_idempotency",
                            "wake_retries",
                        ] {
                            if !self
                                .state
                                .db
                                .query(
                                    &format!("SELECT 1 FROM {table} WHERE hub=? LIMIT 1"),
                                    &[json!(name)],
                                )?
                                .is_empty()
                            {
                                return Err(
                                    "INVALID_INPUT: Destination Hub name has retained local data"
                                        .into(),
                                );
                            }
                        }
                        if let Some(h) = v["hubs"].as_object_mut() {
                            h.remove(&original);
                        }
                        let default = v
                            .get("default_hub")
                            .or_else(|| v.get("defaultHub"))
                            .and_then(Value::as_str);
                        if default == Some(original.as_str()) {
                            v["default_hub"] = json!(name);
                            if let Some(m) = v.as_object_mut() {
                                m.remove("defaultHub");
                            }
                        }
                    }
                    // Retain the identity resolved under the config lock without
                    // adding an internal bookkeeping field to the public response.
                    *resolved_hub = Some(name.clone());
                    renamed_config = cfg.clone();
                    v["hubs"][&name] = cfg;
                    if set_default {
                        v["default_hub"] = json!(name);
                        if let Some(m) = v.as_object_mut() {
                            m.remove("defaultHub");
                        }
                    }
                    Ok(())
                })?;
                if let Some(name) = resolved_hub
                    .as_ref()
                    .filter(|name| editing && **name != original)
                {
                    if let Some(attempt) = self.logins.remove(&original) {
                        attempt.cancelled.store(true, Ordering::SeqCst);
                        drop(attempt.serial.lock().map_err(|_| "Login lock poisoned")?);
                    }
                    if let Some(old) = self.slots.remove(&original) {
                        old.close();
                    }
                    let keys: Vec<_> = self
                        .receivers
                        .keys()
                        .filter(|key| {
                            key.starts_with(&format!("{original}:"))
                                || **key == format!("auto:{original}")
                        })
                        .cloned()
                        .collect();
                    for key in keys {
                        if let Some(worker) = self.receivers.remove(&key) {
                            let _ = worker.join();
                        }
                    }
                    if let Some(pending) = self.oidc_recoveries.remove(&original) {
                        pending.cancelled.store(true, Ordering::SeqCst);
                        let _ = pending.worker.join();
                    }
                    for pending in self.retired_auth.drain(..) {
                        pending.cancelled.store(true, Ordering::SeqCst);
                        let _ = pending.worker.join();
                    }
                    let old_token = self
                        .config
                        .state_dir
                        .join(format!("oidc-token-{original}.json"));
                    let new_token = self
                        .config
                        .state_dir
                        .join(format!("oidc-token-{name}.json"));
                    let migration = self.state.db.transaction(|db| {
                        for table in [
                            "local_listeners",
                            "local_signals",
                            "pending_hub_acks",
                            "pending_hub_detaches",
                            "local_sent_idempotency",
                            "wake_retries",
                        ] {
                            db.execute(
                                &format!("UPDATE {table} SET hub=? WHERE hub=?"),
                                &[json!(name), json!(original)],
                            )?;
                        }
                        if old_token.exists() {
                            fs::rename(&old_token, &new_token).map_err(|e| e.to_string())?;
                        }
                        Ok(())
                    });
                    if let Err(error) = migration {
                        if !old_token.exists() && new_token.exists() {
                            fs::rename(&new_token,&old_token).map_err(|recovery|format!("Hub rename failed: {error}; credential rollback failed: {recovery}"))?;
                        }
                        self.mutate_config(|v| {
                            if v["hubs"][name] != renamed_config { return Err("Hub rename rollback conflicts with a newer configuration".into()); }
                            if let Some(hubs) = v["hubs"].as_object_mut() { hubs.remove(name); if !rename_before["hubs"][&original].is_null() { hubs.insert(original.clone(),rename_before["hubs"][&original].clone()); } }
                            if v["default_hub"] == *name { v["default_hub"] = rename_before.get("default_hub").or_else(||rename_before.get("defaultHub")).cloned().unwrap_or(json!("local")); }
                            Ok(())
                        }).map_err(|recovery|format!("Hub rename failed: {error}; configuration rollback failed: {recovery}"))?;
                        return Err(error);
                    }
                    self.channel_inventories.remove(&original);
                }
            }
            "setup_toggle_hub" | "hub_action" => {
                let enabled = if op == "setup_toggle_hub" {
                    action["enabled"].as_bool().unwrap_or(true)
                } else {
                    string(action, "action") != "disconnect"
                };
                if op == "hub_action" && action["action"] == "reconnect" {
                    if let Some(old) = self.slots.remove(&hub) {
                        old.close();
                    }
                }
                self.mutate_config(|v| {
                    if v["hubs"].is_null() {
                        v["hubs"] = json!({});
                    }
                    if !v["hubs"].is_object() {
                        return Err("INVALID_INPUT: Hubs must be an object".into());
                    }
                    if v["hubs"][&hub].is_null() {
                        v["hubs"][&hub] = json!({});
                    }
                    if !v["hubs"][&hub].is_object() {
                        return Err("INVALID_INPUT: Hub must be an object".into());
                    }
                    v["hubs"][&hub]["enabled"] = json!(enabled);
                    if let Some(m) = v["hubs"][&hub].as_object_mut() {
                        m.remove("disabled");
                    }
                    repair_default_hub(v);
                    Ok(())
                })?;
            }
            "setup_remove_hub" | "remove_hub" => {
                if hub == "local" {
                    return Err("INVALID_INPUT: Cannot remove the local hub".into());
                }
                self.mutate_config(|v| {
                    if let Some(h) = v["hubs"].as_object_mut() {
                        h.remove(&hub);
                    }
                    repair_default_hub(v);
                    Ok(())
                })?;
                self.purge_hub_state(&hub)?;
            }
            "test_hub" => return self.slot(&hub)?.rpc("account_info", &json!({})),
            "paste_hub_invite" => {
                let mut invite = parse_hub_invite(string(action, "invite"))?;
                if action["parse_only"] == true {
                    if let Some(port) = invite
                        .get("redirectPort")
                        .or_else(|| invite.get("oidcRedirectPort"))
                        .cloned()
                    {
                        invite["redirect_port"] = port;
                    }
                    self.emit(json!({"op":"hub_invite","request_id":action["request_id"],"config":invite}));
                    return Ok(json!({"ok":true}));
                }
                let raw = self.raw_config()?;
                self.reject_own_hub_invite(&invite)?;
                let base = normalize_slug(string(&invite, "name"));
                if !string(&invite, "name").is_empty() {
                    let mut name = base.clone();
                    let mut suffix = 2;
                    while name == "local"
                        || raw["hubs"]
                            .get(&name)
                            .is_some_and(|hub| hub["url"] != invite["url"])
                    {
                        let ending = format!("-{suffix}");
                        name = format!(
                            "{}{}",
                            base.chars()
                                .take(32 - ending.len())
                                .collect::<String>()
                                .trim_end_matches('-'),
                            ending
                        );
                        suffix += 1;
                    }
                    invite["name"] = json!(name);
                }
                invite["request_id"] = action["request_id"].clone();
                return self.action_with_precondition(&invite, precondition);
            }
            "generate_hub_invite" => {
                let mut invite = self.slot(&hub)?.config.clone();
                invite["name"] = json!(hub);
                return Ok(json!({"ok":true,"invite":invite}));
            }
            "send_signal" | "send_reply_from_human" => {
                let caller =
                    json!({"harness":"human","session_id":"default","cwd":"","title":"Human"});
                let mut send_args = action.clone();
                if let Some(args) = send_args.as_object_mut() {
                    args.remove("op");
                    args.remove("request_id");
                    if op == "send_reply_from_human" {
                        args.insert(
                            "to".into(),
                            action["to_listener_id"]
                                .as_str()
                                .or_else(|| action["to"].as_str())
                                .map_or(Value::Null, |v| json!(v)),
                        );
                        args.insert(
                            "message".into(),
                            action["message"]
                                .as_str()
                                .or_else(|| action["text"].as_str())
                                .map_or(Value::Null, |v| json!(v)),
                        );
                        args.remove("in_reply_to");
                        if let Some(source) = action
                            .get("in_reply_to")
                            .filter(|v| !v.is_null())
                            .or_else(|| action.get("signal_id").filter(|v| !v.is_null()))
                        {
                            args.insert("in_reply_to".into(), source.clone());
                        }
                        for key in ["to_listener_id", "text", "signal_id"] {
                            args.remove(key);
                        }
                    }
                }
                let result = validate_tool_input("cito_send", &send_args)
                    .and_then(|()| self.send_from(&hub, &send_args, Some(caller)));
                self.emit(match &result {
                    Ok(_) => json!({"op":"composer_result","ok":true,"error":null,"request_id":action["request_id"]}),
                    Err(e) => json!({"op":"composer_result","ok":false,"error":e,"request_id":action["request_id"]}),
                });
                return result.map(|result| {
                    if op == "send_reply_from_human" {
                        // Keep the reply action's response shape while sharing the atomic Human RPC.
                        let unscoped = |key| string(&result, key).rsplit(':').next().unwrap_or("");
                        json!({"ok":true,"signal_id":unscoped("signal_id"),"target_display":unscoped("to"),"error":null})
                    } else {
                        result
                    }
                });
            }
            "ack_signals" | "mark_activity_read" | "mark_all_activity_read" => {
                let ids = action["signal_ids"].as_array().cloned().unwrap_or_default();
                let rows = self.state.db.query(
                    "SELECT s.*,l.hub_url AS target_hub_url,l.principal_id AS target_principal_id,l.status AS target_status,l.harness AS target_harness,l.owner_user AS target_user,l.title AS target_title FROM local_signals s LEFT JOIN local_listeners l ON l.hub=s.hub AND l.listener_id=s.listener_id WHERE s.activity_hidden=0 AND s.acknowledged_at_ms IS NULL AND (?!='me' OR l.harness='human')",
                    &[json!(action["filter"].as_str().unwrap_or(&self.activity_filter))],
                )?;
                for row in rows {
                    if !crate::core::state::joined_signal_is_current(&row) {
                        continue;
                    }
                    let requested_hub = string(action, "hub");
                    if !requested_hub.is_empty() && row["hub"] != requested_hub {
                        continue;
                    }
                    let bulk = op == "mark_all_activity_read" || action["ack_all"] == true;
                    if bulk
                        && !crate::core::state::matches_activity(
                            &row,
                            action["query"].as_str().unwrap_or(""),
                        )
                    {
                        continue;
                    }
                    if bulk
                        || ids.iter().any(|id| {
                            id == &row["signal_id"]
                                || id.as_str()
                                    == Some(
                                        format!(
                                            "{}:{}",
                                            string(&row, "hub"),
                                            string(&row, "signal_id")
                                        )
                                        .as_str(),
                                    )
                                || id == &row["batch_id"]
                        })
                    {
                        self.state.acknowledge(
                            string(&row, "hub"),
                            string(&row, "listener_id"),
                            &[row["signal_id"].clone()],
                            &format!("{}:human", self.host_id),
                        )?;
                    }
                }
                self.flush_acks();
            }
            "copy_activity_message" => {
                let row = self
                    .state
                    .db
                    .query(
                        "SELECT s.*,l.hub_url AS target_hub_url,l.principal_id AS target_principal_id,l.status AS target_status FROM local_signals s LEFT JOIN local_listeners l ON s.hub=l.hub AND s.listener_id=l.listener_id WHERE s.hub=? AND s.signal_id=?",
                        &[json!(hub), action["signal_id"].clone()],
                    )?
                    .into_iter()
                    .next()
                    .filter(crate::core::state::joined_signal_is_current)
                    .ok_or("LISTENER_NOT_FOUND: Message no longer available")?;
                let message = string(&row, "message");
                let characters: Vec<_> = message.chars().collect();
                let chunks: Vec<String> = if characters.is_empty() {
                    vec![String::new()]
                } else {
                    characters
                        .chunks(16_000)
                        .map(|part| part.iter().collect())
                        .collect()
                };
                for (index, chunk) in chunks.iter().enumerate() {
                    self.emit(json!({"op":"activity_message","request_id":action["request_id"],"part":index,"complete":index+1==chunks.len(),"message":chunk}));
                }
                return Ok(json!({"ok":true}));
            }
            "clear_activity_history" => {
                self.state.db.transaction(|c| {
                    let selected = action["hub"].as_str().unwrap_or("");
                    let signal_id = action["signal_id"].as_str().unwrap_or("");
                    let filter=action["filter"].as_str().unwrap_or(&self.activity_filter);
                    c.query_while("SELECT s.*,l.hub_url AS target_hub_url,l.principal_id AS target_principal_id,l.status AS target_status,l.harness AS target_harness,l.owner_user AS target_user,l.title AS target_title FROM local_signals s LEFT JOIN local_listeners l ON l.hub=s.hub AND l.listener_id=s.listener_id WHERE s.activity_hidden=0 AND (?='' OR s.hub=?) AND (?='' OR signal_id=?) AND (?!='me' OR l.harness='human') AND (?!='unread' OR s.acknowledged_at_ms IS NULL)", &[json!(selected),json!(selected),json!(signal_id),json!(signal_id),json!(filter),json!(filter)],|row| {
                        if !crate::core::state::joined_signal_is_current(&row) || !crate::core::state::matches_activity(&row,action["query"].as_str().unwrap_or("")) { return Ok(true); }
                        if row["target_harness"] == "human" && row["acknowledged_at_ms"].is_null() { c.execute("INSERT OR REPLACE INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms,hub_url,principal_id) VALUES(?,?,?,?,?,?,?)", &[row["hub"].clone(),row["listener_id"].clone(),row["signal_id"].clone(),json!(format!("{}:human",self.host_id)),json!(now_ms()),row["hub_url"].clone(),row["principal_id"].clone()])?; }
                        c.execute("UPDATE local_signals SET activity_hidden=1,acknowledged_at_ms=CASE WHEN ?='human' THEN COALESCE(acknowledged_at_ms,?) ELSE acknowledged_at_ms END WHERE hub=? AND signal_id=?", &[row["target_harness"].clone(),json!(now_ms()),row["hub"].clone(),row["signal_id"].clone()])?;
                        Ok(true)
                    })?;
                    Ok(())
                })?;
                self.flush_acks();
            }
            "listen_session" => {
                let mut caller = action.clone();
                if caller["session_id"].is_null() {
                    caller["session_id"] = json!("default");
                }
                if caller["harness"] != "human" {
                    if !self
                        .plugins
                        .agent_plugins()
                        .iter()
                        .any(|p| p.id == string(&caller, "harness"))
                    {
                        return Err("UNSUPPORTED_HARNESS: Unknown harness".into());
                    }
                    let mut meta = json!({"expected_harness":caller["harness"],"expected_session_id":caller["session_id"],"cwd":caller["cwd"]});
                    if caller["endpoint_pid"].is_number() {
                        meta["endpoint_pid"] = caller["endpoint_pid"].clone();
                    }
                    caller = self
                        .caller(&meta, true)?
                        .ok_or("BINDING_REQUIRED: Session is not open")?;
                }
                if caller["harness"] == "human" && string(&caller, "title").trim().is_empty() {
                    caller["title"] = json!("Human");
                }
                let about = action["about"]
                    .as_str()
                    .filter(|about| caller["harness"] != "human" || !about.trim().is_empty())
                    .unwrap_or(if caller["harness"] == "human" {
                        "Human"
                    } else {
                        "Session"
                    });
                return self.listen(
                    &hub,
                    &json!({"about":about,"scope":action["scope"]}),
                    &caller,
                );
            }
            "unlisten" => {
                let id = string(action, "listener_id");
                self.unlisten_id(&hub, id)?;
            }
            "unlisten_all" => {
                // Bulk stop owns Hub policy even when no listener is active.
                let raw = self.raw_config()?;
                let (_, configured, _) =
                    crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))?;
                let selected_hub = action.get("hub").filter(|hub| !hub.is_null());
                let targets: Vec<String> = configured
                    .keys()
                    .filter(|name| selected_hub.is_none_or(|hub| *hub == name.as_str()))
                    .cloned()
                    .collect();
                if targets.is_empty() && action["hub"].is_string() {
                    return Err("INVALID_INPUT: Unknown hub".into());
                }
                let mut removed = 0;
                let mut detach_errors = Vec::new();
                for row in self.state.active()?.into_iter().filter(|row| {
                    row["harness"] != "human" && selected_hub.is_none_or(|hub| row["hub"] == *hub)
                }) {
                    match self.unlisten_id(string(&row, "hub"), string(&row, "listener_id")) {
                        Ok(true) => removed += 1,
                        Ok(false) => {}
                        Err(error) => {
                            detach_errors.push(format!("{}: {error}", string(&row, "hub")))
                        }
                    }
                }
                for name in targets {
                    if let Err(error) = self.set_auto_listen(&name, false) {
                        detach_errors.push(format!("{name}: {error}"));
                    }
                }
                if !detach_errors.is_empty() {
                    return Err(format!("DETACH_PENDING: {}", detach_errors.join("; ")));
                }
                return Ok(json!({"ok":true,"removed":removed}));
            }
            "set_listen_mode" => {
                let mode = string(action, "listen_mode");
                if !matches!(mode, "auto" | "manual") {
                    return Err("INVALID_INPUT: listen_mode must be auto or manual".into());
                }
                let slot = self.slot(&hub)?;
                if mode == "auto" && slot.config["autoListen"] != true {
                    return Err(
                        "INVALID_INPUT: Enable Auto-Listen before unpinning a session".into(),
                    );
                }
                let lock = self.state.registration_lock(&hub)?;
                let _guard = lock
                    .lock()
                    .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
                let row = self
                    .state
                    .listener(&hub, string(action, "listener_id"))?
                    .filter(|r| r["harness"] != "human" && r["status"] == "ACTIVE")
                    .ok_or("LISTENER_NOT_FOUND: Session is not active")?;
                let activity = self
                    .session_activity
                    .as_ref()
                    .into_iter()
                    .flat_map(|a| a.candidates())
                    .find(|c| {
                        c["harness"] == row["harness"] && c["session_id"] == row["session_id"]
                    })
                    .and_then(|c| super::super::state::qualifying_activity(&c));
                let at = activity.map(|(_, at)| at).unwrap_or_else(now_ms);
                self.state.db.execute("UPDATE local_listeners SET listen_mode=?,user_stopped=0,retirement_reason=NULL,retired_activity_revision=NULL,last_activity_at_ms=CASE WHEN listen_mode='manual' AND ?='auto' THEN MAX(COALESCE(last_activity_at_ms,0),?) ELSE last_activity_at_ms END WHERE hub=? AND listener_id=?", &[json!(mode),json!(mode),json!(at),json!(hub),row["listener_id"].clone()])?;
            }
            "set_scope" => {
                let id = string(action, "listener_id");
                let scope = string(action, "scope");
                if !matches!(scope, "self" | "workspace") {
                    return Err("INVALID_INPUT: Invalid scope".into());
                }
                let slot = self.slot(&hub)?;
                let registration = self.state.registration_lock(&hub)?;
                let _registration = registration
                    .lock()
                    .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
                for attempt in 0..3 {
                    let (mut row, rev) = slot.kv_get(id)?.ok_or("LISTENER_NOT_FOUND")?;
                    if !row.is_object() {
                        return Err("INVALID_INPUT: Listener must be an object".into());
                    }
                    if row["harness"] == "human" {
                        let result = slot.human_request("human_inbox_update",&json!({"node_id":id,"expected_control_revision":row["human_control_revision"],"scope":scope}))?;
                        Self::reconcile_human_scope(
                            &self.state,
                            slot,
                            &self.host_id,
                            &result["record"],
                        )?;
                        return Ok(result);
                    }
                    row["scope"] = json!(scope);
                    match slot.kv_save(id, &row, Some(rev)) {
                        Ok(_) => break,
                        Err(error) if attempt == 2 || !error.starts_with("10071:") => {
                            return Err(error)
                        }
                        Err(_) => {}
                    }
                }
                self.state.db.execute(
                    "UPDATE local_listeners SET scope=? WHERE hub=? AND listener_id=?",
                    &[json!(scope), json!(hub), json!(id)],
                )?;
            }
            "open_directory" | "query_directory" => {
                self.directory_open = true;
                if op == "open_directory" {
                    self.refresh_available()?;
                }
                self.activity_filter = action["filter"].as_str().unwrap_or("all").into();
                self.activity_query = if action["mode"] == "activity" {
                    string(action, "query").into()
                } else {
                    String::new()
                };
                let mut args = json!({"query":action["query"].as_str().unwrap_or(""),"limit":50});
                if let Some(hub) = action["hub"].as_str().filter(|s| !s.is_empty()) {
                    args["hub"] = json!(hub);
                }
                validate_tool_input("cito_find", &args)?;
                self.slot(&hub)?;
                let _ = self.caller(&json!({}), false)?;
                let harness = action["harness"]
                    .as_str()
                    .filter(|id| !id.is_empty() && *id != "all");
                let mine = matches!(action["scope"].as_str(), Some("mine" | "self"));
                let result = self.find_listeners(&args, |row| {
                    harness.is_none_or(|id| row["harness"] == id)
                        && (!mine || row["is_self_principal"] == true)
                })?;
                let (activity, total, unread) = self.state.query_activity(
                    action["filter"].as_str().unwrap_or("all"),
                    if action["mode"] == "activity" {
                        string(action, "query")
                    } else {
                        ""
                    },
                )?;
                let activity = self.activity_dto(activity);
                let warnings: Vec<String> = result["warnings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|warning| {
                        format!("{}: {}", string(warning, "hub"), string(warning, "message"))
                    })
                    .collect();
                let frame = json!({"op":"directory","hub":hub,"request_id":action["request_id"],"query":action["query"].as_str().unwrap_or(""),"scope":action["scope"].as_str().unwrap_or("all"),"harness":action["harness"].as_str().unwrap_or("all"),"matched_count":result["matched_count"],"online_count":result["online_count"],"peers":result["listeners"],"warnings":warnings,"activity":activity,"activity_total":total,"activity_all_total":self.state.query_activity("all","")?.1,"activity_has_any":self.state.query_activity("all","")?.1>0,"activity_unread_count":unread,"activity_filter":self.activity_filter,"activity_query":self.activity_query,"unread_count":self.state.activity()?.1});
                self.emit(frame.clone());
                return Ok(frame);
            }
            "switch_hub" => {
                let from = action["from_hub"].as_str().unwrap_or(&hub);
                let to = action["to_hub"]
                    .as_str()
                    .ok_or("INVALID_INPUT: to_hub required")?;
                self.slot(to)?;
                let target = string(action, "listener_id");
                let row = self
                    .state
                    .active()?
                    .into_iter()
                    .find(|r| {
                        string(r, "hub") == from
                            && (string(r, "listener_id") == target
                                || string(r, "public_id") == target)
                    })
                    .ok_or("LISTENER_NOT_FOUND: Session is not active on source Hub")?;
                if from != to {
                    let destination = self.slot(to)?;
                    let existing = crate::core::registration::local_node(
                        &self.state,
                        destination,
                        string(&row, "harness"),
                        string(&row, "session_id"),
                    )?;
                    if !existing.is_some_and(|value| value["status"] == "ACTIVE") {
                        let scope = if destination.config["authMode"] == "oidc"
                            && self.slot(from)?.config["authMode"] != "oidc"
                        {
                            json!("self")
                        } else {
                            row["scope"].clone()
                        };
                        let mut request = json!({"about":row["about"],"scope":scope});
                        if row["tag"].is_string() {
                            request["tag"] = row["tag"].clone();
                        }
                        self.listen(to, &request, &row)?;
                    }
                    self.unlisten_id(from, string(&row, "listener_id"))?;
                }
            }
            "set_host_hub" => {
                let mut server = self.host_server.take();
                let previous = server
                    .as_ref()
                    .map(|server| (server.address.ip(), server.port(), server.auth.clone()));
                let mut applied = false;
                let result = self.mutate_config(|v| {
                    let mut host = v
                        .get("host_hub")
                        .or_else(|| v.get("hostHub"))
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    if host.is_null() {
                        host = json!({});
                    }
                    if !host.is_object() {
                        return Err("INVALID_INPUT: Host hub must be an object".into());
                    }
                    for key in ["enabled", "port", "token", "auth_mode", "bind"] {
                        if let Some(value) = action.get(key) {
                            host[key] = value.clone();
                        }
                    }
                    if (host["enabled"] == true || host_hub_auth_mode(&host) == "token")
                        && host
                            .get("token")
                            .is_none_or(|token| token.as_str() == Some(""))
                    {
                        host["token"] = json!(crate::plugins::try_random_hex(32)?);
                    }
                    let settings = host_hub_server_config(&host)?;
                    if !self.closing {
                        replace_host_hub_server(&self.local, &mut server, settings)?;
                        applied = true;
                    }
                    v["host_hub"] = host;
                    if let Some(m) = v.as_object_mut() {
                        m.remove("hostHub");
                    }
                    Ok(())
                });
                // A failed config write must also undo a successful listener change.
                let rollback = if result.is_err() && applied {
                    replace_host_hub_server(&self.local, &mut server, previous)
                } else {
                    Ok(())
                };
                self.host_server = server;
                self.host_hub_reload_failed = false;
                if let Err(error) = result {
                    self.last_host_hub_error = Some(error.clone());
                    self.emit(json!({"op":"settings","request_id":action["request_id"],"settings":self.settings()}));
                    rollback.map_err(|rollback| {
                        format!("{error}; failed to restore host hub listener: {rollback}")
                    })?;
                    return Err(error);
                }
                self.applied_host_config =
                    self.raw_config().ok().map(|raw| raw["host_hub"].clone());
                self.last_host_hub_error = None;
                self.emit(json!({"op":"settings","request_id":action["request_id"],"settings":self.settings()}));
            }
            "set_launch_at_login" => {
                set_launch_at_login(
                    action["enabled"].as_bool().unwrap_or(false),
                    &self.config.state_dir,
                    &self.config.home_dir,
                    &self.plugins,
                )?;
            }
            "setup_disconnect_all" => {
                for row in self
                    .state
                    .active()?
                    .into_iter()
                    .filter(|row| action["purge_data"] == true || row["harness"] != "human")
                {
                    self.retire_id(
                        string(&row, "hub"),
                        string(&row, "listener_id"),
                        "disconnected",
                    )?;
                }
                for p in self
                    .plugins
                    .agent_plugins()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
                {
                    remove_plugin_setup(&p)?;
                    self.invalidate_setup(&p.id);
                    self.last_app_action
                        .insert(p.id.clone(), "disconnect".into());
                }
                self.caller_cache
                    .lock()
                    .map_err(|_| "Caller cache lock poisoned")?
                    .clear();
                if action["purge_data"] == true {
                    set_launch_at_login(
                        false,
                        &self.config.state_dir,
                        &self.config.home_dir,
                        &self.plugins,
                    )?;
                    let principal =
                        crate::core::auth::local_principal(&self.config.effective_user("local")?);
                    let inventory =
                        self.local
                            .rpc("human_channels_list", &json!({}), Some(&principal))?;
                    for channel in inventory["channels"].as_array().into_iter().flatten() {
                        self.local.rpc(
                            "human_channels_configure",
                            &json!({"plugin_id":channel["id"],"reset_hub":true,"reset_user":true}),
                            Some(&principal),
                        )?;
                    }
                    self.mutate_config(|v| {
                        v["default_hub"] = json!("local");
                        if let Some(object) = v.as_object_mut() {
                            object.remove("defaultHub");
                        }
                        v["hubs"] = json!({});
                        v["host_hub"] = json!({"enabled":false});
                        if let Some(object) = v.as_object_mut() {
                            object.remove("hostHub");
                        }
                        Ok(())
                    })?;
                    let logins = self
                        .logins
                        .drain()
                        .map(|(_, attempt)| attempt)
                        .collect::<Vec<_>>();
                    for attempt in &logins {
                        attempt.cancelled.store(true, Ordering::SeqCst);
                    }
                    for attempt in &logins {
                        drop(attempt.serial.lock().map_err(|_| "Login lock poisoned")?);
                    }
                    for slot in self.slots.values() {
                        slot.close();
                    }
                    for (_, worker) in self.receivers.drain() {
                        let _ = worker.join();
                    }
                    for worker in self.events.drain(..) {
                        let _ = worker.join();
                    }
                    self.slots.clear();
                    self.channel_inventories.clear();
                    self.active_settings_hub = None;
                    if let Some(server) = self.host_server.take() {
                        server.shutdown();
                    }
                    if let Some(mut activity) = self.session_activity.take() {
                        activity.shutdown()?;
                    }
                    for pending in self
                        .oidc_recoveries
                        .values()
                        .chain(self.retired_auth.iter())
                    {
                        pending.cancelled.store(true, Ordering::SeqCst);
                    }
                    for pending in self
                        .oidc_recoveries
                        .drain()
                        .map(|(_, p)| p)
                        .chain(self.retired_auth.drain(..))
                    {
                        let _ = pending.worker.join();
                    }
                    for entry in fs::read_dir(&self.config.state_dir).map_err(|e| e.to_string())? {
                        let entry = entry.map_err(|e| e.to_string())?;
                        if entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with("oidc-token-")
                        {
                            fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
                        }
                    }
                    self.local.reset_data()?;
                    match fs::remove_dir_all(self.config.state_dir.join("activity")) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => {
                            return Err(
                                "STATE_ERROR: Cannot remove session activity metadata".into()
                            )
                        }
                    }
                    self.state.db.transaction(|db| {
                        db.execute("DELETE FROM local_sent_idempotency; DELETE FROM pending_hub_acks; DELETE FROM pending_hub_detaches; DELETE FROM wake_retries;", &[])?;
                        db.execute("DELETE FROM local_signals", &[])?;
                        db.execute("DELETE FROM local_listeners", &[])?;
                        db.execute("DELETE FROM session_custom_titles", &[])?;
                        Ok(())
                    })?;
                    self.session_activity =
                        Some(crate::core::session_activity::SessionActivity::new(
                            &self.config,
                            &self.plugins,
                        ));
                }
                if let Some(activity) = self.session_activity.as_mut() {
                    for plugin in self.plugins.agent_plugins() {
                        activity.remove_candidates(&plugin.id);
                    }
                }
            }
            "setup_uninstall_all" => {
                for row in self.state.active()? {
                    let _ = self.retire_id(
                        string(&row, "hub"),
                        string(&row, "listener_id"),
                        "disconnected",
                    );
                }
                self.uninstall = Some(action["purge_data"].as_bool().unwrap_or(false));
                request_shutdown();
            }
            "quit" => request_shutdown(),
            "set_auto_listen" => {
                if !self.slots.contains_key(&hub) {
                    return Err("INVALID_INPUT: Enabled hub required".into());
                }
                let enabled = action["enabled"]
                    .as_bool()
                    .ok_or("INVALID_INPUT: enabled must be a boolean")?;
                self.set_auto_listen(&hub, enabled)?;
                if enabled {
                    self.auto_listen()?;
                }
            }
            "refresh_available" => {
                self.refresh_available()?;
                self.poll()?;
            }
            "close_settings" => {
                self.active_settings_hub = None;
            }
            "close_directory" => {
                self.directory_open = false;
            }
            _ => return Err(format!("INVALID_INPUT: Unknown UI action: {op}")),
        }
        self.reload_slots()?;
        // Setup receipts require the refreshed inspection, which dispatch_action
        // awaits without holding service. Passive cached projections remain usable.
        if !matches!(
            op,
            "setup_apply"
                | "setup_remove_app"
                | "setup_disconnect_all"
                | "close_menu"
                | "close_settings"
                | "close_directory"
        ) {
            self.push_ui_result(Some(action));
        }
        Ok(json!({"ok":true}))
    }
}
