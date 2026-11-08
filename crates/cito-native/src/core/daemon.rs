use super::{
    hub::HubEngine,
    identity::{hash, iso, listener_id, local_host_name, normalize_slug, sanitize_message, string},
    receiver::{detach_channel_from_other_sessions, plugin_request, poll_listener},
    state::LocalState,
    transport::HubClient,
};
use crate::plugins::{now_ms, random_uuid_v4, LoadedPlugins, NativeConfig, CITO_VERSION};
use crate::setup::{
    apply_plugin_setup, ensure_cli_shims_native, inspect_launch_at_login, inspect_plugin_setup,
    remove_plugin_setup, resolve_package_root, set_launch_at_login, uninstall_setup_native,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const FRAME_LIMIT: usize = 262_144;
fn wire_status(raw: &str) -> &str {
    match raw {
        "reconnecting" | "connecting" => "connecting",
        "auth_required" | "login_required" => "auth_required",
        "connected" => "connected",
        _ => "disconnected",
    }
}
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);
extern "C" fn handle_daemon_sig(_: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
    let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: write is async-signal-safe; the nonblocking self-pipe stays open
        // until all handlers are restored at shutdown. No locks or allocations occur.
        unsafe {
            libc::write(fd, b"x".as_ptr().cast(), 1);
        }
    }
}
fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}
pub(crate) fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

fn cap_text(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.into();
    }
    let mut end = limit.saturating_sub(3);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
fn bound_fields(v: &mut Value, key: &str) {
    match v {
        Value::String(s) => {
            let cap = match key {
                "message" => 512,
                "about" | "from_about" => 256,
                "title" | "cwd" | "summary" | "from_title" => 128,
                _ => 4096,
            };
            *s = cap_text(s, cap);
        }
        Value::Array(a) => {
            let cap = match key {
                "activity" => 80,
                "peers" => 100,
                "sessions" | "listeners" | "my_remote_listeners" => 64,
                "available" => 32,
                _ => 128,
            };
            a.truncate(cap);
            for item in a {
                bound_fields(item, "");
            }
        }
        Value::Object(m) => {
            for (k, v) in m {
                bound_fields(v, k)
            }
        }
        _ => {}
    }
}
pub fn bounded_frame(mut frame: Value) -> Result<Vec<u8>, String> {
    bound_fields(&mut frame, "");
    loop {
        let bytes = serde_json::to_vec(&frame).map_err(|e| e.to_string())?;
        if bytes.len() + 1 < FRAME_LIMIT {
            return Ok(bytes);
        }
        let mut removed = false;
        for key in [
            "activity",
            "peers",
            "my_remote_listeners",
            "available",
            "sessions",
            "listeners",
            "harnesses",
        ] {
            if let Some(list) = frame.get_mut(key).and_then(Value::as_array_mut) {
                if list.pop().is_some() {
                    removed = true;
                    break;
                }
            }
        }
        if !removed {
            if let Some(settings) = frame.get_mut("settings").and_then(Value::as_object_mut) {
                for key in ["apps", "channels", "hubs"] {
                    if let Some(list) = settings.get_mut(key).and_then(Value::as_array_mut) {
                        if list.pop().is_some() {
                            removed = true;
                            break;
                        }
                    }
                }
            }
        }
        if !removed {
            return Err("UI frame exceeds 256 KiB after collection shedding".into());
        }
        frame["truncated"] = json!(true);
    }
}

#[derive(Default)]
struct UiMailbox {
    pending: Mutex<std::collections::VecDeque<(Option<String>, Vec<u8>)>>,
    ready: std::sync::Condvar,
    closed: AtomicBool,
}
impl UiMailbox {
    fn emit(&self, frame: Value) {
        // Snapshots coalesce; command results and banners stay ordered.
        let key = if matches!(string(&frame, "op"), "state" | "settings" | "directory") {
            Some(format!(
                "{}:{}",
                string(&frame, "op"),
                string(&frame, "hub")
            ))
        } else {
            None
        };
        if let Ok(mut bytes) = bounded_frame(frame) {
            bytes.push(b'\n');
            self.push(key, bytes);
        }
    }
    fn push(&self, key: Option<String>, bytes: Vec<u8>) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(mut pending) = self.pending.lock() {
            if let Some(key) = &key {
                if let Some((_, frame)) = pending
                    .iter_mut()
                    .find(|(old, _)| old.as_ref() == Some(key))
                {
                    *frame = bytes;
                    self.ready.notify_one();
                    return;
                }
            }
            pending.push_back((key, bytes));
            self.ready.notify_one();
        }
    }
    fn next(&self) -> Option<Vec<u8>> {
        let mut pending = self.pending.lock().ok()?;
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }
            if let Some((_, bytes)) = pending.pop_front() {
                return Some(bytes);
            }
            pending = self.ready.wait(pending).ok()?;
        }
    }
    fn close(&self) {
        // The predicate and wait share this mutex so shutdown cannot lose its wake.
        let _pending = self.pending.lock();
        self.closed.store(true, Ordering::SeqCst);
        self.ready.notify_all();
    }
}
#[derive(Default)]
struct InteractiveLogin {
    cancelled: AtomicBool,
    serial: Mutex<()>,
}

