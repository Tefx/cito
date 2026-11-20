//! One persisted HubEngine for embedded and standalone HTTP deployments.
use super::auth::{env_value, local_principal, principal_ids_equal, principal_matches, HubAuth};
use super::database::{Connection, Database};
use super::hub_plugins::{self, ChannelPlugin, ChannelProcess};
use super::identity::{hash, listener_id};
use super::lease::evaluate_node_lease;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use std::collections::HashMap;
mod http;
#[cfg(test)]
use http::sse_wait;
pub use http::HubServer;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SIGNALS: &str = "CITO_SIGNALS";
const KV_STREAM: &str = "KV_cito_listeners";
const KV_SELECT: &str =
    "SELECT key,revision,created_ms,CAST(value AS TEXT) AS value,hex(value) AS value_hex,principal_id,scope FROM hub_kv";
const SIGNAL_SELECT: &str = "SELECT seq,subject,signal_id,created_ms,CAST(data AS TEXT) AS data,hex(data) AS data_hex,acked_at_ms,claimed_by_channel,claim_until_ms FROM hub_signals";
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS hub_meta(key TEXT PRIMARY KEY,int_val INTEGER NOT NULL DEFAULT 0,json_val TEXT);
CREATE TABLE IF NOT EXISTS hub_kv(key TEXT PRIMARY KEY,revision INTEGER NOT NULL,created_ms INTEGER NOT NULL,value BLOB NOT NULL,principal_id TEXT,scope TEXT);
CREATE TABLE IF NOT EXISTS hub_signals(seq INTEGER PRIMARY KEY AUTOINCREMENT,subject TEXT NOT NULL,msg_id TEXT UNIQUE,signal_id TEXT,created_ms INTEGER NOT NULL,data BLOB NOT NULL,acked INTEGER NOT NULL DEFAULT 0,claimed_by_channel TEXT,claim_until_ms INTEGER,acked_at_ms INTEGER);
CREATE TABLE IF NOT EXISTS hub_plugin_services(plugin_id TEXT PRIMARY KEY,config_json TEXT NOT NULL DEFAULT '{}',service_state_json TEXT NOT NULL DEFAULT '{}',updated_ms INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS hub_human_channels(principal_id TEXT NOT NULL,plugin_id TEXT NOT NULL,owner_user TEXT NOT NULL DEFAULT 'local',enabled INTEGER NOT NULL DEFAULT 1,user_config_json TEXT NOT NULL DEFAULT '{}',display_label TEXT,last_delivered_seq INTEGER NOT NULL DEFAULT 0,updated_ms INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(principal_id,plugin_id));
CREATE TABLE IF NOT EXISTS hub_channel_deliveries(plugin_id TEXT NOT NULL,principal_id TEXT NOT NULL,signal_id TEXT NOT NULL,seq INTEGER NOT NULL,status TEXT NOT NULL,external_msg_id TEXT,from_listener_id TEXT,from_user TEXT,from_title TEXT,delivered_ms INTEGER NOT NULL,PRIMARY KEY(plugin_id,principal_id,signal_id));
CREATE TABLE IF NOT EXISTS hub_pairing_codes(code TEXT PRIMARY KEY,plugin_id TEXT NOT NULL,principal_id TEXT NOT NULL,owner_user TEXT NOT NULL,instruction TEXT,created_ms INTEGER NOT NULL,expires_ms INTEGER NOT NULL,consumed_ms INTEGER);
CREATE TABLE IF NOT EXISTS hub_audit_log(id INTEGER PRIMARY KEY AUTOINCREMENT,ts_ms INTEGER NOT NULL,principal_id TEXT NOT NULL,action TEXT NOT NULL,target_id TEXT NOT NULL,detail_json TEXT);
CREATE TABLE IF NOT EXISTS hub_channel_delivery_msgs(plugin_id TEXT NOT NULL,principal_id TEXT NOT NULL,external_msg_id TEXT NOT NULL,signal_id TEXT NOT NULL,from_listener_id TEXT NOT NULL,PRIMARY KEY(plugin_id,principal_id,external_msg_id));
CREATE TABLE IF NOT EXISTS hub_processed_replies(plugin_id TEXT NOT NULL,update_id TEXT NOT NULL,processed_at_ms INTEGER NOT NULL,PRIMARY KEY(plugin_id,update_id));
CREATE TABLE IF NOT EXISTS hub_consumers(name TEXT PRIMARY KEY,stream TEXT NOT NULL,filter_subject TEXT,start_seq INTEGER NOT NULL,cursor_seq INTEGER NOT NULL,last_acked_seq INTEGER NOT NULL DEFAULT 0,principal_id TEXT);
CREATE TABLE IF NOT EXISTS hub_consumer_acks(name TEXT NOT NULL,seq INTEGER NOT NULL,PRIMARY KEY(name,seq));
";

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn n(v: &Value, k: &str) -> i64 {
    v[k].as_i64().unwrap_or(0)
}
fn parse(v: &Value, k: &str) -> Value {
    serde_json::from_str(s(v, k)).unwrap_or(Value::Null)
}
fn one(c: &Connection, sql: &str, args: &[Value]) -> Result<Option<Value>, String> {
    Ok(c.query(sql, args)?.into_iter().next())
}
fn required<'a>(p: &'a Value, key: &str) -> Result<&'a str, String> {
    p[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("INVALID_INPUT: {key} is required"))
}
fn decode_payload(p: &Value) -> Result<Vec<u8>, String> {
    if let Some(encoded) = p.get("data_b64").filter(|v| !v.is_null()) {
        return B64
            .decode(
                encoded
                    .as_str()
                    .ok_or("INVALID_INPUT: data_b64 must be a string")?,
            )
            .map_err(|_| "INVALID_INPUT: invalid base64".into());
    }
    let Some(raw) = p.get("data") else {
        return Ok(Vec::new());
    };
    if let Some(text) = raw.as_str() {
        return Ok(text.as_bytes().to_vec());
    }
    let bytes = if raw["type"] == "Buffer" {
        raw.get("data")
    } else {
        Some(raw)
    };
    if let Some(array) = bytes.and_then(Value::as_array) {
        return array
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| "INVALID_INPUT: data must contain bytes in 0..255".into())
            })
            .collect();
    }
    serde_json::to_vec(raw).map_err(|_| "INVALID_INPUT: invalid raw payload".into())
}
fn payload_metadata(raw: &[u8]) -> Value {
    serde_json::from_str::<Value>(&String::from_utf8_lossy(raw))
        .ok()
        .filter(Value::is_object)
        .unwrap_or(Value::Null)
}
fn blob_literal(raw: &[u8]) -> String {
    // Only generated hex digits enter SQL. This preserves arbitrary bytes without
    // requiring changes to the shared Database's JSON/text binding API.
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut literal = String::with_capacity(raw.len() * 2 + 3);
    literal.push_str("X'");
    for b in raw {
        literal.push(HEX[(b >> 4) as usize] as char);
        literal.push(HEX[(b & 15) as usize] as char);
    }
    literal.push('\'');
    literal
}
fn stored_bytes(row: &Value, column: &str) -> Result<Vec<u8>, String> {
    let hex = row[format!("{column}_hex")]
        .as_str()
        .ok_or("INTERNAL_ERROR: missing SQLite blob projection")?;
    if hex.len() % 2 != 0 {
        return Err("INTERNAL_ERROR: invalid SQLite blob projection".into());
    }
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
                .ok_or_else(|| "INTERNAL_ERROR: invalid SQLite blob projection".into())
        })
        .collect()
}
fn encoded(v: &Value) -> String {
    B64.encode(v.to_string())
}
fn random_id(prefix: &str) -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("CSPRNG unavailable: {e}"))?;
    Ok(format!(
        "{prefix}{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}
fn encode_pair_token(bytes: [u8; 6]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    // Six random bytes provide 48 bits. RFC 4648 Base32 adds two zero padding
    // bits to the final symbol, producing ten characters without '=' padding.
    let bits = bytes
        .iter()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
        << 2;
    (0..10)
        .rev()
        .map(|i| ALPHABET[((bits >> (i * 5)) & 31) as usize] as char)
        .collect()
}
fn pair_code() -> Result<String, String> {
    let mut bytes = [0u8; 6];
    getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
    Ok(format!("cito-{}", encode_pair_token(bytes)))
}
fn matches(subject: &str, filter: &str) -> bool {
    filter.is_empty()
        || filter == ">"
        || filter == "*"
        || subject == filter
        || filter.strip_suffix(".*").is_some_and(|p| {
            subject
                .strip_prefix(&format!("{p}."))
                .is_some_and(|x| !x.contains('.'))
        })
        || filter
            .strip_suffix(".>")
            .is_some_and(|p| subject.starts_with(&format!("{p}.")))
}
fn subject_target(subject: &str) -> Option<&str> {
    subject
        .strip_prefix("cito.signal.")
        .or_else(|| subject.strip_prefix("cito.ack."))
}
fn check_subject(subject: &str, wildcard: bool) -> Result<(), String> {
    let target = subject_target(subject).ok_or("PERMISSIONS_VIOLATION: invalid subject")?;
    if target.is_empty()
        || !target.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b"_-".contains(&b) || (wildcard && b".*>".contains(&b))
        })
    {
        return Err("PERMISSIONS_VIOLATION: invalid subject".into());
    }
    Ok(())
}
fn kv_wire(row: &Value) -> Result<Value, String> {
    Ok(
        json!({"key":row["key"],"revision":row["revision"],"created_ms":row["created_ms"],"value_b64":B64.encode(stored_bytes(row,"value")?),"operation":"PUT"}),
    )
}
fn signal_wire(row: &Value) -> Result<Value, String> {
    Ok(
        json!({"seq":row["seq"],"subject":row["subject"],"created_ms":row["created_ms"],"data_b64":B64.encode(stored_bytes(row,"data")?),"acked_at_ms":row["acked_at_ms"]}),
    )
}
fn default_stream(name: &str) -> Value {
    if name == KV_STREAM {
        json!({"name":name,"subjects":["$KV.cito_listeners.>"],"max_msgs_per_subject":1,"max_age":0,"retention":"limits","storage":"file","discard":"old"})
    } else {
        json!({"name":name,"subjects":["cito.signal.*"],"retention":"limits","max_age":1_800_000_000_000i64,"duplicate_window":1_800_000_000_000i64,"storage":"file","discard":"old"})
    }
}
fn merge(base: &mut Value, patch: &Value) {
    if let (Some(a), Some(b)) = (base.as_object_mut(), patch.as_object()) {
        for (k, v) in b {
            a.insert(k.clone(), v.clone());
        }
    }
}
fn audit(
    c: &Connection,
    pid: &str,
    action: &str,
    target: &str,
    detail: &Value,
) -> Result<(), String> {
    c.execute("INSERT INTO hub_audit_log(ts_ms,principal_id,action,target_id,detail_json) VALUES(?,?,?,?,?)",&[json!(now_ms()),json!(pid),json!(action),json!(target),json!(detail.to_string())])?;
    Ok(())
}
fn kv_row(c: &Connection, key: &str) -> Result<Option<Value>, String> {
    one(c, &format!("{KV_SELECT} WHERE key=?"), &[json!(key)])
}
fn principal_id(p: Option<&Value>) -> &str {
    p.map(|p| s(p, "principal_id")).unwrap_or("")
}
fn oidc(p: Option<&Value>) -> bool {
    p.is_some_and(|p| {
        p["oidc"] == true
            || s(p, "principal_id").starts_with("oidc:")
            || s(p, "principal_id").starts_with("https://")
    })
}
fn owns_principal(stored: &str, p: Option<&Value>) -> bool {
    p.is_some_and(|p| principal_matches(stored, p))
}
fn visible(row: &Value, p: Option<&Value>) -> bool {
    !oidc(p) || s(row, "scope") != "self" || owns_principal(s(row, "principal_id"), p)
}
fn own(c: &Connection, key: &str, p: Option<&Value>) -> Result<(), String> {
    if oidc(p) {
        let r = kv_row(c, key)?.ok_or("FORBIDDEN: target registration not found")?;
        if !owns_principal(s(&r, "principal_id"), p) {
            return Err("FORBIDDEN: node belongs to another principal".into());
        }
    }
    Ok(())
}
fn admin(p: Option<&Value>) -> Result<(), String> {
    if !oidc(p) {
        return Ok(());
    }
    let p = p.ok_or("FORBIDDEN: admin required")?;
    let users = env_value("CITO_OIDC_ADMIN_USERS").unwrap_or_default();
    if users
        .split(',')
        .map(str::trim)
        .any(|u| !u.is_empty() && (u == s(p, "owner_user") || u == s(p, "sub")))
    {
        Ok(())
    } else {
        Err("FORBIDDEN: Hub administrator required".into())
    }
}
fn hub_patch_guard(p: &Value, caller: Option<&Value>) -> Result<(), String> {
    if p.get("hub_config")
        .is_some_and(|v| v.as_object().is_none_or(|m| !m.is_empty()))
    {
        admin(caller)?;
    }
    Ok(())
}

struct Subscriber {
    principal: Option<Value>,
    tx: mpsc::SyncSender<Value>,
    wake: Arc<crate::support::WakePipe>,
    identity: Arc<()>,
}
pub struct Subscription {
    pub wake: Arc<crate::support::WakePipe>,
    receiver: mpsc::Receiver<Value>,
    owner: std::sync::Weak<Inner>,
    identity: Arc<()>,
}
impl std::ops::Deref for Subscription {
    type Target = mpsc::Receiver<Value>;
    fn deref(&self) -> &Self::Target {
        &self.receiver
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            if let Ok(mut subscribers) = owner.subscribers.lock() {
                subscribers.retain(|sub| !Arc::ptr_eq(&sub.identity, &self.identity));
            }
        }
    }
}
#[cfg(test)]
type WorkerWaitHook = Box<dyn FnOnce() + Send>;

