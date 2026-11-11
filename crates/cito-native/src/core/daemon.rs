use super::{
    auth::HubAuth,
    hub::{HubEngine, HubServer},
    identity::{hash, iso, listener_id, local_host_name, normalize_slug, sanitize_message, string},
    receiver::{detach_channel_expected, plugin_request, poll_listener},
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
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const FRAME_LIMIT: usize = 262_144;
const REQUEST_LIMIT: usize = 1_048_576;
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
    // Full-message copy uses correlated bounded chunks rather than preview caps.
    if frame["op"] != "activity_message" {
        bound_fields(&mut frame, "");
    }
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

fn repair_default_hub(v: &mut Value) {
    let hubs = v["hubs"].as_object();
    let enabled = |cfg: &Value| cfg["enabled"] != false && cfg["disabled"] != true;
    let current = v["default_hub"]
        .as_str()
        .or_else(|| v["defaultHub"].as_str())
        .filter(|name| hubs.and_then(|h| h.get(*name)).is_some_and(enabled));
    let fallback = current
        .or_else(|| {
            let hubs = hubs?;
            hubs.get_key_value("local")
                .filter(|(_, cfg)| enabled(cfg))
                .map(|(name, _)| name.as_str())
                .or_else(|| {
                    hubs.iter()
                        .filter(|(_, cfg)| enabled(cfg))
                        .map(|(name, _)| name.as_str())
                        .min()
                })
                .or_else(|| hubs.get_key_value("local").map(|(name, _)| name.as_str()))
                .or_else(|| hubs.keys().map(String::as_str).min())
        })
        .unwrap_or("local")
        .to_string();
    if let Some(config) = v.as_object_mut() {
        config.insert("default_hub".into(), json!(fallback));
        config.remove("defaultHub");
    }
}

// Presentation must cover local registrations before the asynchronous directory
// cache catches up. Cached Hub rows retain their lease and attachment metadata.
fn address_peers(state: &LocalState, slot: &HubClient) -> Vec<Value> {
    let mut peers = slot.cached_directory();
    let now = now_ms();
    if let Ok(rows) = state.db.query(
        "SELECT * FROM local_listeners WHERE hub=? AND status='ACTIVE'",
        &[json!(slot.name)],
    ) {
        for mut row in rows {
            if let Some(cached) = peers
                .iter_mut()
                .find(|p| p["listener_id"] == row["listener_id"])
            {
                if !super::identity::is_live_peer(cached, now) {
                    // Local registration is authoritative while the directory catches up.
                    // Preserve cached attachments, including their original lease metadata.
                    cached["state"] = json!("ACTIVE");
                    cached["status"] = json!("ACTIVE");
                    cached["updated_at_ms"] = json!(now);
                }
            } else {
                row["state"] = json!("ACTIVE");
                peers.push(row);
            }
        }
    }
    peers
}
fn sender_display_address(
    hub: &str,
    default_hub: &str,
    source: Option<&str>,
    user: &str,
    harness: &str,
    peers: &[Value],
) -> Option<String> {
    if let Some(id) = source.filter(|id| !id.is_empty()) {
        let canonical = super::identity::canonical_node_id(id).unwrap_or_default();
        if let Some(sender) = peers.iter().find(|p| p["listener_id"] == canonical) {
            return Some(super::identity::display_address(
                hub,
                default_hub,
                sender,
                peers,
            ));
        }
        if !canonical.is_empty() && !user.is_empty() && !harness.is_empty() {
            let sender = json!({"owner_user":user,"harness":harness,"listener_id":canonical,"public_id":super::identity::public_listener_id(user, &canonical)});
            return Some(super::identity::display_address(
                hub,
                default_hub,
                &sender,
                peers,
            ));
        }
        return Some(if hub == default_hub {
            id.into()
        } else {
            format!("{hub}:{id}")
        });
    }
    (harness == "human" && !user.is_empty()).then(|| {
        super::identity::display_address(
            hub,
            default_hub,
            &json!({"owner_user":user,"harness":"human"}),
            peers,
        )
    })
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
        let key = if string(&frame, "request_id").is_empty()
            && string(&frame, "source_op").is_empty()
            && frame["confirm_hub_save"] != true
            && matches!(string(&frame, "op"), "state" | "settings" | "directory")
        {
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

struct ChannelInventory {
    identity: Value,
    serial: Mutex<()>,
    cached: Mutex<CachedChannelInventory>,
}
struct CachedChannelInventory {
    value: Value,
    completed_gen: u64,
    retry_at: Instant,
}
impl ChannelInventory {
    fn new(identity: Value) -> Self {
        Self {
            identity,
            serial: Mutex::new(()),
            cached: Mutex::new(CachedChannelInventory {
                value: json!({"status":"loading"}),
                completed_gen: 0,
                retry_at: Instant::now(),
            }),
        }
    }
    fn refresh(&self, slot: &HubClient, observed_gen: u64) -> Result<(), String> {
        let _serial = self.serial.lock().map_err(|e| e.to_string())?;
        // A read queued behind an inventory request or mutation shares its result.
        if self.cached.lock().map_err(|e| e.to_string())?.completed_gen == observed_gen {
            self.fetch_locked(slot)?;
        }
        Ok(())
    }
    fn mutate(
        &self,
        slot: &HubClient,
        plugins: &LoadedPlugins,
        action: &Value,
    ) -> Result<Value, String> {
        let _serial = self.serial.lock().map_err(|e| e.to_string())?;
        if slot.closed.load(Ordering::SeqCst) || shutdown_requested() {
            return Err("NATS_UNAVAILABLE: Channel request cancelled".into());
        }
        let result = run_channel_action(slot, plugins, action);
        // The same owner spans schema lookup, mutation, and the post-mutation read.
        // No pre-mutation inventory can satisfy or overwrite this acknowledgement.
        let refresh = self.fetch_locked(slot);
        match result {
            Ok(value) => {
                refresh?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
    // Both callers hold serial; cached is never held across remote I/O.
    fn fetch_locked(&self, slot: &HubClient) -> Result<(), String> {
        if slot.closed.load(Ordering::SeqCst) || shutdown_requested() {
            return Err("NATS_UNAVAILABLE: Channel request cancelled".into());
        }
        let value = slot
            .rpc("human_channels_list", &json!({}))
            .unwrap_or_else(|e| json!({"status":"error","detail":e}));
        if slot.closed.load(Ordering::SeqCst) || shutdown_requested() {
            return Err("NATS_UNAVAILABLE: Channel request cancelled".into());
        }
        let mut cached = self.cached.lock().map_err(|e| e.to_string())?;
        cached.completed_gen = cached
            .completed_gen
            .checked_add(1)
            .ok_or("Channel inventory generation exhausted")?;
        cached.value = value;
        cached.retry_at = Instant::now() + Duration::from_secs(1);
        Ok(())
    }
}
fn remote_channels_identity(slot: &HubClient) -> Value {
    json!([
        slot.config["url"],
        slot.config["authMode"],
        slot.bearer,
        slot.identity["owner_user"],
        slot.identity["principal_id"]
    ])
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
    applied_host_config: Option<Value>,
    host_hub_reload_failed: bool,
    uninstall: Option<bool>,
    active_settings_hub: Option<String>,
    last_app_action: HashMap<String, String>,
    last_host_hub_error: Option<String>,
    last_hub_errors: HashMap<String, (String, bool)>,
    channel_inventories: HashMap<String, Arc<ChannelInventory>>,
    activity_filter: String,
    activity_query: String,
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
            applied_host_config: None,
            host_hub_reload_failed: false,
            uninstall: None,
            active_settings_hub: None,
            last_app_action: HashMap::new(),
            last_host_hub_error: None,
            last_hub_errors: HashMap::new(),
            channel_inventories: HashMap::new(),
            activity_filter: "all".into(),
            activity_query: String::new(),
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
        if !crate::plugins::validate_user(&user) {
            return Err("INVALID_INPUT: Invalid user".into());
        }
        Ok(user.to_ascii_lowercase())
    }
    pub fn reload_slots(&mut self) -> Result<(), String> {
        if self.closing {
            return Ok(());
        }
        let _ = self.sync_host_hub();
        let mut raw = self.raw_config()?;
        self.config.user = self.get_effective_user()?;
        repair_default_hub(&mut raw);
        let (default, hubs, _) =
            crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))?;
        self.config.default_hub = std::env::var("CITO_HUB")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(default);
        let mut keep = Vec::new();
        for (name, mut hub) in hubs {
            if hub["enabled"] == false {
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
            self.channel_inventories.remove(&name);
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
                        let _ = detach_channel_expected(
                            &old,
                            string(&row, "listener_id"),
                            &channel,
                            &row,
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
            self.channel_inventories.remove(&name);
            if let Some(slot) = self.slots.remove(&name) {
                slot.closed.store(true, Ordering::SeqCst);
            }
        }
        self.ensure_human_nodes()?;
        Ok(())
    }
    fn reconcile_unlistened(
        state: &LocalState,
        client: &HubClient,
        host_id: &str,
    ) -> Result<(), String> {
        // Re-read durable intent under the enrollment lock: an explicit re-enable
        // supersedes any pending detach before this worker can mutate the Hub.
        let _registration = client
            .registration
            .lock()
            .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
        for pending in state.db.query(
            "SELECT * FROM pending_hub_detaches WHERE hub=?",
            &[json!(client.name)],
        )? {
            let id = string(&pending, "listener_id");
            let clear = || {
                state.db.execute(
                    "DELETE FROM pending_hub_detaches WHERE hub=? AND listener_id=?",
                    &[json!(client.name), json!(id)],
                )
            };
            let Some(row) = state
                .listener(&client.name, id)?
                .filter(|row| row["status"] == "UNLISTENED")
            else {
                clear()?;
                continue;
            };
            if pending["hub_url"].as_str() != Some(string(&client.config, "url")) {
                clear()?;
                continue;
            }
            if !super::auth::principal_ids_equal(
                string(&row, "principal_id"),
                string(&client.identity, "principal_id"),
            ) {
                continue;
            }
            let channel = format!("{host_id}:{}", string(&row, "harness"));
            let Some((remote, _)) = client.kv_get(id)? else {
                clear()?;
                continue;
            };
            let human = row["harness"] == "human";
            let captured = if human
                && row["binding_generation"] == 0
                && id == listener_id(string(&client.identity, "principal_id"), "human", "default")
            {
                // Pause can precede automatic enrollment on this Mac.
                let mut captured = remote.clone();
                if let Some(owner) = remote["channels"][&channel]["owner_token"].as_str() {
                    captured["owner_token"] = json!(owner);
                }
                captured
            } else {
                row.clone()
            };
            if !super::registration::remote_matches_listener(&remote, &captured, &channel) {
                clear()?;
                continue;
            }
            if remote["channels"][&channel]["state"] == "ACTIVE"
                || human && remote["human_paused"] != true
            {
                detach_channel_expected(client, id, &channel, &captured)?;
            }
            clear()?;
        }
        Ok(())
    }
    fn ensure_human_nodes(&mut self) -> Result<(), String> {
        for (slot_name, client) in self.slots.clone() {
            if client.local.is_some() {
                let _ = Self::reconcile_unlistened(&self.state, &client, &self.host_id);
            }
            if client.config["authMode"] == "oidc" && client.bearer.is_none() {
                continue;
            }
            if super::registration::local_node(&self.state, &client, "human", "default")?
                .is_some_and(|r| matches!(string(&r, "status"), "ACTIVE" | "UNLISTENED"))
            {
                continue;
            }
            let args = json!({"about":"Human receiver"});
            let caller = json!({"harness":"human","session_id":"default","cwd":"","title":"Human"});
            if client.local.is_some() {
                match super::registration::register_automatic_human(
                    &self.state,
                    &client,
                    &self.host_id,
                    &args,
                    &caller,
                ) {
                    // A Hub-wide pause can predate this local profile. Enrollment is
                    // optional; preserving that pause must not reject unrelated settings.
                    Err(error) if error.starts_with("BINDING_REQUIRED:") => {}
                    result => {
                        result?;
                    }
                }
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
                json!({"harness":"human","session_id":"default","cwd":"","title":"Human"}),
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
                            cache.retain(|pid, _| crate::plugins::pid_alive(*pid as i32));
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
        let _ = self.slot(hub)?.directory();
        result["listener_id"] = json!(self.scoped(hub, string(&result, "listener_id")));
        Ok(result)
    }
    fn unlisten_id(&self, hub: &str, id: &str) -> Result<bool, String> {
        let client = self.slots.get(hub);
        let hub_url = if let Some(client) = client {
            Some(string(&client.config, "url").to_owned())
        } else {
            // Disabled Hubs have no live slot; their normalized configuration
            // still identifies the only target a deferred detach may affect.
            let raw = self.raw_config()?;
            let (_, hubs, _) =
                crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))?;
            hubs.get(hub).map(|config| string(config, "url").to_owned())
        };
        let registration = client
            .map(|client| {
                client
                    .registration
                    .lock()
                    .map_err(|_| "STATE_ERROR: Registration lock poisoned")
            })
            .transpose()?;
        let row = self.state.listener(hub, id)?;
        let active = row.as_ref().is_some_and(|row| row["status"] == "ACTIVE");
        self.state.db.transaction(|c| {
            if row.is_some() {
                c.execute("UPDATE local_signals SET state='STORED_PASSIVE',held_reason='unlistened',dispatcher_token=NULL WHERE hub=? AND listener_id=? AND acknowledged_at_ms IS NULL AND state IN ('RECEIVED','DISPATCHING')", &[json!(hub),json!(id)])?;
                c.execute("UPDATE local_listeners SET status='UNLISTENED' WHERE hub=? AND listener_id=?", &[json!(hub),json!(id)])?;
            } else if let Some(client) = client.filter(|client|id==listener_id(string(&client.identity,"principal_id"),"human","default")) {
                // Persist intent before queued automatic enrollment can obtain the lock.
                c.execute("INSERT OR IGNORE INTO local_listeners(hub,listener_id,public_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status) VALUES(?,?,?,?,?,?,?,'human','default','','','','self','UNLISTENED')",&[
                    json!(hub),json!(id),json!(super::identity::public_listener_id(string(&client.identity,"owner_user"),id)),
                    client.identity["principal_id"].clone(),client.identity["owner_user"].clone(),json!(self.host_id),json!(local_host_name())])?;
            } else { return Ok(()); }
            if let Some(url) = &hub_url {
                c.execute("INSERT INTO pending_hub_detaches(hub,listener_id,hub_url) VALUES(?,?,?) ON CONFLICT(hub,listener_id) DO UPDATE SET hub_url=excluded.hub_url",&[json!(hub),json!(id),json!(url)])?;
            }
            Ok(())
        })?;
        drop(registration);
        // A failed Hub write leaves durable pending work; poll/reconnect retries it.
        if let Some(client) = client {
            let _ = Self::reconcile_unlistened(&self.state, client, &self.host_id);
        }
        Ok(active)
    }
    fn find_listeners(
        &mut self,
        args: &Value,
        include: impl Fn(&Value) -> bool,
    ) -> Result<Value, String> {
        let hub = args["hub"]
            .as_str()
            .unwrap_or(&self.config.default_hub)
            .to_string();
        let limit = args["limit"].as_u64().unwrap_or(20);
        if !(1..=50).contains(&limit) {
            return Err("INVALID_INPUT: limit must be 1..50".into());
        }
        let query = string(args, "query").to_lowercase();
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
                                && !super::auth::principal_ids_equal(
                                    string(&row, "principal_id"),
                                    string(&slot.identity, "principal_id"),
                                ))
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
                        let scoped_id = self.scoped(&name, string(&row, "public_id"));
                        let display_address = self.display_address(&name, &row);
                        let full_alias = format!(
                            "{}/{}#{}",
                            normalize_slug(string(&row, "owner_user")),
                            string(&row, "harness"),
                            string(&row, "listener_id").strip_prefix("c_").unwrap_or("")
                        );
                        let haystack = [
                            full_alias.as_str(),
                            string(&row, "title"),
                            string(&row, "about"),
                            string(&row, "repo"),
                            string(&row, "cwd"),
                            string(&row, "branch"),
                            string(&row, "owner_user"),
                            string(&row, "host_name"),
                            string(&row, "host_id"),
                            string(&row, "harness"),
                            string(&row, "public_id"),
                            &scoped_id,
                            &display_address,
                            string(&row, "listener_id"),
                        ]
                        .join(" ")
                        .to_lowercase();
                        if !query.split_whitespace().all(|token| {
                            let contains =
                                |field, value| string(&row, field).to_lowercase().contains(value);
                            if let Some(user) = token.strip_prefix('@').filter(|s| !s.is_empty()) {
                                contains("owner_user", user)
                            } else if let Some(user) =
                                token.strip_prefix("user:").filter(|s| !s.is_empty())
                            {
                                contains("owner_user", user)
                            } else if let Some(repo) =
                                token.strip_prefix("repo:").filter(|s| !s.is_empty())
                            {
                                contains("repo", repo)
                            } else if let Some(host) =
                                token.strip_prefix("host:").filter(|s| !s.is_empty())
                            {
                                contains("host_name", host) || contains("host_id", host)
                            } else if let Some(harness) =
                                token.strip_prefix("is:").filter(|s| !s.is_empty())
                            {
                                string(&row, "harness").to_lowercase() == harness
                            } else if let Some(harness) =
                                token.strip_prefix("harness:").filter(|s| !s.is_empty())
                            {
                                contains("harness", harness)
                            } else if let Some(id) =
                                token.strip_prefix('#').filter(|s| !s.is_empty())
                            {
                                contains("public_id", id)
                                    || scoped_id.to_lowercase().contains(id)
                                    || contains("listener_id", id)
                            } else {
                                haystack.contains(token)
                            }
                        }) {
                            continue;
                        }
                        rows.push(self.peer_dto(&name, &row));
                    }
                }
                Err(e) => warnings.push(json!({"hub":name,"message":e})),
            }
        }
        rows.retain(include);
        rows.sort_by_key(|r| format!("{}:{}", string(r, "hub"), string(r, "listener_id")));
        let matched_count = rows.len();
        let online_count = self
            .slots
            .values()
            .map(|s| {
                s.cached_directory()
                    .into_iter()
                    .filter(|r| super::lease::evaluate_node_lease(r, now_ms()) == "ACTIVE")
                    .count()
            })
            .sum::<usize>();
        rows.truncate(limit as usize);
        Ok(
            json!({"ok":true,"listeners":rows,"warnings":warnings,"matched_count":matched_count,"online_count":online_count}),
        )
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
                self.find_listeners(args, |_| true)
            }
            "cito_send" => {
                let result = match self.send(&hub, args, meta) {
                    Err(e)
                        if e.starts_with("UNAUTHORIZED:")
                            || e.starts_with("AUTHORIZATION_VIOLATION:") =>
                    {
                        self.reload_slots()?;
                        self.send(&hub, args, meta)
                    }
                    result => result,
                };
                self.poll()?;
                result
            }
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
        let alias = super::identity::readable_alias(to);
        let canonical_target = if to.starts_with('@') || alias.is_some() {
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
                super::registration::register_automatic_human(
                    &self.state,
                    slot,
                    &self.host_id,
                    &json!({"about":"Human receiver"}),
                    &json!({"harness":"human","session_id":"default","cwd":"","title":"Human"}),
                )?;
                directory = slot.directory()?;
            }
        }
        let exact_handle = handle.is_some_and(|handle| {
            directory.iter().any(|r| {
                r["harness"] == "human" && string(r, "owner_user").eq_ignore_ascii_case(handle)
            })
        });
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
                                || (!exact_handle
                                    && normalize_slug(string(row, "owner_user"))
                                        == normalize_slug(user))
                        }
                })
                .collect()
        } else if let Some((user, harness, prefix)) = alias {
            let mut found: Vec<_> = directory
                .into_iter()
                .filter(|row| {
                    matches!(
                        super::lease::evaluate_node_lease(row, now_ms()),
                        "ACTIVE" | "DISCONNECTED_RETAINED"
                    ) && string(row, "listener_id")
                        .strip_prefix("c_")
                        .is_some_and(|s| s.starts_with(prefix))
                        && match harness {
                            Some(h) => {
                                string(row, "harness") == h
                                    && (string(row, "owner_user").eq_ignore_ascii_case(user)
                                        || normalize_slug(string(row, "owner_user")) == user)
                            }
                            None => {
                                string(row, "harness") == user
                                    || string(row, "owner_user").eq_ignore_ascii_case(user)
                                    || normalize_slug(string(row, "owner_user")) == user
                            }
                        }
                })
                .collect();
            // Bare harness aliases prefer the caller's principal; ambiguity within
            // that chosen scope still requires a full canonical ID.
            if harness.is_none()
                && found.iter().any(|row| {
                    string(row, "harness") == user
                        && super::auth::principal_ids_equal(
                            string(row, "principal_id"),
                            string(&slot.identity, "principal_id"),
                        )
                })
            {
                found.retain(|row| {
                    string(row, "harness") == user
                        && super::auth::principal_ids_equal(
                            string(row, "principal_id"),
                            string(&slot.identity, "principal_id"),
                        )
                });
            }
            if prefix.len() == 12
                && found
                    .iter()
                    .any(|r| string(r, "listener_id").strip_prefix("c_") == Some(prefix))
            {
                found.retain(|r| string(r, "listener_id").strip_prefix("c_") == Some(prefix));
            }
            found
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
        if handle.is_some() && matches.len() > 1 {
            let now = now_ms();
            let is_live = |row: &Value| {
                matches!(
                    super::lease::evaluate_node_lease(row, now),
                    "ACTIVE" | "DISCONNECTED_RETAINED"
                )
            };
            if matches.iter().any(is_live) {
                matches.retain(is_live);
            }
        }
        if matches.len() > 1 {
            if handle.is_none() {
                return Err("INVALID_INPUT: Ambiguous target; use full listener_id".into());
            }
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
        if let Some(caller) = caller.as_ref().filter(|c| {
            !string(c, "harness").is_empty()
                && c["harness"] != "human"
                && !string(c, "session_id").is_empty()
        }) {
            if super::registration::local_node(
                &self.state,
                slot,
                string(caller, "harness"),
                string(caller, "session_id"),
            )?
            .is_none()
            {
                let mut caller = caller.clone();
                let title = string(&caller, "title")
                    .trim()
                    .chars()
                    .take(500)
                    .collect::<String>();
                caller["title"] = json!(if title.is_empty() {
                    "Agent session"
                } else {
                    &title
                });
                self.listen(hub, &json!({"about":caller["title"]}), &caller)?;
            }
        }
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
        let dedup = hash(&format!(
            "{endpoint}|{}|{canonical}|{key}",
            string(&slot.config, "url")
        ));
        let request_hash = hash(&format!("{message}|{}", reply_to));
        let now = now_ms();
        let reservation=self.state.db.transaction(|c|{
            c.execute("DELETE FROM local_sent_idempotency WHERE expires_at_ms<=?",&[json!(now)])?;
            if let Some(old)=c.query("SELECT * FROM local_sent_idempotency WHERE hub=? AND dedup_id=?",&[json!(hub),json!(dedup)])?.into_iter().next(){if old["request_hash"]!=request_hash{return Err("IDEMPOTENCY_KEY_CONFLICT: Different payload".into());}return Ok(old);}
            let id=format!("sig_{}",random_uuid_v4().replace('-',""));c.execute("INSERT INTO local_sent_idempotency(hub,dedup_id,request_hash,signal_id,to_listener_id,status,expires_at_iso,expires_at_ms) VALUES(?,?,?,?,?,'PENDING_PUBLISH',?,?)",&[json!(hub),json!(dedup),json!(request_hash),json!(id),json!(canonical),json!(iso(now+1_800_000)),json!(now+1_800_000)])?;
            c.query("SELECT * FROM local_sent_idempotency WHERE hub=? AND dedup_id=?",&[json!(hub),json!(dedup)])?.into_iter().next().ok_or("Sent reservation missing".into())
        })?;
        let mut signal_id = string(&reservation, "signal_id").to_owned();
        let mut seq = reservation["seq"].as_u64().unwrap_or(0);
        if reservation["status"] == "PENDING_PUBLISH" {
            let human = caller.as_ref().is_some_and(|c| c["harness"] == "human");
            let published = if human {
                // The Hub owns reply publication and source acknowledgment in one
                // transaction. Human sends without a source use the same publication path.
                slot.rpc("send_reply_from_human", &json!({"to":canonical,"message":message,"in_reply_to":reply_to,"reply_signal_id":signal_id,"msg_id":dedup,"host_id":self.host_id,"host_name":local_host_name()}))?
            } else {
                let local_sender = match from_id.as_deref() {
                    Some(id) => self.state.listener(hub, id)?,
                    None => None,
                };
                let signal = json!({"signal_id":signal_id,"to":canonical,"target_generation":target["binding_generation"],"sender_endpoint_key":endpoint,"from":{"principal_id":slot.identity["principal_id"],"user":slot.identity["owner_user"],"host_id":self.host_id,"host_name":local_host_name(),"harness":caller.as_ref().map(|c|c["harness"].clone()),"listener_id":from_id.as_deref().map(|id| local_sender.as_ref().and_then(|s|s["public_id"].as_str()).filter(|s|!s.is_empty()).map(str::to_string).unwrap_or_else(|| super::identity::public_listener_id(string(&slot.identity,"owner_user"),id))),"title":caller.as_ref().map(|c|string(c,"title")).unwrap_or(""),"about":local_sender.as_ref().map(|s|s["about"].clone())},"message":message,"in_reply_to":reply_to});
                slot.rpc("js_publish",&json!({"subject":format!("cito.signal.{canonical}"),"data_b64":STANDARD.encode(serde_json::to_vec(&signal).map_err(|e|e.to_string())?),"opts":{"msgID":dedup}}))?
            };
            seq = published["seq"].as_u64().unwrap_or(0);
            if human {
                signal_id = published["signal_id"]
                    .as_str()
                    .ok_or("Published reply ID missing")?
                    .to_owned();
            }
            // Commit local read state with the sent reservation. If this fails after
            // the Hub commits, retry the RPC with the same key and committed signal ID.
            self.state.db.transaction(|c| {
                if human && reply_to.is_string() {
                    c.execute("UPDATE local_signals SET acknowledged_at_ms=COALESCE(acknowledged_at_ms,?),state=CASE WHEN state='RECEIVED' THEN 'STORED_PASSIVE' ELSE state END WHERE hub=? AND listener_id=? AND signal_id=?", &[published["acked_at_ms"].clone(),json!(hub),json!(from_id),reply_to.clone()])?;
                }
                c.execute("UPDATE local_sent_idempotency SET signal_id=?,seq=?,status='wake_accepted' WHERE hub=? AND dedup_id=?",&[json!(signal_id),json!(seq),json!(hub),json!(dedup)])?;
                Ok(())
            })?;
        }
        Ok(
            json!({"ok":true,"signal_id":self.scoped(hub,&signal_id),"seq":seq,"to":self.scoped(hub,string(target,"public_id")),"status":"wake_accepted","held_reason":null,"expires_at":reservation["expires_at_iso"]}),
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
        // Successful background RPCs or event reconnection supersede transport
        // errors, but cannot resolve a rejected configuration edit.
        self.last_hub_errors.retain(|hub, (_, transient)| {
            !*transient
                || self
                    .slots
                    .get(hub)
                    .is_none_or(|slot| slot.connection_status() != "connected")
        });
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
        // The registry owns background tasks; the per-hub owner serializes these
        // tasks with explicit reads and mutations outside the service mutex.
        if let Some(slot) = self
            .active_settings_hub
            .as_ref()
            .and_then(|hub| self.slots.get(hub))
            .filter(|slot| slot.local.is_none())
            .cloned()
        {
            let key = format!("channels:{}", slot.name);
            let inventory = self.channel_inventory(&slot);
            let (due, observed_gen) = {
                let cached = inventory.cached.lock().map_err(|e| e.to_string())?;
                (
                    cached.value["status"] == "loading"
                        || (cached.retry_at <= Instant::now()
                            && (cached.value["status"] == "error"
                                || cached.value["channels"].as_array().is_some_and(|channels| {
                                    channels.iter().any(|ch| {
                                        matches!(string(ch, "status"), "pairing_pending" | "error")
                                    })
                                }))),
                    cached.completed_gen,
                )
            };
            if due && !self.receivers.contains_key(&key) {
                self.receivers.insert(
                    key,
                    thread::spawn(move || {
                        let _ = inventory.refresh(&slot, observed_gen);
                    }),
                );
            }
        }
        for (hub, slot) in &self.slots {
            let key = format!("directory:{hub}");
            if !self.receivers.contains_key(&key)
                && (slot.config["authMode"] != "oidc" || slot.bearer.is_some())
            {
                let slot = slot.clone();
                let state = self.state.clone();
                let host_id = self.host_id.clone();
                self.receivers.insert(
                    key,
                    thread::spawn(move || {
                        if slot.directory().is_ok() && slot.local.is_none() {
                            let _ = Self::reconcile_unlistened(&state, &slot, &host_id);
                        }
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
            let default_hub = self.config.default_hub.clone();
            let plugin = self.plugins.find_by_id(string(&row, "harness")).cloned();
            self.receivers.insert(key,thread::spawn(move||{
                let channel=format!("{}:{}",host_id,string(&row,"harness"));
                if let Ok(Some((mut remote,revision)))=slot.kv_get(string(&row,"listener_id")){
                    if !super::registration::remote_matches_listener(&remote,&row,&channel){let _=state.fence_listener_for(&row,"fenced_before_dispatch");return;}
                    let _=state.db.execute("UPDATE local_listeners SET scope=? WHERE hub=? AND listener_id=? AND binding_generation=? AND owner_token=?", &[remote["scope"].clone(),row["hub"].clone(),row["listener_id"].clone(),row["binding_generation"].clone(),row["owner_token"].clone()]);
                    if let Some(ch) = remote["channels"]
                        .as_object_mut()
                        .and_then(|m| m.get_mut(&channel))
                        .and_then(Value::as_object_mut)
                    {
                        let updated = ch.get("updated_at_ms").and_then(Value::as_u64).unwrap_or(0);
                        if now_ms().saturating_sub(updated) >= 20_000 {
                            ch.insert("updated_at_ms".into(), json!(now_ms()));
                            let _ = slot.kv_save(string(&row, "listener_id"), &remote, Some(revision));
                        }
                    }
                }
                if let Err(error)=poll_listener(&state,&slot,&row,plugin.as_ref(),&state_dir, |signal| {
                    if let Some(ui) = &ui {
                        if !slot.closed.load(Ordering::SeqCst) {
                            let peers = address_peers(&state, &slot);
                            ui.emit(json!({"op":"banner","payload":{
                                "target_harness":row["harness"],"target_session_id":row["session_id"],
                                "target_title":row["title"],"target_cwd":row["cwd"],
                                "from_user":signal["from"]["user"],"from_host":signal["from"]["host_name"],
                                "from_title":string(&signal["from"],"title"),"message":signal["message"],
                                "effective_wake":signal["target_generation"]==row["binding_generation"],
                                "signal_id":format!("{id_prefix}{}",string(signal,"signal_id")),"hub":slot.name,
                                "from_harness":signal["from"]["harness"],
                                "from_scoped_id":signal["from"]["listener_id"].as_str().filter(|id|!id.is_empty()).map(|id|format!("{id_prefix}{id}")).or_else(|| (signal["from"]["harness"]=="human").then(||format!("{id_prefix}@{}",string(&signal["from"],"user")))),
                                "from_display_address":sender_display_address(&slot.name,&default_hub,signal["from"]["listener_id"].as_str(),string(&signal["from"],"user"),string(&signal["from"],"harness"),&peers),
                                "target_display_address":super::identity::display_address(&slot.name,&default_hub,&row,&peers),"count":1
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
        let hub = self
            .active_settings_hub
            .as_deref()
            .filter(|h| self.slots.contains_key(*h))
            .unwrap_or(&self.config.default_hub);
        self.settings_for_hub(hub)
    }
    fn settings_for_hub(&self, channel_hub: &str) -> Value {
        let apps:Vec<Value>=self.plugins.agent_plugins().into_iter().map(|p| {
            let inspection = super::setup::try_inspect_plugin_setup(p);
            if let Err(error) = &inspection {
                return json!({"id":p.id,"display_name":p.display_name,"tier":p.tier,"installed":false,"paths":[],"configured":Value::Null,"status":"error","detail":error});
            }
            let (installed,configured_in_host,paths)=inspection.unwrap_or_default();
            let repair=configured_in_host && (!self.config.state_dir.join("bin/cito-mcp").is_file() || !super::setup::is_staged_plugin_intact(&self.config.state_dir, p));
            let configured=configured_in_host && !repair;
            let detail=if repair { "Staged Cito files missing — click Repair to restore" }
                else if configured { p.post_setup_hint.as_deref().unwrap_or("Connected and ready") }
                else if self.last_app_action.get(&p.id).map(String::as_str)==Some("disconnect") { p.post_disconnect_hint.as_deref().unwrap_or("Disconnected · Click Set Up to reconnect") }
                else if installed { "Click Set Up to connect" } else { "Not detected; Set Up to pre-configure" };
            json!({"id":p.id,"display_name":p.display_name,"tier":p.tier,"installed":installed,"configured":configured,"paths":paths,
                "status":if repair{"repair_needed"}else if configured{"ready"}else{"not_configured"},"detail":detail})
        }).collect();
        let mut hubs = Vec::new();
        let mut channels = Vec::new();
        for (name, slot) in &self.slots {
            hubs.push(json!({"name":name,"url":slot.config["url"],"auth_mode":slot.config["authMode"],"enabled":true,"status":wire_status(&slot.connection_status()),"user":slot.identity["owner_user"],"is_default":name==&self.config.default_hub,"error":null}));
            if name == channel_hub {
                let inventory = if slot.local.is_some() {
                    slot.rpc("human_channels_list", &json!({}))
                        .unwrap_or_else(|e| json!({"status":"error","detail":e}))
                } else {
                    self.channel_inventories
                        .get(name)
                        .filter(|entry| entry.identity == remote_channels_identity(slot))
                        .and_then(|entry| {
                            entry.cached.lock().ok().map(|cached| cached.value.clone())
                        })
                        .unwrap_or_else(|| json!({"status":"loading"}))
                };
                channels = inventory["channels"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if inventory["status"] == "error" {
                    channels.push(json!({"id":"inventory","display_name":"Human Channels","sf_symbol":"exclamationmark.triangle","tint":"red","enabled":false,"configured":false,"hub_provided":false,"pairing_supported":false,"fields":[],"status":"error","detail":inventory["detail"]}));
                }
            }
        }
        let raw = self.raw_config().unwrap_or_else(|_| json!({}));
        if let Ok((_, configured, _)) =
            crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))
        {
            for (name, cfg) in &configured {
                if !self.slots.contains_key(name) {
                    hubs.push(json!({"name":name,"url":cfg["url"],"auth_mode":cfg.get("authMode").or_else(||cfg.get("auth_mode")).or_else(||cfg.get("auth")).and_then(Value::as_str).unwrap_or("none").to_ascii_lowercase(),"enabled":cfg["enabled"].as_bool().unwrap_or(true),"status":"disconnected","user":self.config.effective_user(name).ok(),"is_default":name==&self.config.default_hub,"error":null}));
                }
            }
        }
        let host = raw
            .get("host_hub")
            .or_else(|| raw.get("hostHub"))
            .unwrap_or(&Value::Null);
        let port = self
            .host_server
            .as_ref()
            .map(|server| u64::from(server.port()))
            .unwrap_or_else(|| host["port"].as_u64().unwrap_or(4291));
        for item in &mut hubs {
            item["error"] = json!(self
                .last_hub_errors
                .get(string(item, "name"))
                .map(|(error, _)| error));
            let cfg = self
                .slots
                .get(string(item, "name"))
                .map(|s| &s.config)
                .or_else(|| raw["hubs"].get(string(item, "name")));
            if let Some(cfg) = cfg {
                item["issuer"] = cfg
                    .get("issuer")
                    .or_else(|| cfg.get("oidcIssuer"))
                    .cloned()
                    .unwrap_or(Value::Null);
                item["client_id"] = cfg
                    .get("clientId")
                    .or_else(|| cfg.get("client_id"))
                    .or_else(|| cfg.get("oidcClientId"))
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            item["connected"] = json!(item["enabled"] != false && item["status"] == "connected");
        }
        let token = string(host, "token");
        let endpoint = |host: &str| {
            let authority = if host.contains(':') {
                format!("[{host}]")
            } else {
                host.to_owned()
            };
            format!("http://{authority}:{port}")
        };
        let endpoint_url = endpoint(&advertised_hub_host(string(host, "bind")));
        let invite = |url: &str| {
            let name = hub_name_from_url(url);
            let mut command = format!("cito setup --hub {name} --url {url}");
            if host_hub_auth_mode(host) == "token" && !token.is_empty() {
                command.push_str(&format!(" --auth token --token {token}"));
            }
            command.push_str(" --default");
            command
        };
        let invite_command = invite(&endpoint_url);
        let tailscale_url = tailscale_advertised_host().map(|host| endpoint(&host));
        let lan_url = endpoint(&lan_advertised_host(string(host, "bind")));
        let tailscale_invite_command = tailscale_url.as_deref().map(invite);
        let lan_invite_command = invite(&lan_url);
        json!({"apps":apps,"channel_hub":channel_hub,"settings_hub":channel_hub,"channels":channels,"hubs":hubs,"host_hub":{"enabled":host["enabled"].as_bool().unwrap_or(false),"running":self.host_server.is_some(),"port":port,"endpoint_url":endpoint_url,"tailscale_url":tailscale_url,"tailscale_invite_command":tailscale_invite_command,"lan_url":lan_url,"lan_invite_command":lan_invite_command,"token":token,"invite_command":invite_command,"auth_mode":host_hub_auth_mode(host),"error":self.last_host_hub_error},"launch_at_login":inspect_launch_at_login(&self.config.home_dir),"desktop_banner_enabled":self.desktop_banner_enabled()})
    }
    fn channel_inventory(&mut self, slot: &HubClient) -> Arc<ChannelInventory> {
        let identity = remote_channels_identity(slot);
        let entry = self
            .channel_inventories
            .entry(slot.name.clone())
            .or_insert_with(|| Arc::new(ChannelInventory::new(identity.clone())));
        if entry.identity != identity {
            *entry = Arc::new(ChannelInventory::new(identity));
        }
        entry.clone()
    }
    fn finish_channel_action(&self, hub: &str, action: &Value, result: &Result<Value, String>) {
        let id = channel_id(action).unwrap_or_default();
        let rpc = channel_rpc(action);
        match result {
            Ok(result) if matches!(rpc, "human_channel_pair_start" | "human_channels_test") => {
                let mut frame = result.clone();
                frame["op"] = json!(if rpc == "human_channel_pair_start" { "pair_code" } else { "channel_test_result" });
                frame["channel_id"] = json!(id);
                frame["hub"] = json!(hub);
                self.emit(frame);
            }
            Err(error) => self.emit(json!({"op":"channel_test_result","channel_id":id,"hub":hub,"ok":false,"error":error,"detail":error})),
            _ => {}
        }
        self.emit(json!({"op":"settings","hub":hub,"channel_id":id,"ok":result.is_ok(),"source_op":action["op"],"request_id":action["request_id"],"settings":self.settings_for_hub(hub)}));
        self.push_ui();
    }
    fn display_address(&self, hub: &str, row: &Value) -> String {
        let peers = self
            .slots
            .get(hub)
            .map(|s| address_peers(&self.state, s))
            .unwrap_or_default();
        super::identity::display_address(hub, &self.config.default_hub, row, &peers)
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
                row["target_display_address"] = if target.is_null() {
                    Value::Null
                } else {
                    json!(self.display_address(string(&row, "hub"), &target))
                };
                let source = row["from_listener_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                row["from_scoped_id"] = source
                    .as_deref()
                    .map(|id| json!(self.scoped(string(&row, "hub"), id)))
                    .unwrap_or_else(|| {
                        if row["from_harness"] == "human" {
                            json!(self.scoped(
                                string(&row, "hub"),
                                &format!("@{}", string(&row, "from_user"))
                            ))
                        } else {
                            Value::Null
                        }
                    });
                let peers = self
                    .slots
                    .get(string(&row, "hub"))
                    .map(|slot| address_peers(&self.state, slot))
                    .unwrap_or_default();
                row["from_display_address"] = json!(sender_display_address(
                    string(&row, "hub"),
                    &self.config.default_hub,
                    source.as_deref(),
                    string(&row, "from_user"),
                    string(&row, "from_harness"),
                    &peers
                ));
                row["message_truncated"] = json!(string(&row, "message").len() > 512);
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
        let settings = self.settings();
        let unconfigured_harnesses: HashSet<&str> = settings["apps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|app| app["configured"] == false && app["status"] != "error")
            .filter_map(|app| app["id"].as_str())
            .collect();
        let (activity, activity_total, activity_unread) = self
            .state
            .query_activity(&self.activity_filter, &self.activity_query)?;
        let unread = self.state.db.query(
            "SELECT COUNT(*) AS n FROM local_signals WHERE acknowledged_at_ms IS NULL",
            &[],
        )?[0]["n"]
            .as_u64()
            .unwrap_or(0);
        let mut sessions: Vec<Value> = self
            .state
            .active()?
            .into_iter()
            .filter(|row| self.slots.contains_key(string(row, "hub")))
            .collect();
        for row in &mut sessions {
            row["scoped_id"] = json!(self.scoped(string(row, "hub"), string(row, "public_id")));
            row["display_address"] = json!(self.display_address(string(row, "hub"), row));
        }
        let available = self
            .session_activity
            .as_ref()
            .map(|activity| activity.available(&sessions, self.slots.len()))
            .unwrap_or_default()
            .into_iter()
            .filter(|session| !unconfigured_harnesses.contains(string(session, "harness")))
            .collect::<Vec<_>>();
        let activity = self.activity_dto(activity);
        let mut remote = Vec::new();
        let mut online = 0;
        let now = now_ms();
        for (hub, slot) in &self.slots {
            for row in slot.cached_directory() {
                let own_principal = super::auth::principal_ids_equal(
                    string(&row, "principal_id"),
                    string(&slot.identity, "principal_id"),
                );
                if super::lease::evaluate_node_lease(&row, now) != "ACTIVE"
                    || (row["scope"] == "self" && !own_principal)
                {
                    continue;
                }
                online += 1;
                if own_principal
                    && !sessions
                        .iter()
                        .any(|s| s["hub"] == *hub && s["listener_id"] == row["listener_id"])
                {
                    remote.push(self.peer_dto(hub, &row));
                }
            }
        }
        let harnesses:Vec<Value>=self.plugins.agent_plugins().into_iter().enumerate().map(|(order,p)|json!({"kind":"agent","id":p.id,"display_name":p.display_name,"sf_symbol":p.sf_symbol,"tint":p.tint,"tier":p.tier,"order":order})).collect();
        let enabled_hubs: Vec<Value> = settings["hubs"]
            .as_array()
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        Ok(
            json!({"op":"state","status":if sessions.is_empty(){"idle"}else{"active"},"hubs":enabled_hubs,"hub":self.config.default_hub,"listeners":sessions,"sessions":sessions,"available":available,"my_remote_listeners":remote,"activity":activity,"activity_total":activity_total,"activity_unread_count":activity_unread,"activity_filter":self.activity_filter,"activity_query":self.activity_query,"unread_count":unread,"total_online":online,"directory_online_count":online,"harnesses":harnesses,"settings":settings}),
        )
    }
    fn peer_dto(&self, hub: &str, row: &Value) -> Value {
        json!({"hub":hub,"listener_id":row["listener_id"],"public_id":row["public_id"],"scoped_id":self.scoped(hub,string(row,"public_id")),"display_address":self.display_address(hub,row),"user":row["owner_user"],"host_id":row["host_id"],"host_name":row["host_name"],"harness":row["harness"],"title":row["title"],"about":row["about"],"scope":row["scope"],"last_seen_at":iso(now_ms()),"cwd":row["cwd"],"repo":row["repo"],"branch":row["branch"],"is_self_principal":self.slots.get(hub).is_some_and(|s|super::auth::principal_ids_equal(string(row,"principal_id"),string(&s.identity,"principal_id"))),"channels":row["channels"].as_object().map(|m|m.values().filter(|c| c["state"]=="ACTIVE").filter_map(|c| c["display_label"].as_str().or_else(|| c["host_name"].as_str()).or_else(|| c["display_name"].as_str())).filter(|s| !s.is_empty()).collect::<Vec<_>>()).unwrap_or_default()})
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
            if !raw.is_object() {
                return Err("INVALID_INPUT: Config must be an object".into());
            }
            action(&mut raw)?;
            let rendered = serde_json::to_string_pretty(&raw).map_err(|e| e.to_string())?;
            let (_, hubs, _) = crate::plugins::try_normalize_existing_config(Some(&rendered))
                .map_err(|error| format!("INVALID_INPUT: {error}"))?;
            for key in ["defaultHub", "default_hub"] {
                if raw
                    .get(key)
                    .is_some_and(|value| value.as_str().is_none_or(|name| !hubs.contains_key(name)))
                {
                    return Err("INVALID_INPUT: Invalid defaultHub".into());
                }
            }
            for cfg in hubs.values().chain(std::iter::once(&raw)) {
                if let Some(user) = cfg.get("user").filter(|user| !user.is_null()) {
                    if !user.as_str().is_some_and(crate::plugins::validate_user) {
                        return Err("INVALID_INPUT: User must contain 1-64 ASCII letters, numbers, dots, underscores or hyphens".into());
                    }
                }
            }
            let tmp = self
                .config
                .config_path
                .with_extension(format!("json.{}.tmp", std::process::id()));
            fs::write(&tmp, rendered).map_err(|e| e.to_string())?;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
            fs::rename(tmp, &self.config.config_path).map_err(|e| e.to_string())?;
            Ok(())
        })
    }
    pub fn action(&mut self, action: &Value) -> Result<Value, String> {
        let mut resolved_hub = None;
        let result = self.action_inner(action, &mut resolved_hub);
        self.record_hub_result(action, &result, resolved_hub.as_deref());
        if result.is_ok() && matches!(string(action, "op"), "setup_save_hub" | "save_hub") {
            self.emit(json!({"op":"settings","ok":true,"source_op":action["op"],"request_id":action["request_id"],"settings":self.settings()}));
        }
        self.push_ui();
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
            if let Some(cancelled) = self.logins.remove(&hub) {
                cancelled.cancelled.store(true, Ordering::SeqCst);
            }
        }
        self.reload_slots()?;
        match op {
            "get_snapshot" => return self.snapshot(),
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
            "setup_apply" | "enable_app" => {
                let id = action["harness"]
                    .as_str()
                    .or_else(|| action["app"].as_str())
                    .ok_or("INVALID_INPUT: harness required")?;
                let p = self
                    .plugins
                    .find_by_id(id)
                    .ok_or("UNSUPPORTED_HARNESS: Unknown harness")?;
                ensure_cli_shims_native(&self.config.state_dir, &self.plugins)?;
                apply_plugin_setup(p, true, false)?;
                self.last_app_action.insert(id.to_owned(), "setup".into());
                if let Some(activity) = self.session_activity.as_mut() {
                    activity.refresh_discovery();
                }
                self.emit(json!({"op":"settings","ok":true,"source_op":action["op"],"request_id":action["request_id"],"settings":self.settings()}));
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
                let notice = p
                    .post_disconnect_hint
                    .clone()
                    .unwrap_or("Disconnected · Active listeners stopped".into());
                for row in self
                    .state
                    .active()?
                    .into_iter()
                    .filter(|r| r["harness"] == id)
                {
                    self.unlisten_id(string(&row, "hub"), string(&row, "listener_id"))?;
                }
                self.caller_cache
                    .lock()
                    .map_err(|_| "Caller cache lock poisoned")?
                    .retain(|_, v| v["harness"] != id);
                self.last_app_action
                    .insert(id.to_owned(), "disconnect".into());
                if let Some(activity) = self.session_activity.as_mut() {
                    activity.refresh_discovery();
                }
                self.emit(json!({"op":"settings","ok":true,"source_op":action["op"],"request_id":action["request_id"],"settings":self.settings()}));
                self.push_ui();
                return Ok(json!({"ok":true,"notice":notice}));
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
                ] {
                    if cfg.get(canonical).is_none() {
                        if let Some(value) = aliases.iter().find_map(|key| cfg.get(*key)).cloned() {
                            cfg[canonical] = value;
                        }
                    }
                }
                let editing = action["edit"] == true;
                let original = action["original_name"].as_str().unwrap_or(&hub).to_string();
                self.mutate_config(|v| {
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
                        if let Some(patch) = cfg.as_object() {
                            for (key, value) in patch {
                                if matches!(
                                    key.as_str(),
                                    "token" | "clientSecret" | "client_secret" | "oidcClientSecret"
                                ) && value.as_str().is_none_or(|s| s.is_empty())
                                {
                                    continue;
                                }
                                base[key] = value.clone();
                            }
                        }
                        cfg = base;
                    }
                    cfg["url"] = json!(crate::plugins::validate_nats_url(
                        string(&cfg, "url").trim()
                    )
                    .map_err(|error| format!("INVALID_INPUT: {error}"))?);
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
                        .unwrap_or("none")
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
                        super::login::Settings::new(&cfg)?;
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
                        ] {
                            m.remove(k);
                        }
                    }
                    if editing && original != name {
                        if !v["hubs"][&name].is_null() {
                            return Err("INVALID_INPUT: Hub name already exists".into());
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
            "setup_toggle_hub" | "hub_action" => {
                let enabled = if op == "setup_toggle_hub" {
                    action["enabled"].as_bool().unwrap_or(true)
                } else {
                    string(action, "action") != "disconnect"
                };
                if op == "hub_action" && action["action"] == "reconnect" {
                    if let Some(old) = self.slots.remove(&hub) {
                        old.closed.store(true, Ordering::SeqCst);
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
            }
            "test_hub" => return self.slot(&hub)?.rpc("account_info", &json!({})),
            "paste_hub_invite" => {
                let mut invite = parse_hub_invite(string(action, "invite"))?;
                let raw = self.raw_config()?;
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
                return self.action(&invite);
            }
            "generate_hub_invite" => {
                let mut invite = self.slot(&hub)?.config.clone();
                invite["name"] = json!(hub);
                return Ok(json!({"ok":true,"invite":invite}));
            }
            "send_signal" | "send_reply_from_human" => {
                let meta = json!({"expected_harness":"human","expected_session_id":"default"});
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
                    .and_then(|()| self.send(&hub, &send_args, &meta));
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
                    "SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL",
                    &[],
                )?;
                for row in rows {
                    let requested_hub = string(action, "hub");
                    if !requested_hub.is_empty() && row["hub"] != requested_hub {
                        continue;
                    }
                    if op == "mark_all_activity_read"
                        || action["ack_all"] == true
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
                        "SELECT message FROM local_signals WHERE hub=? AND signal_id=?",
                        &[json!(hub), action["signal_id"].clone()],
                    )?
                    .into_iter()
                    .next()
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
                    let rows = c.query("SELECT * FROM local_signals WHERE (?='' OR hub=?)", &[json!(selected),json!(selected)])?;
                    for row in rows {
                        if row["acknowledged_at_ms"].is_null() { c.execute("INSERT OR REPLACE INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms) VALUES(?,?,?,?,?)", &[row["hub"].clone(),row["listener_id"].clone(),row["signal_id"].clone(),json!(format!("{}:human",self.host_id)),json!(now_ms())])?; }
                        c.execute("DELETE FROM local_signals WHERE hub=? AND signal_id=?", &[row["hub"].clone(),row["signal_id"].clone()])?;
                    }
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
                for row in self
                    .state
                    .active()?
                    .into_iter()
                    .filter(|row| row["harness"] != "human")
                {
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
                let _registration = slot
                    .registration
                    .lock()
                    .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
                for attempt in 0..3 {
                    let (mut row, rev) = slot.kv_get(id)?.ok_or("LISTENER_NOT_FOUND")?;
                    if !row.is_object() {
                        return Err("INVALID_INPUT: Listener must be an object".into());
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
                let frame = json!({"op":"directory","hub":hub,"request_id":action["request_id"],"query":action["query"].as_str().unwrap_or(""),"scope":action["scope"].as_str().unwrap_or("all"),"harness":action["harness"].as_str().unwrap_or("all"),"matched_count":result["matched_count"],"online_count":result["online_count"],"peers":result["listeners"],"activity":activity,"activity_total":total,"activity_unread_count":unread,"activity_filter":self.activity_filter,"activity_query":self.activity_query,"unread_count":self.state.activity()?.1});
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
            "set_desktop_banner" => {
                self.mutate_config(|v| {
                    v["desktopBannerEnabled"] = action["enabled"].clone();
                    Ok(())
                })?;
            }
            "setup_disconnect_all" => {
                for row in self
                    .state
                    .active()?
                    .into_iter()
                    .filter(|row| action["purge_data"] == true || row["harness"] != "human")
                {
                    self.unlisten_id(string(&row, "hub"), string(&row, "listener_id"))?;
                }
                for p in self.plugins.agent_plugins() {
                    remove_plugin_setup(p)?;
                    self.last_app_action
                        .insert(p.id.clone(), "disconnect".into());
                }
                self.caller_cache
                    .lock()
                    .map_err(|_| "Caller cache lock poisoned")?
                    .clear();
                set_launch_at_login(
                    false,
                    &self.config.state_dir,
                    &self.config.home_dir,
                    &self.plugins,
                )?;
                if let Some(activity) = self.session_activity.as_mut() {
                    activity.refresh_discovery();
                }
                self.emit(json!({"op":"settings","settings":self.settings()}));
            }
            "setup_uninstall_all" | "uninstall_cito" => {
                for row in self.state.active()? {
                    let _ = self.unlisten_id(string(&row, "hub"), string(&row, "listener_id"));
                }
                self.uninstall = Some(action["purge_data"].as_bool().unwrap_or(false));
                request_shutdown();
            }
            "quit" | "quit_daemon" => request_shutdown(),
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
        if self.applied_host_config.as_ref() == Some(host) {
            // Restoring the applied config resolves reload errors, not rejected UI actions.
            if self.host_hub_reload_failed {
                self.last_host_hub_error = None;
                self.host_hub_reload_failed = false;
            }
            return Ok(());
        }
        match host_hub_server_config(host).and_then(|settings| {
            replace_host_hub_server(&self.local, &mut self.host_server, settings)
        }) {
            Ok(()) => {
                self.applied_host_config = Some(host.clone());
                self.last_host_hub_error = None;
                self.host_hub_reload_failed = false;
                Ok(())
            }
            Err(error) => {
                self.last_host_hub_error = Some(error.clone());
                self.host_hub_reload_failed = true;
                Err(error)
            }
        }
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

fn replace_host_hub_server(
    local: &HubEngine,
    current: &mut Option<HubServer>,
    settings: Option<(IpAddr, u16, Arc<HubAuth>)>,
) -> Result<(), String> {
    let Some((bind, port, auth)) = settings else {
        current.take();
        return Ok(());
    };
    // Validation has already resolved the IP. Port zero always requests a new socket.
    let same_socket = current.as_ref().is_some_and(|server| {
        port != 0 && server.address.port() == port && server.address.ip() == bind
    });
    let previous = if same_socket {
        let previous = current
            .as_ref()
            .map(|server| (server.address, server.auth.clone()));
        current.take();
        previous
    } else {
        None
    };
    match local.start_server(&bind.to_string(), port, auth) {
        Ok(server) => {
            *current = Some(server);
            Ok(())
        }
        Err(error) => {
            if let Some((address, auth)) = previous {
                *current = Some(
                    local
                        .start_server(&address.ip().to_string(), address.port(), auth)
                        .map_err(|rollback| {
                            format!("{error}; failed to restore host hub listener: {rollback}")
                        })?,
                );
            }
            Err(format!(
                "INVALID_INPUT: Cannot bind host hub listener: {error}"
            ))
        }
    }
}

fn host_hub_server_config(host: &Value) -> Result<Option<(IpAddr, u16, Arc<HubAuth>)>, String> {
    if !host.is_null() && !host.is_object() {
        return Err("INVALID_INPUT: Host hub must be an object".into());
    }
    if host.get("enabled").is_some_and(|v| !v.is_boolean()) {
        return Err("INVALID_INPUT: Host hub enabled must be a boolean".into());
    }
    if host.get("token").is_some_and(|v| !v.is_string()) {
        return Err("INVALID_INPUT: Host hub token must be a string".into());
    }
    let bind = match host.get("bind") {
        Some(value) => value
            .as_str()
            .filter(|bind| !bind.trim().is_empty())
            .ok_or("INVALID_INPUT: Host hub bind must be a non-empty string")?,
        None => "0.0.0.0",
    };
    let bind = bind
        .parse::<IpAddr>()
        .map_err(|_| "INVALID_INPUT: Invalid host hub bind address")?;
    let port = match host.get("port") {
        Some(value) => value
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .ok_or("INVALID_INPUT: Host hub port must be an integer from 0 to 65535")?,
        None => 4291,
    };
    let mode = host_hub_auth_mode(host);
    if host.get("auth_mode").is_some_and(|v| !v.is_string()) || !matches!(mode, "none" | "token") {
        return Err("INVALID_INPUT: Host hub auth_mode must be none or token".into());
    }
    if host["enabled"] != true {
        return Ok(None);
    }
    let auth = HubAuth::new(mode, host["token"].as_str().map(str::to_string), None, None)?;
    Ok(Some((bind, port, Arc::new(auth))))
}

fn host_hub_auth_mode(host: &Value) -> &str {
    host["auth_mode"].as_str().unwrap_or_else(|| {
        if string(host, "token").is_empty() {
            "none"
        } else {
            "token"
        }
    })
}

fn tailscale_host_from_status(status: &Value) -> Option<String> {
    if status["BackendState"] != "Running" || status["Self"]["Online"] == Value::Bool(false) {
        return None;
    }
    let dns = string(&status["Self"], "DNSName").trim_end_matches('.');
    if !dns.is_empty()
        && !dns.ends_with(".local")
        && status["CurrentTailnet"]["MagicDNSEnabled"] != Value::Bool(false)
    {
        return Some(dns.to_owned());
    }
    status["Self"]["TailscaleIPs"]
        .as_array()?
        .iter()
        .filter_map(|ip| ip.as_str()?.parse::<std::net::Ipv4Addr>().ok())
        .next()
        .map(|ip| ip.to_string())
}

fn probe_tailscale_status() -> Option<Value> {
    use std::os::{fd::OwnedFd, unix::net::UnixStream};
    let executable = [
        "/opt/homebrew/bin/tailscale",
        "/usr/local/bin/tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).is_file())
    .unwrap_or("tailscale");
    let (mut output, writer) = UnixStream::pair().ok()?;
    output.set_nonblocking(true).ok()?;
    let mut child = Command::new(executable)
        .args(["status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let mut failed = false;
        loop {
            match output.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    bytes.extend_from_slice(&buffer[..n]);
                    if bytes.len() > 4 * 1024 * 1024 {
                        failed = true;
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() || failed {
                    return None;
                }
                output
                    .take((4 * 1024 * 1024 - bytes.len()) as u64)
                    .read_to_end(&mut bytes)
                    .ok()?;
                return serde_json::from_slice(&bytes).ok();
            }
            Ok(None) if !failed && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5))
            }
            _ => {
                // Every timeout/read/wait failure reaps the owned CLI process.
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn tailscale_advertised_host() -> Option<String> {
    if let Ok(host) = std::env::var("CITO_TAILSCALE_HOST") {
        let host = host.trim().trim_end_matches('.');
        if !host.is_empty() {
            return Some(host.to_owned());
        }
    }
    // Unit tests use the pure JSON projection; integration fixtures are headless.
    if cfg!(test)
        || std::env::var("CITO_UI_HEADLESS").as_deref() == Ok("1")
        || std::env::var("CITO_DISABLE_TAILSCALE_PROBE").as_deref() == Ok("1")
    {
        return None;
    }
    static CACHE: Mutex<Option<(Instant, Option<String>)>> = Mutex::new(None);
    let mut cached = CACHE.lock().ok()?;
    if let Some((_, host)) = cached
        .as_ref()
        .filter(|(time, _)| time.elapsed() < Duration::from_secs(30))
    {
        return host.clone();
    }
    let host = probe_tailscale_status()
        .as_ref()
        .and_then(tailscale_host_from_status);
    *cached = Some((Instant::now(), host.clone()));
    host
}

fn advertised_hub_host(bind: &str) -> String {
    if !matches!(bind, "" | "0.0.0.0" | "::") {
        return bind.into();
    }
    if let Some(host) = tailscale_advertised_host() {
        return host;
    }
    lan_advertised_host(bind)
}

fn lan_advertised_host(bind: &str) -> String {
    if !matches!(bind, "" | "0.0.0.0" | "::") {
        return bind.into();
    }
    // A UDP connect selects the default route's source IPv4 without sending traffic.
    let routed_ip = UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|socket| {
            socket.connect("192.0.2.1:9").ok()?;
            socket.local_addr().ok().map(|address| address.ip())
        })
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified());
    if let Some(ip) = routed_ip {
        return ip.to_string();
    }
    let hostname = local_host_name();
    if !hostname.is_empty()
        && (hostname.as_str(), 0)
            .to_socket_addrs()
            .is_ok_and(|mut addresses| {
                addresses.any(|addr| !addr.ip().is_loopback() && !addr.ip().is_unspecified())
            })
    {
        return hostname;
    }
    "127.0.0.1".into()
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
        } else if super::identity::readable_alias(address).is_none() {
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
     {"name":"cito_find","description":"Find opted-in active nodes.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"},"query":{"type":"string","maxLength":200},"user":{"type":"string","maxLength":64},"host":{"type":"string","maxLength":128},"harness":{"type":"string","pattern":"^[a-z0-9][a-z0-9_-]{0,31}$"},"repo":{"type":"string","maxLength":200},"limit":{"type":"integer","minimum":1,"maximum":50}}}},
     {"name":"cito_send","description":"Send a lightweight control plane signal of 1..65,536 Unicode code points. Prefer external channels for long debates or large artifacts.","inputSchema":{"type":"object","properties":{"to":{"type":"string","pattern":"^(?:[a-z0-9][a-z0-9_-]{0,31}:)?(?:c_(?:[a-f0-9]{16}|[a-z2-7]{12})|[a-z0-9]+(?:-[a-z0-9]+)*-(?:[a-f0-9]{16}|[a-z2-7]{12})|@[a-zA-Z0-9._-]{1,64}|[a-zA-Z0-9._-]{1,64}(?:/[a-z0-9][a-z0-9_-]{0,31})?#[a-z0-9]{1,16})$"},"message":{"type":"string","minLength":1,"maxLength":65536},"hub":{"type":"string"},"in_reply_to":{"type":"string","minLength":1,"maxLength":161},"idempotency_key":{"type":"string","minLength":1,"maxLength":128}},"required":["to","message"]}},
     {"name":"cito_inbox","description":"Read and acknowledge this node's inbox.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"},"ack_all":{"type":"boolean"},"ack_ids":{"type":"array","items":{"type":"string","minLength":1,"maxLength":161}},"limit":{"type":"integer","minimum":1,"maximum":1000}}}}
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
fn hub_name_from_url(raw: &str) -> String {
    let candidate = url::Url::parse(raw.trim())
        .ok()
        .and_then(|url| match url.host()? {
            url::Host::Domain(host) => host
                .split('.')
                .find(|label| label.chars().any(|c| c.is_ascii_alphanumeric()))
                .map(str::to_owned),
            url::Host::Ipv4(ip) => {
                let octets = ip.octets();
                Some(
                    if ip.is_private() {
                        "lan"
                    } else if octets[0] == 100 && (64..=127).contains(&octets[1]) {
                        "tailscale"
                    } else {
                        "remote"
                    }
                    .into(),
                )
            }
            _ => None,
        })
        .unwrap_or_else(|| "remote".into());
    normalize_slug(&candidate)
}

fn parse_hub_invite(raw: &str) -> Result<Value, String> {
    let raw = raw.trim();
    let invalid = || "INVALID_INPUT: Invalid hub invite".to_owned();
    let mut value = if raw.starts_with('{') {
        let value: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
        if !value.is_object() {
            return Err(invalid());
        }
        value
    } else if raw.starts_with("http://") || raw.starts_with("https://") {
        let url = url::Url::parse(raw).map_err(|_| invalid())?;
        url.host_str().ok_or_else(invalid)?;
        json!({"url":raw,"auth_mode":"none"})
    } else {
        // Join codes generated by Cito contain no shell quoting or expansion.
        // Reject malformed/unknown options rather than executing clipboard text.
        let mut words = raw.split_whitespace();
        if words.next() != Some("cito") || words.next() != Some("setup") {
            return Err(invalid());
        }
        let mut value = json!({});
        while let Some(word) = words.next() {
            if word == "--default" {
                value["set_default"] = json!(true);
                continue;
            }
            let (option, argument) = match word.split_once('=') {
                Some(pair) => pair,
                None => (word, words.next().ok_or_else(invalid)?),
            };
            let key = match option {
                "--hub" => "name",
                "--url" => "url",
                "--auth" => "auth_mode",
                "--token" => "token",
                "--issuer" => "issuer",
                "--client-id" => "client_id",
                "--user" => "user",
                _ => return Err(invalid()),
            };
            if argument.is_empty() || argument.starts_with("--") {
                return Err(invalid());
            }
            value[key] = json!(argument);
        }
        value
    };
    if value["name"].is_null() {
        value["name"] = value["hub"].clone();
    }
    // Leave absent names for setup_save_hub to derive and disambiguate under
    // the configuration lock. Explicit names use the same normalization there.
    if !string(&value, "name").is_empty()
        && !string(&value, "name")
            .chars()
            .any(|c| c.is_ascii_alphanumeric())
    {
        return Err(invalid());
    }
    if string(&value, "url") != "local://" {
        let url = url::Url::parse(string(&value, "url")).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(invalid());
        }
    }
    value["op"] = json!("setup_save_hub");
    if value["set_default"].is_null() {
        value["set_default"] = json!(true);
    }
    Ok(value)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChannelActionKind {
    Read,
    Mutation,
}
fn channel_action_kind(action: &Value) -> Option<ChannelActionKind> {
    match string(action, "op") {
        "open_settings" | "settings_select_hub" => Some(ChannelActionKind::Read),
        "channel_save"
        | "save_hub_service"
        | "configure_human_channel"
        | "disconnect_human_channel"
        | "channel_toggle"
        | "channel_unpair"
        | "channel_cancel_pair"
        | "clear_channel_service"
        | "channel_test"
        | "test_human_channel"
        | "channel_pair"
        | "pair_human_channel" => Some(ChannelActionKind::Mutation),
        _ => None,
    }
}
fn channel_id(action: &Value) -> Result<&str, String> {
    action["channel_id"]
        .as_str()
        .or_else(|| action["plugin_id"].as_str())
        .ok_or_else(|| "INVALID_INPUT: channel_id required".into())
}
fn channel_rpc(action: &Value) -> &'static str {
    if action["op"] == "save_hub_service" || action["scope"] == "hub" {
        "human_channels_configure"
    } else {
        match string(action, "op") {
            "channel_pair" | "pair_human_channel" => "human_channel_pair_start",
            "channel_test" | "test_human_channel" => "human_channels_test",
            _ => "human_channels_configure",
        }
    }
}
fn run_channel_action(
    slot: &HubClient,
    plugins: &LoadedPlugins,
    action: &Value,
) -> Result<Value, String> {
    let id = channel_id(action)?;
    let op = string(action, "op");
    let mut params = action.clone();
    params["plugin_id"] = json!(id);
    if let Some(values) = action["values"].as_object() {
        // The selected Hub owns the schema, including remote-only plugins
        // and remote schemas that differ from a locally installed plugin.
        let schema = slot.rpc("human_channels_list", &json!({}))?;
        let fields = schema["channels"]
            .as_array()
            .and_then(|cs| cs.iter().find(|ch| ch["id"] == id))
            .and_then(|ch| ch["fields"].as_array())
            .cloned()
            .or_else(|| plugins.find_by_id(id).map(|p| p.fields.clone()))
            .ok_or_else(|| format!("INVALID_INPUT: Unknown human channel plugin: {id}"))?;
        let mut hub_config = Map::new();
        let mut user_config = Map::new();
        for field in &fields {
            let key = string(field, "key");
            if let Some(value) = values.get(key) {
                if field["scope"] == "hub" {
                    hub_config.insert(key.into(), value.clone());
                } else {
                    user_config.insert(key.into(), value.clone());
                }
            }
        }
        if !hub_config.is_empty() {
            params["hub_config"] = Value::Object(hub_config);
        }
        if !user_config.is_empty() {
            params["user_config"] = Value::Object(user_config);
        }
    }
    if op == "channel_cancel_pair" {
        params["cancel_pair"] = json!(true);
    }
    if op == "channel_unpair" || (op == "disconnect_human_channel" && action["reset_hub"] != true) {
        params["reset_user"] = json!(action["reset_user"] != false);
    }
    if op == "clear_channel_service" {
        params["reset_hub"] = json!(true);
        params["reset_user"] = json!(true);
    }
    if op == "save_hub_service" || action["scope"] == "hub" {
        // Hub-only saves cannot create, edit, or resume a personal binding.
        let hub_config = params
            .get("hub_config")
            .cloned()
            .unwrap_or_else(|| json!({}));
        params = json!({"plugin_id":id,"hub_config":hub_config});
    }
    let rpc = channel_rpc(action);
    let result = slot.rpc(rpc, &params)?;
    if matches!(rpc, "human_channel_pair_start" | "human_channels_test") && !result.is_object() {
        Err("INVALID_INPUT: Channel result must be an object".into())
    } else {
        Ok(result)
    }
}
fn dispatch_channel_action(
    service: &Arc<Mutex<DaemonService>>,
    action: &Value,
    kind: ChannelActionKind,
) -> Result<Value, String> {
    let (slot, inventory, plugins, observed_gen) = {
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        if s.closing || shutdown_requested() {
            return Err("Shutting down".into());
        }
        if kind == ChannelActionKind::Mutation {
            channel_id(action)?;
        }
        s.reload_slots()?;
        let hub = action["hub"]
            .as_str()
            .unwrap_or(&s.config.default_hub)
            .to_string();
        s.active_settings_hub = Some(hub.clone());
        let slot = match s.slot(&hub).cloned() {
            Ok(slot) => slot,
            Err(error) => {
                if kind == ChannelActionKind::Read {
                    // Settings still opens for disabled hubs so they can be restored.
                    s.emit(json!({"op":"settings","hub":hub,"settings":s.settings_for_hub(&hub)}));
                    return Ok(json!({"ok":true}));
                }
                let result = Err(error);
                s.finish_channel_action(&hub, action, &result);
                return result;
            }
        };
        let inventory = s.channel_inventory(&slot);
        // Pure reads can use the cached projection while this slot's owner is
        // busy. Its completion publishes fresh settings, without another RPC.
        if kind == ChannelActionKind::Read
            && matches!(
                inventory.serial.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            )
        {
            s.emit(json!({"op":"settings","hub":hub,"settings":s.settings_for_hub(&hub)}));
            return Ok(json!({"ok":true}));
        }
        let observed_gen = inventory
            .cached
            .lock()
            .map_err(|e| e.to_string())?
            .completed_gen;
        (slot, inventory, s.plugins.clone(), observed_gen)
    };
    // All waits and channel I/O use the shared per-hub owner without service.
    let result = match kind {
        ChannelActionKind::Read => inventory
            .refresh(&slot, observed_gen)
            .map(|_| json!({"ok":true})),
        ChannelActionKind::Mutation => inventory.mutate(&slot, &plugins, action),
    };
    let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
    if s.closing || shutdown_requested() {
        return Err("Shutting down".into());
    }
    s.reload_slots()?;
    if slot.closed.load(Ordering::SeqCst)
        || !s
            .slots
            .get(&slot.name)
            .is_some_and(|current| remote_channels_identity(current) == inventory.identity)
    {
        let result = Err("NATS_UNAVAILABLE: Hub changed during channel request".into());
        if kind == ChannelActionKind::Mutation {
            s.finish_channel_action(&slot.name, action, &result);
        }
        return result;
    }
    if kind == ChannelActionKind::Mutation {
        s.finish_channel_action(&slot.name, action, &result);
    } else {
        s.emit(json!({"op":"settings","hub":slot.name,"settings":s.settings_for_hub(&slot.name)}));
    }
    result
}
fn interactive_login(action: &Value) -> bool {
    action["op"] == "login_hub"
        || (action["op"] == "hub_action"
            && matches!(action["action"].as_str(), Some("login" | "switch_account")))
}
fn dispatch_action(service: &Arc<Mutex<DaemonService>>, action: &Value) -> Result<Value, String> {
    let result = dispatch_action_inner(service, action);
    if let Ok(mut s) = service.lock() {
        if interactive_login(action) {
            s.record_hub_result(action, &result, None);
        }
        if let Err(error) = &result {
            s.emit(
                json!({"op":"error","error":error,"source_op":action["op"],"request_id":action["request_id"],"hub":action["hub"]}),
            );
        }
        s.push_ui();
    }
    result
}
fn dispatch_action_inner(
    service: &Arc<Mutex<DaemonService>>,
    action: &Value,
) -> Result<Value, String> {
    if let Some(kind) = channel_action_kind(action) {
        return dispatch_channel_action(service, action, kind);
    }
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
        super::login::recover_cancellable_with_options(
            &state_dir,
            &hub,
            &config,
            rejected,
            true,
            action["action"] == "switch_account" || action["force_interactive"] == true,
            &|| shutdown_requested() || attempt.cancelled.load(Ordering::SeqCst),
        )
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
            let mut line = Vec::new();
            match reader
                .by_ref()
                .take((REQUEST_LIMIT + 1) as u64)
                .read_until(b'\n', &mut line)
            {
                Ok(0) | Err(_) => break,
                _ => {}
            }
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.len() > REQUEST_LIMIT {
                // Discard the rest without allocating or interpreting a partial UTF-8 action.
                let drained = reader.skip_until(b'\n');
                if let Ok(s) = service.lock() {
                    let error = "INVALID_INPUT: Message is too large to send";
                    s.emit(json!({"op":"error","error":error}));
                }
                if drained.is_err() {
                    break;
                }
                continue;
            }
            if let Ok(action) = serde_json::from_slice::<Value>(&line) {
                let is_login = interactive_login(&action);
                let service = service.clone();
                let run = move || {
                    // Both UI stdout actions and HTTP control callers share the
                    // same closed-loop error frame and refreshed state emission.
                    let _ = dispatch_action(&service, &action);
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
        .truncate(false)
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
    // Configured, damaged built-ins remain visible for explicit Repair after a
    // restart. Fresh installs and healthy integrations still receive staged updates.
    let mut startup_plugins = plugins.clone();
    startup_plugins.plugins.retain(|plugin| {
        !plugin.is_builtin
            || plugin.kind != "agent-harness"
            || super::setup::is_staged_plugin_intact(&config.state_dir, plugin)
            || !inspect_plugin_setup(plugin).1
    });
    ensure_cli_shims_native(&config.state_dir, &startup_plugins)?;
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
    fn agent_stop_all_and_url_scoped_send_reservations_preserve_personal_state(
    ) -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("cito-daemon-unit-{}", random_uuid_v4()));
        fs::create_dir_all(dir.join("home")).map_err(|e| e.to_string())?;
        let config = NativeConfig {
            state_dir: dir.join("state"),
            config_path: dir.join("config.json"),
            home_dir: dir.join("home"),
            daemon_port: 0,
            user: "alice".into(),
            default_hub: "local".into(),
        };
        fs::write(&config.config_path, json!({"user":"alice","hubs":{"local":{"url":"local://"},"archive":{"url":"https://hub.example.com","enabled":false}}}).to_string()).map_err(|e| e.to_string())?;
        let plugins = LoadedPlugins {
            plugins: vec![],
            rejected_ids: HashSet::new(),
        };
        let mut service = DaemonService::open(&config, &plugins)?;
        service.local.set_plugin_roots(&[])?;
        let result = (|| {
            for harness in ["human", "fixture"] {
                super::super::registration::register_listener(
                    &service.state,
                    service.slot("local")?,
                    &service.host_id,
                    &json!({"about":"Receiver","scope":"workspace"}),
                    &json!({"harness":harness,"session_id":"default","title":"Receiver"}),
                )?;
            }
            let target = service
                .state
                .active()?
                .into_iter()
                .find(|row| row["harness"] == "fixture")
                .ok_or("Missing agent")?;
            let args = json!({"to":target["listener_id"],"message":"Review ready","idempotency_key":"same"});
            let first = service.send("local", &args, &json!({}))?;
            assert_eq!(
                service.send("local", &args, &json!({}))?["signal_id"],
                first["signal_id"]
            );
            // Keep the embedded transport hermetic while changing the target URL input
            // used by the real send reservation path under the same Hub name.
            service
                .slots
                .get_mut("local")
                .ok_or("Missing local slot")?
                .config["url"] = json!("https://hub.example.com");
            let second = service.send("local", &args, &json!({}))?;
            assert_ne!(second["signal_id"], first["signal_id"]);
            assert_eq!(
                service
                    .state
                    .db
                    .query("SELECT * FROM local_sent_idempotency", &[])?
                    .len(),
                2
            );
            service
                .slots
                .get_mut("local")
                .ok_or("Missing local slot")?
                .config["url"] = json!("local://");
            service.action(&json!({"op":"unlisten_all"}))?;
            let remaining = service.state.active()?;
            assert_eq!(remaining.len(), 1);
            assert_eq!(remaining[0]["harness"], "human");
            assert_eq!(
                service
                    .slot("local")?
                    .kv_get(string(&remaining[0], "listener_id"))?
                    .ok_or("Missing personal registration")?
                    .0["human_paused"],
                false
            );
            let settings = service.settings_for_hub("local");
            let disabled = settings["hubs"]
                .as_array()
                .ok_or("Missing hubs")?
                .iter()
                .find(|hub| hub["name"] == "archive")
                .ok_or("Missing disabled Hub")?;
            assert_eq!(disabled["user"], service.config.effective_user("archive")?);
            let personal = remaining[0].clone();
            let personal_signal = service.send(
                "local",
                &json!({"to":personal["listener_id"],"message":"Unread personal notification"}),
                &json!({}),
            )?;
            service.listen(
                "local",
                &json!({"about":"Receiver"}),
                &json!({"harness":"fixture","session_id":"default","title":"Receiver"}),
            )?;
            let client = service.slot("local")?.clone();
            client.closed.store(true, Ordering::SeqCst);
            service.unlisten_id("local", string(&personal, "listener_id"))?;
            service.unlisten_id("local", string(&target, "listener_id"))?;
            assert_eq!(
                LocalState::open(&config.state_dir)?
                    .db
                    .query("SELECT * FROM pending_hub_detaches", &[])?
                    .len(),
                2
            );
            assert_eq!(
                service
                    .state
                    .listener("local", string(&personal, "listener_id"))?
                    .ok_or("Missing paused listener")?["status"],
                "UNLISTENED"
            );
            service.ensure_human_nodes()?; // Failed reconciliation retains local intent.
            client.closed.store(false, Ordering::SeqCst);
            assert_eq!(
                client
                    .kv_get(string(&personal, "listener_id"))?
                    .ok_or("Missing remote listener")?
                    .0["human_paused"],
                false
            );
            service.ensure_human_nodes()?;
            assert_eq!(
                client
                    .kv_get(string(&personal, "listener_id"))?
                    .ok_or("Missing paused registration")?
                    .0["human_paused"],
                true
            );
            assert!(service
                .state
                .db
                .query("SELECT * FROM pending_hub_detaches", &[])?
                .is_empty());
            assert_eq!(
                client
                    .kv_get(string(&target, "listener_id"))?
                    .ok_or("Missing agent registration")?
                    .0["state"],
                "UNLISTENED"
            );
            assert!(service.local.rpc(
                "stream_get_message",
                &json!({"stream":"CITO_SIGNALS","seq":personal_signal["seq"]}),
                None
            )?["acked_at_ms"]
                .is_null());
            let other = LocalState::open(&dir.join("other-mac"))?;
            super::super::registration::register_listener(
                &other,
                &client,
                "other-mac",
                &json!({"about":"Receiver"}),
                &json!({"harness":"human","session_id":"default","title":"Receiver"}),
            )?;
            service.ensure_human_nodes()?;
            assert_eq!(
                client
                    .kv_get(string(&personal, "listener_id"))?
                    .ok_or("Missing personal registration")?
                    .0["human_paused"],
                false,
                "A settled pause must not override another explicit resume"
            );
            client.closed.store(true, Ordering::SeqCst);
            service.unlisten_id("local", string(&personal, "listener_id"))?;
            client.closed.store(false, Ordering::SeqCst);
            assert!(service
                .state
                .db
                .query(
                    "SELECT * FROM local_signals WHERE acknowledged_at_ms IS NOT NULL",
                    &[]
                )?
                .is_empty());
            super::super::registration::register_listener(
                &service.state,
                &client,
                &service.host_id,
                &json!({"about":"Receiver"}),
                &json!({"harness":"human","session_id":"default","title":"Receiver"}),
            )?;
            service.ensure_human_nodes()?;
            assert_eq!(
                client
                    .kv_get(string(&personal, "listener_id"))?
                    .ok_or("Missing resumed registration")?
                    .0["human_paused"],
                false
            );
            assert!(service
                .state
                .db
                .query("SELECT * FROM pending_hub_detaches", &[])?
                .is_empty());
            assert!(service.local.rpc(
                "stream_get_message",
                &json!({"stream":"CITO_SIGNALS","seq":personal_signal["seq"]}),
                None
            )?["acked_at_ms"]
                .is_null());
            let client = HubClient::new(
                "local".into(),
                service.slot("local")?.config.clone(),
                string(&personal, "owner_user"),
                Some(service.local.clone()),
            );
            let mut raw = service.raw_config()?;
            raw["hubs"]["local"]["enabled"] = json!(false);
            fs::write(&config.config_path, raw.to_string()).map_err(|e| e.to_string())?;
            service.reload_slots()?;
            assert!(!service.slots.contains_key("local"));
            service.unlisten_id("local", string(&personal, "listener_id"))?;
            assert_eq!(
                service
                    .state
                    .db
                    .query("SELECT hub_url FROM pending_hub_detaches", &[])?[0]["hub_url"],
                "local://"
            );

            raw["hubs"]["local"]["url"] = json!("https://other.example.com");
            fs::write(&config.config_path, raw.to_string()).map_err(|e| e.to_string())?;
            let mut edited = client.clone();
            // Preserve the embedded transport as a sentinel: URL fencing must
            // reject the detach before it can mutate even an available Hub.
            edited.config["url"] = raw["hubs"]["local"]["url"].clone();
            DaemonService::reconcile_unlistened(&service.state, &edited, &service.host_id)?;
            assert!(service
                .state
                .db
                .query("SELECT * FROM pending_hub_detaches", &[])?
                .is_empty());
            assert_eq!(
                client
                    .kv_get(string(&personal, "listener_id"))?
                    .ok_or("Missing personal registration")?
                    .0["human_paused"],
                false
            );

            // Historical unknown targets must never act as wildcard URLs.
            service.state.db.execute(
                "INSERT INTO pending_hub_detaches(hub,listener_id,hub_url) VALUES('local',?,NULL)",
                &[personal["listener_id"].clone()],
            )?;
            DaemonService::reconcile_unlistened(&service.state, &client, &service.host_id)?;
            assert!(service
                .state
                .db
                .query("SELECT * FROM pending_hub_detaches", &[])?
                .is_empty());
            assert_eq!(
                client
                    .kv_get(string(&personal, "listener_id"))?
                    .ok_or("Missing personal registration")?
                    .0["human_paused"],
                false
            );

            // Rejected work was discarded. Restoring the original URL permits
            // a fresh explicit pause to reconcile against its captured target.
            raw["hubs"]["local"]["url"] = json!("local://");
            fs::write(&config.config_path, raw.to_string()).map_err(|e| e.to_string())?;
            service.unlisten_id("local", string(&personal, "listener_id"))?;
            assert_eq!(
                service
                    .state
                    .db
                    .query("SELECT hub_url FROM pending_hub_detaches", &[])?[0]["hub_url"],
                "local://"
            );
            raw["hubs"]["local"]["enabled"] = json!(true);
            fs::write(&config.config_path, raw.to_string()).map_err(|e| e.to_string())?;
            service.reload_slots()?;
            assert_eq!(
                client
                    .kv_get(string(&personal, "listener_id"))?
                    .ok_or("Missing paused registration")?
                    .0["human_paused"],
                true
            );
            assert!(service
                .state
                .db
                .query("SELECT * FROM pending_hub_detaches", &[])?
                .is_empty());
            assert!(service.local.rpc(
                "stream_get_message",
                &json!({"stream":"CITO_SIGNALS","seq":personal_signal["seq"]}),
                None
            )?["acked_at_ms"]
                .is_null());
            Ok(())
        })();
        service.shutdown();
        fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
        result
    }

    #[test]
    fn local_enrollment_widens_snapshot_addresses_without_a_directory_query() -> Result<(), String>
    {
        let dir = std::env::temp_dir().join(format!("cito-addresses-{}", random_uuid_v4()));
        let config = NativeConfig {
            state_dir: dir.join("state"),
            config_path: dir.join("config.json"),
            home_dir: dir.join("home"),
            daemon_port: 0,
            user: "alice".into(),
            default_hub: "local".into(),
        };
        let plugins = LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        };
        let mut service = DaemonService::open(&config, &plugins)?;
        let result = (|| {
            for session in ["session-26", "session-324"] {
                service.listen(
                    "local",
                    &json!({"about":"Session"}),
                    &json!({"harness":"fixture","session_id":session,"title":"Review","cwd":""}),
                )?;
            }
            let snapshot = service.snapshot()?;
            for (session, address) in [
                ("session-26", "alice/fixture#a5705540"),
                ("session-324", "alice/fixture#a5706e4d"),
            ] {
                let row = snapshot["listeners"]
                    .as_array()
                    .ok_or("Missing listeners")?
                    .iter()
                    .find(|row| row["session_id"] == session)
                    .ok_or("Missing enrolled session")?;
                assert_eq!(row["display_address"], address);
                assert!(service
                    .slot("local")?
                    .cached_directory()
                    .iter()
                    .any(|peer| peer["listener_id"] == row["listener_id"]));
            }
            let id = listener_id("none#alice", "fixture", "session-324");
            for state in ["UNLISTENED", "ACTIVE"] {
                let slot = service.slot("local")?;
                let (mut stale, revision) = slot.kv_get(&id)?.ok_or("Missing collision peer")?;
                stale["state"] = json!(state);
                stale["updated_at_ms"] = json!(1);
                for channel in stale["channels"]
                    .as_object_mut()
                    .ok_or("Missing channels")?
                    .values_mut()
                {
                    channel["state"] = json!("UNLISTENED");
                    channel["updated_at_ms"] = json!(1);
                }
                slot.kv_save(&id, &stale, Some(revision))?;
                slot.directory()?;
                let merged = address_peers(&service.state, slot)
                    .into_iter()
                    .find(|peer| peer["listener_id"] == id)
                    .ok_or("Missing merged witness")?;
                assert_eq!(merged["channels"], stale["channels"]);
                assert!(super::super::identity::is_live_peer(&merged, now_ms()));
                let snapshot = service.snapshot()?;
                for (session, address) in [
                    ("session-26", "alice/fixture#a5705540"),
                    ("session-324", "alice/fixture#a5706e4d"),
                ] {
                    assert_eq!(
                        snapshot["listeners"]
                            .as_array()
                            .ok_or("Missing listeners")?
                            .iter()
                            .find(|row| row["session_id"] == session)
                            .ok_or("Missing session")?["display_address"],
                        address
                    );
                }
            }
            // A registration absent from the Hub cache still participates locally.
            service.local.rpc(
                "kv_delete",
                &json!({"key":id}),
                Some(&super::super::identity::principal("alice")),
            )?;
            service.slot("local")?.directory()?;
            let snapshot = service.snapshot()?;
            let first = snapshot["listeners"]
                .as_array()
                .ok_or("Missing listeners")?
                .iter()
                .find(|row| row["session_id"] == "session-26")
                .ok_or("Missing first session")?;
            assert_eq!(first["display_address"], "alice/fixture#a5705540");
            let activity = service.activity_dto(vec![json!({"hub":"local","listener_id":"missing","from_listener_id":"alice-abcd123456789012","from_user":"alice","from_harness":"fixture","message":"hello"})]);
            assert_eq!(activity[0]["from_display_address"], "alice/fixture#abcd");
            assert!(activity[0]["target_display_address"].is_null());
            Ok(())
        })();
        service.shutdown();
        fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
        result
    }
    #[test]
    fn sender_addresses_use_readable_metadata_before_directory_arrival() {
        for (hub, expected) in [
            ("local", "alice/fixture#abcd"),
            ("work", "work:alice/fixture#abcd"),
        ] {
            assert_eq!(
                sender_display_address(
                    hub,
                    "local",
                    Some("alice-abcd123456789012"),
                    "alice",
                    "fixture",
                    &[]
                )
                .as_deref(),
                Some(expected)
            );
        }
        assert_eq!(
            sender_display_address("local", "local", None, "alice", "fixture", &[]),
            None
        );
        assert_eq!(
            sender_display_address("local", "local", None, "alice", "human", &[]).as_deref(),
            Some("@alice")
        );
    }
    #[test]
    fn default_hub_repair_handles_missing_defaults_and_all_disabled_hubs() {
        for (mut config, expected) in [
            (
                json!({"hubs":{"alpha":{"enabled":true},"local":{"enabled":true}}}),
                "local",
            ),
            (
                json!({"defaultHub":"zeta","hubs":{"alpha":{},"local":{},"zeta":{}}}),
                "zeta",
            ),
            (
                json!({"default_hub":"missing","hubs":{"alpha":{"enabled":false},"local":{"disabled":true}}}),
                "local",
            ),
            (
                json!({"defaultHub":"missing","hubs":{"zeta":{"enabled":false},"alpha":{"disabled":true}}}),
                "alpha",
            ),
            (json!({"hubs":{}}), "local"),
        ] {
            repair_default_hub(&mut config);
            assert_eq!(config["default_hub"], expected);
            assert!(config.get("defaultHub").is_none());
        }
    }
    #[test]
    fn config_mutation_rejects_non_objects_under_lock() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("cito-config-shape-{}", random_uuid_v4()));
        let result = (|| {
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let config = NativeConfig {
                state_dir: dir.join("state"),
                config_path: dir.join("config.json"),
                home_dir: dir.join("home"),
                daemon_port: 0,
                user: "alice".into(),
                default_hub: "local".into(),
            };
            let plugins = LoadedPlugins {
                plugins: vec![],
                rejected_ids: Default::default(),
            };
            let mut service = DaemonService::open(&config, &plugins)?;
            let result = (|| {
                for raw in ["[]", "42", "true", "\"config\"", "null"] {
                    fs::write(&config.config_path, raw).map_err(|e| e.to_string())?;
                    assert_eq!(
                        service.mutate_config(|v| {
                            v["user"] = json!("bob");
                            Ok(())
                        }),
                        Err("INVALID_INPUT: Config must be an object".into())
                    );
                    assert_eq!(
                        fs::read_to_string(&config.config_path).map_err(|e| e.to_string())?,
                        raw
                    );
                }
                let before = r#"{"other":7}"#;
                fs::write(&config.config_path, before).map_err(|e| e.to_string())?;
                for candidate in [
                    json!({"hubs":[]}),
                    json!({"hubs":{"work":[]}}),
                    json!({"hubs":{"work":{"authMode":"invalid"}}}),
                    json!({"hubs":{"work":{"enabled":false,"user":"alice smith"}}}),
                    json!({"default_hub":"missing"}),
                    json!({"defaultHub":"missing"}),
                    json!({"defaultHub":"local","default_hub":"missing"}),
                    json!({"default_hub":"local","defaultHub":"missing"}),
                    json!({"default_hub":42}),
                    json!({"defaultHub":null}),
                    json!({"user":"alice smith"}),
                    json!({"user":42}),
                ] {
                    let error = service
                        .mutate_config(|v| {
                            *v = candidate;
                            Ok(())
                        })
                        .unwrap_err();
                    assert!(error.starts_with("INVALID_INPUT:"), "{error}");
                    assert_eq!(
                        fs::read_to_string(&config.config_path).map_err(|e| e.to_string())?,
                        before
                    );
                }
                service.mutate_config(|v| {
                    v["user"] = json!("bob");
                    Ok(())
                })?;
                assert_eq!(service.raw_config()?, json!({"other":7,"user":"bob"}));
                for key in ["defaultHub", "default_hub"] {
                    let mut candidate = json!({"hubs":{"work":{"url":"local://"}}});
                    candidate[key] = json!("work");
                    service.mutate_config(|v| {
                        *v = candidate.clone();
                        Ok(())
                    })?;
                    assert_eq!(service.raw_config()?, candidate);
                }
                for (requested, url, expected) in [
                    ("Office Mac", "https://office.example.com", "office-mac"),
                    ("Work", "local://", "work"),
                    ("", "https://node.example.com", "node"),
                    ("", "https://node.other.example.com", "node-2"),
                    ("", "https://local.example.com", "local-2"),
                    ("", "http://192.0.2.8:4291", "remote"),
                    ("", "http://[::1]:4291", "remote-2"),
                    ("", "http://192.168.1.2:4291", "lan"),
                    ("", "http://10.1.2.3:4291", "lan-2"),
                    ("", "http://172.16.0.2:4291", "lan-3"),
                    ("", "http://100.64.0.2:4291", "tailscale"),
                ] {
                    service.action(
                        &json!({"op":"setup_save_hub","name":requested,"url":url,"enabled":false}),
                    )?;
                    assert_eq!(service.raw_config()?["hubs"][expected]["url"], url);
                }
                service.mutate_config(|v| {
                    v["host_hub"] = json!({"bind":"::1","enabled":false});
                    Ok(())
                })?;
                let settings = service.settings();
                assert_eq!(settings["host_hub"]["endpoint_url"], "http://[::1]:4291");
                assert_eq!(settings["host_hub"]["lan_url"], "http://[::1]:4291");
                assert!(settings["host_hub"]["lan_invite_command"]
                    .as_str()
                    .unwrap()
                    .contains("--url http://[::1]:4291"));
                service.action(
                    &json!({"op":"setup_save_hub","name":"user","url":"local://","enabled":false}),
                )?;
                assert!(service.action(&json!({"op":"setup_save_hub","edit":true,"original_name":"user","name":"user","url":"http://"})).is_err());
                let user_error = service.last_hub_errors["user"].clone();
                for requested in ["!!!", "local", "Office Mac"] {
                    let before = fs::read(&config.config_path).map_err(|e| e.to_string())?;
                    assert!(service.action(&json!({"op":"setup_save_hub","name":requested,"url":"https://different.example.com","enabled":false})).is_err());
                    assert_eq!(
                        fs::read(&config.config_path).map_err(|e| e.to_string())?,
                        before
                    );
                }
                assert_eq!(service.last_hub_errors["user"], user_error);
                assert!(service.action(&json!({"op":"setup_save_hub","edit":true,"original_name":"work","name":"work","url":"http://"})).is_err());
                let work_error = service.last_hub_errors["work"].clone();
                for (op, requested, canonical, url) in [
                    (
                        "setup_save_hub",
                        Some("Office Mac"),
                        "office-mac",
                        "https://office.example.com",
                    ),
                    ("save_hub", None, "node-2", "https://node.other.example.com"),
                ] {
                    assert!(service.action(&json!({"op":op,"edit":true,"original_name":canonical,"name":canonical,"url":"http://"})).is_err());
                    let failed = service.settings_for_hub(canonical);
                    assert!(failed["hubs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|hub| hub["name"] == canonical)
                        .unwrap()["error"]
                        .is_string());
                    let mut retry = json!({"op":op,"url":url,"enabled":false});
                    if let Some(requested) = requested {
                        retry["name"] = json!(requested);
                    }
                    assert_eq!(service.action(&retry)?, json!({"ok":true}));
                    let recovered = service.settings_for_hub(canonical);
                    assert!(recovered["hubs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|hub| hub["name"] == canonical)
                        .unwrap()["error"]
                        .is_null());
                    assert_eq!(service.last_hub_errors["work"], work_error);
                }
                service.action(&json!({"op":"setup_save_hub","edit":true,"original_name":"office-mac","name":"Office Mac","url":"https://updated.example.com","enabled":false}))?;
                assert_eq!(
                    service.raw_config()?["hubs"]["office-mac"]["url"],
                    "https://updated.example.com"
                );
                Ok(())
            })();
            service.shutdown();
            result
        })();
        fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
        result
    }
    #[test]
    fn ui_acknowledgements_preserve_other_hubs_with_colliding_signal_ids() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("cito-ack-scope-{}", random_uuid_v4()));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let config = NativeConfig {
            state_dir: dir.join("state"),
            config_path: dir.join("config.json"),
            home_dir: dir.join("home"),
            daemon_port: 0,
            user: "alice".into(),
            default_hub: "local".into(),
        };
        fs::write(
            &config.config_path,
            json!({"hubs":{"local":{"url":"local://"},"work":{"url":"local://"}}}).to_string(),
        )
        .map_err(|e| e.to_string())?;
        let plugins = LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        };
        let mut service = DaemonService::open(&config, &plugins)?;
        let result = (|| {
            for hub in ["local", "work"] {
                service.state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) VALUES(?,'shared','node',1,1,'alice','host','','message','now',?,'RECEIVED')", &[json!(hub),json!(now_ms()+60_000)])?;
            }
            service.action(&json!({"op":"ack_signals","hub":"local","ack_all":true}))?;
            let state = service.state.clone();
            let unread = || {
                state.db.query(
                    "SELECT hub FROM local_signals WHERE acknowledged_at_ms IS NULL",
                    &[],
                )
            };
            assert_eq!(unread()?, vec![json!({"hub":"work"})]);
            service.action(&json!({"op":"ack_signals","signal_ids":["work:shared"]}))?;
            assert!(unread()?.is_empty());
            Ok(())
        })();
        service.shutdown();
        fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
        result
    }
    #[test]
    fn invalid_host_hub_reload_preserves_running_server() -> Result<(), String> {
        for (status, expected) in [
            (
                json!({"BackendState":"Running","Self":{"DNSName":"mac-studio.tailnet.ts.net.","TailscaleIPs":["100.64.0.2"]}}),
                Some("mac-studio.tailnet.ts.net"),
            ),
            (
                json!({"BackendState":"Running","Self":{"DNSName":"","TailscaleIPs":["fd7a:115c:a1e0::2","100.64.0.2"]}}),
                Some("100.64.0.2"),
            ),
            (
                json!({"BackendState":"Stopped","Self":{"DNSName":"offline.tailnet.ts.net."}}),
                None,
            ),
            (
                json!({"BackendState":"Running","Self":{"TailscaleIPs":["fd7a:115c:a1e0::2"]}}),
                None,
            ),
        ] {
            assert_eq!(tailscale_host_from_status(&status).as_deref(), expected);
            if status["BackendState"] == "Running" {
                let mut offline = status.clone();
                offline["Self"]["Online"] = json!(false);
                assert_eq!(tailscale_host_from_status(&offline), None);
                for magic in [false, true] {
                    let mut candidate = status.clone();
                    candidate["CurrentTailnet"] = json!({"MagicDNSEnabled":magic});
                    candidate["Self"]["DNSName"] = json!("studio.local.");
                    candidate["Self"]["TailscaleIPs"] = json!(["100.64.0.9"]);
                    assert_eq!(
                        tailscale_host_from_status(&candidate).as_deref(),
                        Some("100.64.0.9")
                    );
                    candidate["Self"]["DNSName"] = json!("studio.tailnet.ts.net.");
                    assert_eq!(
                        tailscale_host_from_status(&candidate).as_deref(),
                        Some(if magic {
                            "studio.tailnet.ts.net"
                        } else {
                            "100.64.0.9"
                        })
                    );
                }
            }
        }
        assert_eq!(advertised_hub_host("127.0.0.1"), "127.0.0.1");
        let dir = std::env::temp_dir().join(format!("cito-host-validation-{}", random_uuid_v4()));
        let result = (|| {
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let config = NativeConfig {
                state_dir: dir.join("state"),
                config_path: dir.join("config.json"),
                home_dir: dir.join("home"),
                daemon_port: 0,
                user: "alice".into(),
                default_hub: "local".into(),
            };
            let plugins = LoadedPlugins {
                plugins: vec![],
                rejected_ids: Default::default(),
            };
            let mut service = DaemonService::open(&config, &plugins)?;
            let result = (|| {
                let host = json!({"enabled":true,"bind":"127.0.0.1","port":0,"auth_mode":"token","token":"previous-token"});
                let mut committed = json!({"host_hub":host,"other":7}).to_string();
                fs::write(&config.config_path, &committed).map_err(|e| e.to_string())?;
                service.reload_slots()?;
                let address = service.host_server.as_ref().unwrap().address;
                service.reload_slots()?;
                assert_eq!(service.host_server.as_ref().unwrap().address, address);
                let rpc = |address, token| -> Result<(), String> {
                    let response: Value = ureq::post(&format!("http://{address}/v1/rpc"))
                        .set("Authorization", &format!("Bearer {token}"))
                        .timeout(Duration::from_secs(2))
                        .send_json(json!({"op":"kv_snapshot"}))
                        .map_err(|e| e.to_string())?
                        .into_json()
                        .map_err(|e| e.to_string())?;
                    assert_eq!(response["ok"], true);
                    assert!(response["result"]["entries"].is_array());
                    Ok(())
                };
                let occupied = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
                let occupied_port = occupied.local_addr().map_err(|e| e.to_string())?.port();
                for (key, value) in [
                    ("auth_mode", json!("invalid")),
                    ("token", json!(" ")),
                    ("port", json!(70000)),
                    ("bind", json!("")),
                    ("bind", json!("127.0.0.1\u{0000}")),
                    ("bind", json!("invalid-bind-host")),
                    ("bind", json!("not:an:ip:address")),
                    ("port", json!(occupied_port)),
                ] {
                    let mut invalid = host.clone();
                    invalid[key] = value.clone();
                    let reload = json!({"host_hub":invalid}).to_string();
                    fs::write(&config.config_path, &reload).map_err(|e| e.to_string())?;
                    service.reload_slots()?;
                    let settings = service.settings();
                    let error = settings["host_hub"]["error"].as_str().unwrap();
                    assert!(error.starts_with("INVALID_INPUT:"), "{error}");
                    assert_eq!(settings["host_hub"]["running"], true);
                    assert_eq!(settings["host_hub"]["port"], address.port());
                    assert_eq!(service.host_server.as_ref().unwrap().address, address);
                    assert_eq!(
                        fs::read(&config.config_path).map_err(|e| e.to_string())?,
                        reload.as_bytes()
                    );
                    rpc(address, "previous-token")?;

                    // Restoring the exact applied config clears reload feedback without rebinding.
                    fs::write(&config.config_path, &committed).map_err(|e| e.to_string())?;
                    service.reload_slots()?;
                    assert_eq!(service.settings()["host_hub"]["error"], Value::Null);
                    assert_eq!(service.host_server.as_ref().unwrap().address, address);
                    rpc(address, "previous-token")?;

                    let mut action = json!({"op":"set_host_hub","enabled":true});
                    action[key] = value;
                    let error = service.action(&action).unwrap_err();
                    assert!(error.starts_with("INVALID_INPUT:"), "{action}: {error}");
                    assert_eq!(service.settings()["host_hub"]["error"], error);
                    // An unchanged config cannot resolve a rejected UI action.
                    service.reload_slots()?;
                    assert_eq!(service.settings()["host_hub"]["error"], error);
                    assert_eq!(
                        fs::read(&config.config_path).map_err(|e| e.to_string())?,
                        committed.as_bytes()
                    );
                    assert_eq!(service.host_server.as_ref().unwrap().address, address);
                    rpc(address, "previous-token")?;
                }

                // A valid external edit supersedes the reload/action error without
                // abandoning the current socket or its working credentials.
                let mut valid = host.clone();
                valid["port"] = json!(address.port());
                committed = json!({"host_hub":valid,"other":7}).to_string();
                fs::write(&config.config_path, &committed).map_err(|e| e.to_string())?;
                service.reload_slots()?;
                assert_eq!(service.settings()["host_hub"]["error"], Value::Null);
                assert_eq!(service.host_server.as_ref().unwrap().address, address);
                rpc(address, "previous-token")?;

                // A write failure after binding must restore the old socket and credentials.
                let tmp = config
                    .config_path
                    .with_extension(format!("json.{}.tmp", std::process::id()));
                fs::create_dir(&tmp).map_err(|e| e.to_string())?;
                let result = service
                    .action(&json!({"op":"set_host_hub","port":0,"token":"uncommitted-token"}));
                fs::remove_dir(&tmp).map_err(|e| e.to_string())?;
                assert!(result.is_err());
                assert_eq!(
                    fs::read(&config.config_path).map_err(|e| e.to_string())?,
                    committed.as_bytes()
                );
                assert_eq!(service.host_server.as_ref().unwrap().address, address);
                rpc(address, "previous-token")?;

                // Updating credentials on the same socket must release and rebind that port.
                service.action(
                    &json!({"op":"set_host_hub","port":address.port(),"token":"replacement-token"}),
                )?;
                assert_eq!(service.host_server.as_ref().unwrap().address, address);
                rpc(address, "replacement-token")?;
                let rejected = ureq::post(&format!("http://{address}/v1/rpc"))
                    .set("Authorization", "Bearer previous-token")
                    .timeout(Duration::from_secs(2))
                    .send_json(json!({"op":"kv_snapshot"}));
                assert!(matches!(rejected, Err(ureq::Error::Status(401, _))));

                // Saving the current port still sends an explicit settings acknowledgement.
                let settings = service.settings();
                let ui = Arc::new(UiMailbox::default());
                service.ui = Some(ui.clone());
                service.action(&json!({"op":"set_host_hub","port":address.port()}))?;
                {
                    let frames = ui.pending.lock().map_err(|e| e.to_string())?;
                    assert!(frames.iter().any(|(_, bytes)| {
                        let frame: Value = serde_json::from_slice(bytes).unwrap();
                        frame["op"] == "settings" && frame["settings"] == settings
                    }));
                }

                // Port zero requests a distinct listener while the old socket is still held.
                service.action(&json!({"op":"set_host_hub","port":0}))?;
                let replacement = service.host_server.as_ref().unwrap().address;
                assert_ne!(replacement, address);
                rpc(replacement, "replacement-token")?;
                fs::write(
                    &config.config_path,
                    json!({"host_hub":{"enabled":false}}).to_string(),
                )
                .map_err(|e| e.to_string())?;
                service.reload_slots()?;
                assert!(service.host_server.is_none());
                assert!(TcpStream::connect(replacement).is_err());
                Ok(())
            })();
            service.shutdown();
            result
        })();
        fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
        result
    }
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
            mailbox.emit(json!({"op":"settings","hub":"local","revision":i}));
        }
        let results = [
            json!({"op":"settings","hub":"local","request_id":"gateway-save"}),
            json!({"op":"settings","hub":"local","request_id":"personal-save"}),
            json!({"op":"settings","hub":"local","confirm_hub_save":true}),
            json!({"op":"settings","hub":"local","ok":true,"source_op":"setup_save_hub"}),
            json!({"op":"composer_result","ok":true}),
            json!({"op":"pair_code","code":"123456"}),
        ];
        for result in &results {
            mailbox.emit(result.clone());
        }
        let snapshot = json!({"op":"settings","hub":"local","revision":40});
        mailbox.emit(snapshot.clone());
        assert_eq!(
            mailbox.pending.lock().map_err(|e| e.to_string())?.len(),
            1 + results.len()
        );
        for expected in std::iter::once(snapshot).chain(results) {
            let bytes = mailbox.next().ok_or("Missing UI frame")?;
            let actual: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            assert_eq!(actual, expected);
        }
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