pub struct DaemonService {
    pub config: NativeConfig,
    pub plugins: LoadedPlugins,
    pub state: LocalState,
    pub local: Arc<HubEngine>,
    pub slots: HashMap<String, HubClient>,
    events: Vec<thread::JoinHandle<()>>,
    receivers: HashMap<String, thread::JoinHandle<()>>,
    caller_cache: Mutex<HashMap<u64, Value>>,
    probes: HashMap<String, (Instant, Option<thread::JoinHandle<()>>)>,
    session_activity: Option<super::session_activity::SessionActivity>,
    ui: Option<Arc<UiMailbox>>,
    logins: HashMap<String, Arc<InteractiveLogin>>,
    last_ui: Instant,
    closing: bool,
    host_id: String,
    host_server: Option<super::hub::HubServer>,
    uninstall: Option<bool>,
}
impl DaemonService {
    pub fn open(config: &NativeConfig, plugins: &LoadedPlugins) -> Result<Self, String> {
        let state = LocalState::open(&config.state_dir)?;
        // Daemon construction runs only after acquiring the exclusive owner lock.
        state.recover_dispatch_leases()?;
        let local = Arc::new(HubEngine::open(&config.state_dir)?);
        let host_path = config.state_dir.join("host-id");
        let host_id = match fs::read_to_string(&host_path) {
            Ok(s) => s.trim().to_string(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let s = random_uuid_v4();
                fs::write(&host_path, &s).map_err(|e| e.to_string())?;
                s
            }
            Err(e) => return Err(e.to_string()),
        };
        if !crate::is_valid_transcript_uuid(&host_id) {
            return Err("Invalid host-id UUID".into());
        }
        fs::set_permissions(&host_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        let mut service = Self {
            config: config.clone(),
            plugins: plugins.clone(),
            state,
            local,
            slots: HashMap::new(),
            events: Vec::new(),
            receivers: HashMap::new(),
            caller_cache: Mutex::new(HashMap::new()),
            probes: HashMap::new(),
            session_activity: Some(super::session_activity::SessionActivity::new(
                config, plugins,
            )),
            ui: None,
            logins: HashMap::new(),
            last_ui: Instant::now() - Duration::from_secs(1),
            closing: false,
            host_id,
            host_server: None,
            uninstall: None,
        };
        service.reload_slots()?;
        Ok(service)
    }
    fn raw_config(&self) -> Result<Value, String> {
        match fs::read_to_string(&self.config.config_path) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("Invalid config.json: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(e) => Err(e.to_string()),
        }
    }
    pub fn get_effective_user(&self) -> Result<String, String> {
        let raw = self.raw_config()?;
        let user = std::env::var("CITO_USER")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| raw["user"].as_str().map(str::to_string))
            .unwrap_or_else(|| self.config.user.clone());
        if user.is_empty()
            || user.len() > 64
            || !user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err("INVALID_INPUT: Invalid user".into());
        }
        Ok(user.to_ascii_lowercase())
    }
    pub fn reload_slots(&mut self) -> Result<(), String> {
        if self.closing {
            return Ok(());
        }
        let raw = self.raw_config()?;
        self.config.user = self.get_effective_user()?;
        let (default, hubs, _) =
            crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))?;
        self.config.default_hub = std::env::var("CITO_HUB")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(default);
        let mut keep = Vec::new();
        for (name, mut hub) in hubs {
            if hub["enabled"] == false || hub["disabled"] == true {
                continue;
            }
            if self.logins.contains_key(&name)
                && self.slots.get(&name).is_some_and(|old| old.config == hub)
            {
                keep.push(name);
                continue;
            }
            let auth = hub
                .get("authMode")
                .or_else(|| hub.get("auth"))
                .and_then(Value::as_str)
                .unwrap_or("none")
                .to_ascii_lowercase();
            if !matches!(auth.as_str(), "none" | "token" | "oidc") {
                return Err("INVALID_INPUT: authMode must be none, token or oidc".into());
            }
            hub["authMode"] = json!(auth);
            if let Some(o) = hub.as_object_mut() {
                o.remove("auth");
            }
            if hub["url"].is_null() {
                hub["url"] = json!("local://");
            }
            let user = self.config.effective_user(&name)?.to_ascii_lowercase();
            keep.push(name.clone());
            let rejected_hash = self
                .slots
                .get(&name)
                .filter(|old| old.config == hub)
                .and_then(|old| {
                    old.rejected_bearer
                        .lock()
                        .ok()
                        .and_then(|v| v.clone())
                        .or_else(|| old.auth_rejected_hash.clone())
                        .or_else(|| {
                            old.status
                                .lock()
                                .ok()
                                .filter(|status| status.as_str() == "auth_required")
                                .and_then(|_| old.bearer.as_deref().map(hash))
                        })
                });
            if self.slots.get(&name).is_some_and(|old| {
                old.config == hub
                    && (auth == "oidc" || string(&old.identity, "owner_user") == user)
                    && (auth != "oidc"
                        || old.auth_retry_at > now_ms()
                        || (rejected_hash.is_none() && old.expires_at > now_ms() / 1000 + 30))
            }) {
                continue;
            }
            let previous = self.slots.remove(&name);
            let registration = previous.as_ref().map(|old| old.registration.clone());
            let previous_principal = previous
                .as_ref()
                .map(|old| string(&old.identity, "principal_id").to_string());
            if let Some(old) = previous {
                let changed = old.config["url"] != hub["url"]
                    || (auth != "oidc"
                        && !super::auth::principal_ids_equal(
                            string(&old.identity, "principal_id"),
                            &format!("none#{user}"),
                        ));
                if changed {
                    for row in self
                        .state
                        .active()?
                        .into_iter()
                        .filter(|r| r["hub"] == name)
                    {
                        let channel = format!("{}:{}", self.host_id, string(&row, "harness"));
                        let _ = detach_channel_from_other_sessions(
                            &old,
                            string(&row, "listener_id"),
                            &channel,
                        );
                    }
                }
                old.closed.store(true, Ordering::SeqCst);
                if changed {
                    for row in self
                        .state
                        .active()?
                        .into_iter()
                        .filter(|row| row["hub"] == name)
                    {
                        self.state
                            .fence_listener_for(&row, "fenced_before_dispatch")?;
                    }
                }
            }
            let mut client = HubClient::new(
                name.clone(),
                hub.clone(),
                &user,
                if hub["url"] == "local://" {
                    Some(self.local.clone())
                } else {
                    None
                },
            );
            if let Some(registration) = registration {
                client.registration = registration;
            }
            if auth == "oidc" {
                match super::login::recover(
                    &self.config.state_dir,
                    &name,
                    &hub,
                    rejected_hash.as_deref(),
                    false,
                ) {
                    Ok((token, identity)) => {
                        client.expires_at = token
                            .split('.')
                            .nth(1)
                            .and_then(|p| {
                                base64::engine::general_purpose::URL_SAFE_NO_PAD
                                    .decode(p)
                                    .ok()
                            })
                            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                            .and_then(|v| v["exp"].as_u64())
                            .unwrap_or(0);
                        client.bearer = Some(token);
                        client.identity = identity;
                    }
                    Err(_) => {
                        client.bearer = None;
                        client.expires_at = 0;
                        client.auth_retry_at = now_ms() + 5000;
                        client.auth_rejected_hash = rejected_hash;
                        if let Some(identity) =
                            super::login::offline_identity(&self.config.state_dir, &name, &hub)
                        {
                            client.identity = identity;
                        }
                        if let Ok(mut status) = client.status.lock() {
                            *status = "auth_required".into();
                        }
                    }
                }
            }
            if auth == "oidc"
                && client.bearer.is_some()
                && previous_principal.as_deref().is_some_and(|previous| {
                    !super::auth::principal_ids_equal(
                        previous,
                        string(&client.identity, "principal_id"),
                    )
                })
            {
                for row in self.state.active()?.into_iter().filter(|row| {
                    row["hub"] == name
                        && !super::auth::principal_ids_equal(
                            string(row, "principal_id"),
                            string(&client.identity, "principal_id"),
                        )
                }) {
                    self.state
                        .fence_listener_for(&row, "fenced_before_dispatch")?;
                }
            }
            if auth != "oidc" || client.bearer.is_some() {
                if let Some(events) = client.start_events() {
                    self.events.push(events);
                }
            }
            self.slots.insert(name.clone(), client);
        }
        let removed: Vec<String> = self
            .slots
            .keys()
            .filter(|k| !keep.contains(k))
            .cloned()
            .collect();
        for name in removed {
            if let Some(slot) = self.slots.remove(&name) {
                slot.closed.store(true, Ordering::SeqCst);
            }
        }
        self.ensure_human_nodes()?;
        Ok(())
    }
    fn ensure_human_nodes(&mut self) -> Result<(), String> {
        for (slot_name, client) in self.slots.clone() {
            if client.config["authMode"] == "oidc" && client.bearer.is_none() {
                continue;
            }
            if super::registration::local_node(&self.state, &client, "human", "default")?
                .is_some_and(|r| matches!(string(&r, "status"), "ACTIVE" | "UNLISTENED"))
            {
                continue;
            }
            let user = string(&client.identity, "owner_user");
            let args = json!({"about":format!("@{user}")});
            let caller = json!({"harness":"human","session_id":"default","cwd":"","title":format!("@{user}")});
            if client.local.is_some() {
                super::registration::register_automatic_human(
                    &self.state,
                    &client,
                    &self.host_id,
                    &args,
                    &caller,
                )?;
                continue;
            }
            let key = format!("auto:{slot_name}");
            if self.receivers.contains_key(&key) {
                continue;
            }
            let state = self.state.clone();
            let host = self.host_id.clone();
            self.receivers.insert(
                key,
                thread::spawn(move || {
                    let _ = super::registration::register_automatic_human(
                        &state, &client, &host, &args, &caller,
                    );
                }),
            );
        }
        Ok(())
    }
    fn slot(&self, name: &str) -> Result<&HubClient, String> {
        self.slots
            .get(name)
            .ok_or_else(|| format!("INVALID_INPUT: Unknown or disabled hub: {name}"))
    }
    fn scoped(&self, hub: &str, id: &str) -> String {
        if self.slots.len() == 1 && matches!(self.config.default_hub.as_str(), "local" | "default")
        {
            id.into()
        } else {
            format!("{hub}:{id}")
        }
    }
    fn caller(&self, meta: &Value, required: bool) -> Result<Option<Value>, String> {
        let requested_pid = caller_pid_hint(meta)?.or_else(|| meta["_http_client"].as_u64());
        let explicit_pid = meta
            .as_object()
            .is_some_and(|m| m.keys().any(|k| k.ends_with("_pid") || k.ends_with("Pid")));
        let expected = string(meta, "expected_harness");
        if expected == "human" {
            if !matches!(string(meta, "expected_session_id"), "" | "default") {
                return Err("INVALID_INPUT: Human node session_id must be 'default'".into());
            }
            return Ok(Some(
                json!({"harness":"human","session_id":"default","cwd":"","title":format!("@{}",self.get_effective_user()?)}),
            ));
        }
        let mut failures = Vec::new();
        for plugin in self.plugins.agent_plugins() {
            if !expected.is_empty() && plugin.id != expected {
                continue;
            }
            let mut request = json!({"op":"resolve_caller","meta":meta,"headers":meta["headers"],"cwd":meta["cwd"],"http_transport":true,"http_client_pid":requested_pid,"require_open":required,"explicit_session_id":meta["expected_session_id"],"cli_cmd":meta["cli_cmd"],"state_dir":self.config.state_dir,"env":{}});
            if !required {
                if let Some(cached) = requested_pid.and_then(|pid| {
                    self.caller_cache.lock().ok().and_then(|cache| {
                        cache
                            .get(&pid)
                            .filter(|caller| caller["harness"] == plugin.id)
                            .cloned()
                    })
                }) {
                    request["cached_caller"] = cached;
                }
            }
            match plugin_request(plugin, &request, None, None) {
                Ok(reply) if reply["ok"] == true => {
                    let caller = reply["caller"].clone();
                    if string(&caller, "harness") != plugin.id
                        || string(&caller, "session_id").is_empty()
                    {
                        return Err(
                            "BINDING_REQUIRED: Provider returned an invalid caller identity".into(),
                        );
                    }
                    if !string(meta, "expected_session_id").is_empty()
                        && caller["session_id"] != meta["expected_session_id"]
                    {
                        return Err("BINDING_REQUIRED: Caller session mismatch".into());
                    }
                    if let Some(pid) = caller["endpoint_pid"]
                        .as_u64()
                        .filter(|pid| *pid > 0 && *pid <= i32::MAX as u64)
                    {
                        if let Ok(mut cache) = self.caller_cache.lock() {
                            cache.retain(|pid, _| {
                                // SAFETY: signal zero only observes the process.
                                unsafe {
                                    libc::kill(*pid as i32, 0) == 0
                                        || std::io::Error::last_os_error().raw_os_error()
                                            == Some(libc::EPERM)
                                }
                            });
                            cache.insert(pid, caller.clone());
                            if let Some(requested_pid) = requested_pid {
                                cache.insert(requested_pid, caller.clone());
                            }
                        }
                    }
                    return Ok(Some(caller));
                }
                Ok(reply) if reply["error_code"] == "INVALID_INPUT" => {
                    return Err(format!("INVALID_INPUT: {}", string(&reply, "message")));
                }
                Ok(reply) => {
                    if explicit_pid && reply["error_code"] != "BINDING_REQUIRED" {
                        return Err(format!(
                            "{}: {}",
                            string(&reply, "error_code"),
                            string(&reply, "message")
                        ));
                    }
                    failures.push(reply)
                }
                Err(error) => {
                    failures.push(json!({"error_code":"BINDING_REQUIRED","message":error}))
                }
            }
        }
        if !expected.is_empty() || explicit_pid || !string(meta, "expected_session_id").is_empty() {
            return Err(failures
                .first()
                .map(|reply| {
                    format!(
                        "{}: {}",
                        string(reply, "error_code"),
                        string(reply, "message")
                    )
                })
                .unwrap_or_else(|| "UNSUPPORTED_HARNESS: Unknown harness".into()));
        }
        if required {
            return Err("BINDING_REQUIRED: No caller socket bridge found".into());
        }
        Ok(None)
    }
    fn listen(&self, hub: &str, args: &Value, caller: &Value) -> Result<Value, String> {
        let previous: Vec<Value> = self
            .state
            .active()?
            .into_iter()
            .filter(|row| {
                row["hub"] == hub
                    && row["harness"] == caller["harness"]
                    && row["session_id"] != caller["session_id"]
                    && caller["endpoint_pid"].is_number()
                    && row["endpoint_pid"] == caller["endpoint_pid"]
            })
            .collect();
        let mut result = super::registration::register_listener(
            &self.state,
            self.slot(hub)?,
            &self.host_id,
            args,
            caller,
        )?;
        // The new remote/local binding commits before the previous endpoint is revoked.
        for row in previous {
            self.unlisten_id(hub, string(&row, "listener_id"))?;
        }
        self.state.db.execute(
            "UPDATE local_listeners SET endpoint_pid=? WHERE harness=? AND session_id=?",
            &[
                caller["endpoint_pid"].clone(),
                caller["harness"].clone(),
                caller["session_id"].clone(),
            ],
        )?;
        result["listener_id"] = json!(self.scoped(hub, string(&result, "listener_id")));
        Ok(result)
    }
    fn unlisten_id(&self, hub: &str, id: &str) -> Result<bool, String> {
        let client = self.slots.get(hub);
        let _registration = client
            .map(|client| {
                client
                    .registration
                    .lock()
                    .map_err(|_| "STATE_ERROR: Registration lock poisoned")
            })
            .transpose()?;
        if let Some(row) = self.state.listener(hub, id)? {
            let active = row["status"] == "ACTIVE";
            self.state.db.execute("UPDATE local_signals SET state='STORED_PASSIVE',held_reason='unlistened',dispatcher_token=NULL WHERE hub=? AND listener_id=? AND acknowledged_at_ms IS NULL AND state IN ('RECEIVED','DISPATCHING')", &[json!(hub),json!(id)])?;
            self.state.db.execute(
                "UPDATE local_listeners SET status='UNLISTENED' WHERE hub=? AND listener_id=?",
                &[json!(hub), json!(id)],
            )?;
            if let Some(client) = client {
                let channel = format!("{}:{}", self.host_id, string(&row, "harness"));
                // Local revocation is durable even while the Hub is unavailable.
                let _ = detach_channel_from_other_sessions(client, id, &channel);
            }
            return Ok(active);
        }
        if let Some(client) = client {
            if id == listener_id(string(&client.identity, "principal_id"), "human", "default") {
                // Persist intent before a queued background enrollment can obtain the lock.
                self.state.db.execute("INSERT OR IGNORE INTO local_listeners(hub,listener_id,public_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status) VALUES(?,?,?,?,?,?,?,'human','default','','','','self','UNLISTENED')",&[
                    json!(hub),json!(id),json!(super::identity::public_listener_id(string(&client.identity,"owner_user"),id)),
                    client.identity["principal_id"].clone(),client.identity["owner_user"].clone(),json!(self.host_id),json!(local_host_name())])?;
            }
        }
        Ok(false)
    }
    pub fn tool(&mut self, name: &str, args: &Value, meta: &Value) -> Result<Value, String> {
        validate_tool_input(name, args)?;
        if meta
            .get("expected_session_id")
            .is_some_and(|v| v.as_str().is_none_or(str::is_empty))
        {
            return Err("INVALID_INPUT: expected_session_id must be nonempty".into());
        }
        if meta.get("http_transport").is_some() || meta.get("http_client_pid").is_some() {
            return Err("INVALID_INPUT: Reserved caller metadata".into());
        }
        self.reload_slots()?;
        let hub = args["hub"]
            .as_str()
            .unwrap_or(&self.config.default_hub)
            .to_string();
        self.slot(&hub)?;
        match name {
            "cito_listen" => {
                let caller = self
                    .caller(meta, true)?
                    .ok_or("BINDING_REQUIRED: Caller required")?;
                self.listen(&hub, args, &caller)
            }
            "cito_unlisten" => {
                let caller = self
                    .caller(meta, false)?
                    .ok_or("BINDING_REQUIRED: Caller required")?;
                let hubs: Vec<String> = if args["hub"].is_string() {
                    vec![hub]
                } else {
                    self.slots.keys().cloned().collect()
                };
                let mut removed = 0;
                for hub in hubs {
                    let slot = self.slot(&hub)?;
                    let id = super::registration::local_node_id(
                        &self.state,
                        slot,
                        string(&caller, "harness"),
                        string(&caller, "session_id"),
                    )?;
                    if self.unlisten_id(&hub, &id)? {
                        removed += 1;
                    }
                }
                Ok(
                    json!({"ok":true,"status":if removed>0{"unlistened"}else{"not_listening"},"removed":removed}),
                )
            }
            "cito_find" => {
                let _ = self.caller(meta, false)?;
                let limit = args["limit"].as_u64().unwrap_or(20);
                if !(1..=50).contains(&limit) {
                    return Err("INVALID_INPUT: limit must be 1..50".into());
                }
                let query = string(args, "query").to_ascii_lowercase();
                let mut rows = Vec::new();
                let mut warnings = Vec::new();
                let mut retried = false;
                let snapshots = loop {
                    let snapshots = thread::scope(|scope| {
                        let workers: Vec<_> = self
                            .slots
                            .iter()
                            .filter(|(name, _)| !args["hub"].is_string() || *name == &hub)
                            .map(|(name, slot)| {
                                let worker = scope.spawn(move || slot.directory());
                                (name.clone(), slot.clone(), worker)
                            })
                            .collect();
                        workers
                            .into_iter()
                            .map(|(name, slot, worker)| {
                                (
                                    name,
                                    slot,
                                    worker.join().unwrap_or_else(|_| {
                                        Err("NATS_UNAVAILABLE: Directory worker failed".into())
                                    }),
                                )
                            })
                            .collect::<Vec<_>>()
                    });
                    if !retried
                        && snapshots.iter().any(|(_, slot, result)| {
                            slot.config["authMode"] == "oidc"
                                && result.as_ref().is_err_and(|e| {
                                    e.starts_with("UNAUTHORIZED:")
                                        || e.starts_with("AUTHORIZATION_VIOLATION:")
                                })
                        })
                    {
                        retried = true;
                        self.reload_slots()?;
                        continue;
                    }
                    break snapshots;
                };
                for (name, slot, snapshot) in snapshots {
                    match snapshot {
                        Ok(directory) => {
                            for row in directory {
                                if super::lease::evaluate_node_lease(&row, now_ms()) != "ACTIVE"
                                    || (row["scope"] == "self"
                                        && row["principal_id"] != slot.identity["principal_id"])
                                {
                                    continue;
                                }
                                if ["user", "harness", "repo", "host"].iter().any(|key| {
                                    let filter = string(args, key);
                                    let field = match *key {
                                        "user" => "owner_user",
                                        "host" => "host_name",
                                        other => other,
                                    };
                                    !filter.is_empty()
                                        && !string(&row, field)
                                            .to_ascii_lowercase()
                                            .contains(&filter.to_ascii_lowercase())
                                }) {
                                    continue;
                                }
                                let haystack = format!(
                                    "{} {} {} {} {}",
                                    string(&row, "title"),
                                    string(&row, "about"),
                                    string(&row, "repo"),
                                    string(&row, "cwd"),
                                    string(&row, "public_id")
                                )
                                .to_ascii_lowercase();
                                if !query.split_whitespace().all(|w| {
                                    if let Some((field, token)) = w.split_once(':') {
                                        let field = match field {
                                            "user" => "owner_user",
                                            "host" => "host_name",
                                            other => other,
                                        };
                                        return string(&row, field)
                                            .to_ascii_lowercase()
                                            .contains(token);
                                    }
                                    haystack.contains(w)
                                }) {
                                    continue;
                                }
                                rows.push(json!({"hub":name,"listener_id":self.scoped(&name,string(&row,"public_id")),"public_id":row["public_id"],"scoped_id":self.scoped(&name,string(&row,"public_id")),"user":row["owner_user"],"host_id":row["host_id"],"host_name":row["host_name"],"harness":row["harness"],"about":row["about"],"title":row["title"],"cwd":row["cwd"],"repo":row["repo"],"branch":row["branch"],"scope":row["scope"],"last_seen_at":iso(now_ms()),"is_self_principal":row["principal_id"]==slot.identity["principal_id"]}));
                            }
                        }
                        Err(e) => warnings.push(json!({"hub":name,"message":e})),
                    }
                }
                rows.sort_by_key(|r| format!("{}:{}", string(r, "hub"), string(r, "listener_id")));
                rows.truncate(limit as usize);
                Ok(json!({"ok":true,"listeners":rows,"warnings":warnings}))
            }
            "cito_send" => match self.send(&hub, args, meta) {
                Err(e)
                    if e.starts_with("UNAUTHORIZED:")
                        || e.starts_with("AUTHORIZATION_VIOLATION:") =>
                {
                    self.reload_slots()?;
                    self.send(&hub, args, meta)
                }
                result => result,
            },
            "cito_inbox" => self.inbox(&hub, args, meta),
            _ => Err(format!("INVALID_INPUT: Unknown tool: {name}")),
        }
    }
    fn send(&self, default: &str, args: &Value, meta: &Value) -> Result<Value, String> {
        let message = sanitize_message(
            args["message"]
                .as_str()
                .ok_or("INVALID_INPUT: message is required")?,
        )?;
        let raw = string(args, "to");
        if raw.is_empty() {
            return Err("INVALID_INPUT: to is required".into());
        }
        let (hub, to) = raw.split_once(':').unwrap_or((default, raw));
        if args["hub"].is_string() && hub != default {
            return Err("INVALID_INPUT: Conflicting hub prefixes".into());
        }
        let slot = self.slot(hub)?;
        let caller = self.caller(meta, false)?;
        let mut directory = slot.directory()?;
        let canonical_target = if to.starts_with('@') {
            None
        } else {
            Some(super::identity::canonical_node_id(to)?)
        };
        let reply_to = if let Some(raw) = args["in_reply_to"].as_str() {
            if let Some((reply_hub, id)) = raw.split_once(':') {
                if reply_hub != hub {
                    return Err("INVALID_INPUT: Reply belongs to another Hub".into());
                }
                json!(id)
            } else {
                json!(raw)
            }
        } else {
            Value::Null
        };
        let handle = to.strip_prefix('@');
        let own_handle = handle.is_some_and(|handle| {
            handle == "self" || handle.eq_ignore_ascii_case(string(&slot.identity, "owner_user"))
        });
        if own_handle
            && !directory.iter().any(|row| {
                row["harness"] == "human"
                    && super::auth::principal_ids_equal(
                        string(row, "principal_id"),
                        string(&slot.identity, "principal_id"),
                    )
            })
        {
            let disabled = super::registration::local_node(&self.state, slot, "human", "default")?
                .is_some_and(|row| row["status"] == "UNLISTENED");
            if !disabled {
                let owner = string(&slot.identity, "owner_user");
                super::registration::register_automatic_human(
                    &self.state,
                    slot,
                    &self.host_id,
                    &json!({"about":format!("@{owner}")}),
                    &json!({"harness":"human","session_id":"default","cwd":"","title":format!("@{owner}")}),
                )?;
                directory = slot.directory()?;
            }
        }
        let mut matches: Vec<Value> = if let Some(handle) = handle {
            let user = if own_handle {
                string(&slot.identity, "owner_user")
            } else {
                handle
            };
            directory
                .into_iter()
                .filter(|row| {
                    row["harness"] == "human"
                        && if own_handle {
                            super::auth::principal_ids_equal(
                                string(row, "principal_id"),
                                string(&slot.identity, "principal_id"),
                            )
                        } else {
                            string(row, "owner_user").eq_ignore_ascii_case(user)
                                || normalize_slug(string(row, "owner_user")) == normalize_slug(user)
                        }
                })
                .collect()
        } else {
            directory
                .into_iter()
                .filter(|row| Some(string(row, "listener_id")) == canonical_target.as_deref())
                .collect()
        };
        if matches.is_empty() {
            if let Some(id) = canonical_target.as_deref() {
                match slot.kv_get(id) {
                    Ok(Some((row, _))) => matches.push(row),
                    Ok(None) => {}
                    Err(error) if error.starts_with("FORBIDDEN:") => {
                        return Err("FORBIDDEN_TARGET_SCOPE: Target is private".into())
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        if matches.len() > 1 {
            let principal = string(&matches[0], "principal_id");
            if matches.iter().any(|row| {
                !super::auth::principal_ids_equal(principal, string(row, "principal_id"))
            }) {
                return Err("INVALID_INPUT: Ambiguous target; use full listener_id".into());
            }
            let preferred =
                super::registration::local_node_id(&self.state, slot, "human", "default")?;
            matches.sort_by_key(|row| row["listener_id"] != preferred);
        }
        let target = matches
            .first()
            .ok_or("LISTENER_NOT_FOUND: Target listener does not exist")?;
        match super::lease::evaluate_node_lease(target, now_ms()) {
            "UNLISTENED" => return Err("LISTENER_NOT_FOUND: Target listener was revoked".into()),
            "EXPIRED" => return Err("LISTENER_EXPIRED: Target listener expired".into()),
            _ => {}
        }
        let own_principal = super::auth::principal_ids_equal(
            string(target, "principal_id"),
            string(&slot.identity, "principal_id"),
        );
        if !own_principal && target["scope"] != "workspace" {
            return Err("FORBIDDEN_TARGET_SCOPE: Target is private".into());
        }
        let canonical = string(target, "listener_id");
        let from_id = caller
            .as_ref()
            .map(|caller| {
                super::registration::local_node_id(
                    &self.state,
                    slot,
                    string(caller, "harness"),
                    string(caller, "session_id"),
                )
            })
            .transpose()?;
        if from_id.as_deref() == Some(canonical)
            || caller.as_ref().is_some_and(|caller| {
                own_principal
                    && caller["harness"] == target["harness"]
                    && caller["session_id"] == target["session_id"]
            })
        {
            return Err("SELF_SEND_FORBIDDEN: Cannot send to your own session".into());
        }
        let endpoint = format!(
            "{}|{}|{}|{}",
            string(&slot.identity, "principal_id"),
            self.host_id,
            caller
                .as_ref()
                .map(|c| string(c, "harness"))
                .unwrap_or("unbound"),
            caller
                .as_ref()
                .map(|c| string(c, "session_id"))
                .unwrap_or("default")
        );
        let key = args["idempotency_key"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(random_uuid_v4);
        let dedup = hash(&format!("{endpoint}|{canonical}|{key}"));
        let request_hash = hash(&format!("{message}|{}", reply_to));
        let now = now_ms();
        let reservation=self.state.db.transaction(|c|{
            c.execute("DELETE FROM local_sent_idempotency WHERE expires_at_ms<=?",&[json!(now)])?;
            if let Some(old)=c.query("SELECT * FROM local_sent_idempotency WHERE hub=? AND dedup_id=?",&[json!(hub),json!(dedup)])?.into_iter().next(){if old["request_hash"]!=request_hash{return Err("IDEMPOTENCY_KEY_CONFLICT: Different payload".into());}return Ok(old);}
            let id=format!("sig_{}",random_uuid_v4().replace('-',""));c.execute("INSERT INTO local_sent_idempotency(hub,dedup_id,request_hash,signal_id,to_listener_id,status,expires_at_iso,expires_at_ms) VALUES(?,?,?,?,?,'PENDING_PUBLISH',?,?)",&[json!(hub),json!(dedup),json!(request_hash),json!(id),json!(canonical),json!(iso(now+1_800_000)),json!(now+1_800_000)])?;
            c.query("SELECT * FROM local_sent_idempotency WHERE hub=? AND dedup_id=?",&[json!(hub),json!(dedup)])?.into_iter().next().ok_or("Sent reservation missing".into())
        })?;
        let signal_id = string(&reservation, "signal_id");
        let mut seq = reservation["seq"].as_u64().unwrap_or(0);
        if reservation["status"] == "PENDING_PUBLISH" {
            let local_sender = match from_id {
                Some(ref id) => self.state.listener(hub, id)?,
                None => None,
            };
            let signal = json!({"signal_id":signal_id,"to":canonical,"target_generation":target["binding_generation"],"sender_endpoint_key":endpoint,"from":{"principal_id":slot.identity["principal_id"],"user":slot.identity["owner_user"],"host_id":self.host_id,"host_name":local_host_name(),"harness":caller.as_ref().map(|c|c["harness"].clone()),"listener_id":local_sender.as_ref().map(|s|s["public_id"].clone()),"title":caller.as_ref().map(|c|string(c,"title")).unwrap_or(""),"about":local_sender.as_ref().map(|s|s["about"].clone())},"message":message,"in_reply_to":reply_to});
            let published=slot.rpc("js_publish",&json!({"subject":format!("cito.signal.{canonical}"),"data_b64":STANDARD.encode(serde_json::to_vec(&signal).map_err(|e|e.to_string())?),"opts":{"msgID":dedup}}))?;
            seq = published["seq"].as_u64().unwrap_or(0);
            self.state.db.execute("UPDATE local_sent_idempotency SET seq=?,status='wake_accepted' WHERE hub=? AND dedup_id=?",&[json!(seq),json!(hub),json!(dedup)])?;
        }
        Ok(
            json!({"ok":true,"signal_id":self.scoped(hub,signal_id),"seq":seq,"to":self.scoped(hub,string(target,"public_id")),"status":"wake_accepted","held_reason":null,"expires_at":reservation["expires_at_iso"]}),
        )
    }
    fn inbox(&self, hub: &str, args: &Value, meta: &Value) -> Result<Value, String> {
        let caller = self
            .caller(meta, false)?
            .ok_or("BINDING_REQUIRED: Caller required")?;
        let mut rows = Vec::new();
        for (name, slot) in &self.slots {
            if args["hub"].is_string() && name != hub {
                continue;
            }
            if slot.config["authMode"] == "oidc" && slot.bearer.is_none() {
                if args["hub"].is_string() {
                    return Err("UNAUTHORIZED: Hub requires authentication".into());
                }
                continue;
            }
            let id = super::registration::local_node_id(
                &self.state,
                slot,
                string(&caller, "harness"),
                string(&caller, "session_id"),
            )?;
            rows.extend(self.state.db.query("SELECT * FROM local_signals WHERE hub=? AND listener_id=? AND acknowledged_at_ms IS NULL ORDER BY seq",&[json!(name),json!(id)])?);
        }
        rows.sort_by_key(|r| {
            (
                string(r, "created_at").to_string(),
                string(r, "hub").to_string(),
                r["seq"].as_u64().unwrap_or(0),
            )
        });
        let page: Vec<Value> = rows
            .iter()
            .take(args["limit"].as_u64().unwrap_or(50) as usize)
            .cloned()
            .collect();
        let ack_all = args["ack_all"]
            .as_bool()
            .unwrap_or(args["ack_ids"].is_null());
        let mut selected: Vec<Value> = if ack_all { page.clone() } else { Vec::new() };
        for raw in args["ack_ids"].as_array().into_iter().flatten() {
            let raw = raw
                .as_str()
                .ok_or("INVALID_INPUT: Ack ID must be a string")?;
            let (scoped_hub, id) = match raw.split_once(':') {
                Some((h, id)) => {
                    if args["hub"].is_string() && h != hub {
                        return Err("INVALID_INPUT: Ack hub outside inbox".into());
                    }
                    (Some(h), id)
                }
                None => (None, raw),
            };
            let matches: Vec<Value> = rows
                .iter()
                .filter(|r| {
                    scoped_hub.is_none_or(|h| string(r, "hub") == h)
                        && (string(r, "signal_id") == id || string(r, "batch_id") == id)
                })
                .cloned()
                .collect();
            if scoped_hub.is_none()
                && matches
                    .iter()
                    .map(|r| string(r, "hub"))
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    > 1
            {
                return Err("INVALID_INPUT: Ambiguous ack ID; use hub prefix".into());
            }
            for row in matches {
                if !selected
                    .iter()
                    .any(|r| r["hub"] == row["hub"] && r["signal_id"] == row["signal_id"])
                {
                    selected.push(row);
                }
            }
        }
        for row in &selected {
            self.state.acknowledge(
                string(row, "hub"),
                string(row, "listener_id"),
                &[row["signal_id"].clone()],
                &format!("{}:{}", self.host_id, string(&caller, "harness")),
            )?;
        }
        let signals:Vec<Value>=page.iter().map(|row|json!({"hub":row["hub"],"signal_id":self.scoped(string(row,"hub"),string(row,"signal_id")),"seq":row["seq"],"from":{"user":row["from_user"],"host_name":row["from_host_name"],"harness":row["from_harness"],"listener_id":row["from_listener_id"],"title":row["from_title"],"about":row["from_about"]},"message":row["message"],"in_reply_to":row["in_reply_to"].as_str().map(|id|self.scoped(string(row,"hub"),id)),"created_at":row["created_at"],"held_reason":row["held_reason"]})).collect();
        self.flush_acks();
        Ok(
            json!({"ok":true,"signals":signals,"acknowledged_count":selected.len(),"replay_gap_detected":false,"replay_gap_range":null}),
        )
    }
    fn flush_acks(&self) {
        if let Ok(rows) = self.state.db.query("SELECT * FROM pending_hub_acks", &[]) {
            for row in rows {
                if let Ok(slot) = self.slot(string(&row, "hub")) {
                    if slot.rpc("signal_ack",&json!({"node_id":row["node_id"],"signal_ids":[row["signal_id"]],"channel_id":row["channel_id"]})).is_ok(){let _=self.state.db.execute("DELETE FROM pending_hub_acks WHERE hub=? AND node_id=? AND signal_id=?",&[row["hub"].clone(),row["node_id"].clone(),row["signal_id"].clone()]);}
                }
            }
        }
    }
    pub fn poll(&mut self) -> Result<(), String> {
        self.reload_slots()?;
        self.flush_acks();
        let finished: Vec<String> = self
            .receivers
            .iter()
            .filter(|(_, t)| t.is_finished())
            .map(|(k, _)| k.clone())
            .collect();
        for key in finished {
            if let Some(thread) = self.receivers.remove(&key) {
                let _ = thread.join();
            }
        }
        for (hub, slot) in &self.slots {
            let key = format!("directory:{hub}");
            if !self.receivers.contains_key(&key)
                && (slot.config["authMode"] != "oidc" || slot.bearer.is_some())
            {
                let slot = slot.clone();
                self.receivers.insert(
                    key,
                    thread::spawn(move || {
                        let _ = slot.directory();
                    }),
                );
            }
        }
        for row in self.state.active()? {
            let hub = string(&row, "hub");
            let probe_key = format!("{hub}:{}", string(&row, "listener_id"));
            let probe = self
                .probes
                .entry(probe_key)
                .or_insert_with(|| (Instant::now() + Duration::from_secs(1), None));
            if probe.1.as_ref().is_some_and(|t| t.is_finished()) {
                if let Some(t) = probe.1.take() {
                    let _ = t.join();
                }
                probe.0 = Instant::now() + Duration::from_millis(500);
            }
            if probe.1.is_none() && probe.0 <= Instant::now() && row["harness"] != "human" {
                if let (Some(plugin), Some(client)) = (
                    self.plugins.find_by_id(string(&row, "harness")).cloned(),
                    self.slots
                        .get(hub)
                        .filter(|s| !s.closed.load(Ordering::SeqCst))
                        .cloned(),
                ) {
                    let state = self.state.clone();
                    let captured = row.clone();
                    let dir = self.config.state_dir.clone();
                    probe.1 = Some(thread::spawn(move || {
                        let _ = super::receiver::probe_listener(
                            &state, &client, &captured, &plugin, &dir,
                        );
                    }));
                }
            }
            let key = format!("{hub}:{}", string(&row, "listener_id"));
            if self.receivers.contains_key(&key) {
                continue;
            }
            let Ok(slot) = self.slot(hub).cloned() else {
                continue;
            };
            if slot.config["authMode"] == "oidc" && slot.bearer.is_none() {
                continue;
            }
            let state = self.state.clone();
            let state_dir = self.config.state_dir.clone();
            let host_id = self.host_id.clone();
            let ui = self.ui.clone().filter(|_| self.desktop_banner_enabled());
            let id_prefix = self.scoped(hub, "");
            let plugin = self.plugins.find_by_id(string(&row, "harness")).cloned();
            self.receivers.insert(key,thread::spawn(move||{
                if let Ok(Some((mut remote,revision)))=slot.kv_get(string(&row,"listener_id")){
                    if remote["binding_generation"]!=row["binding_generation"]||remote["owner_token"]!=row["owner_token"]{let _=state.fence_listener_for(&row,"fenced_before_dispatch");return;}
                    let _=state.db.execute("UPDATE local_listeners SET scope=? WHERE hub=? AND listener_id=? AND binding_generation=? AND owner_token=?", &[remote["scope"].clone(),row["hub"].clone(),row["listener_id"].clone(),row["binding_generation"].clone(),row["owner_token"].clone()]);
                    let channel=format!("{}:{}",host_id,string(&row,"harness"));
                    if let Some(ch)=remote["channels"].as_object_mut().and_then(|m|m.get_mut(&channel)){if now_ms().saturating_sub(ch["updated_at_ms"].as_u64().unwrap_or(0))>=20_000 {ch["updated_at_ms"]=json!(now_ms());let _=slot.kv_save(string(&row,"listener_id"),&remote,Some(revision));}}
                }
                if let Err(error)=poll_listener(&state,&slot,&row,plugin.as_ref(),&state_dir, |signal| {
                    if let Some(ui) = &ui {
                        if !slot.closed.load(Ordering::SeqCst) {
                            ui.emit(json!({"op":"banner","payload":{
                                "target_harness":row["harness"],"target_session_id":row["session_id"],
                                "target_title":row["title"],"target_cwd":row["cwd"],
                                "from_user":signal["from"]["user"],"from_host":signal["from"]["host_name"],
                                "from_title":string(&signal["from"],"title"),"message":signal["message"],
                                "effective_wake":signal["target_generation"]==row["binding_generation"],
                                "signal_id":format!("{id_prefix}{}",string(signal,"signal_id")),"hub":slot.name,
                                "from_scoped_id":signal["from"]["listener_id"].as_str().map(|id|format!("{id_prefix}{id}")),"count":1
                            }}));
                        }
                    }
                }){if !slot.closed.load(Ordering::SeqCst){eprintln!("cito receiver {}: {error}",slot.name);}}
            }));
        }
        let live = self.live_receivers()?;
        let changed = self
            .session_activity
            .as_mut()
            .map(|activity| activity.tick(&self.state, &live))
            .transpose()?
            .unwrap_or(false);
        if changed || self.last_ui.elapsed() >= Duration::from_secs(1) {
            self.push_ui();
            self.last_ui = Instant::now();
        }
        Ok(())
    }
    pub fn settings(&self) -> Value {
        self.settings_for_hub(&self.config.default_hub)
    }
    fn settings_for_hub(&self, channel_hub: &str) -> Value {
        let apps:Vec<Value>=self.plugins.agent_plugins().into_iter().map(|p|{let(installed,configured,paths)=inspect_plugin_setup(p);json!({"id":p.id,"display_name":p.display_name,"tier":p.tier,"installed":installed,"configured":configured,"paths":paths,"status":if configured{"ready"}else{"not_configured"},"detail":if configured{"Connected and ready"}else if installed{"Click Set Up to connect"}else{"Not detected; Set Up to pre-configure"}})}).collect();
        let mut hubs = Vec::new();
        let mut channels = Vec::new();
        for (name, slot) in &self.slots {
            hubs.push(json!({"name":name,"url":slot.config["url"],"auth_mode":slot.config["authMode"],"enabled":true,"status":wire_status(&slot.connection_status()),"user":slot.identity["owner_user"],"is_default":name==&self.config.default_hub,"error":null}));
            if name == channel_hub {
                if let Ok(res) = slot.rpc("human_channels_list", &json!({})) {
                    if let Some(cs) = res["channels"].as_array() {
                        channels = cs.clone();
                    }
                }
            }
        }
        let raw = self.raw_config().unwrap_or_else(|_| json!({}));
        if let Ok((_, configured, _)) =
            crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))
        {
            for (name, cfg) in &configured {
                if !self.slots.contains_key(name) {
                    hubs.push(json!({"name":name,"url":cfg["url"],"auth_mode":cfg.get("authMode").or_else(||cfg.get("auth_mode")).or_else(||cfg.get("auth")).and_then(Value::as_str).unwrap_or("none").to_ascii_lowercase(),"enabled":cfg["enabled"].as_bool().unwrap_or(true),"status":"disconnected","user":cfg["user"],"is_default":name==&self.config.default_hub,"error":null}));
                }
            }
        }
        let host = raw
            .get("host_hub")
            .or_else(|| raw.get("hostHub"))
            .unwrap_or(&Value::Null);
        let port = host["port"].as_u64().unwrap_or(4291);
        let token = string(host, "token");
        json!({"apps":apps,"channel_hub":channel_hub,"channels":channels,"hubs":hubs,"host_hub":{"enabled":host["enabled"].as_bool().unwrap_or(false),"running":self.host_server.is_some(),"port":port,"endpoint_url":format!("http://127.0.0.1:{port}"),"token":token,"invite_command":format!("cito setup --hub lan --url http://127.0.0.1:{port}"),"error":null},"launch_at_login":inspect_launch_at_login(&self.config.home_dir),"desktop_banner_enabled":self.desktop_banner_enabled()})
    }
    fn activity_dto(&self, rows: Vec<Value>) -> Vec<Value> {
        rows.into_iter()
            .map(|mut row| {
                let target = self
                    .state
                    .listener(string(&row, "hub"), string(&row, "listener_id"))
                    .ok()
                    .flatten()
                    .unwrap_or(Value::Null);
                row["unread"] = json!(row["acknowledged_at_ms"].is_null());
                row["target_harness"] = json!(string(&target, "harness"));
                row["target_title"] = json!(string(&target, "title"));
                row["target_scoped_id"] =
                    json!(self.scoped(string(&row, "hub"), string(&target, "public_id")));
                row["from_scoped_id"] = json!(self.scoped(
                    string(&row, "hub"),
                    row["from_listener_id"]
                        .as_str()
                        .unwrap_or(&format!("@{}", string(&row, "from_user")))
                ));
                row["for_me"] = json!(target["harness"] == "human");
                row
            })
            .collect()
    }
    fn live_receivers(&self) -> Result<Vec<Value>, String> {
        Ok(self
            .state
            .active()?
            .into_iter()
            .filter(|row| {
                self.slots
                    .get(string(row, "hub"))
                    .is_some_and(|slot| !slot.closed.load(Ordering::SeqCst))
                    && self.receivers.contains_key(&format!(
                        "{}:{}",
                        string(row, "hub"),
                        string(row, "listener_id")
                    ))
            })
            .collect())
    }
    pub fn snapshot(&self) -> Result<Value, String> {
        let (activity, unread) = self.state.activity()?;
        let mut sessions: Vec<Value> = self
            .state
            .active()?
            .into_iter()
            .filter(|row| self.slots.contains_key(string(row, "hub")))
            .collect();
        for row in &mut sessions {
            row["scoped_id"] = json!(self.scoped(string(row, "hub"), string(row, "public_id")));
        }
        let available = self
            .session_activity
            .as_ref()
            .map(|activity| activity.available(&sessions, self.slots.len()))
            .unwrap_or_default();
        let activity = self.activity_dto(activity);
        let mut remote = Vec::new();
        let mut online = 0;
        for (hub, slot) in &self.slots {
            {
                let directory = slot.cached_directory();
                online += directory.len();
                for row in directory {
                    if super::auth::principal_ids_equal(
                        string(&row, "principal_id"),
                        string(&slot.identity, "principal_id"),
                    ) && !sessions
                        .iter()
                        .any(|s| s["hub"] == *hub && s["listener_id"] == row["listener_id"])
                    {
                        remote.push(self.peer_dto(hub, &row));
                    }
                }
            }
        }
        let harnesses:Vec<Value>=self.plugins.plugins.iter().enumerate().map(|(order,p)|json!({"id":p.id,"display_name":p.display_name,"sf_symbol":p.sf_symbol,"tint":p.tint,"tier":p.tier,"order":order})).collect();
        let settings = self.settings();
        let enabled_hubs: Vec<Value> = settings["hubs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|h| h["enabled"] != false)
            .cloned()
            .collect();
        Ok(
            json!({"op":"state","status":if sessions.is_empty(){"idle"}else{"active"},"hubs":enabled_hubs,"hub":self.config.default_hub,"listeners":sessions,"sessions":sessions,"available":available,"my_remote_listeners":remote,"activity":activity,"unread_count":unread,"total_online":online,"directory_online_count":online,"harnesses":harnesses,"settings":settings}),
        )
    }
    fn peer_dto(&self, hub: &str, row: &Value) -> Value {
        json!({"hub":hub,"listener_id":row["listener_id"],"public_id":row["public_id"],"scoped_id":self.scoped(hub,string(row,"public_id")),"user":row["owner_user"],"host_id":row["host_id"],"host_name":row["host_name"],"harness":row["harness"],"title":row["title"],"about":row["about"],"scope":row["scope"],"last_seen_at":iso(now_ms()),"cwd":row["cwd"],"repo":row["repo"],"branch":row["branch"],"is_self_principal":self.slots.get(hub).is_some_and(|s|s.identity["principal_id"]==row["principal_id"]),"channels":row["channels"].as_object().map(|m|m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default()})
    }
    fn emit(&self, frame: Value) {
        if let Some(ui) = &self.ui {
            ui.emit(frame);
        }
    }
    fn desktop_banner_enabled(&self) -> bool {
        self.raw_config()
            .ok()
            .and_then(|raw| {
                raw.get("desktopBannerEnabled")
                    .or_else(|| raw.get("desktopBanner"))
                    .and_then(Value::as_bool)
            })
            .unwrap_or(true)
    }
    fn push_ui(&self) {
        if self.ui.is_some() {
            if let Ok(frame) = self.snapshot() {
                self.emit(frame);
            }
        }
    }
    fn mutate_config(
        &self,
        action: impl FnOnce(&mut Value) -> Result<(), String>,
    ) -> Result<(), String> {
        if let Some(parent) = self.config.config_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        crate::plugins::with_config_lock(&self.config.config_path, || {
            let mut raw = self.raw_config()?;
            action(&mut raw)?;
            let tmp = self
                .config
                .config_path
                .with_extension(format!("json.{}.tmp", std::process::id()));
            fs::write(
                &tmp,
                serde_json::to_vec_pretty(&raw).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
            fs::rename(tmp, &self.config.config_path).map_err(|e| e.to_string())?;
            Ok(())
        })
    }
    pub fn action(&mut self, action: &Value) -> Result<Value, String> {
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
            if let Some(cancelled) = self.logins.remove(&hub) {
                cancelled.cancelled.store(true, Ordering::SeqCst);
            }
        }
        self.reload_slots()?;
        match op {
            "get_snapshot" => return self.snapshot(),
            "set_user" => {
                let user = string(action, "user");
                if user.is_empty()
                    || user.len() > 64
                    || !user
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
                {
                    return Err("INVALID_INPUT: Invalid user".into());
                }
                self.mutate_config(|v| {
                    v["user"] = json!(user);
                    Ok(())
                })?;
            }
            "setup_apply" | "enable_app" => {
                let id = action["harness"]
                    .as_str()
                    .or_else(|| action["app"].as_str())
                    .ok_or("INVALID_INPUT: harness required")?;
                let p = self
                    .plugins
                    .find_by_id(id)
                    .ok_or("UNSUPPORTED_HARNESS: Unknown harness")?;
                apply_plugin_setup(p, true, false)?;
            }
            "setup_remove_app" | "disable_app" => {
                let id = action["harness"]
                    .as_str()
                    .or_else(|| action["app"].as_str())
                    .ok_or("INVALID_INPUT: harness required")?;
                let p = self
                    .plugins
                    .find_by_id(id)
                    .ok_or("UNSUPPORTED_HARNESS: Unknown harness")?;
                remove_plugin_setup(p)?;
            }
            "setup_save_hub" | "save_hub" => {
                let name = action["name"].as_str().unwrap_or(&hub).to_string();
                if !crate::plugins::validate_hub_name(&name) {
                    return Err("INVALID_INPUT: Invalid hub name".into());
                }
                let mut cfg = action["config"]
                    .as_object()
                    .cloned()
                    .map(Value::Object)
                    .unwrap_or_else(|| action.clone());
                let auth = cfg
                    .get("authMode")
                    .or_else(|| cfg.get("auth_mode"))
                    .or_else(|| cfg.get("auth"))
                    .and_then(Value::as_str)
                    .unwrap_or("none")
                    .to_ascii_lowercase();
                if !matches!(auth.as_str(), "none" | "token" | "oidc") {
                    return Err("INVALID_INPUT: Invalid authMode".into());
                }
                cfg["authMode"] = json!(auth);
                crate::plugins::validate_nats_url(string(&cfg, "url"))?;
                let set_default = cfg["set_default"].as_bool().unwrap_or(false);
                if let Some(m) = cfg.as_object_mut() {
                    for k in ["op", "name", "hub", "auth", "auth_mode", "set_default"] {
                        m.remove(k);
                    }
                }
                self.mutate_config(|v| {
                    if !v["hubs"].is_object() {
                        v["hubs"] = json!({});
                    }
                    v["hubs"][&name] = cfg;
                    if set_default {
                        v["default_hub"] = json!(name);
                        if let Some(m) = v.as_object_mut() {
                            m.remove("defaultHub");
                        }
                    }
                    Ok(())
                })?;
            }
            "setup_toggle_hub" => {
                let enabled = action["enabled"].as_bool().unwrap_or(true);
                self.mutate_config(|v| {
                    v["hubs"][&hub]["enabled"] = json!(enabled);
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
                    Ok(())
                })?;
            }
            "test_hub" => return self.slot(&hub)?.rpc("account_info", &json!({})),
            "paste_hub_invite" => {
                let invite = string(action, "invite");
                let value: Value = serde_json::from_str(invite)
                    .map_err(|_| "INVALID_INPUT: Invalid hub invite")?;
                let mut value = value;
                value["op"] = json!("save_hub");
                return self.action(&value);
            }
            "generate_hub_invite" => {
                return Ok(json!({"ok":true,"invite":self.slot(&hub)?.config}))
            }
            "channel_save"
            | "configure_human_channel"
            | "disconnect_human_channel"
            | "channel_test"
            | "test_human_channel"
            | "channel_pair"
            | "pair_human_channel" => {
                let id = action["channel_id"]
                    .as_str()
                    .or_else(|| action["plugin_id"].as_str())
                    .ok_or("INVALID_INPUT: channel_id required")?;
                let rpc = match op {
                    "channel_pair" | "pair_human_channel" => "human_channel_pair_start",
                    "channel_test" | "test_human_channel" => "human_channels_test",
                    _ => "human_channels_configure",
                };
                let mut params = action.clone();
                params["plugin_id"] = json!(id);
                if let Some(values) = action["values"].as_object() {
                    let plugin = self
                        .plugins
                        .find_by_id(id)
                        .ok_or("INVALID_INPUT: Unknown channel")?;
                    let mut hub_config = Map::new();
                    let mut user_config = Map::new();
                    for field in &plugin.fields {
                        let key = string(field, "key");
                        if let Some(value) = values.get(key) {
                            if field["scope"] == "hub" {
                                hub_config.insert(key.into(), value.clone());
                            } else {
                                user_config.insert(key.into(), value.clone());
                            }
                        }
                    }
                    params["hub_config"] = Value::Object(hub_config);
                    params["user_config"] = Value::Object(user_config);
                }
                if op == "disconnect_human_channel" {
                    params["enabled"] = json!(false);
                }
                let result = self.slot(&hub)?.rpc(rpc, &params)?;
                if rpc == "human_channel_pair_start" {
                    let mut frame = result.clone();
                    frame["op"] = json!("pair_code");
                    frame["channel_id"] = json!(id);
                    frame["hub"] = json!(hub);
                    self.emit(frame);
                }
                if rpc == "human_channels_test" {
                    let mut frame = result.clone();
                    frame["op"] = json!("channel_test_result");
                    frame["channel_id"] = json!(id);
                    frame["hub"] = json!(hub);
                    self.emit(frame);
                }
                return Ok(result);
            }
            "send_reply_from_human" => {
                return self.slot(&hub)?.rpc("send_reply_from_human", action)
            }
            "send_signal" => {
                let meta = json!({"expected_harness":"human","expected_session_id":"default"});
                let result = self.send(&hub, action, &meta);
                self.emit(match &result {
                    Ok(_) => json!({"op":"composer_result","ok":true,"error":null}),
                    Err(e) => json!({"op":"composer_result","ok":false,"error":e}),
                });
                return result;
            }
            "ack_signals" | "mark_activity_read" | "mark_all_activity_read" => {
                let ids = action["signal_ids"].as_array().cloned().unwrap_or_default();
                let rows = self.state.db.query(
                    "SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL",
                    &[],
                )?;
                for row in rows {
                    if op == "mark_all_activity_read"
                        || action["ack_all"] == true
                        || ids.iter().any(|id| {
                            id == &row["signal_id"]
                                || id.as_str()
                                    == Some(
                                        self.scoped(string(&row, "hub"), string(&row, "signal_id"))
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
            "clear_activity_history" => {
                self.state.db.execute(
                    "DELETE FROM local_signals WHERE acknowledged_at_ms IS NOT NULL",
                    &[],
                )?;
            }
            "listen_session" => {
                let mut caller = action.clone();
                if caller["session_id"].is_null() {
                    caller["session_id"] = json!("default");
                }
                if caller["harness"] != "human"
                    && self
                        .plugins
                        .find_by_id(string(&caller, "harness"))
                        .is_some()
                {
                    let mut meta = json!({"expected_harness":caller["harness"],"expected_session_id":caller["session_id"],"cwd":caller["cwd"]});
                    if caller["endpoint_pid"].is_number() {
                        meta["endpoint_pid"] = caller["endpoint_pid"].clone();
                    }
                    caller = self
                        .caller(&meta, true)?
                        .ok_or("BINDING_REQUIRED: Session is not open")?;
                }
                let about = action["about"].as_str().unwrap_or("Session");
                return self.listen(
                    &hub,
                    &json!({"about":about,"scope":action["scope"]}),
                    &caller,
                );
            }
            "unlisten" => {
                self.unlisten_id(&hub, string(action, "listener_id"))?;
            }
            "unlisten_all" => {
                for row in self.state.active()? {
                    self.unlisten_id(string(&row, "hub"), string(&row, "listener_id"))?;
                }
            }
            "set_scope" => {
                let id = string(action, "listener_id");
                let scope = string(action, "scope");
                if !matches!(scope, "self" | "workspace") {
                    return Err("INVALID_INPUT: Invalid scope".into());
                }
                let slot = self.slot(&hub)?;
                let (mut row, rev) = slot.kv_get(id)?.ok_or("LISTENER_NOT_FOUND")?;
                row["scope"] = json!(scope);
                slot.kv_save(id, &row, Some(rev))?;
                self.state.db.execute(
                    "UPDATE local_listeners SET scope=? WHERE hub=? AND listener_id=?",
                    &[json!(scope), json!(hub), json!(id)],
                )?;
            }
            "open_directory" | "query_directory" => {
                let result = self.tool(
                    "cito_find",
                    &json!({"hub":hub,"query":action["query"].as_str().unwrap_or(""),"limit":50}),
                    &json!({}),
                )?;
                let (activity, unread) = self.state.activity()?;
                let activity = self.activity_dto(activity);
                let frame = json!({"op":"directory","hub":hub,"peers":result["listeners"],"activity":activity,"unread_count":unread});
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
                    let existing = super::registration::local_node(
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
            "hub_action" => {
                let connected = string(action, "action") != "disconnect";
                if action["action"] == "reconnect" {
                    if let Some(old) = self.slots.remove(&hub) {
                        old.closed.store(true, Ordering::SeqCst);
                    }
                }
                self.mutate_config(|v| {
                    v["hubs"][&hub]["enabled"] = json!(connected);
                    Ok(())
                })?;
            }
            "set_host_hub" => {
                self.mutate_config(|v| {
                    let mut host = v
                        .get("host_hub")
                        .or_else(|| v.get("hostHub"))
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    for key in ["enabled", "port", "token", "auth_mode", "bind"] {
                        if let Some(value) = action.get(key) {
                            host[key] = value.clone();
                        }
                    }
                    if host["enabled"] == true && string(&host, "token").is_empty() {
                        let mut bytes = [0u8; 32];
                        getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
                        host["token"] =
                            json!(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
                    }
                    v["host_hub"] = host;
                    if let Some(m) = v.as_object_mut() {
                        m.remove("hostHub");
                    }
                    Ok(())
                })?;
                self.sync_host_hub()?;
            }
            "set_launch_at_login" => {
                set_launch_at_login(
                    action["enabled"].as_bool().unwrap_or(false),
                    &self.config.state_dir,
                    &self.config.home_dir,
                    &self.plugins,
                )?;
            }
            "set_desktop_banner" => {
                self.mutate_config(|v| {
                    v["desktopBannerEnabled"] = action["enabled"].clone();
                    Ok(())
                })?;
            }
            "setup_uninstall_all" | "uninstall_cito" => {
                for row in self.state.active()? {
                    let _ = self.unlisten_id(string(&row, "hub"), string(&row, "listener_id"));
                }
                self.uninstall = Some(action["purge_data"].as_bool().unwrap_or(false));
                request_shutdown();
            }
            "quit" | "quit_daemon" => request_shutdown(),
            "open_settings" => {
                self.emit(
                    json!({"op":"settings","hub":hub,"settings":self.settings_for_hub(&hub)}),
                );
            }
            "refresh_available" => {
                if let Some(activity) = self.session_activity.as_mut() {
                    activity.refresh_discovery();
                }
                self.poll()?;
            }
            "close_settings" | "close_directory" | "copy_id" => {}
            _ => return Err(format!("INVALID_INPUT: Unknown UI action: {op}")),
        }
        self.reload_slots()?;
        self.push_ui();
        Ok(json!({"ok":true}))
    }
    fn sync_host_hub(&mut self) -> Result<(), String> {
        if self.closing {
            return Ok(());
        }
        let raw = self.raw_config()?;
        let host = raw
            .get("host_hub")
            .or_else(|| raw.get("hostHub"))
            .unwrap_or(&Value::Null);
        if self.host_server.is_some() {
            self.host_server.take();
        }
        if host["enabled"] == true {
            let mode = if let Some(mode) = host["auth_mode"].as_str() {
                mode
            } else if string(host, "token").is_empty() {
                "none"
            } else {
                "token"
            };
            let auth = super::auth::HubAuth::new(
                mode,
                host["token"].as_str().map(str::to_string),
                None,
                None,
            )?;
            let server = self.local.start_server(
                host["bind"].as_str().unwrap_or("0.0.0.0"),
                host["port"].as_u64().unwrap_or(4291) as u16,
                auth,
            )?;
            if self.closing {
                drop(server);
            } else {
                self.host_server = Some(server);
            }
        }
        Ok(())
    }
    fn shutdown(&mut self) {
        // The caller sets closing while holding the service mutex after all calls drain.
        self.closing = true;
        for slot in self.slots.values() {
            slot.closed.store(true, Ordering::SeqCst);
        }
        for (_, receiver) in self.receivers.drain() {
            let _ = receiver.join();
        }
        for (_, (_, probe)) in self.probes.drain() {
            if let Some(probe) = probe {
                let _ = probe.join();
            }
        }
        self.host_server.take();
        self.local.shutdown();
        if let Some(ui) = self.ui.take() {
            ui.close();
        }
        for event in self.events.drain(..) {
            let _ = event.join();
        }
        let _ = self.state.db.execute(
            "DELETE FROM daemon_owner WHERE pid=?",
            &[json!(std::process::id())],
        );
    }
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
fn caller_pid_hint(meta: &Value) -> Result<Option<u64>, String> {
    let mut pending = vec![(meta, 0usize)];
    let mut pid = None;
    let mut visited = 0;
    while let Some((value, depth)) = pending.pop() {
        visited += 1;
        if visited > 512 {
            return Err("INVALID_INPUT: Caller metadata is too complex".into());
        }
        if depth > 8 {
            continue;
        }
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if key != "http_client_pid"
                        && (key == "pid"
                            || key.ends_with("_pid")
                            || key.ends_with("Pid")
                            || key.ends_with("PID"))
                        && !value.is_null()
                    {
                        let next = value
                            .as_u64()
                            .filter(|value| *value > 0 && *value <= i32::MAX as u64)
                            .ok_or("INVALID_INPUT: Caller PID must be a positive process ID")?;
                        if pid.is_some_and(|previous| previous != next) {
                            return Err("INVALID_INPUT: Conflicting caller PID claims".into());
                        }
                        pid = Some(next);
                    }
                    if value.is_object() || value.is_array() {
                        pending.push((value, depth + 1));
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    pending.push((value, depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(pid)
}

fn validate_tool_input(name: &str, args: &Value) -> Result<(), String> {
    let map = args
        .as_object()
        .ok_or("INVALID_INPUT: Tool arguments must be an object")?;
    let allowed: &[&str] = match name {
        "cito_listen" => &["about", "tag", "hub", "scope"],
        "cito_unlisten" => &["hub"],
        "cito_find" => &["query", "hub", "user", "host", "harness", "repo", "limit"],
        "cito_send" => &["to", "hub", "message", "in_reply_to", "idempotency_key"],
        "cito_inbox" => &["hub", "ack_ids", "ack_all", "limit"],
        _ => return Err("INVALID_INPUT: Unknown tool".into()),
    };
    for (key, value) in map {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("INVALID_INPUT: Unknown argument: {key}"));
        }
        let valid = match key.as_str() {
            "limit" => value
                .as_u64()
                .is_some_and(|n| n > 0 && n <= if name == "cito_find" { 50 } else { 1000 }),
            "ack_all" | "wake" => value.is_boolean(),
            "ack_ids" => value.as_array().is_some_and(|a| {
                a.len() <= 1000
                    && a.iter()
                        .all(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 161))
            }),
            _ => value.is_string(),
        };
        if !valid {
            return Err(format!("INVALID_INPUT: Invalid argument: {key}"));
        }
    }
    if name == "cito_listen"
        && !args["about"]
            .as_str()
            .is_some_and(|about| !about.trim().is_empty() && about.chars().count() <= 500)
    {
        return Err("INVALID_INPUT: about must contain 1..500 characters".into());
    }
    if let Some(scope) = args["scope"].as_str() {
        if !matches!(scope, "self" | "workspace") {
            return Err("INVALID_INPUT: Invalid scope".into());
        }
    }
    if name == "cito_send" && (!args["to"].is_string() || !args["message"].is_string()) {
        return Err("INVALID_INPUT: to and message are required".into());
    }
    if name == "cito_send" {
        let raw = string(args, "to");
        let (prefix, address) = raw
            .split_once(':')
            .map_or((None, raw), |(hub, address)| (Some(hub), address));
        if prefix.is_some_and(|hub| !crate::plugins::validate_hub_name(hub)) {
            return Err("INVALID_INPUT: Invalid scoped Hub".into());
        }
        if let Some(handle) = address.strip_prefix('@') {
            if handle.is_empty()
                || handle.chars().count() > 64
                || handle.chars().any(|ch| {
                    ch.is_whitespace() || ch.is_control() || matches!(ch, ':' | '@' | '/')
                })
            {
                return Err("INVALID_INPUT: Invalid Human address".into());
            }
        } else {
            super::identity::canonical_node_id(address)?;
        }
    }
    if let Some(hub) = args["hub"].as_str() {
        if !crate::plugins::validate_hub_name(hub) {
            return Err("INVALID_INPUT: Invalid hub".into());
        }
    }
    if let Some(harness) = args["harness"].as_str() {
        if !crate::plugins::validate_harness_id(harness) {
            return Err("UNSUPPORTED_HARNESS: Invalid harness".into());
        }
    }
    for (key, cap) in [
        ("tag", 64),
        ("query", 200),
        ("user", 64),
        ("host", 128),
        ("repo", 200),
        ("in_reply_to", 161),
        ("idempotency_key", 128),
    ] {
        if let Some(value) = args[key].as_str() {
            if value.chars().count() > cap
                || matches!(key, "tag" | "in_reply_to" | "idempotency_key")
                    && value.trim().is_empty()
            {
                return Err(format!("INVALID_INPUT: Invalid {key} length"));
            }
        }
    }
    Ok(())
}
fn tools_list() -> Value {
    let mut tools = json!({"tools":[
     {"name":"cito_listen","description":"Enroll this session in the lightweight control plane for signaling and wake notifications.","inputSchema":{"type":"object","properties":{"about":{"type":"string","minLength":1,"maxLength":500},"tag":{"type":"string","maxLength":64},"hub":{"type":"string"},"scope":{"type":"string","enum":["self","workspace"]}},"required":["about"]}},
     {"name":"cito_unlisten","description":"Revoke this session on one or all hubs.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"}}}},
     {"name":"cito_find","description":"Find opted-in active nodes.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"},"query":{"type":"string","maxLength":200},"user":{"type":"string","maxLength":64},"host":{"type":"string","maxLength":128},"harness":{"type":"string","pattern":"^[a-z][a-z0-9_-]{0,31}$"},"repo":{"type":"string","maxLength":200},"limit":{"type":"integer","minimum":1,"maximum":50}}}},
     {"name":"cito_send","description":"Send a lightweight control plane signal of 1..65,536 Unicode code points. Prefer external channels for long debates or large artifacts.","inputSchema":{"type":"object","properties":{"to":{"type":"string","pattern":"^(?:[a-z0-9][a-z0-9_-]{0,31}:)?(?:c_(?:[a-f0-9]{16}|[a-z2-7]{12})|[a-z0-9]+(?:-[a-z0-9]+)*-(?:[a-f0-9]{16}|[a-z2-7]{12})|@[a-zA-Z0-9._-]{1,64})$"},"message":{"type":"string","minLength":1,"maxLength":65536},"hub":{"type":"string"},"in_reply_to":{"type":"string","minLength":1,"maxLength":161},"idempotency_key":{"type":"string","minLength":1,"maxLength":128}},"required":["to","message"]}},
     {"name":"cito_inbox","description":"Read and acknowledge this node's inbox.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"},"ack_all":{"type":"boolean"},"ack_ids":{"type":"array","items":{"type":"string","minLength":1,"maxLength":161}}}}}
    ]});
    if let Some(list) = tools["tools"].as_array_mut() {
        for tool in list {
            tool["inputSchema"]["additionalProperties"] = json!(false);
        }
    }
    tools
}
fn respond_bytes(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let header=format!("HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",body.len());
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}
fn respond(stream: &mut TcpStream, status: u16, value: &Value) {
    if let Ok(body) = serde_json::to_vec(value) {
        respond_bytes(stream, status, &body);
    }
}
fn http_client_pid(stream: &TcpStream) -> Option<u32> {
    let remote = stream.peer_addr().ok()?.port();
    let local = stream.local_addr().ok()?.port();
    let output = Command::new("/usr/sbin/lsof")
        .args([
            "-nP",
            &format!("-iTCP:{remote}"),
            "-sTCP:ESTABLISHED",
            "-Fpn",
        ])
        .output()
        .ok()?;
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
fn interactive_login(action: &Value) -> bool {
    action["op"] == "login_hub" || (action["op"] == "hub_action" && action["action"] == "login")
}
fn dispatch_action(service: &Arc<Mutex<DaemonService>>, action: &Value) -> Result<Value, String> {
    let (hub, config, state_dir, attempt, snapshot) = {
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
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
        super::login::recover_cancellable(&state_dir, &hub, &config, rejected, true, &|| {
            shutdown_requested() || attempt.cancelled.load(Ordering::SeqCst)
        })
    };
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
        old.closed.store(true, Ordering::SeqCst);
    }
    for row in s.state.active()?.into_iter().filter(|row| {
        row["hub"] == hub
            && !super::auth::principal_ids_equal(
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
fn handle_http(mut stream: TcpStream, service: Arc<Mutex<DaemonService>>) {
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
            headers.insert(k, json!(v.trim()));
        }
    }
    if headers
        .get("origin")
        .and_then(Value::as_str)
        .is_some_and(|origin| {
            !url::Url::parse(origin).ok().is_some_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && match url.host() {
                        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
                        Some(url::Host::Ipv4(host)) => host == std::net::Ipv4Addr::LOCALHOST,
                        Some(url::Host::Ipv6(host)) => host == std::net::Ipv6Addr::LOCALHOST,
                        None => false,
                    }
            })
        })
    {
        respond(&mut stream, 403, &json!({"error":"Forbidden Origin"}));
        return;
    }
    if length > 1_048_576 {
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
        if let Ok(s) = service.lock() {
            respond(
                &mut stream,
                200,
                &json!({"ok":true,"pid":std::process::id(),"stateDir":s.config.state_dir,"version":CITO_VERSION}),
            );
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
        match dispatch_action(&service, &parsed) {
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
    if parsed["jsonrpc"] != "2.0" || !parsed["method"].is_string() {
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
                        if let Err(error) = wait_send_receipt(&service, &mut payload) {
                            payload = json!({"ok":false,"error":{"code":"NATS_UNAVAILABLE","message":error,"retryable":true}});
                            return respond(
                                &mut stream,
                                200,
                                &json!({"jsonrpc":"2.0","id":id,"result":{"isError":true,"content":[{"type":"text","text":payload.to_string()}]}}),
                            );
                        }
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
fn wait_send_receipt(
    service: &Arc<Mutex<DaemonService>>,
    payload: &mut Value,
) -> Result<(), String> {
    if payload["status"] == "PENDING_PUBLISH" {
        return Ok(());
    }
    let (state, slot, signal) = {
        let s = service.lock().map_err(|_| "Service lock poisoned")?;
        let raw = string(payload, "signal_id");
        let (hub, signal) = raw.split_once(':').unwrap_or((&s.config.default_hub, raw));
        (s.state.clone(), s.slot(hub)?.clone(), signal.to_string())
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if shutdown_requested() || slot.closed.load(Ordering::SeqCst) {
            return Err("Send interrupted".into());
        }
        let remote = slot.rpc(
            "stream_get_message",
            &json!({"stream":"CITO_SIGNALS","seq":payload["seq"]}),
        )?;
        if remote["received"] == true {
            break;
        }
        if Instant::now() >= deadline {
            payload["status"] = json!("stored_passive");
            payload["held_reason"] = json!("sender_ack_timeout");
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    state.db.execute(
        "UPDATE local_sent_idempotency SET status=?,held_reason=? WHERE hub=? AND signal_id=?",
        &[
            payload["status"].clone(),
            payload["held_reason"].clone(),
            json!(slot.name),
            json!(signal),
        ],
    )?;
    Ok(())
}

struct UiChild {
    child: Child,
    reader: thread::JoinHandle<()>,
    writer: thread::JoinHandle<()>,
}
fn resolve_ui_binary(
    root: &std::path::Path,
    plugins: &LoadedPlugins,
    explicit: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    let plugin_binary = plugins.find_by_id("human").and_then(|plugin| {
        let command = plugin.command.first()?;
        Some(plugin.plugin_dir.join(command))
    });
    explicit
        .into_iter()
        .chain(plugin_binary)
        .chain([
            root.join("Helpers/cito-ui"),
            root.join("Resources/plugins/human/bin/cito-ui"),
            root.join("plugins/human/bin/cito-ui"),
        ])
        .find(|path| path.is_file())
}
fn spawn_ui(service: Arc<Mutex<DaemonService>>) -> Option<UiChild> {
    if std::env::var("CITO_DISABLE_UI").ok().as_deref() == Some("1") {
        return None;
    }
    let bin = resolve_ui_binary(
        &resolve_package_root(),
        &service.lock().ok()?.plugins,
        std::env::var_os("CITO_UI_BINARY").map(std::path::PathBuf::from),
    )?;
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut input = child.stdin.take()?;
    let output = child.stdout.take()?;
    let mailbox = Arc::new(UiMailbox::default());
    let output_mailbox = mailbox.clone();
    let writer = thread::spawn(move || {
        while let Some(bytes) = output_mailbox.next() {
            if input.write_all(&bytes).and_then(|_| input.flush()).is_err() {
                break;
            }
        }
    });
    if let Ok(mut s) = service.lock() {
        s.ui = Some(mailbox);
        s.push_ui();
    }
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(output);
        let mut logins: Vec<thread::JoinHandle<()>> = Vec::new();
        loop {
            let mut line = String::new();
            match reader
                .by_ref()
                .take(FRAME_LIMIT as u64)
                .read_line(&mut line)
            {
                Ok(0) | Err(_) => break,
                Ok(_) if line.len() >= FRAME_LIMIT => break,
                _ => {}
            }
            if let Ok(action) = serde_json::from_str::<Value>(&line) {
                let is_login = interactive_login(&action);
                let service = service.clone();
                let run = move || {
                    if let Err(e) = dispatch_action(&service, &action) {
                        if let Ok(s) = service.lock() {
                            s.emit(json!({"op":"error","error":e}));
                        }
                    }
                };
                if is_login {
                    logins.push(thread::spawn(run));
                } else {
                    run();
                }
                let mut pending = Vec::new();
                for login in logins.drain(..) {
                    if login.is_finished() {
                        let _ = login.join();
                    } else {
                        pending.push(login);
                    }
                }
                logins = pending;
            }
        }
        for login in logins {
            let _ = login.join();
        }
    });
    Some(UiChild {
        child,
        reader,
        writer,
    })
}
pub fn run_daemon(config: &NativeConfig, plugins: &LoadedPlugins) {
    if let Err(e) = run_daemon_inner(config, plugins) {
        eprintln!("cito daemon: {e}");
        std::process::exit(1);
    }
}
fn run_daemon_inner(config: &NativeConfig, plugins: &LoadedPlugins) -> Result<(), String> {
    if crate::plugins::handover_stale_daemon(config)? {
        return Ok(());
    }
    fs::create_dir_all(&config.state_dir).map_err(|e| e.to_string())?;
    fs::set_permissions(&config.state_dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(config.state_dir.join("daemon.lock"))
        .map_err(|e| e.to_string())?;
    // SAFETY: descriptor is live for daemon lifetime. Nonblocking lock prevents dual writers.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Daemon owner lock is held".into());
    }
    let listener =
        TcpListener::bind(("127.0.0.1", config.daemon_port)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    ensure_cli_shims_native(&config.state_dir, plugins)?;
    let service = Arc::new(Mutex::new(DaemonService::open(config, plugins)?));
    {
        let mut s = service.lock().map_err(|_| "Service lock poisoned")?;
        s.state.db.execute(
            "INSERT OR REPLACE INTO daemon_owner VALUES(1,?,?)",
            &[json!(std::process::id()), json!(config.daemon_port)],
        )?;
        s.sync_host_hub()?;
    }
    let mut pipe = [-1; 2];
    // SAFETY: pipe points at two writable descriptors; flags keep signal writes nonblocking.
    if unsafe { libc::pipe(pipe.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let read_pipe = unsafe { std::fs::File::from_raw_fd(pipe[0]) };
    let write_pipe = unsafe { std::fs::File::from_raw_fd(pipe[1]) };
    unsafe {
        libc::fcntl(write_pipe.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
    }
    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
    SIGNAL_PIPE.store(write_pipe.as_raw_fd(), Ordering::SeqCst);
    let previous_term = unsafe {
        libc::signal(
            libc::SIGTERM,
            handle_daemon_sig as *const () as libc::sighandler_t,
        )
    };
    let previous_int = unsafe {
        libc::signal(
            libc::SIGINT,
            handle_daemon_sig as *const () as libc::sighandler_t,
        )
    };
    let mut ui = spawn_ui(service.clone());
    let mut retired_ui: Vec<UiChild> = Vec::new();
    let mut calls = Vec::new();
    let mut last_poll = Instant::now();
    'running: while !SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let s = service.clone();
                    calls.push(thread::spawn(move || handle_http(stream, s)));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    eprintln!("cito accept: {e}");
                    break 'running;
                }
            }
        }
        if last_poll.elapsed() > Duration::from_millis(250) {
            if let Ok(mut s) = service.try_lock() {
                if let Err(e) = s.poll() {
                    eprintln!("cito poll: {e}");
                }
                last_poll = Instant::now();
            }
        }
        let mut pending = Vec::new();
        for call in calls.drain(..) {
            if call.is_finished() {
                let _ = call.join();
            } else {
                pending.push(call);
            }
        }
        calls = pending;
        if ui
            .as_mut()
            .is_some_and(|u| u.child.try_wait().ok().flatten().is_some())
        {
            let restart = if let Ok(mut s) = service.try_lock() {
                if let Some(mailbox) = s.ui.take() {
                    mailbox.close();
                }
                !s.closing && !shutdown_requested()
            } else {
                false
            };
            if restart {
                if let Some(old) = ui.take() {
                    retired_ui.push(old);
                }
                ui = spawn_ui(service.clone());
            }
        }
        let mut pending_ui = Vec::new();
        for old in retired_ui.drain(..) {
            if old.reader.is_finished() && old.writer.is_finished() {
                let _ = old.reader.join();
                let _ = old.writer.join();
            } else {
                pending_ui.push(old);
            }
        }
        retired_ui = pending_ui;
        let mut poll = libc::pollfd {
            fd: read_pipe.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe {
            libc::poll(&mut poll, 1, 20);
        }
    }
    drop(listener);
    // Set closing before draining calls, then close the final host server after drain.
    if let Ok(mut s) = service.lock() {
        s.closing = true;
    }
    for call in calls {
        let _ = call.join();
    }
    let (uninstall, mut session_activity) = if let Ok(mut s) = service.lock() {
        s.shutdown();
        (s.uninstall, s.session_activity.take())
    } else {
        (None, None)
    };
    // Plugin workers drain after receivers stop, without retaining the service mutex.
    if let Some(activity) = session_activity.as_mut() {
        if let Err(error) = activity.shutdown() {
            eprintln!("cito badge cleanup: {error}");
        }
    }
    for mut ui in ui.take().into_iter().chain(retired_ui) {
        if ui.child.try_wait().ok().flatten().is_none() {
            let _ = ui.child.kill();
        }
        let _ = ui.child.wait();
        let _ = ui.reader.join();
        let _ = ui.writer.join();
    }
    drop(service);
    if let Some(purge) = uninstall {
        uninstall_setup_native(
            &config.state_dir,
            &config.config_path,
            &config.home_dir,
            plugins,
            purge,
        )?;
    }
    unsafe {
        libc::signal(libc::SIGTERM, previous_term);
        libc::signal(libc::SIGINT, previous_int);
    }
    SIGNAL_PIPE.store(-1, Ordering::SeqCst);
    drop(write_pipe);
    drop(read_pipe);
    Ok(())
}

pub use super::cli::run_cli_command;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packaged_app_resolves_ui_and_honors_overrides() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("cito-ui-layout-{}", random_uuid_v4()));
        let root = dir.join("Cito.app/Contents");
        let result = (|| {
            fs::create_dir_all(root.join("Helpers")).map_err(|e| e.to_string())?;
            let bin = root.join("Helpers/cito-ui");
            fs::write(&bin, "").map_err(|e| e.to_string())?;
            let plugins = LoadedPlugins {
                plugins: vec![],
                rejected_ids: Default::default(),
            };
            assert_eq!(resolve_ui_binary(&root, &plugins, None), Some(bin));
            let override_bin = dir.join("custom-ui");
            fs::write(&override_bin, "").map_err(|e| e.to_string())?;
            assert_eq!(
                resolve_ui_binary(&root, &plugins, Some(override_bin.clone())),
                Some(override_bin)
            );
            Ok(())
        })();
        let _ = fs::remove_dir_all(dir);
        result
    }
    #[test]
    fn coalesced_snapshots_retain_command_results() -> Result<(), String> {
        let mailbox = UiMailbox::default();
        for i in 0..40 {
            mailbox.push(Some("settings:local".into()), vec![i]);
        }
        mailbox.push(None, b"composer_result".to_vec());
        mailbox.push(None, b"pair_code".to_vec());
        assert_eq!(mailbox.next(), Some(vec![39]));
        assert_eq!(mailbox.next(), Some(b"composer_result".to_vec()));
        assert_eq!(mailbox.next(), Some(b"pair_code".to_vec()));
        mailbox.close();
        assert_eq!(mailbox.next(), None);
        Ok(())
    }
    #[test]
    fn bounded_empty_activity_with_multibyte_listeners() -> Result<(), String> {
        let f = json!({"op":"state","activity":[],"listeners":(0..150).map(|_|json!({"title":"🦀".repeat(10000),"about":"🦀".repeat(10000)})).collect::<Vec<_>>(),"unread_count":200});
        let bytes = bounded_frame(f)?;
        assert!(bytes.len() < FRAME_LIMIT);
        let frame: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        assert_eq!(frame["unread_count"], 200);
        assert_eq!(frame["listeners"].as_array().map(Vec::len), Some(64));
        Ok(())
    }
}