struct Inner {
    db: Database,
    user_plugins_dir: PathBuf,
    closed: AtomicBool,
    subscribers: Mutex<Vec<Subscriber>>,
    plugins: Mutex<Vec<ChannelPlugin>>,
    processes: Mutex<HashMap<String, Arc<ChannelProcess>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    changed: Arc<(Mutex<u64>, std::sync::Condvar)>,
    lifecycle: Mutex<()>,
    #[cfg(test)]
    worker_ticks: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    before_worker_wait: Mutex<Option<WorkerWaitHook>>,
    default_human_scope: &'static str,
}
#[derive(Clone)]
pub struct HubEngine {
    inner: Arc<Inner>,
}
impl HubEngine {
    pub fn reset_data(&self) -> Result<(), String> {
        let _lifecycle = self
            .inner
            .lifecycle
            .lock()
            .map_err(|_| "Channel lifecycle lock poisoned")?;
        let ids = self
            .inner
            .processes
            .lock()
            .map_err(|_| "Channel process lock poisoned")?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            self.stop_process(&id)?;
        }
        self.inner.db.transaction(|db| {
            for table in [
                "hub_channel_delivery_msgs",
                "hub_channel_deliveries",
                "hub_consumer_acks",
                "hub_consumers",
                "hub_signals",
                "hub_processed_replies",
                "hub_audit_log",
                "hub_pairing_codes",
                "hub_human_channels",
                "hub_plugin_services",
                "hub_kv",
            ] {
                db.execute(&format!("DELETE FROM {table}"), &[])?;
            }
            Ok(())
        })
    }
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        Self::open_with_auth_mode(data_dir, "none")
    }
    pub fn open_with_auth_mode(data_dir: &Path, auth_mode: &str) -> Result<Self, String> {
        if !matches!(auth_mode, "none" | "token" | "oidc") {
            return Err("INVALID_INPUT: Unknown Hub auth mode".into());
        }
        std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
        std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let path = data_dir.join("hub.sqlite");
        let db = Database::open(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        db.execute("PRAGMA journal_mode=WAL", &[])?;
        db.execute("PRAGMA busy_timeout=5000", &[])?;
        db.transaction(|c|{
            for stmt in SCHEMA.split(';').map(str::trim).filter(|s|!s.is_empty()){c.execute(stmt,&[])?;}
            for(table,column,ty)in [("hub_signals","signal_id","TEXT"),("hub_signals","claimed_by_channel","TEXT"),("hub_signals","claim_until_ms","INTEGER"),("hub_signals","acked_at_ms","INTEGER"),("hub_pairing_codes","initial_human_scope","TEXT NOT NULL DEFAULT 'self'"),("hub_pairing_codes","instruction","TEXT"),("hub_pairing_codes","pairing_command","TEXT"),("hub_pairing_codes","pairing_url","TEXT"),("hub_pairing_codes","bot_username","TEXT"),("hub_channel_deliveries","reservation_id","TEXT"),("hub_channel_deliveries","attempts","INTEGER NOT NULL DEFAULT 0"),("hub_channel_deliveries","next_retry_ms","INTEGER NOT NULL DEFAULT 0"),("hub_channel_deliveries","last_error","TEXT")]{let columns=c.query(&format!("PRAGMA table_info({table})"),&[])?;if !columns.iter().any(|v|s(v,"name")==column){c.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"),&[])?;}}
            for sql in ["CREATE INDEX IF NOT EXISTS idx_hub_signals_subject_seq ON hub_signals(subject,seq)","CREATE INDEX IF NOT EXISTS idx_hub_signals_signal_id ON hub_signals(signal_id)","CREATE INDEX IF NOT EXISTS idx_hub_channel_deliveries_ext_msg ON hub_channel_deliveries(plugin_id,principal_id,external_msg_id)","CREATE INDEX IF NOT EXISTS idx_hub_channel_deliveries_recent ON hub_channel_deliveries(plugin_id,principal_id,delivered_ms DESC)"]{c.execute(sql,&[])?;}
            c.execute("CREATE INDEX IF NOT EXISTS idx_hub_kv_principal ON hub_kv(principal_id)", &[])?;
            c.execute("INSERT OR IGNORE INTO hub_meta(key,int_val) VALUES('kv_seq',0)",&[])?;
            // Existing single-message deliveries retain reply and acknowledgment mappings.
            c.execute("INSERT OR IGNORE INTO hub_channel_delivery_msgs(plugin_id,principal_id,external_msg_id,signal_id,from_listener_id) SELECT plugin_id,principal_id,external_msg_id,signal_id,COALESCE(from_listener_id,'') FROM hub_channel_deliveries WHERE external_msg_id IS NOT NULL",&[])?;
            // Backfill the additive ID column without changing legacy payload bytes.
            for row in c.query("SELECT seq,CAST(data AS TEXT) AS data FROM hub_signals WHERE signal_id IS NULL",&[])?{let data=parse(&row,"data");if let Some(id)=data["signal_id"].as_str(){c.execute("UPDATE hub_signals SET signal_id=? WHERE seq=?",&[json!(id),row["seq"].clone()])?;}}
            Ok(())
        })?;
        let engine = Self {
            inner: Arc::new(Inner {
                db,
                user_plugins_dir: data_dir.join("plugins"),
                closed: AtomicBool::new(false),
                subscribers: Mutex::new(Vec::new()),
                plugins: Mutex::new(hub_plugins::discover(
                    &super::plugins::plugin_roots_for_executable(
                        data_dir,
                        std::env::current_exe().ok().as_deref(),
                    ),
                    Some(&data_dir.join("plugins")),
                )?),
                processes: Mutex::new(HashMap::new()),
                worker: Mutex::new(None),
                changed: Arc::new((Mutex::new(0), std::sync::Condvar::new())),
                lifecycle: Mutex::new(()),
                #[cfg(test)]
                worker_ticks: std::sync::atomic::AtomicUsize::new(0),
                #[cfg(test)]
                before_worker_wait: Mutex::new(None),
                default_human_scope: if auth_mode == "none" {
                    "workspace"
                } else {
                    "self"
                },
            }),
        };
        engine.seed_environment()?;
        engine.restore_attachments()?;
        // Establish the maintenance epoch before publishing the engine or starting workers.
        engine.inner.db.transaction(maintain)?;
        let committed = Arc::downgrade(&engine.inner);
        engine.inner.db.set_commit_notifier(move || {
            if let Some(inner) = committed.upgrade() {
                HubEngine { inner }.wake_worker();
            }
        })?;
        let weak = Arc::downgrade(&engine.inner);
        let changed = engine.inner.changed.clone();
        let worker = thread::spawn(move || loop {
            let observed = match changed.0.lock() {
                Ok(v) => *v,
                Err(_) => break,
            };
            let Some(inner) = weak.upgrade() else { break };
            if inner.closed.load(Ordering::SeqCst) {
                break;
            }
            let e = HubEngine { inner };
            let failed = e
                .tick()
                .map_err(|err| eprintln!("cito hub: {err}"))
                .is_err();
            let delay = e.worker_delay().unwrap_or(Duration::from_secs(5));
            #[cfg(test)]
            if failed {
                if let Some(hook) = e.inner.before_worker_wait.lock().unwrap().take() {
                    hook();
                }
            }
            let Ok(version) = changed.0.lock() else { break };
            if e.inner.closed.load(Ordering::SeqCst) {
                break;
            }
            // Keep work committed during tick pending. Diagnostic-only transactions
            // do not advance this generation, so persistent failures still back off.
            let _ = changed.1.wait_timeout_while(
                version,
                if failed {
                    Duration::from_secs(5)
                } else {
                    delay
                },
                |v| *v == observed && !e.inner.closed.load(Ordering::SeqCst),
            );
        });
        *engine
            .inner
            .worker
            .lock()
            .map_err(|_| "Hub worker lock poisoned")? = Some(worker);
        Ok(engine)
    }
    pub fn set_plugin_roots(&self, roots: &[PathBuf]) -> Result<(), String> {
        let roots: Vec<_> = roots.iter().map(|root| (root.clone(), false)).collect();
        let plugins = hub_plugins::discover(&roots, Some(&self.inner.user_plugins_dir))?;
        *self
            .inner
            .plugins
            .lock()
            .map_err(|_| "Plugin registry lock poisoned")? = plugins;
        self.seed_environment()?;
        Ok(())
    }
    pub fn subscribe(&self, principal: Option<Value>) -> Result<Subscription, String> {
        let (tx, receiver) = mpsc::sync_channel(256);
        let wake = Arc::new(crate::support::WakePipe::new()?);
        let identity = Arc::new(());
        let mut subscribers = self
            .inner
            .subscribers
            .lock()
            .map_err(|_| "Subscriber lock poisoned")?;
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err("UNAVAILABLE: Hub is closing".into());
        }
        subscribers.push(Subscriber {
            principal,
            tx,
            wake: wake.clone(),
            identity: identity.clone(),
        });
        Ok(Subscription {
            wake,
            receiver,
            owner: Arc::downgrade(&self.inner),
            identity,
        })
    }
    pub fn shutdown(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.wake_worker();
        if let Ok(mut subs) = self.inner.subscribers.lock() {
            for sub in subs.iter() {
                sub.wake.notify();
            }
            subs.clear();
        }
        let processes = self
            .inner
            .processes
            .lock()
            .map(|mut p| p.drain().map(|(_, v)| v).collect::<Vec<_>>())
            .unwrap_or_default();
        for p in processes {
            p.shutdown()
        }
        if let Ok(mut w) = self.inner.worker.lock() {
            if let Some(w) = w.take() {
                let _ = w.join();
            }
        }
    }
    #[cfg(test)]
    pub fn pause_worker(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.wake_worker();
        if let Some(worker) = self.inner.worker.lock().unwrap().take() {
            worker.join().unwrap();
        }
        self.inner.closed.store(false, Ordering::SeqCst);
    }

    fn wake_worker(&self) {
        if let Ok(mut version) = self.inner.changed.0.lock() {
            *version = version.wrapping_add(1);
            self.inner.changed.1.notify_all();
        }
    }
    fn worker_delay(&self) -> Result<Duration, String> {
        self.inner.db.transaction(|c| {
            let now = now_ms();
            let maintenance = one(c, "SELECT int_val FROM hub_meta WHERE key='last_maintenance_ms'", &[])?
                .map(|r| n(&r, "int_val") + 60_000).unwrap_or(now);
            let retry = one(c, "SELECT MIN(CASE WHEN status='in_flight' THEN delivered_ms+60001 ELSE next_retry_ms END) AS due FROM hub_channel_deliveries WHERE (status='in_flight' AND delivered_ms+60001>?) OR (status='failed' AND next_retry_ms>?)", &[json!(now),json!(now)])?
                .and_then(|r| r["due"].as_i64()).unwrap_or(maintenance);
            Ok(Duration::from_millis(maintenance.min(retry).saturating_sub(now).max(1) as u64))
        })
    }
    fn emit(&self, frame: Value) {
        self.wake_worker();
        if let Ok(mut subs) = self.inner.subscribers.lock() {
            subs.retain(|sub| {
                let permitted = self
                    .inner
                    .db
                    .transaction(|c| event_allowed(c, &frame, sub.principal.as_ref()))
                    .unwrap_or(false);
                if !permitted {
                    return true;
                }
                match sub.tx.try_send(frame.clone()) {
                    Ok(()) => {
                        sub.wake.notify();
                        true
                    }
                    Err(mpsc::TrySendError::Full(_)) | Err(mpsc::TrySendError::Disconnected(_)) => {
                        false
                    }
                }
            });
        }
    }
    pub fn rpc(
        &self,
        op: &str,
        params: &Value,
        principal: Option<&Value>,
    ) -> Result<Value, String> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err("UNAVAILABLE: Hub is closing".into());
        }
        if op.starts_with("human_channel") {
            return self.human_rpc(op, params, principal);
        }
        let mut events = Vec::new();
        let (result, ack_request) = self
            .inner
            .db
            .transaction(|c| self.rpc_conn(c, op, params, principal, &mut events))?;
        for event in events {
            self.emit(event)
        }
        if result["control_revision_conflict"] == true {
            return Err(format!("CONTROL_REVISION_CONFLICT: {}", result));
        }
        if op == "signal_ack" {
            if let Err(error) = self.notify_ack(params) {
                eprintln!("Post-commit acknowledgment notification failed: {error}");
            }
        }
        if let Some(request) = ack_request {
            if let Err(error) = self.notify_ack(&request) {
                eprintln!("Post-commit acknowledgment notification failed: {error}");
            }
        }
        Ok(result)
    }
    fn rpc_conn(
        &self,
        c: &Connection,
        op: &str,
        p: &Value,
        caller: Option<&Value>,
        events: &mut Vec<Value>,
    ) -> Result<(Value, Option<Value>), String> {
        maintain(c)?;
        let result = match op {
            "human_device_attach"
            | "human_device_heartbeat"
            | "human_device_detach"
            | "human_inbox_update" => {
                human_operation(c, op, p, caller, self.inner.default_human_scope, events)
            }
            "stream_info" => stream_info(c, required(p, "stream")?),
            "stream_add" | "stream_update" => {
                admin(caller)?;
                let name = if op == "stream_add" {
                    required(&p["config"], "name")?
                } else {
                    required(p, "stream")?
                };
                let row = one(
                    c,
                    "SELECT json_val FROM hub_meta WHERE key=?",
                    &[json!(format!("stream:{name}"))],
                )?;
                let mut cfg = row
                    .as_ref()
                    .map(|r| parse(r, "json_val"))
                    .unwrap_or_else(|| default_stream(name));
                if op == "stream_update" || row.is_none() {
                    merge(
                        &mut cfg,
                        if op == "stream_add" {
                            &p["config"]
                        } else {
                            &p["patch"]
                        },
                    );
                    cfg["name"] = json!(name);
                    c.execute("INSERT INTO hub_meta(key,json_val) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_val=excluded.json_val",&[json!(format!("stream:{name}")),json!(cfg.to_string())])?;
                }
                stream_info(c, name)
            }
            "stream_get_message" => {
                if s(p, "stream") != SIGNALS {
                    return Err("10037: no message found".into());
                }
                if let Some(seqs) = p.get("seqs") {
                    let seqs = seqs
                        .as_array()
                        .filter(|s| {
                            !s.is_empty()
                                && s.len() <= 128
                                && s.iter().all(|s| s.as_u64().is_some())
                        })
                        .ok_or("INVALID_INPUT: seqs must contain 1..128 sequence numbers")?;
                    let placeholders = vec!["?"; seqs.len()].join(",");
                    let mut messages = Vec::new();
                    for row in c.query(
                        &format!("{SIGNAL_SELECT} WHERE seq IN ({placeholders})"),
                        seqs,
                    )? {
                        if !signal_readable(c, &row, caller)? {
                            return Err("FORBIDDEN: signal is private".into());
                        }
                        messages.push(json!({"seq":row["seq"],"signal_id":row["signal_id"],"acked_at_ms":row["acked_at_ms"]}));
                    }
                    return Ok((json!({"messages":messages}), None));
                }
                let row = one(
                    c,
                    &format!("{SIGNAL_SELECT} WHERE seq=?"),
                    &[p["seq"].clone()],
                )?
                .ok_or("10037: no message found")?;
                if !signal_readable(c, &row, caller)? {
                    return Err("FORBIDDEN: signal is private".into());
                }
                let mut result = signal_wire(&row)?;
                result["received"] = json!(one(
                    c,
                    "SELECT 1 AS received FROM hub_consumer_acks WHERE seq=? LIMIT 1",
                    &[p["seq"].clone()]
                )?
                .is_some());
                Ok(result)
            }
            "kv_get" => {
                let Some(row) = kv_row(c, required(p, "key")?)? else {
                    return Ok((Value::Null, None));
                };
                if !visible(&row, caller) {
                    return Ok((json!({"forbidden_scope":true}), None));
                }
                let row = reconcile_human_row(c, row)?;
                let mut wire = kv_wire(&row)?;
                wire["server_now_ms"] = json!(now_ms());
                Ok(wire)
            }
            "kv_keys" | "kv_snapshot" => {
                let sql = format!("{KV_SELECT} WHERE key>? ORDER BY key");
                let params = [json!(s(p, "after_key"))];
                if op == "kv_keys" {
                    let mut keys = Vec::new();
                    c.query_while(&sql, &params, |row| {
                        if visible(&row, caller) && matches(s(&row, "key"), s(p, "key")) {
                            keys.push(row["key"].clone());
                        }
                        Ok(true)
                    })?;
                    Ok(json!({"keys":keys}))
                } else {
                    const PAGE_BYTES: usize = 2 * 1024 * 1024;
                    let limit = p["limit"].as_u64().unwrap_or(256).clamp(1, 1024) as usize;
                    let mut rows = Vec::new();
                    let mut bytes = 128;
                    let mut more = false;
                    c.query_while(&sql, &params, |row| {
                        if !visible(&row, caller) || !matches(s(&row, "key"), s(p, "key")) {
                            return Ok(true);
                        }
                        let size = serde_json::to_vec(&kv_wire(&row)?)
                            .map_err(|e| e.to_string())?
                            .len()
                            + 1;
                        if rows.len() >= limit || bytes + size > PAGE_BYTES {
                            more = true;
                            return Ok(false);
                        }
                        bytes += size;
                        rows.push(row);
                        Ok(true)
                    })?;
                    // Reconciliation may write hub_kv. The SELECT cursor must be
                    // dropped before any connection re-entry or mutation.
                    let mut entries = Vec::new();
                    bytes = 128;
                    for row in rows {
                        let wire = kv_wire(&reconcile_human_row(c, row)?)?;
                        let size = serde_json::to_vec(&wire).map_err(|e| e.to_string())?.len() + 1;
                        if bytes + size > PAGE_BYTES {
                            more = true;
                            break;
                        }
                        bytes += size;
                        entries.push(wire);
                    }
                    if more && entries.is_empty() {
                        return Err(
                            "INVALID_INPUT: Registration exceeds snapshot page budget".into()
                        );
                    }
                    let next_cursor = if more {
                        entries.last().map(|entry: &Value| entry["key"].clone())
                    } else {
                        None
                    };
                    Ok(
                        json!({"entries":entries,"next_cursor":next_cursor,"server_now_ms":now_ms()}),
                    )
                }
            }
            "kv_create" | "kv_update" | "kv_put" | "kv_delete" => {
                let key = required(p, "key")?;
                let existing = kv_row(c, key)?;
                if oidc(caller)
                    && existing.as_ref().is_some_and(|r| {
                        !s(r, "principal_id").is_empty()
                            && !owns_principal(s(r, "principal_id"), caller)
                    })
                {
                    return Err(
                        "PERMISSIONS_VIOLATION: registration belongs to another principal".into(),
                    );
                }
                if op == "kv_create" && existing.is_some()
                    || op == "kv_update"
                        && existing
                            .as_ref()
                            .is_none_or(|r| n(r, "revision") != n(p, "revision"))
                {
                    return Err("10071: wrong last sequence".into());
                }
                let mut raw = if op == "kv_delete" {
                    Vec::new()
                } else {
                    decode_payload(p)?
                };
                let mut data = payload_metadata(&raw);
                if data["harness"] == "human"
                    || existing
                        .as_ref()
                        .is_some_and(|row| parse(row, "value")["harness"] == "human")
                {
                    return Err(
                        "FORBIDDEN: Human records require explicit Human RPC operations".into(),
                    );
                }
                if op != "kv_delete" {
                    if let Some(existing) = &existing {
                        let previous = parse(existing, "value");
                        if ["principal_id", "harness", "session_id"]
                            .iter()
                            .all(|key| !s(&previous, key).is_empty())
                            && (!principal_ids_equal(
                                s(&previous, "principal_id"),
                                s(&data, "principal_id"),
                            ) || previous["harness"] != data["harness"]
                                || previous["session_id"] != data["session_id"])
                        {
                            return Err("PERMISSIONS_VIOLATION: listener_id tuple collision".into());
                        }
                    }
                    if s(&data, "harness") == "human" && s(&data, "session_id") != "default" {
                        return Err("INVALID_INPUT: Human node session_id must be 'default'".into());
                    }
                    if oidc(caller)
                        && data["principal_id"]
                            .as_str()
                            .is_some_and(|pid| !owns_principal(pid, caller))
                    {
                        return Err("PERMISSIONS_VIOLATION: registration principal mismatch".into());
                    }
                }
                let revision = next_revision(c)?;
                let now = now_ms();
                // Only changed attachments renew. Unmodified siblings retain their leases.
                let previous = existing
                    .as_ref()
                    .map(|row| parse(row, "value"))
                    .unwrap_or(Value::Null);
                let mut renewed = false;
                if let Some(channels) = data["channels"].as_object_mut() {
                    for (id, channel) in channels {
                        if channel["state"] == "ACTIVE" && *channel != previous["channels"][id] {
                            channel["updated_at_ms"] = json!(now);
                            renewed = true;
                        }
                    }
                }
                if renewed {
                    if data["updated_at_ms"] != previous["updated_at_ms"] {
                        data["updated_at_ms"] = json!(now);
                    }
                    super::lease::reconcile_registration_state(&mut data, now.max(0) as u64);
                    raw = serde_json::to_vec(&data).map_err(|e| e.to_string())?;
                }
                let owner = if op == "kv_delete" {
                    existing
                        .as_ref()
                        .and_then(|r| r["principal_id"].as_str())
                        .map(str::to_owned)
                } else {
                    data["principal_id"]
                        .as_str()
                        .or_else(|| caller.and_then(|p| p["principal_id"].as_str()))
                        .map(str::to_owned)
                };
                if op == "kv_delete" {
                    c.execute("DELETE FROM hub_kv WHERE key=?", &[json!(key)])?;
                } else {
                    c.execute(&format!("INSERT INTO hub_kv(key,revision,created_ms,value,principal_id,scope) VALUES(?,?,?,{},?,?) ON CONFLICT(key) DO UPDATE SET revision=excluded.revision,created_ms=excluded.created_ms,value=excluded.value,principal_id=excluded.principal_id,scope=excluded.scope",blob_literal(&raw)),&[json!(key),json!(revision),json!(now),json!(owner),json!(data["scope"].as_str())])?;
                }
                audit(
                    c,
                    principal_id(caller),
                    if op == "kv_delete" || data["state"] == "UNLISTENED" {
                        "unlisten"
                    } else {
                        "register"
                    },
                    key,
                    &data,
                )?;
                events.push(json!({"type":"kv","key":key,"revision":revision,"created_ms":now,"value_b64":B64.encode(&raw),"operation":if op=="kv_delete"{"DEL"}else{"PUT"},"principal_id":owner,"scope":if op=="kv_delete"{existing.as_ref().map(|r|r["scope"].clone()).unwrap_or(Value::Null)}else{data["scope"].clone()}}));
                if previous["scope"] == "workspace" && data["scope"] == "self" {
                    events.push(json!({"type":"kv"}));
                }
                Ok(if op == "kv_delete" {
                    json!({"deleted":true})
                } else {
                    json!({"revision":revision})
                })
            }
            "js_publish" | "stream_publish" | "nc_publish" => {
                let subject = required(p, "subject")?;
                check_subject(subject, false)?;
                let raw = decode_payload(p)?;
                if subject.starts_with("cito.ack.") {
                    own(c, subject_target(subject).unwrap_or(""), caller)?;
                    events.push(json!({"type":"nc","subject":subject,"data_b64":B64.encode(&raw)}));
                    return Ok((
                        if op == "nc_publish" {
                            json!({"ok":true})
                        } else {
                            json!({"seq":0,"duplicate":false,"stream":""})
                        },
                        None,
                    ));
                }
                let data = payload_metadata(&raw);
                let result = publish_payload(
                    c,
                    subject,
                    &data,
                    Some(&raw),
                    p["opts"]["msgID"].as_str().or_else(|| p["msg_id"].as_str()),
                    caller,
                    events,
                )?;
                Ok(if op == "nc_publish" {
                    json!({"ok":true})
                } else {
                    result
                })
            }
            "nc_subscribe_check" => {
                let subject = required(p, "subject")?;
                check_subject(subject, true)?;
                if oidc(caller) {
                    if subject.contains('*') || subject.contains('>') {
                        return Err("FORBIDDEN: wildcard subscription denied".into());
                    }
                    own(c, subject_target(subject).unwrap_or(""), caller)?;
                }
                Ok(json!({"ok":true}))
            }
            "consumer_add" => {
                let cfg = &p["config"];
                let name = cfg["durable_name"]
                    .as_str()
                    .or_else(|| cfg["name"].as_str())
                    .map(String::from)
                    .unwrap_or(random_id("consumer_")?);
                let filter = s(cfg, "filter_subject");
                if !filter.is_empty() {
                    check_subject(filter, true)?;
                    if !filter.contains('*') && !filter.contains('>') {
                        own(c, subject_target(filter).unwrap_or(""), caller)?;
                    }
                }
                if let Some(row) = one(
                    c,
                    "SELECT principal_id FROM hub_consumers WHERE name=?",
                    &[json!(name)],
                )? {
                    consumer_owner(&row, caller)?;
                }
                let start = if cfg["deliver_policy"] == "by_start_sequence" {
                    n(cfg, "opt_start_seq").max(1)
                } else {
                    1
                };
                let stream = p["stream"].as_str().unwrap_or(SIGNALS);
                c.execute("INSERT INTO hub_consumers(name,stream,filter_subject,start_seq,cursor_seq,principal_id) VALUES(?,?,?,?,?,?) ON CONFLICT(name) DO UPDATE SET stream=excluded.stream,filter_subject=excluded.filter_subject,start_seq=excluded.start_seq,cursor_seq=excluded.cursor_seq",&[json!(name),json!(stream),json!(filter),json!(start),json!(start),caller.map(|p|p["principal_id"].clone()).unwrap_or(Value::Null)])?;
                let row = consumer(c, &name, caller)?;
                let backlog = backlog_from(c, &row, caller, n(&row, "start_seq"))?;
                Ok(
                    json!({"name":name,"stream_name":stream,"config":cfg,"backlog":backlog["backlog"],"server_now_ms":backlog["server_now_ms"]}),
                )
            }
            "consumer_backlog" => {
                let row = consumer(c, required(p, "name")?, caller)?;
                backlog_from(c, &row, caller, n(&row, "start_seq"))
            }
            "consumer_next" => {
                let name = required(p, "name")?;
                let row = consumer(c, name, caller)?;
                let rows = backlog_from(
                    c,
                    &row,
                    caller,
                    n(&row, "start_seq").max(n(&row, "cursor_seq")),
                )?;
                let item = rows["backlog"]
                    .as_array()
                    .and_then(|rows| rows.first())
                    .cloned()
                    .unwrap_or(Value::Null);
                if !item.is_null() {
                    c.execute(
                        "UPDATE hub_consumers SET cursor_seq=? WHERE name=?",
                        &[json!(n(&item, "seq") + 1), json!(name)],
                    )?;
                }
                Ok(item)
            }
            "consumer_ack" | "consumer_delete" => {
                let name = required(p, "name")?;
                let row = consumer(c, name, caller)?;
                if op == "consumer_delete" {
                    c.execute("DELETE FROM hub_consumers WHERE name=?", &[json!(name)])?;
                    c.execute("DELETE FROM hub_consumer_acks WHERE name=?", &[json!(name)])?;
                    Ok(json!({"deleted":true}))
                } else {
                    let sig = one(
                        c,
                        &format!("{SIGNAL_SELECT} WHERE seq=?"),
                        &[p["seq"].clone()],
                    )?
                    .ok_or("NOT_FOUND: signal not found")?;
                    if !matches(s(&sig, "subject"), s(&row, "filter_subject"))
                        || !can_consume(
                            c,
                            subject_target(s(&sig, "subject")).unwrap_or(""),
                            caller,
                        )?
                    {
                        return Err("FORBIDDEN: signal outside consumer authority".into());
                    }
                    let inserted = c.execute(
                        "INSERT OR IGNORE INTO hub_consumer_acks(name,seq) VALUES(?,?)",
                        &[json!(name), p["seq"].clone()],
                    )?;
                    if inserted > 0 {
                        events.push(json!({"type":"receipt","seq":p["seq"]}));
                    }
                    c.execute("UPDATE hub_consumers SET last_acked_seq=MAX(last_acked_seq,?) WHERE name=?",&[p["seq"].clone(),json!(name)])?;
                    Ok(json!({"acked":true}))
                }
            }
            "send_reply_from_human" => {
                let sender = caller.ok_or("FORBIDDEN: Human identity required")?;
                return publish_human_reply(c, sender, "human", p, events);
            }
            "signal_claim_dispatch" => claim(c, p, caller),
            "signal_ack" => ack(c, p, caller, events),
            "account_info" => {
                let consumers =
                    one(c, "SELECT COUNT(*) AS n FROM hub_consumers", &[])?.unwrap_or(Value::Null);
                let streams = one(
                    c,
                    "SELECT COUNT(*) AS n FROM hub_meta WHERE key LIKE 'stream:%'",
                    &[],
                )?
                .unwrap_or(Value::Null);
                Ok(
                    json!({"streams":streams["n"],"consumers":consumers["n"],"memory":0,"storage":0}),
                )
            }
            _ => Err(format!("INVALID_INPUT: Unknown hub RPC op: {op}")),
        }?;
        Ok((result, None))
    }
    fn plugin(&self, id: &str) -> Result<ChannelPlugin, String> {
        self.inner
            .plugins
            .lock()
            .map_err(|_| "Plugin registry lock poisoned")?
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| format!("INVALID_INPUT: Unknown human channel plugin: {id}"))
    }
    fn seed_environment(&self) -> Result<(), String> {
        let plugins = self
            .inner
            .plugins
            .lock()
            .map_err(|_| "Plugin registry lock poisoned")?
            .clone();
        self.inner.db.transaction(|c| {
            for plugin in plugins {
                let (mut cfg, _) = service(c, &plugin.id)?;
                for field in fields(&plugin) {
                    if s(&field, "scope") != "hub" {
                        continue;
                    }
                    let key = s(&field, "key");
                    let mut snake = String::new();
                    for (i, ch) in key.chars().enumerate() {
                        if ch.is_uppercase() && i > 0 {
                            snake.push('_')
                        }
                        snake.extend(ch.to_uppercase());
                    }
                    let prefix = plugin.id.to_uppercase().replace('-', "_");
                    if let Some(value) = env_value(&format!("CITO_PLUGIN_{prefix}_{snake}"))
                        .or_else(|| {
                            env_value(&format!("CITO_PLUGIN_{prefix}_{}", key.to_uppercase()))
                        })
                    {
                        cfg[key] = json!(value);
                    }
                }
                save_channel_service(c, &plugin, &cfg)?;
            }
            Ok(())
        })
    }
    fn restore_attachments(&self) -> Result<(), String> {
        self.inner.db.transaction(|c| {
            for b in c.query("SELECT * FROM hub_human_channels WHERE enabled=1", &[])? {
                let active = match self.plugin(s(&b, "plugin_id")) {
                    Ok(plugin) => {
                        let (cfg, _) = service(c, &plugin.id)?;
                        ready(&plugin, &cfg, "hub")
                            && ready(&plugin, &parse(&b, "user_config_json"), "user")
                    }
                    Err(_) => false,
                };
                attach(
                    c,
                    &b,
                    active,
                    self.inner.default_human_scope,
                    self.plugin(s(&b, "plugin_id")).ok().as_ref(),
                )?;
            }
            Ok(())
        })
    }
    fn ensure_process(&self, plugin: &ChannelPlugin) -> Result<Arc<ChannelProcess>, String> {
        let mut processes = self
            .inner
            .processes
            .lock()
            .map_err(|_| "Plugin process lock poisoned")?;
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err("UNAVAILABLE: Hub is closing".into());
        }
        if let Some(p) = processes.get(&plugin.id) {
            if p.alive() {
                return Ok(p.clone());
            }
        }
        let old = processes.remove(&plugin.id);
        drop(processes);
        if let Some(old) = old {
            old.shutdown();
        }
        let mut processes = self
            .inner
            .processes
            .lock()
            .map_err(|_| "Plugin process lock poisoned")?;
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err("UNAVAILABLE: Hub is closing".into());
        }
        if let Some(process) = processes.get(&plugin.id).filter(|process| process.alive()) {
            return Ok(process.clone());
        }
        // Every replacement owns recovery, including a replacement requested by Send Test.
        self.inner.db.execute(
            "UPDATE hub_channel_deliveries SET status='effect_unknown',last_error='Channel process exited before confirming delivery' WHERE plugin_id=? AND status='in_flight'",
            &[json!(plugin.id)],
        )?;
        let weak = Arc::downgrade(&self.inner);
        let id = plugin.id.clone();
        let callback = Arc::new(move |frame: Value| {
            let inner = weak.upgrade()?;
            if inner.closed.load(Ordering::SeqCst) {
                return None;
            }
            let engine = HubEngine { inner };
            engine.plugin_frame(&id, &frame)
        });
        let process = Arc::new(
            ChannelProcess::spawn(plugin, callback)
                .map_err(|error| self.process_error(&plugin.id, error))?,
        );
        processes.insert(plugin.id.clone(), process.clone());
        drop(processes);
        if let Err(error) = self.configure_process(&plugin.id, &process) {
            if let Ok(mut processes) = self.inner.processes.lock() {
                if processes
                    .get(&plugin.id)
                    .is_some_and(|current| Arc::ptr_eq(current, &process))
                {
                    processes.remove(&plugin.id);
                }
            }
            process.shutdown();
            return Err(self.process_error(&plugin.id, error));
        }
        Ok(process)
    }
    fn process_error(&self, id: &str, error: String) -> String {
        match self.set_poll_error(id, Some(&error)) {
            Ok(()) => error,
            Err(_) => format!("{error}; channel health persistence failed"),
        }
    }
    fn set_poll_error(&self, id: &str, error: Option<&str>) -> Result<(), String> {
        self.inner.db.diagnostic_transaction(|c| {
            let (cfg, mut state) = service(c, id)?;
            save_service(c, id, &cfg)?;
            let previous = state.to_string();
            if let Some(error) = error {
                state["poll_error"] = json!(error);
            } else if let Some(state) = state.as_object_mut() {
                state.remove("poll_error");
            }
            let serialized = state.to_string();
            if serialized != previous {
                c.execute(
                    "UPDATE hub_plugin_services SET service_state_json=? WHERE plugin_id=?",
                    &[json!(serialized), json!(id)],
                )?;
            }
            Ok(())
        })
    }
    fn configure_process(&self, id: &str, process: &ChannelProcess) -> Result<(), String> {
        let frame=self.inner.db.transaction(|c|{let(cfg,state)=service(c,id)?;let bindings=c.query("SELECT principal_id,owner_user,user_config_json FROM hub_human_channels WHERE plugin_id=? AND enabled=1",&[json!(id)])?.iter().map(|b|json!({"principal_id":b["principal_id"],"user":b["owner_user"],"config":parse(b,"user_config_json")})).collect::<Vec<_>>();Ok(json!({"op":"configure","hub_config":cfg,"bindings":bindings,"cursor":state["cursor"]}))})?;
        process.send(&frame)?;
        self.set_poll_error(id, None)
    }
    fn human_rpc(&self, op: &str, p: &Value, caller: Option<&Value>) -> Result<Value, String> {
        if !p.is_object() {
            return Err("INVALID_INPUT: Channel params must be an object".into());
        }
        if op == "human_channel_pair_cancel" {
            let mut patch = p.clone();
            patch["cancel_pair"] = json!(true);
            return self.human_rpc("human_channels_configure", &patch, caller);
        }
        let local = local_principal(
            &env_value("CITO_USER")
                .or_else(|| env_value("USER"))
                .unwrap_or_else(|| "local".into()),
        );
        let supplied = caller.unwrap_or(&local);
        let mut effective = supplied.clone();
        if oidc(Some(supplied)) {
            if let Some(legacy) = supplied["legacy_principal_id"].as_str() {
                let legacy_binding = self.inner.db.query(
                    "SELECT 1 AS n FROM hub_human_channels WHERE principal_id=? LIMIT 1",
                    &[json!(legacy)],
                )?;
                let current_binding = self.inner.db.query(
                    "SELECT 1 AS n FROM hub_human_channels WHERE principal_id=? LIMIT 1",
                    &[supplied["principal_id"].clone()],
                )?;
                if current_binding.is_empty() && !legacy_binding.is_empty() {
                    effective["canonical_principal_id"] = supplied["principal_id"].clone();
                    effective["principal_id"] = json!(legacy);
                }
            }
        }
        let caller = &effective;
        if op == "human_channels_list" {
            return self.channel_list(caller);
        }
        let id = required(p, "plugin_id")?;
        let plugin = self.plugin(id)?;
        hub_patch_guard(p, Some(caller))?;
        // Serialize configuration and supervisor work; never hold this lock in child callbacks.
        let _lifecycle = self
            .inner
            .lifecycle
            .lock()
            .map_err(|_| "Channel lifecycle lock poisoned")?;
        let clear_secret =
            p["clear_hub_config"] == true || p["reset_hub"] == true || p["reset"] == true;
        for scope in ["hub", "user"] {
            if let Some(patch) = p.get(format!("{scope}_config")) {
                validate_config(&plugin, patch, scope)?;
            }
        }
        if p["reset_hub"] == true || p["clear_hub_config"] == true {
            admin(Some(caller))?;
        }
        let reset_hub = p["reset_hub"] == true;
        let reset_user = p["reset_user"] == true || (reset_hub && p["reset_user"] != false);
        let reconcile_bindings = |c: &Connection,
                                  cfg: &Value,
                                  changed: bool|
         -> Result<(), String> {
            if changed || p.get("user_config").is_some() {
                c.execute("UPDATE hub_channel_deliveries SET status='failed',attempts=0,next_retry_ms=0,last_error=NULL WHERE plugin_id=? AND status IN ('failed','failed_permanent') AND (? OR principal_id=?)", &[json!(id),json!(changed),caller["principal_id"].clone()])?;
            }
            for b in c.query(
                "SELECT * FROM hub_human_channels WHERE plugin_id=?",
                &[json!(id)],
            )? {
                let active = n(&b, "enabled") == 1
                    && ready(&plugin, cfg, "hub")
                    && ready(&plugin, &parse(&b, "user_config_json"), "user");
                attach(
                    c,
                    &b,
                    active,
                    initial_human_scope(caller, self.inner.default_human_scope),
                    Some(&plugin),
                )?;
            }
            Ok(())
        };
        if op == "human_channels_configure" {
            let changed = self.inner.db.transaction(|c| {
                let (old_cfg, _) = service(c, id)?;
                let mut cfg = if reset_hub {
                    json!({})
                } else {
                    old_cfg.clone()
                };
                if !reset_hub {
                    if let Some(patch) = p.get("hub_config") {
                        merge_channel_config(&plugin, "hub", &mut cfg, patch, clear_secret);
                    }
                }
                Ok(cfg != old_cfg || reset_hub)
            })?;
            // Reap the previous reader before resetting persistent cursor/mapping state.
            if changed {
                self.stop_process(id)?;
            }
            self.inner.db.transaction(|c| {
                let (mut cfg, _) = service(c, id)?;
                if reset_hub { cfg = json!({}); }
                else if let Some(patch) = p.get("hub_config") {
                    merge_channel_config(&plugin, "hub", &mut cfg, patch, clear_secret);
                }
                if changed { save_channel_service(c, &plugin, &cfg)?; }
                if reset_hub {
                    c.execute("UPDATE hub_plugin_services SET service_state_json='{}' WHERE plugin_id=?", &[json!(id)])?;
                    c.execute("UPDATE hub_pairing_codes SET consumed_ms=? WHERE plugin_id=? AND consumed_ms IS NULL", &[json!(now_ms()), json!(id)])?;
                }
                if reset_user || p["cancel_pair"] == true {
                    cancel_pair(c, id, caller)?;
                }
                if reset_hub {
                    for b in c.query("SELECT * FROM hub_human_channels WHERE plugin_id=?", &[json!(id)])? {
                        attach(c, &b, false, initial_human_scope(caller, self.inner.default_human_scope), Some(&plugin))?;
                    }
                }
                if reset_user || p.get("user_config").is_some() || p.get("enabled").is_some() || p.get("display_label").is_some() {
                    let old = one(c, "SELECT * FROM hub_human_channels WHERE principal_id=? AND plugin_id=?", &[caller["principal_id"].clone(), json!(id)])?;
                    let mut config = old.as_ref().map(|b| parse(b,"user_config_json")).unwrap_or(json!({}));
                    let mut label = old.as_ref().map(|b| b["display_label"].clone()).unwrap_or(Value::Null);
                    let mut identity_changed = reset_user;
                    if reset_user { config = json!({}); label = Value::Null; }
                    else if let Some(patch) = p.get("user_config") {
                        let prev = config.clone();
                        merge_channel_config(&plugin, "user", &mut config, patch, clear_secret);
                        if let Some(key) = plugin.manifest["pairing"]["privateIdentityField"].as_str() {
                            if let Some(value) = config[key].as_str() {
                                config[key] = json!(value.trim());
                            }
                        }
                        identity_changed = plugin.manifest["pairing"]["privateIdentityField"].as_str().map(|key|prev[key]!=config[key]).unwrap_or_else(||fields(&plugin).iter().any(|f| s(f,"scope") == "user" && prev[s(f,"key")] != config[s(f,"key")]));
                        if identity_changed {
                            let cleared = plugin.manifest["pairing"]["privateIdentityField"].as_str().is_some_and(|identity| s(&config, identity).is_empty());
                            if let Some(m) = config.as_object_mut() {
                                for key in ["_external_chat_id", "_external_user_id"] {
                                    if patch.get(key).is_none() || cleared { m.remove(key); }
                                }
                            }
                            label = Value::Null;
                        }
                    }
                    if let Some(identity) = plugin.manifest["pairing"]["privateIdentityField"].as_str() {
                        if identity_changed && !s(&config, identity).is_empty() {
                            let chat = s(&config, identity).parse::<i64>().ok().filter(|n| *n > 0).map(|n| n.to_string());
                            config["_external_chat_id"] = json!(chat);
                            config["_external_user_id"] = json!(chat);
                            label = json!(chat.as_ref().map(|chat| format!("Chat {chat}")).unwrap_or_else(|| "Outbound only".into()));
                        }
                    }
                    replace_destination(c, &plugin, s(caller,"principal_id"), old.as_ref().map(|b|parse(b,"user_config_json")).as_ref(), &config)?;
                    if let Some(value) = p.get("display_label") { label = value.clone(); }
                    let enabled = !reset_user && p["enabled"].as_bool().unwrap_or_else(|| old.as_ref().is_none_or(|b| n(b,"enabled")==1));
                    let boundary = if enabled && old.as_ref().is_none_or(|b| n(b,"enabled")==0) { max_seq(c)? } else { old.as_ref().map(|b| n(b,"last_delivered_seq")).unwrap_or(0) };
                    c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,enabled,user_config_json,display_label,last_delivered_seq,updated_ms) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(principal_id,plugin_id) DO UPDATE SET owner_user=excluded.owner_user,enabled=excluded.enabled,user_config_json=excluded.user_config_json,display_label=excluded.display_label,last_delivered_seq=excluded.last_delivered_seq,updated_ms=excluded.updated_ms",
                        &[caller["principal_id"].clone(),json!(id),caller["owner_user"].clone(),json!(if enabled{1}else{0}),json!(config.to_string()),label,json!(boundary),json!(now_ms())])?;
                }
                reconcile_bindings(c, &cfg, changed)?;
                audit(c,s(caller,"principal_id"),"channel_configure",id,&json!({"enabled":p["enabled"]}))?;
                Ok(())
            })?;
            self.emit(json!({"type":"kv"}));
            let mut result = json!({"ok":true,"committed":true});
            let maintenance = (|| {
                let process = self
                    .inner
                    .processes
                    .lock()
                    .map_err(|_| "Process lock poisoned")?
                    .get(id)
                    .cloned();
                if let Some(process) = process {
                    self.configure_process(id, &process)?;
                }
                self.tick_channels()
            })();
            if let Err(error) = maintenance {
                result["maintenance_warning"] = json!(error);
            }
            match self.channel_list(caller) {
                Ok(inventory) => result["channels"] = inventory["channels"].clone(),
                Err(error) => result["inventory_warning"] = json!(error),
            }
            return Ok(result);
        }
        if op == "human_channel_poll" {
            self.tick_channels()?;
            return Ok(json!({"ok":true}));
        }
        if op != "human_channels_test" && op != "human_channel_pair_start" {
            return Err(format!("INVALID_INPUT: Unknown hub RPC op: {op}"));
        }
        if op == "human_channel_pair_start" && plugin.manifest["pairing"]["supported"] != true {
            return Err("INVALID_INPUT: pairing is not supported".into());
        }
        let (cfg,user,changed) = self.inner.db.transaction(|c| {
            let (old,_) = service(c,id)?;
            let mut cfg=old.clone();
            if let Some(patch)=p.get("hub_config") { merge_channel_config(&plugin,"hub",&mut cfg,patch,clear_secret); }
            let row=one(c,"SELECT user_config_json FROM hub_human_channels WHERE principal_id=? AND plugin_id=?", &[caller["principal_id"].clone(),json!(id)])?;
            let mut user=row.as_ref().map(|b|parse(b,"user_config_json")).unwrap_or(json!({}));
            if let Some(patch)=p.get("user_config") { merge_channel_config(&plugin,"user",&mut user,patch,clear_secret); }
            let changed=cfg!=old;
            Ok((cfg,user,changed))
        })?;
        if op == "human_channels_test" {
            let process = self.ensure_process(&plugin)?;
            return process.request(json!({"op":"test","user":caller["owner_user"],"hub_config":cfg,"user_config":user}));
        }
        if changed {
            self.stop_process(id)?;
        }
        // Prepare the pairing instruction before committing either configuration
        // or the code, so an unavailable executable leaves no partial mutation.
        let process = self.ensure_process(&plugin)?;
        self.configure_process(id, &process)?;
        let code = pair_code()?;
        let expires = now_ms() + 600_000;
        let prepared = process.request(json!({"op":"format_pair","code":code,"hub_config":cfg}));
        // Preparation still polls with the persisted credentials. Join its reader
        // before resetting service state, including when instruction lookup fails.
        self.stop_process(id)?;
        let result = prepared?;
        if result["ok"] != true {
            return Err(format!("PLUGIN_EXEC_FAILED: {}", s(&result, "error")));
        }
        let instruction = required(&result, "instruction")?;
        self.inner.db.transaction(|c| {
            if changed {
                save_channel_service(c, &plugin, &cfg)?;
                reconcile_bindings(c, &cfg, true)?;
            }
            cancel_pair(c,id,caller)?;
            c.execute("INSERT INTO hub_pairing_codes(code,plugin_id,principal_id,owner_user,created_ms,expires_ms,instruction,pairing_command,pairing_url,bot_username,initial_human_scope) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                &[json!(code),json!(id),caller["principal_id"].clone(),caller["owner_user"].clone(),json!(now_ms()),json!(expires),json!(instruction),result["pairing_command"].clone(),result["pairing_url"].clone(),result["bot_username"].clone(),json!(initial_human_scope(caller, self.inner.default_human_scope))])?;
            Ok(())
        })?;
        if changed {
            self.emit(json!({"type":"kv"}));
        }
        let mut result = json!({"ok":true,"committed":true,"code":code,"instruction":instruction,"pairing_command":result["pairing_command"],"pairing_url":result["pairing_url"],"bot_username":result["bot_username"],"expires_at_ms":expires});
        if let Err(error) = self.ensure_process(&plugin) {
            result["maintenance_warning"] = json!(error);
        }
        Ok(result)
    }
    fn stop_process(&self, id: &str) -> Result<(), String> {
        let old = self
            .inner
            .processes
            .lock()
            .map_err(|_| "Process lock poisoned")?
            .remove(id);
        if let Some(process) = old {
            process.shutdown();
        }
        self.inner.db.transaction(|c| {
            let (_, mut state) = service(c, id)?;
            if state
                .as_object_mut()
                .is_some_and(|fields| fields.remove("poll_error").is_some())
            {
                c.execute(
                    "UPDATE hub_plugin_services SET service_state_json=? WHERE plugin_id=?",
                    &[json!(state.to_string()), json!(id)],
                )?;
            }
            Ok(())
        })
    }
    fn channel_list(&self, caller: &Value) -> Result<Value, String> {
        let plugins = self
            .inner
            .plugins
            .lock()
            .map_err(|_| "Registry lock poisoned")?
            .clone();
        let is_admin = admin(Some(caller)).is_ok();
        self.inner.db.transaction(|c| {
            let inbox_paused = find_human(c,caller)?.and_then(|id| kv_row(c,&id).ok().flatten()).is_some_and(|row| parse(&row,"value")["human_paused"] == true);
            let mut channels=Vec::new();
            for plugin in plugins {
                let (cfg,health) = service(c,&plugin.id)?;
                let binding=one(c,"SELECT * FROM hub_human_channels WHERE principal_id=? AND plugin_id=?", &[caller["principal_id"].clone(),json!(plugin.id)])?.unwrap_or(Value::Null);
                let user=parse(&binding,"user_config_json");
                let has_hub_service=fields(&plugin).iter().any(|f|s(f,"scope")=="hub");
                let can_configure_hub=has_hub_service && is_admin;
                let hub_configured_any=fields(&plugin).iter().any(|f|s(f,"scope")=="hub" && configured(&cfg[s(f,"key")]));
                let can_edit_personal=fields(&plugin).iter().any(|f|s(f,"scope")=="user");
                let pairing_supported=plugin.manifest["pairing"]["supported"]==true;
                let hub_ready=ready(&plugin,&cfg,"hub");
                let user_ready=ready(&plugin,&user,"user");
                let enabled=!binding.is_null() && n(&binding,"enabled")==1 && user_ready;
                let pair=one(c,"SELECT code,instruction,pairing_command,pairing_url,bot_username FROM hub_pairing_codes WHERE principal_id=? AND plugin_id=? AND consumed_ms IS NULL AND expires_ms>? ORDER BY created_ms DESC LIMIT 1", &[caller["principal_id"].clone(),json!(plugin.id),json!(now_ms())])?.unwrap_or(Value::Null);
                let status=if hub_ready && !pair.is_null() && !user_ready { "pairing_pending" }
                    else if hub_ready && user_ready && enabled { "ready" }
                    else if hub_ready && user_ready { "paused" }
                    else if hub_ready { "hub_ready" } else { "not_configured" };
                let label=binding["display_label"].as_str().filter(|s| !s.is_empty());
                let mut detail=match status {
                    "ready" | "paused" if inbox_paused=>"Delivery is paused while the Human Inbox is paused".into(),
                    "ready"=>label.map(|l|format!("Connected as {l} · Ready for notifications")).unwrap_or("Connected and ready for notifications".into()),
                    "paused"=>label.map(|l|format!("Paused ({l}) · Enable to resume notifications")).unwrap_or("Paused · Enable to resume notifications".into()),
                    "pairing_pending"=>"Pairing in progress · Send the pairing command below to finish linking".into(),
                    "hub_ready" if pairing_supported=>"Service ready · Pair your account with one click".into(),
                    "hub_ready"=>"Service ready · Enter your personal destination fields below".into(),
                    _ if !can_configure_hub=>"Shared service credentials must be configured by the Hub administrator".into(),
                    _=>"Configure service credentials and pair your account".into(),
                };
                let mut warning = false;
                let mut error_origin: Option<&str> = None;
                if let Some(failed) = one(c,"SELECT status,attempts,last_error FROM hub_channel_deliveries WHERE plugin_id=? AND principal_id=? AND (delivered_ms>=? OR status='effect_unknown') ORDER BY (status='effect_unknown') DESC,delivered_ms DESC,rowid DESC LIMIT 1",&[json!(plugin.id),caller["principal_id"].clone(),json!(n(&binding,"updated_ms"))])? {
                    if (matches!(s(&failed,"status"),"failed"|"failed_permanent"|"effect_unknown") || (failed["status"] == "in_flight" && n(&failed,"attempts") > 0)) && !s(&failed,"last_error").is_empty() { warning = true; error_origin = Some("user"); detail = if failed["status"] == "effect_unknown" { format!("Delivery outcome unknown; automatic replay stopped to avoid duplicates. {}", s(&failed,"last_error")) } else { s(&failed,"last_error").into() };  }
                }
                if let Some(error) = health["poll_error"].as_str() { warning = true; error_origin = Some("hub"); detail = error.into(); }
                let fs=fields(&plugin).iter().map(|f| {
                    let hub=s(f,"scope")=="hub";
                    let val=if hub { &cfg[s(f,"key")] } else { &user[s(f,"key")] };
                    let is_secret=s(f,"type")=="secret";
                    let hidden=hub && !is_admin;
                    let redacted=hidden || is_secret;
                    let value=if redacted || val.is_null() { String::new() } else { val.as_str().map(str::to_owned).unwrap_or_else(||val.to_string()) };
                    json!({"key":f["key"],"scope":f["scope"],"label":f["label"],"type":f["type"],"required":f["required"].as_bool().unwrap_or(false),
                        "placeholder":f["placeholder"].as_str().unwrap_or(""),"configured_placeholder":f["configuredPlaceholder"].as_str().unwrap_or(if is_secret{"•••••••• (Saved — enter new value to replace)"}else{s(f,"placeholder")}),
                        "help_text":f["helpText"].as_str(),"value":value,"configured":configured(val),"hidden":hidden,"redacted":redacted})
                }).collect::<Vec<_>>();
                channels.push(json!({"id":plugin.id,"display_name":plugin.manifest["displayName"],
                    "sf_symbol":plugin.manifest["sfSymbol"].as_str().unwrap_or("paperplane.fill"),"tint":plugin.manifest["tint"].as_str().unwrap_or("blue"),
                    "has_hub_service":has_hub_service,"hub_ready":hub_ready,"can_configure_hub":can_configure_hub,
                    "can_clear_hub":can_configure_hub&&hub_configured_any,"can_edit_personal":can_edit_personal,
                    "can_pair":pairing_supported&&(hub_ready||can_configure_hub),
                    "can_test":((hub_ready&&user_ready)||can_edit_personal)&&(hub_ready||can_configure_hub),
                    "can_unpair":(hub_ready&&user_ready)||!pair.is_null(),"error_origin":error_origin,
                    "enabled":enabled,"configured":hub_ready&&user_ready,"inbox_paused":inbox_paused,"delivery_enabled":enabled&&hub_ready&&user_ready&&!inbox_paused,"status":status,"detail":detail,"display_label":binding["display_label"],
                    "hub_provided":hub_ready&&fields(&plugin).iter().any(|f|s(f,"scope")=="hub"),
                    "setup_guide":plugin.manifest.get("setupGuide").cloned().unwrap_or(Value::Null),
                    "pairing_help_text":plugin.manifest["pairing"]["helpText"].as_str(),
                    "pairing_supported":plugin.manifest["pairing"]["supported"]==true,
                    "pairing_action_label":plugin.manifest["pairing"]["actionLabel"].as_str().unwrap_or("Pair Account…"),
                    "warning":warning,"pairing_manual_disclosure":plugin.manifest["pairing"]["manualDisclosure"]==true,"pairing_open_url_label":plugin.manifest["pairing"]["openUrlLabel"],"pairing_pending":!pair.is_null(),"pairing_code":pair["code"],"pairing_instruction":pair["instruction"],"pairing_command":pair["pairing_command"],"pairing_url":pair["pairing_url"],"bot_username":pair["bot_username"],"fields":fs}));
            }
            Ok(json!({"channels":channels}))
        })
    }
    fn tick(&self) -> Result<(), String> {
        #[cfg(test)]
        self.inner.worker_ticks.fetch_add(1, Ordering::SeqCst);
        let _lifecycle = self
            .inner
            .lifecycle
            .lock()
            .map_err(|_| "Channel lifecycle lock poisoned")?;
        self.tick_channels()
    }
    fn tick_channels(&self) -> Result<(), String> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.inner.db.transaction(maintain)?;
        let plugins = self
            .inner
            .plugins
            .lock()
            .map_err(|_| "Registry lock poisoned")?
            .clone();
        let mut first_error = None;
        for plugin in plugins {
            let should_run=self.inner.db.transaction(|c|{let(cfg,_)=service(c,&plugin.id)?;let bound=one(c,"SELECT 1 AS n FROM hub_human_channels WHERE plugin_id=? AND enabled=1 LIMIT 1",&[json!(plugin.id)])?.is_some();let pairing=one(c,"SELECT 1 AS n FROM hub_pairing_codes WHERE plugin_id=? AND consumed_ms IS NULL AND expires_ms>? LIMIT 1",&[json!(plugin.id),json!(now_ms())])?.is_some();Ok(ready(&plugin,&cfg,"hub")&&(cfg.as_object().is_some_and(|m|m.values().any(configured))||bound||pairing))})?;
            if !should_run {
                self.stop_process(&plugin.id)?;
                continue;
            }
            if let Err(error) = self
                .ensure_process(&plugin)
                .and_then(|process| self.replay(&plugin, &process))
            {
                self.set_poll_error(&plugin.id, Some(&error))?;
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
    fn replay(&self, plugin: &ChannelPlugin, process: &ChannelProcess) -> Result<(), String> {
        // Reservation expiry measures delivery progress, independently of process liveness.
        let reservations = self.reserve_deliveries(plugin)?;
        let mut send_error = None;
        for send in reservations {
            if let Err(err) = process.send(&send) {
                self.inner.db.execute("UPDATE hub_channel_deliveries SET status='effect_unknown',last_error='Channel pipe failed during delivery' WHERE reservation_id=? AND status='in_flight'", &[send["delivery_id"].clone()])?;
                send_error = Some(err);
            }
        }
        send_error.map_or(Ok(()), Err)
    }
    fn reserve_deliveries(&self, plugin: &ChannelPlugin) -> Result<Vec<Value>, String> {
        self.inner.db.transaction(|c|{let bindings=c.query("SELECT * FROM hub_human_channels WHERE plugin_id=? AND enabled=1",&[json!(plugin.id)])?;let (service_cfg, _) = service(c, &plugin.id)?;let mut sends=Vec::new();let in_flight=one(c,"SELECT COUNT(*) AS count FROM hub_channel_deliveries WHERE plugin_id=? AND status='in_flight' AND delivered_ms>?",&[json!(plugin.id),json!(now_ms()-60_000)])?.map(|row|n(&row,"count")).unwrap_or(0);let capacity=32usize.saturating_sub(in_flight.max(0) as usize);for b in bindings{if !ready(plugin, &service_cfg, "hub") || !ready(plugin, &parse(&b, "user_config_json"), "user") { continue; }let node=find_human(c,&b)?.unwrap_or_else(||listener_id(s(&b,"principal_id"), "human", "default"));if let Some(reg) = kv_row(c,&node)?.map(|r|parse(&r,"value")) { if reg["human_paused"] == true || reg["state"] == "UNLISTENED" { continue; } }let rows=c.query(&format!("{SIGNAL_SELECT} WHERE created_ms>=? AND subject=? AND seq>? AND acked_at_ms IS NULL AND signal_id<>'' AND NOT EXISTS(SELECT 1 FROM hub_channel_deliveries d WHERE d.plugin_id=? AND d.principal_id=? AND d.signal_id=hub_signals.signal_id AND (d.status IN ('delivered','failed_permanent','effect_unknown') OR d.status='in_flight' AND d.delivered_ms>? OR d.status='failed' AND d.next_retry_ms>?)) ORDER BY seq LIMIT 200"),&[json!(signal_cutoff(c)?),json!(format!("cito.signal.{node}")),b["last_delivered_seq"].clone(),json!(plugin.id),b["principal_id"].clone(),json!(now_ms()-60_000),json!(now_ms())])?;for row in rows{if sends.len() >= capacity { break; }let id=s(&row,"signal_id");let data=parse(&row,"data");let token=random_id("delivery_")?;let reserved_ms=now_ms();c.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,from_listener_id,from_user,from_title,delivered_ms,reservation_id) VALUES(?,?,?,?,'in_flight',?,?,?,?,?) ON CONFLICT(plugin_id,principal_id,signal_id) DO UPDATE SET status='in_flight',delivered_ms=excluded.delivered_ms,reservation_id=excluded.reservation_id,from_listener_id=excluded.from_listener_id",&[json!(plugin.id),b["principal_id"].clone(),json!(id),row["seq"].clone(),data["from"]["listener_id"].clone(),data["from"]["user"].clone(),data["from"]["title"].clone(),json!(reserved_ms),json!(token)])?;let already_sent_ids=c.query("SELECT external_msg_id FROM hub_channel_delivery_msgs WHERE plugin_id=? AND principal_id=? AND signal_id=? ORDER BY rowid",&[json!(plugin.id),b["principal_id"].clone(),json!(id)])?.into_iter().map(|r|r["external_msg_id"].clone()).collect::<Vec<_>>();let mut payload=data.clone();payload["created_at_ms"]=row["created_ms"].clone();payload["to"]=json!({"listener_id":node});sends.push(json!({"op":"deliver","claim_until_ms":reserved_ms+60_000,"already_sent_ids":already_sent_ids,"delivery_id":token,"principal_id":b["principal_id"],"user":b["owner_user"],"config":parse(&b,"user_config_json"),"signal":payload}));}}Ok(sends)})
    }
    fn plugin_frame(&self, id: &str, frame: &Value) -> Option<Value> {
        let result=match s(frame,"op"){
        "cursor_save"=>self.inner.db.transaction(|c|{
            let(_,mut state)=service(c,id)?;
            let numeric = |v:&Value| v.as_u64().or_else(||v.as_str().and_then(|s|s.parse::<u64>().ok()));
            if let Some(cursor) = numeric(&frame["cursor"]).filter(|n|*n <= i64::MAX as u64) {
                let floor = cursor.max(numeric(&state["reply_floor"]).unwrap_or(0));
                state["cursor"]=json!(floor); state["reply_floor"]=json!(floor);
                c.execute("DELETE FROM hub_processed_replies WHERE plugin_id=? AND update_id!='' AND update_id NOT GLOB '*[^0-9]*' AND CAST(update_id AS INTEGER)<?",&[json!(id),json!(floor)])?;
            } else { state["cursor"]=frame["cursor"].clone(); }
            c.execute("UPDATE hub_plugin_services SET service_state_json=?,updated_ms=? WHERE plugin_id=?",&[json!(state.to_string()),json!(now_ms()),json!(id)])?;Ok(Value::Null)}),
        "health"=>self.inner.db.transaction(|c|{let(_,mut state)=service(c,id)?;state["poll_error"]=frame["error"].clone();c.execute("UPDATE hub_plugin_services SET service_state_json=? WHERE plugin_id=?",&[json!(state.to_string()),json!(id)])?;Ok(Value::Null)}),
        "delivered"=>self.delivery_result(id,frame),
        "pair_redeem"=>self.redeem(id,frame),
        "inbound_reply"=>self.inbound_reply(id,frame),
        _=>Ok(Value::Null),
    };
        self.wake_worker();
        match result {
            Ok(v) if v.is_null() => None,
            Ok(v) => Some(v),
            Err(err) => {
                eprintln!("cito hub plugin {id}: {err}");
                match s(frame, "op") {
                    "pair_redeem" => {
                        Some(json!({"op":"pair_ack","code":frame["code"],"ok":false,"error":err}))
                    }
                    "inbound_reply" => Some(
                        json!({"op":"reply_ack","update_id":frame["update_id"],"ok":false,"error":err}),
                    ),
                    _ => None,
                }
            }
        }
    }
    fn delivery_result(&self, id: &str, f: &Value) -> Result<Value, String> {
        self.inner.db.transaction(|c| {
            let token = required(f, "delivery_id")?;
            let Some(row) = one(c, "SELECT * FROM hub_channel_deliveries WHERE plugin_id=? AND reservation_id=? AND status='in_flight'", &[json!(id),json!(token)])? else { return Ok(Value::Null); };
            let mut external = f["external_msg_ids"].as_array().cloned().unwrap_or_default();
            if let Some(primary) = f["external_msg_id"].as_str() {
                if !external.contains(&json!(primary)) { external.push(json!(primary)); }
            }
            for ext in external {
                let ext = ext.as_str().ok_or("INVALID_INPUT: external message IDs must be strings")?;
                c.execute("INSERT INTO hub_channel_delivery_msgs(plugin_id,principal_id,external_msg_id,signal_id,from_listener_id) VALUES(?,?,?,?,?) ON CONFLICT(plugin_id,principal_id,external_msg_id) DO UPDATE SET signal_id=excluded.signal_id,from_listener_id=excluded.from_listener_id",
                    &[json!(id),row["principal_id"].clone(),json!(ext),row["signal_id"].clone(),json!(s(&row,"from_listener_id"))])?;
            }
            if f["progress"] == true {
                c.execute("UPDATE hub_channel_deliveries SET delivered_ms=? WHERE plugin_id=? AND reservation_id=? AND status=\'in_flight\'", &[json!(now_ms()),json!(id),json!(token)])?;
                return Ok(Value::Null);
            }
            if f["effect_unknown"] != true && f["retryable"] == true
                && matches!(s(f, "error"), "Outbound queue is full" | "Delivery reservation expired before send")
            {
                c.execute("UPDATE hub_channel_deliveries SET status='failed',next_retry_ms=?,last_error=?,reservation_id=NULL WHERE plugin_id=? AND reservation_id=?", &[json!(now_ms()+2000),f["error"].clone(),json!(id),json!(token)])?;
                return Ok(Value::Null);
            }
            let attempts = n(&row, "attempts").max(0);
            let (status, retry_at) = delivery_transition(f, attempts, now_ms());
            c.execute("UPDATE hub_channel_deliveries SET status=?,external_msg_id=?,delivered_ms=?,attempts=attempts+1,next_retry_ms=?,last_error=? WHERE plugin_id=? AND reservation_id=?",
                &[json!(status),f["external_msg_id"].clone(),json!(now_ms()),json!(retry_at),f["error"].clone(),json!(id),json!(token)])?;
            Ok(Value::Null)
        })
    }
    fn redeem(&self, id: &str, f: &Value) -> Result<Value, String> {
        let plugin = self.plugin(id)?;
        let mut result = self.inner.db.transaction(|c| {
            if f["is_private_chat"] != true {
                return Err("FORBIDDEN: pairing requires a private conversation".into());
            }
            let chat = required(f, "external_chat_id")?;
            let user = required(f, "external_user_id")?;
            let code = required(f, "code")?.trim();
            let row = one(c, "SELECT * FROM hub_pairing_codes WHERE code=? AND plugin_id=? AND consumed_ms IS NULL AND expires_ms>?",
                &[json!(code), json!(id), json!(now_ms())])?
                .ok_or("INVALID_INPUT: expired or consumed pairing code")?;
            validate_config(&plugin, &f["user_config_patch"], "user")?;
            let old = one(c, "SELECT * FROM hub_human_channels WHERE principal_id=? AND plugin_id=?",
                &[row["principal_id"].clone(), json!(id)])?;
            let mut config = old.as_ref().map(|b| parse(b, "user_config_json")).unwrap_or(json!({}));
            merge_channel_config(&plugin, "user", &mut config, &f["user_config_patch"], false);
            config["_external_chat_id"] = json!(chat);
            config["_external_user_id"] = json!(user);
            replace_destination(c, &plugin, s(&row,"principal_id"), old.as_ref().map(|b|parse(b,"user_config_json")).as_ref(), &config)?;
            let (service_cfg, _) = service(c, id)?;
            let active = ready(&plugin, &service_cfg, "hub") && ready(&plugin, &config, "user");
            let boundary = if let Some(old) = old.as_ref().filter(|old| n(old,"enabled") != 0) { n(old,"last_delivered_seq") } else { max_seq(c)? };
            c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,enabled,user_config_json,display_label,last_delivered_seq,updated_ms) VALUES(?,?,?,1,?,?,?,?) ON CONFLICT(principal_id,plugin_id) DO UPDATE SET enabled=1,user_config_json=excluded.user_config_json,display_label=excluded.display_label,last_delivered_seq=excluded.last_delivered_seq,updated_ms=excluded.updated_ms",
                &[row["principal_id"].clone(), json!(id), row["owner_user"].clone(), json!(config.to_string()), f["display_label"].clone(), json!(boundary), json!(now_ms())])?;
            c.execute("UPDATE hub_channel_deliveries SET status='failed',attempts=0,next_retry_ms=0,last_error=NULL WHERE plugin_id=? AND principal_id=? AND status IN ('failed','failed_permanent')", &[json!(id),row["principal_id"].clone()])?;
            c.execute("UPDATE hub_pairing_codes SET consumed_ms=? WHERE code=?", &[json!(now_ms()), json!(code)])?;
            let binding = one(c, "SELECT * FROM hub_human_channels WHERE principal_id=? AND plugin_id=?",
                &[row["principal_id"].clone(), json!(id)])?.ok_or("Binding missing")?;
            attach(c, &binding, active, s(&row, "initial_human_scope"), Some(&plugin))?;
            Ok(json!({"op":"pair_ack","code":code,"ok":true,"owner_user":row["owner_user"],"error":null}))
        })?;
        self.emit(json!({"type":"kv"}));
        // The callback runs on the child's reader thread; configure is a one-way write.
        let process = self
            .inner
            .processes
            .lock()
            .ok()
            .and_then(|p| p.get(id).cloned());
        if let Some(p) = process {
            if let Err(error) = self.configure_process(id, &p) {
                result["maintenance_warning"] = json!(error);
            }
        }
        Ok(result)
    }
    fn ack_frames(c: &Connection, p: &Value) -> Result<Vec<(String, Value)>, String> {
        let Some(node) = kv_row(c, s(p, "node_id"))? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for b in c.query("SELECT * FROM hub_human_channels WHERE enabled=1", &[])? {
            if !principal_ids_equal(s(&b, "principal_id"), s(&node, "principal_id")) {
                continue;
            }
            let mut ext = Vec::new();
            for id in ids(p)? {
                for row in c.query(
                        "SELECT external_msg_id FROM hub_channel_delivery_msgs WHERE plugin_id=? AND principal_id=? AND signal_id=?",
                        &[b["plugin_id"].clone(), b["principal_id"].clone(), json!(id)],
                    )? {
                        ext.push(row["external_msg_id"].clone());
                    }
            }
            out.push((
                    s(&b, "plugin_id").to_owned(),
                    json!({"op":"on_acked","principal_id":b["principal_id"],"signal_ids":p["signal_ids"],"acked_at_ms":now_ms(),"external_msg_ids":ext,"user_config":parse(&b,"user_config_json")}),
                ));
        }
        Ok(out)
    }
    fn notify_ack(&self, p: &Value) -> Result<(), String> {
        let frames = self.inner.db.transaction(|c| Self::ack_frames(c, p))?;
        self.send_ack_frames(frames)
    }
    fn send_ack_frames(&self, frames: Vec<(String, Value)>) -> Result<(), String> {
        for (id, frame) in frames {
            let proc = self
                .inner
                .processes
                .lock()
                .map_err(|_| "Process lock poisoned")?
                .get(&id)
                .cloned();
            if let Some(proc) = proc {
                let _ = proc.send(&frame);
            }
        }
        Ok(())
    }
    fn inbound_reply(&self, id: &str, f: &Value) -> Result<Value, String> {
        let mut events = Vec::new();
        let mut ack_frames = Vec::new();
        let result=self.inner.db.transaction(|c|{
        let update=required(f,"update_id")?;
        let (_,state) = service(c,id)?;
        if update.parse::<u64>().ok().is_some_and(|n|n < state["reply_floor"].as_u64().unwrap_or(0)) { return Ok(json!({"op":"reply_ack","update_id":update,"ok":true,"duplicate":true,"error":null})); }
        if one(c,"SELECT 1 AS n FROM hub_processed_replies WHERE plugin_id=? AND update_id=?",&[json!(id),json!(update)])?.is_some(){return Ok(json!({"op":"reply_ack","update_id":update,"ok":true,"duplicate":true,"error":null}))}
        let chat=required(f,"external_chat_id")?;let user=required(f,"external_user_id")?;
        let bindings=c.query("SELECT * FROM hub_human_channels WHERE plugin_id=?",&[json!(id)])?;
        let matches: Vec<_> = bindings.into_iter().filter(|b|{let cfg=parse(b,"user_config_json");s(&cfg,"_external_chat_id")==chat&&s(&cfg,"_external_user_id")==user}).collect();
        if matches.is_empty() { return Err("FORBIDDEN: external account is not bound".into()); }
        let (binding,target)=if let Some(ext)=f["reply_to_external_msg_id"].as_str() {
            let mut mapped=Vec::new();
            for binding in &matches {
                if let Some(target)=one(c,"SELECT signal_id,from_listener_id FROM hub_channel_delivery_msgs WHERE plugin_id=? AND principal_id=? AND external_msg_id=? AND from_listener_id IS NOT NULL AND from_listener_id!=''",&[json!(id),binding["principal_id"].clone(),json!(ext)])? { mapped.push((binding,target)); }
            }
            if mapped.len()>1 { return Err("AMBIGUOUS_REPLY: This message maps to multiple accounts; unpair duplicate accounts before replying".into()); }
            mapped.pop().ok_or("LISTENER_NOT_FOUND: Message reference expired; reply to a recent message")?
        } else {
            if matches.len()!=1 { return Err("AMBIGUOUS_REPLY: External account has duplicate bindings; reply to the specific message to choose its sender".into()); }
            let binding=&matches[0];
            let recent=c.query("SELECT signal_id,from_listener_id FROM hub_channel_deliveries WHERE plugin_id=? AND principal_id=? AND status='delivered' AND delivered_ms>=? AND from_listener_id IS NOT NULL AND from_listener_id!='' ORDER BY delivered_ms DESC,seq DESC",&[json!(id),binding["principal_id"].clone(),json!(now_ms()-604_800_000)])?;
            if recent.is_empty() { return Err("LISTENER_NOT_FOUND: No recent sender; reply to a specific message".into()); }
            if recent.iter().map(|r| s(r,"from_listener_id")).collect::<std::collections::HashSet<_>>().len()!=1 { return Err("AMBIGUOUS_REPLY: Reply to the specific message to choose its sender".into()); }
            (binding,recent.into_iter().next().ok_or("LISTENER_NOT_FOUND: no recent sender to reply to")?)
        };
        if n(binding,"enabled")!=1 { return Err("FORBIDDEN: external account is paused".into()); }
        let request=json!({"to":required(&target,"from_listener_id")?,"message":required(f,"text")?,"in_reply_to":target["signal_id"]});
        let (reply, request)=publish_human_reply(c,binding,id,&request,&mut events)?;
        if let Some(request)=request { ack_frames=Self::ack_frames(c,&request)?; }
        c.execute("INSERT INTO hub_processed_replies(plugin_id,update_id,processed_at_ms) VALUES(?,?,?)",&[json!(id),json!(update),json!(now_ms())])?;
        Ok(json!({"op":"reply_ack","update_id":update,"ok":true,"signal_id":reply["signal_id"],"target_display":reply["target_display"],"error":null}))
        })?;
        for event in events {
            self.emit(event)
        }
        self.send_ack_frames(ack_frames)?;
        Ok(result)
    }
}

fn publish_human_reply(
    c: &Connection,
    sender: &Value,
    channel: &str,
    p: &Value,
    events: &mut Vec<Value>,
) -> Result<(Value, Option<Value>), String> {
    let target = p["to_listener_id"]
        .as_str()
        .or_else(|| p["to"].as_str())
        .ok_or("INVALID_INPUT: to_listener_id is required")?;
    let text = p["message"]
        .as_str()
        .or_else(|| p["text"].as_str())
        .ok_or("INVALID_INPUT: message is required")?;
    let in_reply_to = p
        .get("in_reply_to")
        .filter(|v| !v.is_null())
        .or_else(|| p.get("signal_id").filter(|v| !v.is_null()))
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty())
                .ok_or("INVALID_INPUT: in_reply_to must be a non-empty string")
        })
        .transpose()?;
    let pid = required(sender, "principal_id")?;
    let owner = required(sender, "owner_user")?;
    let raw = target.rsplit_once(':').map(|(_, s)| s).unwrap_or(target);
    let target_row =
        resolve_registration(c, raw)?.ok_or("LISTENER_NOT_FOUND: reply target not found")?;
    let registration = parse(&target_row, "value");
    if !matches!(
        evaluate_node_lease(&registration, now_ms() as u64),
        "ACTIVE" | "DISCONNECTED_RETAINED"
    ) {
        return Err("LISTENER_NOT_FOUND: reply target is inactive or expired".into());
    }
    if s(&target_row, "scope") == "self"
        && !principal_ids_equal(s(&target_row, "principal_id"), pid)
    {
        return Err("FORBIDDEN_TARGET_SCOPE: reply target is private".into());
    }
    let text = super::identity::sanitize_message(text)?;
    let human = find_human(c, sender)?.unwrap_or_else(|| listener_id(pid, "human", "default"));
    let target_id = s(&target_row, "key");
    if human == target_id {
        return Err("SELF_SEND_FORBIDDEN: cannot reply to the same node".into());
    }
    let in_reply_to = in_reply_to.map(|raw| raw.rsplit_once(':').map(|(_, id)| id).unwrap_or(raw));
    // The daemon reserves the reply ID durably before its first publication attempt.
    let sig = match p.get("reply_signal_id").filter(|v| !v.is_null()) {
        Some(id) => id
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("INVALID_INPUT: reply_signal_id must be a non-empty string")?
            .to_owned(),
        None => random_id("sig_")?,
    };
    let host_id = p["host_id"].as_str().unwrap_or("hub");
    let host_name = p["host_name"].as_str().unwrap_or(channel);
    let msg_id = p["msg_id"]
        .as_str()
        .map(|key| {
            let current = hash(&json!([pid, host_id, "human", target_id, key]).to_string());
            let legacy = hash(&format!("{pid}|{host_id}|human|{target_id}|{key}"));
            let window = stream_info(c, SIGNALS)?["config"]["duplicate_window"]
                .as_i64()
                .unwrap_or(1_800_000_000_000)
                / 1_000_000;
            // Prefer the unambiguous key; retain live pre-upgrade publications.
            let cutoff = now_ms() - window.max(0);
            if let Some(row) = one(
                c,
                "SELECT CAST(data AS TEXT) AS data FROM hub_signals WHERE msg_id=? AND created_ms>=?",
                &[json!(current), json!(cutoff)],
            )? {
                let stored = parse(&row, "data");
                if stored["message"] != text || stored["in_reply_to"] != json!(in_reply_to) {
                    return Err("IDEMPOTENCY_CONFLICT: reply payload differs".into());
                }
            } else if let Some(row) = one(
                    c,
                    "SELECT CAST(data AS TEXT) AS data FROM hub_signals WHERE msg_id=? AND created_ms>=?",
                    &[json!(legacy), json!(cutoff)],
                )? {
                    // The legacy hash lost tuple boundaries. Prove stable identity
                    // separately from payload compatibility, ignoring display names.
                    let stored = parse(&row, "data");
                    if stored["from"]["principal_id"] == pid
                        && stored["from"]["host_id"] == host_id
                        && stored["from"]["harness"] == "human"
                        && super::identity::canonical_node_id(s(&stored["from"], "listener_id"))
                            .is_ok_and(|id| id == human)
                        && stored["to"] == target_id
                    {
                        if stored["message"] != text
                            || stored["in_reply_to"] != json!(in_reply_to)
                        {
                            return Err("IDEMPOTENCY_CONFLICT: retained legacy reply payload differs".into());
                        }
                        return Ok(legacy);
                    }
            }
            Ok::<_, String>(current)
        })
        .transpose()?;
    let payload = json!({"signal_id":sig,"to":target_id,"target_generation":registration["binding_generation"],"sender_endpoint_key":format!("{pid}|{host_id}|human|default"),"from":{"principal_id":pid,"user":owner,"host_id":host_id,"host_name":host_name,"harness":"human","listener_id":super::identity::public_listener_id(owner, &human),"title":"Human","about":"Human channel"},"message":text,"in_reply_to":in_reply_to});
    let published = publish(
        c,
        &format!("cito.signal.{target_id}"),
        &payload,
        msg_id.as_deref().or(Some(&sig)),
        None,
        events,
    )?;
    // A retry must return the committed signal ID, not the newly generated candidate.
    let stored = one(
        c,
        "SELECT signal_id FROM hub_signals WHERE seq=?",
        &[published["seq"].clone()],
    )?
    .ok_or("Published reply missing")?;
    let (ack_request, acked_at_ms) = if let Some(source) = in_reply_to {
        let request = json!({"node_id":human,"signal_ids":[source],"channel_id":format!("{host_id}:{channel}")});
        if one(
            c,
            "SELECT 1 FROM hub_signals WHERE signal_id=?",
            &[json!(source)],
        )?
        .is_some()
        {
            let acked = ack(c, &request, None, events)?;
            (Some(request), acked["acked_at_ms"].clone())
        } else {
            (None, json!(now_ms()))
        }
    } else {
        (None, Value::Null)
    };
    Ok((
        json!({"ok":true,"seq":published["seq"],"signal_id":stored["signal_id"],"target_display":registration["public_id"],"acked_at_ms":acked_at_ms,"error":null}),
        ack_request,
    ))
}

fn next_revision(c: &Connection) -> Result<i64, String> {
    c.execute(
        "UPDATE hub_meta SET int_val=int_val+1 WHERE key='kv_seq'",
        &[],
    )?;
    Ok(
        one(c, "SELECT int_val FROM hub_meta WHERE key='kv_seq'", &[])?
            .map(|v| n(&v, "int_val"))
            .unwrap_or(1),
    )
}
fn stream_info(c: &Connection, name: &str) -> Result<Value, String> {
    let row = one(
        c,
        "SELECT json_val FROM hub_meta WHERE key=?",
        &[json!(format!("stream:{name}"))],
    )?;
    let config = if let Some(row) = row {
        parse(&row, "json_val")
    } else if matches!(name, SIGNALS | KV_STREAM) {
        default_stream(name)
    } else {
        return Err(format!("10059: stream not found: {name}"));
    };
    let state = if name == KV_STREAM {
        let count = one(c, "SELECT COUNT(*) AS messages FROM hub_kv", &[])?.unwrap_or(Value::Null);
        let rev =
            one(c, "SELECT int_val FROM hub_meta WHERE key='kv_seq'", &[])?.unwrap_or(Value::Null);
        json!({"messages":count["messages"],"last_seq":rev["int_val"],"first_seq":if n(&count,"messages")==0{0}else{1},"bytes":0,"consumer_count":0})
    } else {
        one(c,"SELECT COUNT(*) AS messages,COALESCE(MAX(seq),0) AS last_seq,COALESCE(MIN(seq),0) AS first_seq,0 AS bytes,(SELECT COUNT(*) FROM hub_consumers) AS consumer_count FROM hub_signals",&[])?.unwrap_or(Value::Null)
    };
    Ok(json!({"config":config,"state":state,"ts":super::identity::iso(now_ms() as u64)}))
}
// Policy-only and idle Hubs still bound retained history. The database clock
// also amortizes maintenance across RPCs and the channel-independent worker.
fn maintain(c: &Connection) -> Result<(), String> {
    let now = now_ms();
    let last = one(
        c,
        "SELECT int_val FROM hub_meta WHERE key='last_maintenance_ms'",
        &[],
    )?
    .map(|row| n(&row, "int_val"))
    .unwrap_or(0);
    if now >= last && now - last < 60_000 {
        return Ok(());
    }
    prune(c)?;
    c.execute("INSERT INTO hub_meta(key,int_val) VALUES('last_maintenance_ms',?) ON CONFLICT(key) DO UPDATE SET int_val=excluded.int_val", &[json!(now)])?;
    Ok(())
}
fn signal_cutoff(c: &Connection) -> Result<i64, String> {
    let cfg = stream_info(c, SIGNALS)?["config"].clone();
    let age = cfg["max_age"].as_i64().unwrap_or(1_800_000_000_000) / 1_000_000;
    Ok(if age > 0 {
        now_ms().saturating_sub(age)
    } else {
        0
    })
}
fn prune(c: &Connection) -> Result<(), String> {
    let cfg = stream_info(c, SIGNALS)?["config"].clone();
    let now = now_ms();
    let age = cfg["max_age"].as_i64().unwrap_or(1_800_000_000_000) / 1_000_000;
    if age > 0 {
        c.execute(
            "DELETE FROM hub_signals WHERE created_ms<?",
            &[json!(now - age)],
        )?;
    }
    let window = cfg["duplicate_window"]
        .as_i64()
        .unwrap_or(1_800_000_000_000)
        / 1_000_000;
    c.execute(
        "UPDATE hub_signals SET msg_id=NULL WHERE created_ms<?",
        &[json!(now - window.max(0))],
    )?;
    c.execute(
        "DELETE FROM hub_channel_deliveries WHERE delivered_ms<?",
        &[json!(now - 604_800_000)],
    )?;
    c.execute("DELETE FROM hub_channel_delivery_msgs WHERE NOT EXISTS(SELECT 1 FROM hub_channel_deliveries d WHERE d.plugin_id=hub_channel_delivery_msgs.plugin_id AND d.principal_id=hub_channel_delivery_msgs.principal_id AND d.signal_id=hub_channel_delivery_msgs.signal_id)",&[])?;
    c.execute("DELETE FROM hub_consumer_acks WHERE NOT EXISTS(SELECT 1 FROM hub_signals s WHERE s.seq=hub_consumer_acks.seq)",&[])?;
    c.execute(
        "DELETE FROM hub_audit_log WHERE ts_ms<?",
        &[json!(now - 604_800_000)],
    )?;
    c.execute(
        "DELETE FROM hub_pairing_codes WHERE expires_ms<?",
        &[json!(now - 86_400_000)],
    )?;
    Ok(())
}
fn signal_readable(c: &Connection, row: &Value, p: Option<&Value>) -> Result<bool, String> {
    if !oidc(p) {
        return Ok(true);
    }
    let data = parse(row, "data");
    if owns_principal(s(&data["from"], "principal_id"), p) {
        return Ok(true);
    }
    let target = subject_target(s(row, "subject")).unwrap_or("");
    let Some(reg) = kv_row(c, target)? else {
        return Ok(false);
    };
    Ok(owns_principal(s(&reg, "principal_id"), p))
}
fn can_consume(c: &Connection, target: &str, p: Option<&Value>) -> Result<bool, String> {
    if !oidc(p) {
        return Ok(true);
    }
    Ok(kv_row(c, target)?.is_some_and(|r| owns_principal(s(&r, "principal_id"), p)))
}
fn event_allowed(c: &Connection, event: &Value, p: Option<&Value>) -> Result<bool, String> {
    if !oidc(p) {
        return Ok(true);
    }
    match s(event, "type") {
        "kv" => Ok(visible(event, p)),
        "receipt" => {
            match one(
                c,
                &format!("{SIGNAL_SELECT} WHERE seq=?"),
                &[event["seq"].clone()],
            )? {
                Some(row) => signal_readable(c, &row, p),
                None => Ok(false),
            }
        }
        "nc" | "node_ack" => can_consume(
            c,
            subject_target(s(event, "subject")).unwrap_or(s(event, "node_id")),
            p,
        ),
        "msg" | "signal" => can_consume(c, subject_target(s(event, "subject")).unwrap_or(""), p),
        _ => Ok(false),
    }
}
fn publish(
    c: &Connection,
    subject: &str,
    data: &Value,
    msg_id: Option<&str>,
    caller: Option<&Value>,
    events: &mut Vec<Value>,
) -> Result<Value, String> {
    publish_payload(c, subject, data, None, msg_id, caller, events)
}
fn publish_payload(
    c: &Connection,
    subject: &str,
    data: &Value,
    raw: Option<&[u8]>,
    msg_id: Option<&str>,
    caller: Option<&Value>,
    events: &mut Vec<Value>,
) -> Result<Value, String> {
    check_subject(subject, false)?;
    let target = subject
        .strip_prefix("cito.signal.")
        .ok_or("INVALID_INPUT: expected signal subject")?;
    if kv_row(c, target)?.is_some_and(|row| parse(&row, "value")["human_paused"] == true) {
        return Err("LISTENER_NOT_FOUND: Human inbox is paused".into());
    }
    if oidc(caller) {
        if data.get("from").is_some() && !owns_principal(s(&data["from"], "principal_id"), caller) {
            return Err("PERMISSIONS_VIOLATION: sender principal mismatch".into());
        }
        if kv_row(c, target)?.is_some_and(|row| !visible(&row, caller)) {
            return Err("FORBIDDEN_TARGET_SCOPE: target is private".into());
        }
    }
    if let Some(mid) = msg_id {
        let config = stream_info(c, SIGNALS)?["config"].clone();
        let window = config["duplicate_window"]
            .as_i64()
            .unwrap_or(1_800_000_000_000)
            / 1_000_000;
        // Reusing an expired idempotency key is part of this publish, not storage pruning.
        c.execute(
            "UPDATE hub_signals SET msg_id=NULL WHERE msg_id=? AND created_ms<?",
            &[json!(mid), json!(now_ms() - window.max(0))],
        )?;
        if let Some(row) = one(
            c,
            "SELECT seq FROM hub_signals WHERE msg_id=?",
            &[json!(mid)],
        )? {
            return Ok(json!({"stream":SIGNALS,"seq":row["seq"],"duplicate":true}));
        }
    }
    let now = now_ms();
    let bytes = raw
        .map(Vec::from)
        .unwrap_or_else(|| data.to_string().into_bytes());
    c.execute(
        &format!(
            "INSERT INTO hub_signals(subject,msg_id,signal_id,created_ms,data) VALUES(?,?,?,?,{})",
            blob_literal(&bytes)
        ),
        &[
            json!(subject),
            json!(msg_id),
            json!(data["signal_id"].as_str()),
            json!(now),
        ],
    )?;
    let seq =
        one(c, "SELECT last_insert_rowid() AS seq", &[])?.unwrap_or(Value::Null)["seq"].clone();
    audit(c, principal_id(caller), "send", target, data)?;
    events.push(json!({"type":"signal","seq":seq,"subject":subject,"created_ms":now,"data_b64":B64.encode(&bytes),"acked_at_ms":null}));
    Ok(json!({"stream":SIGNALS,"seq":seq,"duplicate":false}))
}
fn consumer_owner(row: &Value, p: Option<&Value>) -> Result<(), String> {
    if oidc(p) && !owns_principal(s(row, "principal_id"), p) {
        Err("FORBIDDEN: consumer belongs to another principal".into())
    } else {
        Ok(())
    }
}
fn consumer(c: &Connection, name: &str, p: Option<&Value>) -> Result<Value, String> {
    let row = one(
        c,
        "SELECT * FROM hub_consumers WHERE name=?",
        &[json!(name)],
    )?
    .ok_or("NOT_FOUND: consumer not found")?;
    consumer_owner(&row, p)?;
    Ok(row)
}
fn backlog_from(
    c: &Connection,
    row: &Value,
    p: Option<&Value>,
    min_seq: i64,
) -> Result<Value, String> {
    let mut items = Vec::new();
    let mut page_bytes = 0;
    let filter = s(row, "filter_subject");
    let cutoff = signal_cutoff(c)?;
    let wildcard = filter.is_empty() || filter.contains('*') || filter.contains('>');
    c.query_while(&format!("{SIGNAL_SELECT} WHERE created_ms>=? AND seq>=? AND (? OR subject=?) AND seq NOT IN(SELECT seq FROM hub_consumer_acks WHERE name=?) ORDER BY seq"),&[json!(cutoff),json!(min_seq),json!(wildcard),json!(filter),row["name"].clone()],|item| {
        if matches(s(&item, "subject"), s(row, "filter_subject"))
            && can_consume(c, subject_target(s(&item, "subject")).unwrap_or(""), p)?
        {
            let wire = signal_wire(&item)?;
            let size = wire.to_string().len() + 1;
            // Apply the caller's sequence floor before byte pagination so unread pages
            // cannot hide later items from sequential consumers.
            if !items.is_empty() && page_bytes + size > 4 * 1024 * 1024 { return Ok(false); }
            page_bytes += size;
            items.push(wire);
        }
        Ok(true)
    })?;
    Ok(
        json!({"filterSubject":row["filter_subject"],"startSeq":row["start_seq"],"backlog":items,"server_now_ms":now_ms()}),
    )
}
fn ids(p: &Value) -> Result<Vec<String>, String> {
    let array = p["signal_ids"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("INVALID_INPUT: signal_ids must contain at least one ID")?;
    let mut out = Vec::new();
    for id in array {
        let id = id
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("INVALID_INPUT: signal ID must be a string")?
            .to_owned();
        if !out.contains(&id) {
            out.push(id)
        }
    }
    Ok(out)
}
fn claim(c: &Connection, p: &Value, caller: Option<&Value>) -> Result<Value, String> {
    let node = required(p, "node_id")?;
    let channel = required(p, "channel_id")?;
    own(c, node, caller)?;
    let ids = ids(p)?;
    let now = now_ms();
    let mut rows = Vec::new();
    let mut acked = Vec::new();
    let mut conflict = false;
    for id in &ids {
        let row=one(c,"SELECT seq,claimed_by_channel,claim_until_ms,acked_at_ms FROM hub_signals WHERE subject=? AND signal_id=?",&[json!(format!("cito.signal.{node}")),json!(id)])?;
        match row {
            Some(row) if row["acked_at_ms"].is_null() => {
                if !row["claimed_by_channel"].is_null()
                    && s(&row, "claimed_by_channel") != channel
                    && n(&row, "claim_until_ms") > now
                {
                    conflict = true;
                }
                rows.push(row)
            }
            _ => acked.push(id),
        }
    }
    if !acked.is_empty() {
        return Ok(
            json!({"claimed":false,"reason":"already_acked","claimed_ids":[],"acked_ids":acked}),
        );
    }
    if conflict {
        return Ok(
            json!({"claimed":false,"reason":"already_claimed","claimed_ids":[],"acked_ids":[]}),
        );
    }
    let lease = p["lease_ms"]
        .as_i64()
        .unwrap_or(30_000)
        .clamp(1000, 300_000);
    for row in rows {
        c.execute(
            "UPDATE hub_signals SET claimed_by_channel=?,claim_until_ms=? WHERE seq=?",
            &[json!(channel), json!(now + lease), row["seq"].clone()],
        )?;
    }
    Ok(json!({"claimed":true,"reason":null,"claimed_ids":ids,"acked_ids":[]}))
}
fn ack(
    c: &Connection,
    p: &Value,
    caller: Option<&Value>,
    events: &mut Vec<Value>,
) -> Result<Value, String> {
    let node = required(p, "node_id")?;
    own(c, node, caller)?;
    let ids = ids(p)?;
    let now = now_ms();
    let mut count = 0;
    for id in &ids {
        count+=c.execute("UPDATE hub_signals SET acked=1,acked_at_ms=COALESCE(acked_at_ms,?),claimed_by_channel=NULL,claim_until_ms=NULL WHERE subject=? AND signal_id=?",&[json!(now),json!(format!("cito.signal.{node}")),json!(id)])?;
    }
    audit(c, principal_id(caller), "ack", node, p)?;
    let data = json!({"type":"node_ack","node_id":node,"signal_ids":ids,"acked_at_ms":now,"channel_id":p["channel_id"]});
    events
        .push(json!({"type":"nc","subject":format!("cito.ack.{node}"),"data_b64":encoded(&data)}));
    Ok(json!({"acked_count":count,"acked_at_ms":now,"signal_ids":ids}))
}

fn fields(plugin: &ChannelPlugin) -> Vec<Value> {
    plugin.manifest["fields"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}
fn configured(v: &Value) -> bool {
    match v {
        Value::String(s) => !s.trim().is_empty(),
        Value::Null => false,
        _ => true,
    }
}
fn ready(plugin: &ChannelPlugin, config: &Value, scope: &str) -> bool {
    fields(plugin)
        .iter()
        .filter(|f| s(f, "scope") == scope && f["required"] == true)
        .all(|f| configured(&config[s(f, "key")]))
}
fn validate_config(plugin: &ChannelPlugin, patch: &Value, scope: &str) -> Result<(), String> {
    let patch = patch
        .as_object()
        .ok_or("INVALID_INPUT: channel config must be an object")?;
    let fields = fields(plugin);
    for (key, value) in patch {
        let field = fields
            .iter()
            .find(|f| s(f, "key") == key && s(f, "scope") == scope)
            .ok_or_else(|| format!("INVALID_INPUT: unknown {scope} field: {key}"))?;
        if !(value.is_null()
            || value.is_string()
            || s(field, "type") == "boolean" && value.is_boolean())
        {
            return Err(format!("INVALID_INPUT: wrong type for {key}"));
        }
    }
    Ok(())
}
fn service(c: &Connection, id: &str) -> Result<(Value, Value), String> {
    let row = one(
        c,
        "SELECT config_json,service_state_json FROM hub_plugin_services WHERE plugin_id=?",
        &[json!(id)],
    )?;
    Ok(row
        .as_ref()
        .map(|r| (parse(r, "config_json"), parse(r, "service_state_json")))
        .unwrap_or((json!({}), json!({}))))
}
fn service_identity(plugin: &ChannelPlugin, cfg: &Value) -> Value {
    let identity = &plugin.manifest["serviceIdentity"];
    let Some(field) = identity["field"].as_str() else {
        return cfg.clone();
    };
    let value = s(cfg, field);
    json!(identity["separator"]
        .as_str()
        .filter(|separator| !separator.is_empty())
        .and_then(|separator| value.split_once(separator).map(|(prefix, _)| prefix))
        .filter(|prefix| !prefix.is_empty())
        .unwrap_or(value))
}
fn save_channel_service(
    c: &Connection,
    plugin: &ChannelPlugin,
    cfg: &Value,
) -> Result<bool, String> {
    if plugin.manifest["serviceIdentity"]["field"]
        .as_str()
        .is_none()
    {
        return save_service(c, &plugin.id, cfg);
    }
    let (old, _) = service(c, &plugin.id)?;
    let change = if service_identity(plugin, &old) != service_identity(plugin, cfg) {
        ServiceChange::Identity
    } else {
        ServiceChange::SameIdentity
    };
    save_service_state(c, &plugin.id, cfg, change)
}
fn save_service(c: &Connection, id: &str, cfg: &Value) -> Result<bool, String> {
    save_service_state(c, id, cfg, ServiceChange::Configuration)
}
enum ServiceChange {
    Configuration,
    SameIdentity,
    Identity,
}
fn save_service_state(
    c: &Connection,
    id: &str,
    cfg: &Value,
    change: ServiceChange,
) -> Result<bool, String> {
    let (old, _) = service(c, id)?;
    let changed = old != *cfg;
    if !changed
        && one(
            c,
            "SELECT 1 FROM hub_plugin_services WHERE plugin_id=?",
            &[json!(id)],
        )?
        .is_some()
    {
        return Ok(false);
    }
    c.execute("INSERT INTO hub_plugin_services(plugin_id,config_json,service_state_json,updated_ms) VALUES(?,?,'{}',?) ON CONFLICT(plugin_id) DO UPDATE SET config_json=excluded.config_json,service_state_json=CASE WHEN ? THEN '{}' ELSE service_state_json END,updated_ms=excluded.updated_ms", &[json!(id),json!(cfg.to_string()),json!(now_ms()),json!(!matches!(change, ServiceChange::SameIdentity))])?;
    if matches!(change, ServiceChange::Identity) {
        c.execute(
            "DELETE FROM hub_channel_delivery_msgs WHERE plugin_id=?",
            &[json!(id)],
        )?;
        c.execute("UPDATE hub_channel_deliveries SET external_msg_id=NULL,from_listener_id=NULL,reservation_id=NULL,status=CASE WHEN status='in_flight' THEN 'effect_unknown' ELSE status END,last_error=CASE WHEN status='in_flight' THEN 'Service identity replaced during delivery; check the previous account' ELSE last_error END WHERE plugin_id=?", &[json!(id)])?;
    }
    if !matches!(change, ServiceChange::SameIdentity) {
        c.execute(
            "DELETE FROM hub_processed_replies WHERE plugin_id=?",
            &[json!(id)],
        )?;
        // Generic configuration edits retain confirmed chunks; identity replacement fences them.
        c.execute(
            "UPDATE hub_pairing_codes SET consumed_ms=? WHERE plugin_id=? AND consumed_ms IS NULL",
            &[json!(now_ms()), json!(id)],
        )?;
    }
    Ok(changed)
}
const REDACTED_SECRET: &str = "••••••••";
fn merge_channel_config(
    plugin: &ChannelPlugin,
    scope: &str,
    base: &mut Value,
    patch: &Value,
    allow_clear_secret: bool,
) {
    if let Some(patch) = patch.as_object() {
        for (key, value) in patch {
            let secret = fields(plugin)
                .iter()
                .any(|f| s(f, "key") == key && s(f, "scope") == scope && s(f, "type") == "secret");
            if secret
                && (value.as_str() == Some(REDACTED_SECRET)
                    || (!configured(value) && !allow_clear_secret))
            {
                continue;
            }
            if !configured(value) {
                if let Some(m) = base.as_object_mut() {
                    m.remove(key);
                }
            } else {
                base[key] = value.clone();
            }
        }
    }
}
// Destination replacement retires reservation tokens atomically with the binding.
// Unknown effects remain terminal; confirmed delivery cursors never move backwards.
fn replace_destination(
    c: &Connection,
    plugin: &ChannelPlugin,
    pid: &str,
    old: Option<&Value>,
    config: &Value,
) -> Result<(), String> {
    let id = &plugin.id;
    let identity = |v: &Value| {
        (
            v["_external_chat_id"].as_str().map(|s| s.trim().to_owned()),
            v["_external_user_id"].as_str().map(|s| s.trim().to_owned()),
        )
    };
    let next = identity(config);
    if next.0.is_some() && next.1.is_some() {
        for binding in c.query(
            "SELECT * FROM hub_human_channels WHERE plugin_id=?",
            &[json!(id)],
        )? {
            if !principal_ids_equal(s(&binding, "principal_id"), pid)
                && identity(&parse(&binding, "user_config_json")) == next
            {
                return Err("DESTINATION_IN_USE: This external account belongs to another user, including paused accounts; unpair it first".into());
            }
        }
    }
    if old.is_some_and(|old| {
        identity(old) != next
            || next.0.is_none()
                && next.1.is_none()
                && plugin.manifest["pairing"]["privateIdentityField"]
                    .as_str()
                    .map(|key| old[key] != config[key])
                    .unwrap_or(old != config)
    }) {
        c.execute(
            "DELETE FROM hub_channel_delivery_msgs WHERE plugin_id=? AND principal_id=?",
            &[json!(id), json!(pid)],
        )?;
        c.execute("UPDATE hub_channel_deliveries SET external_msg_id=NULL,reservation_id=NULL,from_listener_id=NULL,status=CASE WHEN status='in_flight' THEN 'effect_unknown' ELSE status END,last_error=CASE WHEN status='in_flight' THEN 'Destination replaced while delivery was in flight; check the previous destination' ELSE last_error END WHERE plugin_id=? AND principal_id=?", &[json!(id),json!(pid)])?;
    }
    Ok(())
}
fn cancel_pair(c: &Connection, id: &str, caller: &Value) -> Result<(), String> {
    c.execute("UPDATE hub_pairing_codes SET consumed_ms=? WHERE principal_id=? AND plugin_id=? AND consumed_ms IS NULL", &[json!(now_ms()),caller["principal_id"].clone(),json!(id)])?;
    Ok(())
}
fn max_seq(c: &Connection) -> Result<i64, String> {
    Ok(one(
        c,
        "SELECT COALESCE(MAX(seq),0) AS seq FROM hub_signals",
        &[],
    )?
    .map(|r| n(&r, "seq"))
    .unwrap_or(0))
}
fn delivery_transition(frame: &Value, attempts: i64, now: i64) -> (&'static str, i64) {
    if frame["ok"] == true {
        ("delivered", 0)
    } else if frame["effect_unknown"] == true {
        ("effect_unknown", 0)
    } else if frame["retryable"] == false || attempts >= 5 {
        ("failed_permanent", 0)
    } else {
        (
            "failed",
            now.saturating_add(
                (2_000_i64 << attempts.clamp(0, 5)).min(60_000).max(
                    frame["retry_after"]
                        .as_u64()
                        .map(|seconds| seconds.saturating_mul(1000).min(i64::MAX as u64) as i64)
                        .unwrap_or(0),
                ),
            ),
        )
    }
}

#[cfg(test)]
mod regression_tests;

#[cfg(test)]
mod resilience_tests {
    use super::*;
    #[test]
    fn legacy_oidc_agent_reenrollment_preserves_tuple_boundaries() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!(
            "cito-legacy-agent-{}",
            crate::plugins::random_uuid_v4()
        ));
        let hub = Arc::new(HubEngine::open(&dir)?);
        let result = (|| {
            use crate::core::{
                registration::register_listener, state::LocalState, transport::HubClient,
            };
            let first = LocalState::open(&dir.join("first"))?;
            let second = LocalState::open(&dir.join("second"))?;
            let mut client = HubClient::new(
                "work".into(),
                json!({"url":"local://","authMode":"oidc"}),
                "alice",
                Some(hub.clone()),
            );
            client.identity = json!({"principal_id":"https://id.example.com#alice","owner_user":"alice","oidc":true});
            let agent = json!({"harness":"fixture","session_id":"opaque|session ","title":"Agent"});
            let args = json!({"about":"Review changes"});
            let enrolled = register_listener(&first, &client, "first-host", &args, &agent)?;
            let id = s(&enrolled, "canonical_listener_id");
            assert_eq!(
                client.kv_get(id)?.unwrap().0["principal_id"],
                "https://id.example.com#alice"
            );
            client.identity = json!({"principal_id":"oidc:https://id.example.com|alice","legacy_principal_id":"https://id.example.com#alice","owner_user":"alice","oidc":true});
            for (state, host) in [(&first, "first-host"), (&second, "second-host")] {
                assert_eq!(
                    register_listener(state, &client, host, &args, &agent)?
                        ["canonical_listener_id"],
                    id
                );
            }
            let remote = client.kv_get(id)?.unwrap().0;
            assert_eq!(remote["channels"]["first-host:fixture"]["state"], "ACTIVE");
            assert_eq!(remote["channels"]["second-host:fixture"]["state"], "ACTIVE");
            assert_eq!(remote["session_id"], "opaque|session ");
            let mut row = json!({"principal_id":"https://id.example.com#alice","harness":"fixture|agent","session_id":"session","channels":{}});
            hub.rpc(
                "kv_create",
                &json!({"key":"agent","data":row.to_string()}),
                None,
            )?;
            row["principal_id"] = json!("oidc:https://id.example.com|alice");
            let caller = json!({"principal_id":row["principal_id"],"legacy_principal_id":"https://id.example.com#alice","oidc":true});
            for device in ["first", "second"] {
                row["channels"][device] = json!({"state":"ACTIVE"});
                hub.rpc(
                    "kv_put",
                    &json!({"key":"agent","data":row.to_string()}),
                    Some(&caller),
                )?;
            }
            for patch in [
                json!({"principal_id":"oidc:https://id.example.com|bob"}),
                json!({"harness":"fixture","session_id":"agent|session"}),
            ] {
                let mut collision = row.clone();
                merge(&mut collision, &patch);
                assert_eq!(
                    hub.rpc(
                        "kv_put",
                        &json!({"key":"agent","data":collision.to_string()}),
                        None
                    )
                    .unwrap_err(),
                    "PERMISSIONS_VIOLATION: listener_id tuple collision"
                );
            }
            Ok(())
        })();
        hub.shutdown();
        let _ = std::fs::remove_dir_all(dir);
        result
    }

    // Stop a real missing-executable tick at the exact tick-to-wait boundary.
    fn failed_worker_boundary(blocked_input: bool) -> (HubEngine, PathBuf, mpsc::Sender<()>) {
        let dir = std::env::temp_dir().join(format!(
            "cito-worker-boundary-{}",
            crate::plugins::random_uuid_v4()
        ));
        let hub = HubEngine::open(&dir).unwrap();
        let guard = hub.inner.lifecycle.lock().unwrap();
        std::fs::create_dir_all(dir.join("missing")).unwrap();
        *hub.inner.plugins.lock().unwrap() = vec![ChannelPlugin {
            is_builtin: false,
            id: "missing".into(),
            dir: dir.join("missing"),
            command: if blocked_input {
                vec!["/bin/sh".into(), "-c".into(), "exec sleep 30".into()]
            } else {
                vec![dir.join("unavailable").to_string_lossy().into_owned()]
            },
            manifest: json!({"kind":"human-channel","fields":[]}),
        }];
        let (entered, boundary) = mpsc::channel();
        let (resume, released) = mpsc::channel();
        *hub.inner.before_worker_wait.lock().unwrap() = Some(Box::new(move || {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(3)).unwrap();
        }));
        hub.inner.db.transaction(|c| save_service(c, "missing", &json!({"payload": if blocked_input { "x".repeat(128 * 1024) } else { "enabled".into() }}))).unwrap();
        drop(guard);
        boundary.recv_timeout(Duration::from_secs(4)).unwrap();
        let error = hub
            .inner
            .db
            .transaction(|c| service(c, "missing"))
            .unwrap()
            .1;
        let error = error["poll_error"].as_str().unwrap();
        assert!(
            error.contains(if blocked_input {
                "timed out"
            } else {
                "No such file"
            }),
            "{error}"
        );
        (hub, dir, resume)
    }

    #[test]
    fn shutdown_at_failed_tick_wait_boundary_is_prompt() {
        for blocked_input in [false, true] {
            let (hub, dir, resume) = failed_worker_boundary(blocked_input);
            let closing = hub.clone();
            let started = std::time::Instant::now();
            let shutdown = thread::spawn(move || closing.shutdown());
            while !hub.inner.closed.load(Ordering::SeqCst) {
                assert!(started.elapsed() < Duration::from_secs(1));
                thread::yield_now();
            }
            resume.send(()).unwrap();
            shutdown.join().unwrap();
            assert!(started.elapsed() < Duration::from_secs(1));
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn work_committed_at_failed_tick_wait_boundary_is_not_lost() {
        let (hub, dir, resume) = failed_worker_boundary(false);
        let before = hub.inner.worker_ticks.load(Ordering::SeqCst);
        // No event emission: the durable commit notifier alone must preserve the wake.
        hub.inner.db.execute("INSERT INTO hub_signals(subject,signal_id,created_ms,data) VALUES('cito.signal.node','new',?,X'7B7D')", &[json!(now_ms())]).unwrap();
        let started = std::time::Instant::now();
        resume.send(()).unwrap();
        while hub.inner.worker_ticks.load(Ordering::SeqCst) == before {
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "Work inherited the failed-channel retry delay"
            );
            thread::sleep(Duration::from_millis(1));
        }
        hub.shutdown();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persistent_failed_tick_waits_five_seconds_without_health_writes() {
        let (hub, dir, resume) = failed_worker_boundary(false);
        let revision = hub.inner.db.mutation_revision().unwrap();
        let started = std::time::Instant::now();
        resume.send(()).unwrap();
        // The setup commit may itself have arrived during the first tick; allow
        // that genuine pending generation to drain before observing the retry.
        thread::sleep(Duration::from_millis(350));
        let (retried, retry) = mpsc::channel();
        *hub.inner.before_worker_wait.lock().unwrap() = Some(Box::new(move || {
            retried.send(()).unwrap();
        }));
        retry.recv_timeout(Duration::from_secs(7)).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(4800));
        assert!(started.elapsed() < Duration::from_secs(7));
        assert_eq!(hub.inner.db.mutation_revision().unwrap(), revision);
        let started = std::time::Instant::now();
        hub.shutdown();
        assert!(started.elapsed() < Duration::from_secs(1));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn live_failed_worker_keeps_retry_wait_and_healthy_sibling_progress() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!(
            "cito-live-retry-{}",
            crate::plugins::random_uuid_v4()
        ));
        let hub = HubEngine::open(&dir)?;
        let result = (|| {
            let guard = hub.inner.lifecycle.lock().map_err(|e| e.to_string())?;
            *hub.inner.plugins.lock().map_err(|e| e.to_string())? = [
                ("missing", vec![dir.join("unavailable").to_string_lossy().into_owned()]),
                ("healthy", vec!["/bin/sh".into(), "-c".into(), "while IFS= read -r line; do case \"$line\" in *configure*) printf '%s\\n' '{\"op\":\"cursor_save\",\"cursor\":7}';; esac; done".into()]),
            ].into_iter().map(|(id, command)| ChannelPlugin { is_builtin:false, id:id.into(), dir:dir.join(id), command, manifest:json!({"kind":"human-channel","fields":[]}) }).collect();
            for id in ["missing", "healthy"] {
                std::fs::create_dir_all(dir.join(id)).map_err(|e| e.to_string())?;
            }
            hub.inner.db.transaction(|c| {
                for id in ["missing", "healthy"] {
                    save_service(c, id, &json!({"enabled":true}))?;
                }
                Ok(())
            })?;
            drop(guard);
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                let (_, healthy) = hub.inner.db.transaction(|c| service(c, "healthy"))?;
                if healthy["cursor"] == 7 {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "Healthy sibling did not progress"
                );
                thread::sleep(Duration::from_millis(5));
            }
            let before = hub.inner.worker_ticks.load(Ordering::SeqCst);
            let writes = hub.inner.db.query("SELECT total_changes() AS n", &[])?;
            for _ in 0..5 {
                hub.set_poll_error("missing", Some("Persistent failure"))?;
            }
            let settled = hub.inner.db.query("SELECT total_changes() AS n", &[])?;
            hub.set_poll_error("missing", Some("Persistent failure"))?;
            assert_eq!(
                hub.inner.db.query("SELECT total_changes() AS n", &[])?,
                settled
            );
            assert_ne!(writes, settled);
            thread::sleep(Duration::from_millis(350));
            assert!(
                hub.inner.worker_ticks.load(Ordering::SeqCst) - before < 8,
                "Worker spun on diagnostic commits"
            );
            let channel = &hub
                .channel_list(&json!({"principal_id":"none#alice","owner_user":"alice"}))?
                ["channels"][0];
            assert_eq!(channel["warning"], true);
            assert_eq!(channel["error_origin"], "hub");
            Ok(())
        })();
        hub.shutdown();
        let _ = std::fs::remove_dir_all(dir);
        result
    }

    #[test]
    fn channel_inventory_projects_caller_authority_and_error_origin() {
        let dir = std::env::temp_dir().join(format!(
            "cito-channel-authority-{}",
            crate::plugins::random_uuid_v4()
        ));
        let hub = HubEngine::open(&dir).unwrap();
        hub.pause_worker();
        let plugin = ChannelPlugin {
            is_builtin: false,
            id: "sample".into(),
            dir: dir.clone(),
            command: vec![],
            manifest: json!({"fields":[{"key":"credential","scope":"hub","required":true},
                {"key":"destination","scope":"user","required":true}],"pairing":{"supported":true}}),
        };
        *hub.inner.plugins.lock().unwrap() = vec![plugin.clone()];
        let caller = json!({"principal_id":"oidc:alice","owner_user":"alice","oidc":true});
        let channel = hub.channel_list(&caller).unwrap()["channels"][0].clone();
        assert_eq!(channel["can_configure_hub"], false);
        assert_eq!(channel["can_pair"], false);
        assert_eq!(channel["error_origin"], Value::Null);
        assert_eq!(
            channel["detail"],
            "Shared service credentials must be configured by the Hub administrator"
        );
        let admin = json!({"principal_id":"none#alice","owner_user":"alice"});
        assert_eq!(
            hub.channel_list(&admin).unwrap()["channels"][0]["can_configure_hub"],
            true
        );
        hub.inner
            .db
            .transaction(|c| save_service(c, "sample", &json!({"credential":"saved"})))
            .unwrap();
        let channel = hub.channel_list(&caller).unwrap()["channels"][0].clone();
        assert_eq!(channel["hub_ready"], true);
        assert_eq!(channel["can_pair"], true);
        assert!(s(&channel, "detail").contains("one click"));
        hub.inner
            .db
            .execute(
                "UPDATE hub_plugin_services SET service_state_json=?",
                &[json!(
                    json!({"poll_error":"Service unavailable"}).to_string()
                )],
            )
            .unwrap();
        assert_eq!(
            hub.channel_list(&caller).unwrap()["channels"][0]["error_origin"],
            "hub"
        );
        hub.inner
            .db
            .execute(
                "UPDATE hub_plugin_services SET service_state_json='{}'",
                &[],
            )
            .unwrap();
        hub.inner.db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,delivered_ms,attempts,last_error) VALUES('sample','oidc:alice','signal',1,'in_flight',?,1,'Retry pending')",&[json!(now_ms())]).unwrap();
        let channel = hub.channel_list(&caller).unwrap()["channels"][0].clone();
        assert_eq!(channel["warning"], true);
        assert_eq!(channel["error_origin"], "user");
        hub.shutdown();
        drop(hub);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn delivery_retry_honors_channel_retry_after() {
        let frame = json!({"ok":false,"retryable":true,"retry_after":120});
        assert_eq!(delivery_transition(&frame, 0, 1000), ("failed", 121_000));
        assert_eq!(
            delivery_transition(&json!({"ok":false,"retryable":true}), 0, 1000),
            ("failed", 3000)
        );
    }
    #[test]
    fn worker_delay_ignores_expired_reservations_on_disabled_bindings() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!(
            "cito-worker-delay-{}",
            crate::plugins::random_uuid_v4()
        ));
        let hub = HubEngine::open(&dir)?;
        hub.pause_worker();
        let result = (|| {
            let now = now_ms();
            hub.inner.db.execute(
                "UPDATE hub_meta SET int_val=? WHERE key='last_maintenance_ms'",
                &[json!(now)],
            )?;
            hub.inner.db.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,enabled) VALUES('none#alice','fixture',0)", &[])?;
            hub.inner.db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,delivered_ms) VALUES('fixture','none#alice','expired',1,'in_flight',?)", &[json!(now-70_000)])?;
            assert!(hub.worker_delay()? > Duration::from_secs(1));
            hub.inner.db.execute(
                "UPDATE hub_channel_deliveries SET delivered_ms=?",
                &[json!(now - 59_500)],
            )?;
            assert!(
                hub.worker_delay()? < Duration::from_secs(1),
                "An unexpired reservation still supplies a deadline"
            );
            hub.inner.db.execute(
                "UPDATE hub_channel_deliveries SET status='failed',next_retry_ms=?",
                &[json!(now + 500)],
            )?;
            assert!(hub.worker_delay()? < Duration::from_secs(1));
            hub.inner.db.execute(
                "UPDATE hub_channel_deliveries SET next_retry_ms=?",
                &[json!(now - 1)],
            )?;
            assert!(hub.worker_delay()? > Duration::from_secs(1));
            Ok(())
        })();
        hub.shutdown();
        let _ = std::fs::remove_dir_all(dir);
        result
    }
    #[test]
    fn idle_backlog_and_reservations_do_not_prune_or_write() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!(
            "cito-maintenance-{}",
            crate::plugins::random_uuid_v4()
        ));
        let hub = HubEngine::open(&dir)?;
        let lifecycle = hub.inner.lifecycle.lock().map_err(|e| e.to_string())?;
        let result = (|| {
            hub.inner.db.execute("INSERT INTO hub_signals(subject,signal_id,created_ms,data) VALUES('cito.signal.node','old',?,CAST('{}' AS BLOB))", &[json!(now_ms()-2_000_000)])?;
            let plugin = ChannelPlugin {
                is_builtin: false,
                id: "fixture".into(),
                dir: dir.clone(),
                manifest: json!({}),
                command: vec![],
            };
            let changes = || hub.inner.db.query("SELECT total_changes() AS n", &[]);
            let before = changes()?;
            for _ in 0..8 {
                hub.inner.db.transaction(|c| {
                    assert!(backlog_from(c,&json!({"name":"consumer","filter_subject":"cito.signal.node","start_seq":1}),None,0)?["backlog"].as_array().is_some_and(Vec::is_empty));
                    Ok(())
                })?;
                assert!(hub.reserve_deliveries(&plugin)?.is_empty());
            }
            assert_eq!(
                changes()?,
                before,
                "Read-side idle checks must not issue SQLite cleanup writes"
            );
            assert_eq!(
                hub.inner
                    .db
                    .query("SELECT COUNT(*) AS n FROM hub_signals", &[])?[0]["n"],
                1
            );
            hub.inner.db.transaction(maintain)?;
            assert_eq!(
                changes()?,
                before,
                "Maintenance must honor its persisted 60-second throttle"
            );
            hub.inner.db.execute(
                "UPDATE hub_meta SET int_val=? WHERE key='last_maintenance_ms'",
                &[json!(now_ms() - 60_001)],
            )?;
            hub.inner.db.transaction(maintain)?;
            assert_eq!(
                hub.inner
                    .db
                    .query("SELECT COUNT(*) AS n FROM hub_signals", &[])?[0]["n"],
                0
            );
            Ok(())
        })();
        drop(lifecycle);
        hub.shutdown();
        let _ = std::fs::remove_dir_all(dir);
        result
    }
    #[test]
    fn transport_receipt_hints_are_visible_only_to_the_sender_and_target_principals(
    ) -> Result<(), String> {
        let dir =
            std::env::temp_dir().join(format!("cito-receipt-{}", crate::plugins::random_uuid_v4()));
        let db = Database::open(&dir.join("hub.sqlite"))?;
        let result = db.transaction(|c| {
            for statement in SCHEMA.split(';').map(str::trim).filter(|s|!s.is_empty()) { c.execute(statement,&[])?; }
            let sender = "https://id.example.com#alice";
            let target = "https://id.example.com#bob";
            c.execute("INSERT INTO hub_kv(key,revision,created_ms,value,principal_id,scope) VALUES('target',1,0,CAST(? AS BLOB),?,'self')", &[json!(json!({"principal_id":target}).to_string()),json!(target)])?;
            c.execute("INSERT INTO hub_signals(subject,signal_id,created_ms,data) VALUES('cito.signal.target','signal',?,CAST(? AS BLOB))", &[json!(now_ms()),json!(json!({"from":{"principal_id":sender}}).to_string())])?;
            let event = json!({"type":"receipt","seq":1});
            for (principal,allowed) in [(sender,true),(target,true),("https://id.example.com#charlie",false)] {
                assert_eq!(event_allowed(c,&event,Some(&json!({"principal_id":principal})))?,allowed);
            }
            Ok(())
        });
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
        result
    }
    #[test]
    fn sse_keepalive_has_a_calm_cadence_and_never_outlives_authentication() {
        assert_eq!(sse_wait(None, 1000), Duration::from_secs(30));
        assert_eq!(sse_wait(Some(100.0), 1000), Duration::from_secs(30));
        assert_eq!(sse_wait(Some(3.5), 1000), Duration::from_millis(2500));
        assert_eq!(sse_wait(Some(0.0), 1000), Duration::ZERO);
    }
    #[test]
    fn committed_channel_configuration_survives_missing_executable() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!(
            "cito-channel-commit-{}",
            crate::plugins::random_uuid_v4()
        ));
        let hub = HubEngine::open(&dir)?;
        *hub.inner.plugins.lock().map_err(|e| e.to_string())? = vec![ChannelPlugin {
            is_builtin: false,
            id: "messenger".into(),
            manifest: json!({"displayName":"Messenger","fields":[{"key":"destination","scope":"user","type":"text","required":true}]}),
            dir: dir.clone(),
            command: vec![dir.join("unavailable").to_string_lossy().into_owned()],
        }];
        let caller = json!({"principal_id":"none#alice","owner_user":"alice"});
        let result = hub.rpc(
            "human_channels_configure",
            &json!({"plugin_id":"messenger","enabled":true,"user_config":{"destination":"room"}}),
            Some(&caller),
        )?;
        assert_eq!(result["ok"], true);
        assert_eq!(result["committed"], true);
        assert!(result["maintenance_warning"].is_string(), "{result}");
        let inventory = hub.rpc("human_channels_list", &json!({}), Some(&caller))?;
        assert_eq!(inventory["channels"][0]["enabled"], true);
        assert_eq!(inventory["channels"][0]["warning"], true);
        assert_eq!(inventory["channels"][0]["error_origin"], "hub");
        let health = hub.inner.db.transaction(|c| service(c, "messenger"))?.1;
        assert!(health["poll_error"].is_string());
        let stored = hub
            .inner
            .db
            .query("SELECT user_config_json FROM hub_human_channels", &[])?;
        assert_eq!(parse(&stored[0], "user_config_json")["destination"], "room");
        hub.shutdown();
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn retry_reservation_keeps_warning_and_process_stop_clears_poll_health() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!(
            "cito-channel-health-{}",
            crate::plugins::random_uuid_v4()
        ));
        let hub = HubEngine::open(&dir)?;
        // Serialize the manually driven channel transitions against the mutation-woken worker.
        let lifecycle = hub.inner.lifecycle.lock().map_err(|e| e.to_string())?;
        *hub.inner.plugins.lock().map_err(|e| e.to_string())? = vec![ChannelPlugin {
            is_builtin: false,
            id: "messenger".into(),
            manifest: json!({"displayName":"Messenger","fields":[]}),
            dir: dir.clone(),
            command: vec![],
        }];
        hub.inner.db.execute("INSERT INTO hub_plugin_services(plugin_id,config_json,service_state_json,updated_ms) VALUES('messenger','{}','{}',0)",&[])?;
        hub.inner.db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,attempts,delivered_ms,last_error) VALUES('messenger','none#alice','signal',1,'failed',1,1,'Retry pending')",&[])?;
        let caller = json!({"principal_id":"none#alice","owner_user":"alice"});
        assert_eq!(hub.channel_list(&caller)?["channels"][0]["warning"], true);
        hub.inner
            .db
            .execute("UPDATE hub_channel_deliveries SET status='in_flight'", &[])?;
        let channel = hub.channel_list(&caller)?["channels"][0].clone();
        assert_eq!(channel["warning"], true);
        assert_eq!(channel["detail"], "Retry pending");
        hub.inner.db.execute(
            "UPDATE hub_channel_deliveries SET status='delivered',last_error=NULL",
            &[],
        )?;
        assert_eq!(hub.channel_list(&caller)?["channels"][0]["warning"], false);
        hub.plugin_frame(
            "messenger",
            &json!({"op":"health","error":"Temporary poll failure"}),
        );
        assert_eq!(hub.channel_list(&caller)?["channels"][0]["warning"], true);
        hub.tick_channels()?;
        assert_eq!(hub.channel_list(&caller)?["channels"][0]["warning"], false);
        drop(lifecycle);
        hub.shutdown();
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn save_human(c: &Connection, id: &str, reg: &mut Value) -> Result<Value, String> {
    let now = now_ms();
    super::lease::reconcile_registration_state(reg, now.max(0) as u64);
    let revision = next_revision(c)?;
    c.execute("INSERT INTO hub_kv(key,revision,created_ms,value,principal_id,scope) VALUES(?,?,?,CAST(? AS BLOB),?,?) ON CONFLICT(key) DO UPDATE SET revision=excluded.revision,created_ms=excluded.created_ms,value=excluded.value,principal_id=excluded.principal_id,scope=excluded.scope", &[json!(id),json!(revision),json!(now),json!(reg.to_string()),reg["principal_id"].clone(),reg["scope"].clone()])?;
    Ok(
        json!({"type":"kv","key":id,"revision":revision,"created_ms":now,"value_b64":encoded(reg),"operation":"PUT","principal_id":reg["principal_id"],"scope":reg["scope"]}),
    )
}

fn reconcile_human_row(c: &Connection, row: Value) -> Result<Value, String> {
    let mut reg = parse(&row, "value");
    if reg["harness"] != "human" {
        return Ok(row);
    }
    let before = reg.clone();
    if reg["human_control_revision"].is_null() {
        reg["human_control_revision"] = json!(1);
    }
    super::lease::reconcile_registration_state(&mut reg, now_ms().max(0) as u64);
    if reg != before {
        save_human(c, s(&row, "key"), &mut reg)?;
        return kv_row(c, s(&row, "key"))?
            .ok_or_else(|| "STATE_ERROR: Human record disappeared".into());
    }
    Ok(row)
}

fn initial_human_scope(principal: &Value, default_scope: &'static str) -> &'static str {
    if oidc(Some(principal)) || principal["auth_mode"] == "token" {
        "self"
    } else {
        default_scope
    }
}

/// Each operation owns one explicit write set under the Hub database transaction.
fn human_operation(
    c: &Connection,
    op: &str,
    p: &Value,
    caller: Option<&Value>,
    default_scope: &'static str,
    events: &mut Vec<Value>,
) -> Result<Value, String> {
    let caller = caller.ok_or("FORBIDDEN: Authenticated principal required")?;
    let pid = required(caller, "principal_id")?;
    let id = find_human(c, caller)?.unwrap_or_else(|| listener_id(pid, "human", "default"));
    if p.get("node_id").is_some_and(|node| node != &id) {
        return Err("FORBIDDEN: Human operation belongs to another principal".into());
    }
    let old = kv_row(c, &id)?;
    let mut reg = if let Some(old) = old {
        if !owns_principal(s(&old, "principal_id"), Some(caller)) {
            return Err("FORBIDDEN: Human principal mismatch".into());
        }
        parse(&old, "value")
    } else {
        if !matches!(op, "human_device_attach" | "human_inbox_update") {
            return Err("LISTENER_NOT_FOUND: Human inbox not found".into());
        }
        json!({"listener_id":id,"public_id":super::identity::public_listener_id(s(caller,"owner_user"),&id),"principal_id":pid,"owner_user":caller["owner_user"],"harness":"human","session_id":"default","title":"Human","about":"Human","scope":initial_human_scope(caller, default_scope),"human_paused":false,"human_control_revision":1,"binding_generation":1,"channels":{}})
    };
    if !reg.is_object() {
        return Err("STATE_ERROR: Human record must be an object".into());
    }
    if reg["human_control_revision"].is_null() {
        reg["human_control_revision"] = json!(1);
    }
    let now = now_ms();
    let previous_scope = reg["scope"].clone();
    super::lease::reconcile_registration_state(&mut reg, now.max(0) as u64);
    if op == "human_inbox_update" {
        let revision = reg["human_control_revision"]
            .as_u64()
            .ok_or("STATE_ERROR: Invalid Human control revision")?;
        if p["expected_control_revision"].as_u64() != Some(revision) {
            // Initialize legacy revisions even when the requested update conflicts.
            // The current state travels with the error; callers must ask the user again.
            events.push(save_human(c, &id, &mut reg)?);
            return Ok(json!({"control_revision_conflict":true,"record":reg,"server_now_ms":now}));
        }
        for (input, field) in [
            ("paused", "human_paused"),
            ("scope", "scope"),
            ("about", "about"),
        ] {
            if let Some(value) = p.get(input) {
                let valid = match input {
                    "paused" => value.is_boolean(),
                    "scope" => matches!(value.as_str(), Some("self" | "workspace")),
                    _ => value
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 500),
                };
                if !valid {
                    return Err(format!("INVALID_INPUT: Invalid {input}"));
                }
                reg[field] = value.clone();
            }
        }
        reg["human_control_revision"] = json!(revision
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or("STATE_ERROR: Human revision exhausted")?);
    } else {
        let channel = required(p, "channel_id")?;
        let generation = p["binding_generation"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= i64::MAX as u64)
            .ok_or("INVALID_INPUT: binding_generation required")?;
        let token = required(p, "owner_token")?;
        if !reg["channels"].is_object() {
            reg["channels"] = json!({});
        }
        let existing = &reg["channels"][channel];
        let same = existing["binding_generation"] == generation && existing["owner_token"] == token;
        if op == "human_device_attach" {
            let host = required(p, "host_id")?;
            if host == "hub" || channel != format!("{host}:human") {
                return Err("INVALID_INPUT: Device channel must match host_id".into());
            }
            if generation
                <= reg["device_generation_fences"][channel]
                    .as_u64()
                    .unwrap_or(0)
                || (!existing.is_null()
                    && !same
                    && generation <= existing["binding_generation"].as_u64().unwrap_or(0))
            {
                return Err("STALE_GENERATION: Human device attachment superseded".into());
            }
            reg["channels"][channel] = json!({"channel_id":channel,"host_id":host,"host_name":p["host_name"],"kind":"human","delivery_mode":"notify_fanout","priority":100,"state":"ACTIVE","updated_at_ms":now,"binding_generation":generation,"owner_token":token});
        } else {
            if !same {
                return Err("STALE_GENERATION: Human device attachment superseded".into());
            }
            if op == "human_device_heartbeat" {
                if reg["channels"][channel]["state"] != "ACTIVE" {
                    return Err("STALE_GENERATION: Human device attachment is inactive".into());
                }
                reg["channels"][channel]["updated_at_ms"] = json!(now);
            } else {
                reg["channels"][channel]["state"] = json!("UNLISTENED");
            }
        }
    }
    events.push(save_human(c, &id, &mut reg)?);
    if previous_scope == "workspace" && reg["scope"] == "self" {
        events.push(json!({"type":"kv"}));
    }
    audit(
        c,
        pid,
        op,
        &id,
        &json!({"human_control_revision":reg["human_control_revision"]}),
    )?;
    Ok(json!({"record":reg,"server_now_ms":now}))
}

fn find_human(c: &Connection, principal: &Value) -> Result<Option<String>, String> {
    let mut principals = Vec::new();
    for key in [
        "principal_id",
        "legacy_principal_id",
        "canonical_principal_id",
    ] {
        let value = s(principal, key);
        if value.is_empty() {
            continue;
        }
        principals.push(json!(value));
        if let Some((issuer, sub)) = value
            .strip_prefix("oidc:")
            .and_then(|value| value.split_once('|'))
        {
            principals.push(json!(format!("{issuer}#{sub}")));
        } else if value.starts_with("https://") {
            if let Some((issuer, sub)) = value.split_once('#') {
                principals.push(json!(format!("oidc:{issuer}|{sub}")));
            }
        }
    }
    if principals.is_empty() {
        return Ok(None);
    }
    let placeholders = vec!["?"; principals.len()].join(",");
    for row in c.query(
        &format!("{KV_SELECT} WHERE principal_id IN ({placeholders}) ORDER BY revision DESC"),
        &principals,
    )? {
        if !principal_matches(s(&row, "principal_id"), principal) {
            continue;
        }
        let v = parse(&row, "value");
        if v["harness"] == "human" && v["session_id"] == "default" {
            return Ok(Some(s(&row, "key").into()));
        }
    }
    Ok(None)
}
fn resolve_registration(c: &Connection, id: &str) -> Result<Option<Value>, String> {
    if let Some(row) = kv_row(c, id)? {
        return Ok(Some(row));
    }
    for row in c.query(KV_SELECT, &[])? {
        let v = parse(&row, "value");
        if s(&v, "public_id") == id {
            return Ok(Some(row));
        }
    }
    Ok(None)
}
fn attach(
    c: &Connection,
    b: &Value,
    enabled: bool,
    default_scope: &str,
    plugin: Option<&ChannelPlugin>,
) -> Result<(), String> {
    let pid = required(b, "principal_id")?;
    let id = find_human(c, b)?.unwrap_or_else(|| listener_id(pid, "human", "default"));
    let old = kv_row(c, &id)?;
    let mut reg=old.as_ref().map(|r|parse(r,"value")).unwrap_or_else(||json!({"listener_id":id,"public_id":super::identity::public_listener_id(s(b,"owner_user"), &id),"principal_id":pid,"owner_user":b["owner_user"],"host_id":"hub","host_name":"Hub","harness":"human","session_id":"default","cwd":"","repo":null,"branch":null,"title":"Human","about":"Human","human_paused":false,"human_control_revision":1,"scope":default_scope,"state":"ACTIVE","binding_generation":1,"owner_token":"hub","channels":{}}));
    if !reg.is_object() {
        return Err("INVALID_INPUT: Listener must be an object".into());
    }
    if !reg["channels"].is_object() {
        reg["channels"] = json!({})
    }
    if reg["human_control_revision"].is_null() {
        reg["human_control_revision"] = json!(1);
    }
    let channel = format!("hub:{}", s(b, "plugin_id"));
    reg["channels"][&channel] = json!({"channel_id":channel,"kind":"human","host_id":"hub","host_name":"Hub","display_name":plugin.map(|p|p.manifest["displayName"].clone()).unwrap_or_else(||b["plugin_id"].clone()),"display_label":b["display_label"],"delivery_mode":"notify_fanout","priority":100,"state":if enabled{"ACTIVE"}else{"UNLISTENED"},"updated_at_ms":now_ms()});
    super::lease::reconcile_registration_state(&mut reg, now_ms().max(0) as u64);
    reg["updated_at_ms"] = json!(now_ms());
    let rev = next_revision(c)?;
    c.execute("INSERT INTO hub_kv(key,revision,created_ms,value,principal_id,scope) VALUES(?,?,?,CAST(? AS BLOB),?,?) ON CONFLICT(key) DO UPDATE SET revision=excluded.revision,created_ms=excluded.created_ms,value=excluded.value,principal_id=excluded.principal_id,scope=excluded.scope",&[json!(id),json!(rev),json!(now_ms()),json!(reg.to_string()),json!(pid),reg["scope"].clone()])?;
    Ok(())
}

#[cfg(test)]
pub(super) struct TestDirectory(pub PathBuf);
#[cfg(test)]
impl std::ops::Deref for TestDirectory {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}
#[cfg(test)]
impl AsRef<Path> for TestDirectory {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}
#[cfg(test)]
impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            assert!(
                error.kind() == std::io::ErrorKind::NotFound || std::thread::panicking(),
                "Cannot remove fixture directory: {error}"
            );
        }
    }
}

#[cfg(test)]
pub(super) struct TestEngine(pub HubEngine);
#[cfg(test)]
impl std::ops::Deref for TestEngine {
    type Target = HubEngine;
    fn deref(&self) -> &HubEngine {
        &self.0
    }
}
#[cfg(test)]
impl Drop for TestEngine {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

#[cfg(test)]
#[path = "../../tests/hub/cases.rs"]
mod tests;

static HUB_STOP: AtomicBool = AtomicBool::new(false);
static SIGNAL_WRITE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
extern "C" fn hub_signal(_: libc::c_int) {
    HUB_STOP.store(true, Ordering::SeqCst);
    let fd = SIGNAL_WRITE.load(Ordering::SeqCst);
    if fd >= 0 {
        // SAFETY: writing one byte to a live nonblocking self-pipe is async-signal-safe.
        unsafe {
            libc::write(fd, b"s".as_ptr().cast(), 1);
        }
    }
}
fn cli_setting(flag: &str) -> &str {
    match flag {
        "--auth" => "CITO_AUTH_MODE",
        "--port" => "CITO_HUB_PORT",
        "--bind" => "CITO_HUB_BIND",
        "--token" => "CITO_HUB_TOKEN",
        "--issuer" => "CITO_OIDC_ISSUER",
        "--client-id" => "CITO_OIDC_CLIENT_ID",
        "--redirect-port" => "CITO_OIDC_REDIRECT_PORT",
        "--data-dir" => "CITO_HUB_DATA_DIR",
        _ => flag,
    }
}
fn cli_options(args: &[String]) -> Result<HashMap<String, String>, String> {
    let mut opts = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let key = arg.split_once('=').map(|(key, _)| key).unwrap_or(arg);
        if ![
            "--port",
            "--bind",
            "--auth",
            "--token",
            "--issuer",
            "--client-id",
            "--redirect-port",
            "--data-dir",
        ]
        .contains(&key)
        {
            return Err(if arg.starts_with('-') {
                format!("Unknown flag: {arg}")
            } else {
                format!("Unexpected argument: {arg}")
            });
        }
        let value = super::cli_value::read(args, &mut i, key, true)
            .map_err(|_| format!("Missing value for {key} ({})", cli_setting(key)))?
            .to_owned();
        if value.trim().is_empty() {
            return Err(format!("{key} ({}) must not be empty", cli_setting(key)));
        }
        opts.insert(key.into(), value);
        i += 1;
    }
    Ok(opts)
}
pub fn run_hub_cli(args: &[String]) {
    if let Err(err) = hub_cli(args) {
        eprintln!("cito hub: {err}");
        std::process::exit(1);
    }
}
fn hub_cli(args: &[String]) -> Result<(), String> {
    let opts = cli_options(args)?;
    let value = |flag: &str, env: &str| opts.get(flag).cloned().or_else(|| env_value(env));
    let mode = value("--auth", "CITO_AUTH_MODE").unwrap_or_else(|| "none".into());
    let port = value("--port", "CITO_HUB_PORT")
        .unwrap_or_else(|| "4291".into())
        .parse::<u16>()
        .map_err(|_| "CITO_HUB_PORT must be an integer in 0..65535")?;
    let bind = value("--bind", "CITO_HUB_BIND").unwrap_or_else(|| {
        if mode == "none" {
            "127.0.0.1".into()
        } else {
            "0.0.0.0".into()
        }
    });
    if bind.parse::<std::net::IpAddr>().is_err() {
        return Err("CITO_HUB_BIND must be an IP address".into());
    }
    let mut auth = HubAuth::new(
        &mode,
        value("--token", "CITO_HUB_TOKEN"),
        value("--issuer", "CITO_OIDC_ISSUER"),
        value("--client-id", "CITO_OIDC_CLIENT_ID"),
    )?;
    auth.redirect_port = value("--redirect-port", "CITO_OIDC_REDIRECT_PORT")
        .map(|raw| {
            raw.parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .ok_or("INVALID_INPUT: redirect port must be an integer in 1..65535")
        })
        .transpose()?;
    let raw = value("--data-dir", "CITO_HUB_DATA_DIR").unwrap_or_else(|| "~/.cito/hub".into());
    let home = env_value("HOME").unwrap_or_else(|| ".".into());
    let data_dir = super::plugins::expand_home_dir(&raw, "CITO_HUB_DATA_DIR", Some(&home))?;
    let engine = HubEngine::open_with_auth_mode(&data_dir, &mode)?;
    let server = engine.start_server(&bind, port, auth)?;
    eprintln!("cito hub: ready on http://{} (auth={mode})", server.address);
    let host = if bind == "0.0.0.0" || bind == "::" {
        "<host>"
    } else {
        &bind
    };
    let url = format!("http://{host}:{}", server.port());
    let join = match mode.as_str() {
        "token" => format!("cito setup --hub work --url {url} --auth token --token <token>"),
        "oidc" => format!(
            "cito setup --hub work --url {url} --auth oidc --issuer {} --client-id {}",
            value("--issuer", "CITO_OIDC_ISSUER").unwrap_or_default(),
            value("--client-id", "CITO_OIDC_CLIENT_ID").unwrap_or_default()
        ),
        _ => format!("cito setup --hub personal --url {url}"),
    };
    eprintln!("cito hub: join with: {join}");
    let mut fds = [-1; 2];
    // SAFETY: pipe and sigaction receive valid initialized out-pointers. The handler only
    // stores atomics and writes a byte; all cleanup runs below in ordinary thread context.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            server.shutdown();
            engine.shutdown();
            return Err(std::io::Error::last_os_error().to_string());
        }
        for fd in fds {
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        SIGNAL_WRITE.store(fds[1], Ordering::SeqCst);
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = hub_signal as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
    while !HUB_STOP.load(Ordering::SeqCst) {
        let mut pfd = libc::pollfd {
            fd: fds[0],
            events: libc::POLLIN,
            revents: 0,
        }; // SAFETY: pfd is live for the bounded poll.
        unsafe {
            libc::poll(&mut pfd, 1, -1);
        }
    }
    server.shutdown();
    engine.shutdown();
    SIGNAL_WRITE.store(-1, Ordering::SeqCst); // SAFETY: the handler can no longer access the descriptors.
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
    Ok(())
}
