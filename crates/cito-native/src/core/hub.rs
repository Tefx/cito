//! One persisted HubEngine for embedded and standalone HTTP deployments.
use super::auth::{env_value, principal_ids_equal, principal_matches, HubAuth};
use super::database::{Connection, Database};
use super::hub_plugins::{self, ChannelProcess, CHANNEL_RETRY_DELAY};
use super::identity::{alternate_oidc_principal, hash, listener_id, principal, string as s};
use super::lease::evaluate_node_lease;
use super::plugins::PluginDescriptor as ChannelPlugin;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use std::collections::HashMap;
mod http;
#[cfg(test)]
use http::sse_wait;
pub use http::HubServer;
pub(in crate::core) use http::PreparedHubServer;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SIGNALS: &str = "CITO_SIGNALS";
const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
const MAX_RPC_BYTES: usize = 8 * 1024 * 1024;
const MAX_STREAM_CONFIG_BYTES: usize = 64 * 1024;
const MAX_STREAM_IDENTIFIER_BYTES: usize = 256;
// Includes the RPC envelope and fixed snapshot fields; the cursor is counted separately.
const PAGE_OVERHEAD: usize = 128;
const KV_STREAM: &str = "KV_cito_listeners";
const KV_SELECT: &str =
    "SELECT key,revision,created_ms,CAST(value AS TEXT) AS value,hex(value) AS value_hex,principal_id,scope FROM hub_kv";
const SIGNAL_SELECT: &str = "SELECT seq,subject,signal_id,created_ms,CAST(data AS TEXT) AS data,hex(data) AS data_hex,acked_at_ms,sender_principal_id FROM hub_signals";
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS hub_meta(key TEXT PRIMARY KEY,int_val INTEGER NOT NULL DEFAULT 0,json_val TEXT);
CREATE TABLE IF NOT EXISTS hub_kv(key TEXT PRIMARY KEY,revision INTEGER NOT NULL,created_ms INTEGER NOT NULL,value BLOB NOT NULL,principal_id TEXT,scope TEXT);
CREATE TABLE IF NOT EXISTS hub_signals(seq INTEGER PRIMARY KEY AUTOINCREMENT,subject TEXT NOT NULL,msg_id TEXT UNIQUE,signal_id TEXT,created_ms INTEGER NOT NULL,sender_principal_id TEXT,data BLOB NOT NULL,claimed_by_channel TEXT,claim_until_ms INTEGER,acked_at_ms INTEGER);
CREATE TABLE IF NOT EXISTS hub_plugin_services(plugin_id TEXT PRIMARY KEY,config_json TEXT NOT NULL DEFAULT '{}',service_state_json TEXT NOT NULL DEFAULT '{}',updated_ms INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS hub_human_channels(principal_id TEXT NOT NULL,plugin_id TEXT NOT NULL,owner_user TEXT NOT NULL DEFAULT 'local',enabled INTEGER NOT NULL DEFAULT 1,user_config_json TEXT NOT NULL DEFAULT '{}',display_label TEXT,last_delivered_seq INTEGER NOT NULL DEFAULT 0,updated_ms INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(principal_id,plugin_id));
CREATE TABLE IF NOT EXISTS hub_channel_deliveries(plugin_id TEXT NOT NULL,principal_id TEXT NOT NULL,signal_id TEXT NOT NULL,seq INTEGER NOT NULL,status TEXT NOT NULL,external_msg_id TEXT,from_listener_id TEXT,delivered_ms INTEGER NOT NULL,PRIMARY KEY(plugin_id,principal_id,signal_id));
CREATE TABLE IF NOT EXISTS hub_pairing_codes(code TEXT PRIMARY KEY,plugin_id TEXT NOT NULL,principal_id TEXT NOT NULL,owner_user TEXT NOT NULL,instruction TEXT,created_ms INTEGER NOT NULL,expires_ms INTEGER NOT NULL,consumed_ms INTEGER);
CREATE TABLE IF NOT EXISTS hub_audit_log(id INTEGER PRIMARY KEY AUTOINCREMENT,ts_ms INTEGER NOT NULL,principal_id TEXT NOT NULL,action TEXT NOT NULL,target_id TEXT NOT NULL,detail_json TEXT);
CREATE TABLE IF NOT EXISTS hub_channel_delivery_msgs(plugin_id TEXT NOT NULL,principal_id TEXT NOT NULL,external_msg_id TEXT NOT NULL,signal_id TEXT NOT NULL,from_listener_id TEXT NOT NULL,PRIMARY KEY(plugin_id,principal_id,external_msg_id));
CREATE TABLE IF NOT EXISTS hub_processed_replies(plugin_id TEXT NOT NULL,update_id TEXT NOT NULL,processed_at_ms INTEGER NOT NULL,PRIMARY KEY(plugin_id,update_id));
CREATE TABLE IF NOT EXISTS hub_consumers(name TEXT PRIMARY KEY,stream TEXT NOT NULL,filter_subject TEXT,start_seq INTEGER NOT NULL,cursor_seq INTEGER NOT NULL,principal_id TEXT);
CREATE TABLE IF NOT EXISTS hub_consumer_acks(name TEXT NOT NULL,seq INTEGER NOT NULL,PRIMARY KEY(name,seq));
";

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
fn n(v: &Value, k: &str) -> i64 {
    v[k].as_i64().unwrap_or(0)
}
fn valid_seq(seq: &Value) -> bool {
    seq.as_i64().is_some_and(|seq| seq > 0)
}
fn authoritative_lease(registration: &mut Value, now: u64) -> &'static str {
    if let Some(fields) = registration.as_object_mut() {
        fields.remove("server_now_ms");
    }
    evaluate_node_lease(registration, now)
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
    Ok(format!("{prefix}{}", crate::support::try_random_hex(16)?))
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
    Ok(kv_entry(
        s(row, "key"),
        n(row, "revision"),
        n(row, "created_ms"),
        &stored_bytes(row, "value")?,
    ))
}
fn kv_entry(key: &str, revision: i64, now: i64, raw: &[u8]) -> Value {
    json!({"key":key,"revision":revision,"created_ms":now,"value_b64":B64.encode(raw),"operation":"PUT"})
}
fn write_kv(
    c: &Connection,
    key: &str,
    now: i64,
    raw: &[u8],
    owner: &Value,
    scope: &Value,
) -> Result<Value, String> {
    let revision = next_revision(c)?;
    let mut entry = kv_entry(key, revision, now, raw);
    if PAGE_OVERHEAD + entry.to_string().len() + 1 + entry["key"].to_string().len()
        > MAX_SNAPSHOT_BYTES
    {
        return Err("INVALID_INPUT: Registration exceeds snapshot page budget".into());
    }
    c.execute(&format!("INSERT INTO hub_kv(key,revision,created_ms,value,principal_id,scope) VALUES(?,?,?,{},?,?) ON CONFLICT(key) DO UPDATE SET revision=excluded.revision,created_ms=excluded.created_ms,value=excluded.value,principal_id=excluded.principal_id,scope=excluded.scope",blob_literal(raw)),&[json!(key),json!(revision),json!(now),owner.clone(),json!(scope.as_str())])?;
    entry["type"] = json!("kv");
    entry["principal_id"] = owner.clone();
    entry["scope"] = scope.clone();
    Ok(entry)
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
pub(super) fn configured_auth_mode(config: &Value) -> Option<&str> {
    if config["url"] == "local://" {
        return Some("none");
    }
    config["authMode"]
        .as_str()
        .or_else(|| config["auth_mode"].as_str())
}

pub(super) fn default_registration_scope(auth_mode: &str) -> &'static str {
    if auth_mode == "oidc" {
        "self"
    } else {
        "workspace"
    }
}

fn oidc(p: Option<&Value>) -> bool {
    p.is_some_and(|p| {
        p["oidc"] == true
            || s(p, "principal_id").starts_with("oidc:")
            // Treat even malformed URL-shaped stored identities as restricted callers.
            || s(p, "principal_id").starts_with("https://") || s(p,"principal_id").starts_with("http://")
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
        .any(|u| !u.is_empty() && u == s(p, "sub"))
    {
        Ok(())
    } else {
        Err("FORBIDDEN: Hub administrator required".into())
    }
}
fn hub_patch_guard(p: &Value, caller: &Value, reset_hub: bool) -> Result<bool, String> {
    let hub_patch = reset_hub
        || p["clear_hub_config"] == true
        || p.get("hub_config")
            .is_some_and(|v| v.as_object().is_none_or(|m| !m.is_empty()));
    if hub_patch || p.get("inbound_mode").is_some() {
        admin(Some(caller))?;
    }
    Ok(hub_patch)
}

const MAX_SUBSCRIBER_BYTES: usize = 2 * MAX_RPC_BYTES;

#[derive(Debug)]
pub struct EventFrame {
    bytes: Arc<str>,
    queued_bytes: Arc<AtomicUsize>,
    #[cfg(test)]
    decoded: std::sync::OnceLock<Value>,
}
impl std::fmt::Display for EventFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.bytes)
    }
}
impl Drop for EventFrame {
    fn drop(&mut self) {
        self.queued_bytes
            .fetch_sub(self.bytes.len(), Ordering::Relaxed);
    }
}
#[cfg(test)]
impl std::ops::Deref for EventFrame {
    type Target = Value;
    fn deref(&self) -> &Value {
        self.decoded
            .get_or_init(|| serde_json::from_str(&self.bytes).unwrap())
    }
}
#[cfg(test)]
impl PartialEq<Value> for EventFrame {
    fn eq(&self, other: &Value) -> bool {
        &**self == other
    }
}
struct Subscriber {
    principal: Option<Value>,
    tx: mpsc::SyncSender<EventFrame>,
    queued_bytes: Arc<AtomicUsize>,
    wake: Arc<crate::support::WakePipe>,
    identity: Arc<()>,
}
impl Subscriber {
    fn disconnect(self) {
        // A receiver awakened for the last time must observe disconnection after draining.
        drop(self.tx);
        self.wake.notify();
    }
}
pub struct Subscription {
    pub wake: Arc<crate::support::WakePipe>,
    receiver: mpsc::Receiver<EventFrame>,
    owner: std::sync::Weak<Inner>,
    identity: Arc<()>,
}
impl std::ops::Deref for Subscription {
    type Target = mpsc::Receiver<EventFrame>;
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
    channels_active: AtomicBool,
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
        Self::clear_tables(&self.inner.db)
    }

    pub(super) fn reset_offline(dir: &Path) -> Result<(), String> {
        if dir
            .join("hub.sqlite")
            .try_exists()
            .map_err(|e| e.to_string())?
        {
            Self::clear_tables(&Self::open_database(dir)?)?;
        }
        Ok(())
    }

    fn clear_tables(db: &Database) -> Result<(), String> {
        db.transaction(|db| {
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
    fn open_database(data_dir: &Path) -> Result<Database, String> {
        std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
        std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let path = data_dir.join("hub.sqlite");
        let db = Database::open(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        db.transaction(|c|{
            for stmt in SCHEMA.split(';').map(str::trim).filter(|s|!s.is_empty()){c.execute(stmt,&[])?;}
            for (table, column, ty) in [
                ("hub_signals", "signal_id", "TEXT"),
                ("hub_signals", "claimed_by_channel", "TEXT"),
                ("hub_signals", "claim_until_ms", "INTEGER"),
                ("hub_signals", "acked_at_ms", "INTEGER"),
                ("hub_pairing_codes", "initial_human_scope", "TEXT NOT NULL DEFAULT 'self'"),
                ("hub_pairing_codes", "instruction", "TEXT"),
                ("hub_pairing_codes", "pairing_command", "TEXT"),
                ("hub_pairing_codes", "pairing_url", "TEXT"),
                ("hub_pairing_codes", "bot_username", "TEXT"),
                ("hub_pairing_codes", "redemption_json", "TEXT"),
                ("hub_human_channels", "delivery_verified_ms", "INTEGER"),
                ("hub_channel_deliveries", "reservation_id", "TEXT"),
                ("hub_channel_deliveries", "attempts", "INTEGER NOT NULL DEFAULT 0"),
                ("hub_channel_deliveries", "next_retry_ms", "INTEGER NOT NULL DEFAULT 0"),
                ("hub_channel_deliveries", "last_error", "TEXT"),
            ] {
                let columns = c.query(&format!("PRAGMA table_info({table})"), &[])?;
                if !columns.iter().any(|v| s(v, "name") == column) {
                    c.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"), &[])?;
                }
            }
            for sql in ["CREATE INDEX IF NOT EXISTS idx_hub_signals_subject_seq ON hub_signals(subject,seq)","CREATE INDEX IF NOT EXISTS idx_hub_signals_signal_id ON hub_signals(signal_id)","CREATE INDEX IF NOT EXISTS idx_hub_channel_deliveries_recent ON hub_channel_deliveries(plugin_id,principal_id,delivered_ms DESC)"]{c.execute(sql,&[])?;}
            c.execute("CREATE INDEX IF NOT EXISTS idx_hub_kv_principal ON hub_kv(principal_id)", &[])?;
            c.execute("INSERT OR IGNORE INTO hub_meta(key,int_val) VALUES('kv_seq',0)",&[])?;
            c.execute("DROP INDEX IF EXISTS idx_hub_channel_deliveries_ext_msg", &[])?;
            // Existing single-message deliveries retain reply and acknowledgment mappings.
            c.execute("INSERT OR IGNORE INTO hub_channel_delivery_msgs(plugin_id,principal_id,external_msg_id,signal_id,from_listener_id) SELECT plugin_id,principal_id,external_msg_id,signal_id,COALESCE(from_listener_id,'') FROM hub_channel_deliveries WHERE external_msg_id IS NOT NULL",&[])?;
            // Materialize the sender once, rather than fetching payload bytes for every receipt.
            let signal_columns = c.query("PRAGMA table_info(hub_signals)", &[])?;
            if !signal_columns.iter().any(|r| r["name"] == "sender_principal_id") {
                c.execute("ALTER TABLE hub_signals ADD COLUMN sender_principal_id TEXT", &[])?;
                let mut after = 0;
                while let Some(row) = one(c, "SELECT seq,CAST(data AS TEXT) AS data FROM hub_signals WHERE seq>? ORDER BY seq LIMIT 1", &[json!(after)])? {
                    after = n(&row, "seq");
                    let data = parse(&row,"data");
                    if let Some(sender) = data["from"]["principal_id"].as_str() {
                        c.execute("UPDATE hub_signals SET sender_principal_id=? WHERE seq=?", &[json!(sender),json!(after)])?;
                    }
                }
                // Older versions stored arbitrary payloads and registration credentials here.
                c.execute("UPDATE hub_audit_log SET detail_json=NULL WHERE action IN ('send','register','unlisten','ack')", &[])?;
                for row in c.query("SELECT plugin_id,service_state_json,updated_ms FROM hub_plugin_services", &[])? {
                    let mut state = parse(&row, "service_state_json");
                    if state["cursor"].as_u64().is_some() && state["cursor_last_update_ms"].is_null() {
                        state["cursor_last_update_ms"] = row["updated_ms"].clone();
                        write_service_state(c, s(&row,"plugin_id"), &state)?;
                    }
                }
            }
            // Backfill the additive ID column without changing legacy payload bytes.
            for row in c.query("SELECT seq,CAST(data AS TEXT) AS data FROM hub_signals WHERE signal_id IS NULL",&[])?{let data=parse(&row,"data");if let Some(id)=data["signal_id"].as_str(){c.execute("UPDATE hub_signals SET signal_id=? WHERE seq=?",&[json!(id),row["seq"].clone()])?;}}
            Ok(())
        })?;
        Ok(db)
    }

    pub fn open_with_auth_mode(data_dir: &Path, auth_mode: &str) -> Result<Self, String> {
        Self::open_with_channels(data_dir, auth_mode, true)
    }

    pub(super) fn open_with_channels(
        data_dir: &Path,
        auth_mode: &str,
        channels_active: bool,
    ) -> Result<Self, String> {
        if !matches!(auth_mode, "none" | "token" | "oidc") {
            return Err("INVALID_INPUT: Unknown Hub auth mode".into());
        }
        let engine = Self {
            inner: Arc::new(Inner {
                db: Self::open_database(data_dir)?,
                user_plugins_dir: data_dir.join("plugins"),
                closed: AtomicBool::new(false),
                channels_active: AtomicBool::new(channels_active),
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
                default_human_scope: default_registration_scope(auth_mode),
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
            let delay = e.worker_delay().unwrap_or(CHANNEL_RETRY_DELAY);
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
                if failed { CHANNEL_RETRY_DELAY } else { delay },
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
            queued_bytes: Arc::new(AtomicUsize::new(0)),
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
        self.shutdown_before(|| {});
    }
    pub(in crate::core) fn shutdown_before(&self, join_listener: impl FnOnce()) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.wake_worker();
        if let Ok(mut subs) = self.inner.subscribers.lock() {
            for sub in subs.drain(..) {
                sub.disconnect();
            }
        }
        let processes = self
            .inner
            .processes
            .lock()
            .map(|mut p| p.drain().map(|(_, v)| v).collect::<Vec<_>>())
            .unwrap_or_default();
        for p in &processes {
            p.cancel_requests();
        }
        join_listener();
        for p in processes {
            p.shutdown();
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

    pub fn set_channels_active(&self, active: bool) {
        if self.inner.channels_active.swap(active, Ordering::SeqCst) != active {
            self.wake_worker();
        }
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
            let mut bytes = None;
            for sub in subs.extract_if(.., |sub| {
                let permitted = self
                    .inner
                    .db
                    .transaction(|c| event_allowed(c, &frame, sub.principal.as_ref()))
                    .unwrap_or(false);
                if !permitted {
                    return false;
                }
                let bytes = bytes.get_or_insert_with(|| Arc::<str>::from(frame.to_string()));
                if sub.queued_bytes.load(Ordering::Relaxed) + bytes.len() > MAX_SUBSCRIBER_BYTES {
                    return true;
                }
                // Emitters serialize under the registry lock; receivers only subtract.
                sub.queued_bytes.fetch_add(bytes.len(), Ordering::Relaxed);
                if sub
                    .tx
                    .try_send(EventFrame {
                        bytes: bytes.clone(),
                        queued_bytes: sub.queued_bytes.clone(),
                        #[cfg(test)]
                        decoded: std::sync::OnceLock::new(),
                    })
                    .is_err()
                {
                    return true;
                }
                sub.wake.notify();
                false
            }) {
                sub.disconnect();
            }
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
        if matches!(
            op,
            "consumer_add"
                | "consumer_next"
                | "consumer_backlog"
                | "consumer_ack"
                | "consumer_delete"
        ) && p.get("stream").is_some_and(|stream| stream != SIGNALS)
        {
            return Err("INVALID_INPUT: Unsupported consumer stream".into());
        }
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
                let (name, patch) = if op == "stream_add" {
                    (required(&p["config"], "name")?, &p["config"])
                } else {
                    (required(p, "stream")?, &p["patch"])
                };
                if name.len() > MAX_STREAM_IDENTIFIER_BYTES {
                    return Err("INVALID_INPUT: stream name exceeds 256 bytes".into());
                }
                if let Some(subjects) = patch.get("subjects") {
                    let subjects = subjects
                        .as_array()
                        .ok_or("INVALID_INPUT: subjects must be an array")?;
                    if !subjects.iter().all(|s| {
                        s.as_str()
                            .is_some_and(|s| s.len() <= MAX_STREAM_IDENTIFIER_BYTES)
                    }) {
                        return Err(
                            "INVALID_INPUT: subjects must be strings of at most 256 bytes".into(),
                        );
                    }
                }
                if !patch.is_object() {
                    return Err("INVALID_INPUT: Stream config must be an object".into());
                }
                for key in ["max_age", "duplicate_window"] {
                    if patch
                        .get(key)
                        .is_some_and(|v| v.as_i64().is_none_or(|n| n < 0))
                    {
                        return Err(format!(
                            "INVALID_INPUT: {key} must be a nonnegative integer"
                        ));
                    }
                }
                if patch.to_string().len() > MAX_STREAM_CONFIG_BYTES {
                    return Err("INVALID_INPUT: stream config exceeds 64 KiB".into());
                }
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
                    merge(&mut cfg, patch);
                    cfg["name"] = json!(name);
                    let config_json = cfg.to_string();
                    if config_json.len() > MAX_STREAM_CONFIG_BYTES {
                        return Err("INVALID_INPUT: stream config exceeds 64 KiB".into());
                    }
                    c.execute("INSERT INTO hub_meta(key,json_val) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_val=excluded.json_val",&[json!(format!("stream:{name}")),json!(config_json)])?;
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
                        .filter(|s| !s.is_empty() && s.len() <= 128 && s.iter().all(valid_seq))
                        .ok_or("INVALID_INPUT: seqs must contain 1..128 sequence numbers")?;
                    let placeholders = vec!["?"; seqs.len()].join(",");
                    let mut messages = Vec::new();
                    let mut bytes = PAGE_OVERHEAD;
                    c.query_while(
                        &format!("SELECT seq,subject,signal_id,acked_at_ms,sender_principal_id FROM hub_signals WHERE seq IN ({placeholders}) ORDER BY seq"),
                        seqs, |row| {
                        if !signal_readable(c, &row, caller)? {
                            return Err("FORBIDDEN: signal is private".into());
                        }
                        let receipt = json!({"seq":row["seq"],"signal_id":row["signal_id"],"acked_at_ms":row["acked_at_ms"]});
                        bytes += receipt.to_string().len() + 1;
                        if bytes > MAX_RPC_BYTES {
                            return Err("INVALID_INPUT: Receipt batch exceeds response budget; request fewer sequences".into());
                        }
                        messages.push(receipt);
                        Ok(true)
                    })?;
                    return Ok((json!({"messages":messages}), None));
                }
                if !valid_seq(&p["seq"]) {
                    return Err("INVALID_INPUT: seq must be a positive integer".into());
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
                if PAGE_OVERHEAD + result.to_string().len() > MAX_RPC_BYTES {
                    return Err("INVALID_INPUT: Signal exceeds response frame budget".into());
                }
                Ok(result)
            }
            "kv_get" => {
                let Some(row) = kv_row(c, required(p, "key")?)? else {
                    return Ok((Value::Null, None));
                };
                if !visible(&row, caller) {
                    return Ok((json!({"forbidden_scope":true}), None));
                }
                let row = reconcile_human_row(c, row, events)?;
                let mut wire = kv_wire(&row)?;
                wire["server_now_ms"] = json!(now_ms());
                Ok(wire)
            }
            "kv_keys" | "kv_snapshot" => {
                let limit = p["limit"]
                    .as_u64()
                    .unwrap_or(if op == "kv_keys" { 1024 } else { 256 })
                    .clamp(1, 1024) as usize;
                let params = [json!(s(p, "after_key"))];
                if op == "kv_keys" {
                    let mut keys = Vec::new();
                    let mut bytes = PAGE_OVERHEAD;
                    let mut cursor = None;
                    c.query_while(
                        "SELECT key,principal_id,scope FROM hub_kv WHERE key>? ORDER BY key",
                        &params,
                        |row| {
                            if visible(&row, caller) && matches(s(&row, "key"), s(p, "key")) {
                                let key = &row["key"];
                                let size = key.to_string().len();
                                if keys.len() == limit || bytes + size * 2 + 1 > MAX_RPC_BYTES {
                                    cursor = Some(
                                        keys.last()
                                            .cloned()
                                            .ok_or("INVALID_INPUT: Key exceeds response budget")?,
                                    );
                                    return Ok(false);
                                }
                                bytes += size + 1;
                                keys.push(key.clone());
                            }
                            Ok(true)
                        },
                    )?;
                    Ok(json!({"keys":keys,"next_cursor":cursor}))
                } else {
                    let sql = format!("{KV_SELECT} WHERE key>? ORDER BY key");
                    let mut rows = Vec::new();
                    let mut bytes = PAGE_OVERHEAD;
                    let mut more = false;
                    c.query_while(&sql, &params, |row| {
                        if !visible(&row, caller) || !matches(s(&row, "key"), s(p, "key")) {
                            return Ok(true);
                        }
                        let size = serde_json::to_vec(&kv_wire(&row)?)
                            .map_err(|e| e.to_string())?
                            .len()
                            + 1;
                        if rows.len() >= limit
                            || bytes + size + row["key"].to_string().len() > MAX_SNAPSHOT_BYTES
                        {
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
                    bytes = PAGE_OVERHEAD;
                    for row in rows {
                        let wire = kv_wire(&reconcile_human_row(c, row, events)?)?;
                        let size = serde_json::to_vec(&wire).map_err(|e| e.to_string())?.len() + 1;
                        if bytes + size + wire["key"].to_string().len() > MAX_SNAPSHOT_BYTES {
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
                if op != "kv_delete" && key.len() > 1024 {
                    return Err("INVALID_INPUT: Registration key exceeds 1024 bytes".into());
                }
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
                    if oidc(caller)
                        && data["principal_id"]
                            .as_str()
                            .is_some_and(|pid| !owns_principal(pid, caller))
                    {
                        return Err("PERMISSIONS_VIOLATION: registration principal mismatch".into());
                    }
                }
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
                let event = if op == "kv_delete" {
                    let revision = next_revision(c)?;
                    c.execute("DELETE FROM hub_kv WHERE key=?", &[json!(key)])?;
                    json!({"type":"kv","key":key,"revision":revision,"created_ms":now,"value_b64":"","operation":"DEL","principal_id":owner,"scope":existing.as_ref().map(|r| &r["scope"])})
                } else {
                    write_kv(c, key, now, &raw, &json!(owner), &data["scope"])?
                };
                audit(
                    c,
                    principal_id(caller),
                    if op == "kv_delete" || data["state"] == "UNLISTENED" {
                        "unlisten"
                    } else {
                        "register"
                    },
                    key,
                    &json!({"revision":event["revision"]}),
                )?;
                let revision = event["revision"].clone();
                events.push(event);
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
                for key in ["name", "durable_name", "filter_subject"] {
                    if s(cfg, key).len() > MAX_STREAM_IDENTIFIER_BYTES {
                        return Err(format!("INVALID_INPUT: {key} exceeds 256 bytes"));
                    }
                }
                if cfg.to_string().len() > MAX_STREAM_CONFIG_BYTES {
                    return Err("INVALID_INPUT: consumer config exceeds 64 KiB".into());
                }
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
                    "SELECT principal_id,stream FROM hub_consumers WHERE name=?",
                    &[json!(name)],
                )? {
                    consumer_owner(&row, caller)?;
                }
                let start = if cfg["deliver_policy"] == "by_start_sequence" {
                    n(cfg, "opt_start_seq").max(1)
                } else {
                    1
                };
                let stream = SIGNALS;
                c.execute("INSERT INTO hub_consumers(name,stream,filter_subject,start_seq,cursor_seq,principal_id) VALUES(?,?,?,?,?,?) ON CONFLICT(name) DO UPDATE SET stream=excluded.stream,filter_subject=excluded.filter_subject,start_seq=excluded.start_seq,cursor_seq=excluded.cursor_seq",&[json!(name),json!(stream),json!(filter),json!(start),json!(start),caller.map(|p|p["principal_id"].clone()).unwrap_or(Value::Null)])?;
                let row = consumer(c, &name, caller)?;
                let mut result = json!({"name":name,"stream_name":stream,"config":cfg,"backlog":[],"server_now_ms":now_ms()});
                let budget = MAX_RPC_BYTES - PAGE_OVERHEAD - result.to_string().len();
                let mut backlog = backlog_from(c, &row, caller, n(&row, "start_seq"), budget)?;
                result["backlog"] = backlog["backlog"].take();
                result["server_now_ms"] = backlog["server_now_ms"].take();
                Ok(result)
            }
            "consumer_backlog" => {
                let row = consumer(c, required(p, "name")?, caller)?;
                backlog_from(c, &row, caller, n(&row, "start_seq"), MAX_RPC_BYTES)
            }
            "consumer_next" => {
                let name = required(p, "name")?;
                let row = consumer(c, name, caller)?;
                let rows = backlog_from(
                    c,
                    &row,
                    caller,
                    n(&row, "start_seq").max(n(&row, "cursor_seq")),
                    MAX_RPC_BYTES,
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
                    if !valid_seq(&p["seq"]) {
                        return Err("INVALID_INPUT: seq must be a positive integer".into());
                    }
                    let sig = one(
                        c,
                        "SELECT subject FROM hub_signals WHERE seq=?",
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
                    Ok(json!({"acked":true}))
                }
            }
            "send_reply_from_human" => {
                let sender = caller.ok_or("FORBIDDEN: Human identity required")?;
                // The daemon already validated the Hub and removed its prefix. The
                // remaining bytes are opaque, even when the source no longer exists.
                if p["in_reply_to_is_raw"] == true {
                    return publish_human_reply(c, sender, "human", p, events);
                }
                let mut request = p.clone();
                let key = if p["in_reply_to"].is_null() {
                    "signal_id"
                } else {
                    "in_reply_to"
                };
                let id = s(p, key);
                if let Some((_, raw)) = id
                    .split_once(':')
                    .filter(|(hub, _)| super::plugins::validate_hub_name(hub))
                {
                    // Opaque stored IDs take precedence over hub-qualified references.
                    if one(
                        c,
                        "SELECT 1 FROM hub_signals WHERE signal_id=? LIMIT 1",
                        &[json!(id)],
                    )?
                    .is_none()
                    {
                        request[key] = json!(raw);
                    }
                }
                return publish_human_reply(c, sender, "human", &request, events);
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
                let old = cfg.clone();
                for field in &plugin.fields {
                    if s(field, "scope") != "hub" {
                        continue;
                    }
                    let key = s(field, "key");
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
                if old != cfg {
                    retire_service_warnings(c, &plugin.id)?;
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
        if !self.inner.channels_active.load(Ordering::SeqCst) {
            return Err("HUB_DISABLED: Local Hub is disabled".into());
        }
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
        if !self.inner.channels_active.load(Ordering::SeqCst) {
            return Err("HUB_DISABLED: Local Hub is disabled".into());
        }
        // Every replacement owns recovery, including a replacement requested by Send Test.
        self.inner.db.transaction(|c| {
            fence_in_flight(
                c,
                &plugin.id,
                None,
                "Channel process exited before confirming delivery",
            )
        })?;
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
        self.inner
            .db
            .diagnostic_transaction(|c| Self::persist_poll_error(c, id, error))
    }
    fn persist_poll_error(c: &Connection, id: &str, error: Option<&str>) -> Result<(), String> {
        let (cfg, mut state) = service(c, id)?;
        let previous = state.clone();
        if let Some(error) = error {
            state["poll_error"] = json!(error);
        } else if let Some(state) = state.as_object_mut() {
            state.remove("poll_error");
            state.remove("process_retry_at_ms");
        }
        if state != previous {
            save_service(c, id, &cfg)?;
            write_service_state(c, id, &state)?;
        }
        Ok(())
    }
    fn configure_process(&self, id: &str, process: &ChannelProcess) -> Result<(), String> {
        let frame=self.inner.db.transaction(|c|{let(cfg,state)=service(c,id)?;save_service(c,id,&cfg)?;let bindings=c.query("SELECT principal_id,owner_user,user_config_json FROM hub_human_channels WHERE plugin_id=? AND enabled=1",&[json!(id)])?.iter().map(|b|json!({"principal_id":b["principal_id"],"user":b["owner_user"],"config":parse(b,"user_config_json")})).collect::<Vec<_>>();Ok(json!({"op":"configure","hub_config":cfg,"bindings":bindings,"cursor":state["cursor"],"cursor_last_update_ms":state["cursor_last_update_ms"],"inbound_mode":inbound_mode(&state)}))})?;
        process.send(&frame)?;
        self.inner.db.diagnostic_transaction(|c| {
            // Serialize with the reader's terminal health update. An exit after this
            // check is recorded after this transaction releases the database.
            if !process.alive() {
                return Err("PLUGIN_EXEC_FAILED: Plugin subprocess exited".into());
            }
            if service(c, id)?.1.get("process_retry_at_ms").is_some() {
                Self::persist_poll_error(c, id, None)?;
            }
            Ok(())
        })
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
        let local = principal(
            &env_value("CITO_USER")
                .or_else(|| env_value("USER"))
                .unwrap_or_else(|| "local".into()),
        );
        let caller = caller.unwrap_or(&local);
        let pid = s(caller, "principal_id");
        if op == "human_channels_list" {
            return self.channel_list(caller);
        }
        let id = required(p, "plugin_id")?;
        let plugin = self.plugin(id)?;
        let reset_hub = p["reset_hub"] == true;
        let hub_patch = hub_patch_guard(p, caller, reset_hub)?;
        // Serialize configuration and supervisor work; never hold this lock in child callbacks.
        let lifecycle = self
            .inner
            .lifecycle
            .lock()
            .map_err(|_| "Channel lifecycle lock poisoned")?;
        let clear_hub_secret = p["clear_hub_config"] == true || reset_hub;
        let clear_user_secret = clear_hub_secret || p["reset"] == true;
        for scope in ["hub", "user"] {
            if let Some(patch) = p.get(format!("{scope}_config")) {
                validate_config(&plugin, patch, scope)?;
            }
        }
        let reset_user = p["reset_user"] == true || (reset_hub && p["reset_user"] != false);
        let reconcile_bindings = |c: &Connection,
                                  cfg: &Value,
                                  changed: bool|
         -> Result<(), String> {
            if changed {
                c.execute("UPDATE hub_channel_deliveries SET status='failed',attempts=0,next_retry_ms=0,last_error=NULL WHERE plugin_id=? AND status IN ('failed','failed_permanent')", &[json!(id)])?;
            } else if p.get("user_config").is_some() {
                // A personal Save can reopen a terminal error but must not bypass an active cooldown.
                c.execute("UPDATE hub_channel_deliveries SET status='failed',attempts=0,next_retry_ms=0,last_error=NULL WHERE plugin_id=? AND principal_id IN (?,?) AND status='failed_permanent'", &[json!(id),json!(pid),json!(alternate_oidc_principal(pid))])?;
            }
            for b in c.query(
                "SELECT * FROM hub_human_channels WHERE plugin_id=?",
                &[json!(id)],
            )? {
                let active = n(&b, "enabled") == 1
                    && ready(&plugin, cfg, "hub")
                    && ready(&plugin, &parse(&b, "user_config_json"), "user");
                attach(c, &b, active, self.inner.default_human_scope, Some(&plugin))?;
            }
            Ok(())
        };
        if op == "human_channels_configure" {
            let mode = p
                .get("inbound_mode")
                .map(|mode| match mode.as_str() {
                    Some("primary") => Ok("primary"),
                    Some("outbound_only") if plugin.channel.supports_outbound_only => {
                        Ok("outbound_only")
                    }
                    _ => Err("INVALID_INPUT: Unsupported inbound_mode"),
                })
                .transpose()?;
            let display_label = p
                .get("display_label")
                .map(|label| {
                    if label.is_null() {
                        return Ok(Value::Null);
                    }
                    channel_display_label(label)
                        .map(|label| json!(label))
                        .ok_or(
                        "INVALID_INPUT: display_label must contain 1 to 128 bytes after trimming",
                    )
                })
                .transpose()?;
            let edit_user = reset_user
                || ["user_config", "enabled", "display_label"]
                    .iter()
                    .any(|key| p.get(key).is_some());
            let destination = |old: Option<&Value>| -> Result<_, String> {
                let identity = plugin.channel.pairing["privateIdentityField"].as_str();
                let mut config = old
                    .map(|b| parse(b, "user_config_json"))
                    .unwrap_or(json!({}));
                let mut label = old
                    .map(|b| b["display_label"].clone())
                    .unwrap_or(Value::Null);
                let mut identity_changed = reset_user;
                if reset_user {
                    config = json!({});
                    label = Value::Null;
                } else if let Some(patch) = p.get("user_config") {
                    let prev = config.clone();
                    merge_channel_config(&plugin, "user", &mut config, patch, clear_user_secret);
                    if let Some(key) = identity {
                        if let Some(value) = config[key].as_str() {
                            config[key] = json!(value.trim());
                        }
                    }
                    identity_changed =
                        identity
                            .map(|key| prev[key] != config[key])
                            .unwrap_or_else(|| {
                                plugin.fields.iter().any(|f| {
                                    s(f, "scope") == "user"
                                        && prev[s(f, "key")] != config[s(f, "key")]
                                })
                            });
                    if identity_changed {
                        let cleared = identity.is_some_and(|key| s(&config, key).is_empty());
                        if let Some(m) = config.as_object_mut() {
                            for key in ["_external_chat_id", "_external_user_id"] {
                                if patch.get(key).is_none() || cleared {
                                    m.remove(key);
                                }
                            }
                        }
                        label = Value::Null;
                    }
                }
                if let Some(identity) = identity {
                    if identity_changed && !s(&config, identity).is_empty() {
                        let chat = s(&config, identity)
                            .parse::<i64>()
                            .ok()
                            .filter(|n| *n > 0)
                            .map(|n| n.to_string());
                        config["_external_chat_id"] = json!(chat);
                        config["_external_user_id"] = json!(chat);
                        label = json!(chat
                            .as_ref()
                            .map(|chat| format!("Chat {chat}"))
                            .unwrap_or_else(|| "Outbound only".into()));
                    }
                }
                let serialized = channel_config_json(&config)?;
                Ok((config, serialized, display_label.clone().unwrap_or(label)))
            };
            let expected = p.get("expected_service_identity");
            let check_identity = |expected: &Value, cfg: &Value| -> Result<(), String> {
                let expected = expected
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or("CONFLICT: expected_service_identity must be a non-empty string")?;
                if public_service_identity(&plugin, cfg).as_str() != Some(expected) {
                    return Err("CONFLICT: Channel service identity changed before Link completed; refresh and try again.".into());
                }
                Ok(())
            };
            let (cfg, changed, mode_changed) = self.inner.db.transaction(|c| {
                let (old, state) = service(c, id)?;
                let mut cfg = if reset_hub { json!({}) } else { old.clone() };
                if !reset_hub {
                    if let Some(patch) = p.get("hub_config") {
                        merge_channel_config(&plugin, "hub", &mut cfg, patch, clear_hub_secret);
                    }
                }
                channel_config_json(&cfg)?;
                let changed = reset_hub || old != cfg;
                if changed && edit_user {
                    destination(channel_binding(c, id, pid)?.as_ref())?;
                }
                let mode_changed = mode.is_some_and(|mode| mode != inbound_mode(&state));
                if changed || mode_changed {
                    // A stale save must not stop the current account's reader.
                    if let Some(expected) = expected {
                        check_identity(expected, &old)?;
                    }
                }
                Ok((cfg, changed, mode_changed))
            })?;
            // Reap the previous reader (and any outstanding poll) before switching modes or credentials.
            if changed || mode_changed {
                self.stop_process(id)?;
                // Reconcile the live service even if the subsequent transaction conflicts.
                self.wake_worker();
            }
            self.inner.db.transaction(|c| {
                if let Some(expected) = expected {
                    check_identity(expected, &service(c, id)?.0)?;
                }
                let hub_saved = changed || hub_patch;
                if hub_saved { retire_service_warnings(c, id)?; }
                // Persist the personal watermark before either replacement can produce a warning.
                let destination_edit = if edit_user {
                    let old = channel_binding(c, id, pid)?;
                    let binding_pid = old.as_ref().map_or(pid, |b| s(b,"principal_id"));
                    // Pairing callbacks may have updated the binding while its reader was reaped.
                    let (config, serialized, label) = destination(old.as_ref())?;
                    let updated_ms = if hub_saved && old.is_some() {
                        now_ms() // The upsert preserves the existing Hub Save watermark.
                    } else {
                        retire_channel_warnings(c, id, binding_pid, None)?
                    };
                    let enabled = !reset_user && p["enabled"].as_bool().unwrap_or_else(|| old.as_ref().is_none_or(|b| n(b,"enabled")==1));
                    let boundary = if enabled && old.as_ref().is_none_or(|b| n(b,"enabled")==0) { max_seq(c)? } else { old.as_ref().map(|b| n(b,"last_delivered_seq")).unwrap_or(0) };
                    c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,enabled,user_config_json,display_label,last_delivered_seq,updated_ms) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(principal_id,plugin_id) DO UPDATE SET owner_user=excluded.owner_user,enabled=excluded.enabled,user_config_json=excluded.user_config_json,display_label=excluded.display_label,last_delivered_seq=excluded.last_delivered_seq,updated_ms=MAX(updated_ms,excluded.updated_ms)",
                        &[json!(binding_pid),json!(id),caller["owner_user"].clone(),json!(if enabled{1}else{0}),json!(serialized),label,json!(boundary),json!(updated_ms)])?;
                    Some((binding_pid.to_owned(), old.as_ref().map(|b|parse(b,"user_config_json")), config))
                } else {
                    None
                };
                if hub_saved { save_channel_service(c, &plugin, &cfg)?; }
                if reset_hub {
                    c.execute("UPDATE hub_plugin_services SET service_state_json='{}' WHERE plugin_id=?", &[json!(id)])?;
                    c.execute("UPDATE hub_pairing_codes SET consumed_ms=? WHERE plugin_id=? AND consumed_ms IS NULL", &[json!(now_ms()), json!(id)])?;
                }
                if let Some(mode) = mode {
                    save_service(c, id, &cfg)?;
                    let (_, mut state) = service(c, id)?;
                    state["inbound_mode"] = json!(mode);
                    if mode == "outbound_only" {
                        if let Some(fields) = state.as_object_mut() {
                            fields.remove("poll_error");
                            fields.remove("process_retry_at_ms");
                        }
                        c.execute("UPDATE hub_pairing_codes SET consumed_ms=? WHERE plugin_id=? AND consumed_ms IS NULL", &[json!(now_ms()), json!(id)])?;
                    }
                    write_service_state(c, id, &state)?;
                }
                if reset_user || p["cancel_pair"] == true { cancel_pair(c, id, pid)?; }
                if let Some((binding_pid, old, config)) = destination_edit {
                    replace_destination(c, &plugin, &binding_pid, old.as_ref(), &config)?;
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
        if op == "human_channel_pair_start" && plugin.channel.pairing["supported"] != true {
            return Err("INVALID_INPUT: pairing is not supported".into());
        }
        let (cfg, user, changed, saved_user_ready) = self.inner.db.transaction(|c| {
            let (old, state) = service(c, id)?;
            if op == "human_channel_pair_start" && inbound_mode(&state) == "outbound_only" {
                return Err("INVALID_INPUT: Pairing requires Replies enabled (Primary mode); link an existing account or enter your destination manually".into());
            }
            let mut cfg = old.clone();
            if let Some(patch) = p.get("hub_config") {
                merge_channel_config(&plugin, "hub", &mut cfg, patch, clear_hub_secret);
            }
            let saved_user = channel_binding(c, id, pid)?.map(|b| parse(&b, "user_config_json"));
            let mut user = saved_user.clone().unwrap_or(json!({}));
            if let Some(patch) = p.get("user_config") {
                merge_channel_config(&plugin, "user", &mut user, patch, clear_user_secret);
            }
            channel_config_json(&cfg)?;
            channel_config_json(&user)?;
            let changed = cfg != old;
            let saved_user_ready =
                saved_user.as_ref() == Some(&user) && ready(&plugin, &user, "user");
            Ok((cfg, user, changed, saved_user_ready))
        })?;
        if op == "human_channels_test" {
            let process = self.ensure_process(&plugin)?;
            let result = process.request_after_send(
                json!({"op":"test","user":caller["owner_user"],"hub_config":cfg,"user_config":user}),
                || drop(lifecycle),
            );
            self.wake_worker();
            let mut result = result?;
            match result["ok"].as_bool() {
                Some(false) => return Ok(result),
                None => {
                    return Ok(
                        json!({"ok":false,"effect_unknown":true,"error":"Channel test returned no valid delivery status"}),
                    )
                }
                Some(true) => {}
            }
            // A test can preview unsaved credentials; do not label a different saved service.
            if !changed {
                match self.inner.db.diagnostic_transaction(|c| {
                    if service(c, id)?.0 != cfg || !process.alive() {
                        return Ok(false);
                    }
                    let label_changed = persist_service_label(c, id, &result)?;
                    // A successful draft test must not verify a saved destination. Recheck the
                    // binding as config and pairing can change while the child sends the test.
                    if !saved_user_ready {
                        return Ok(label_changed);
                    }
                    let Some(binding) = channel_binding(c, id, pid)?
                        .filter(|b| parse(b, "user_config_json") == user)
                    else {
                        return Ok(label_changed);
                    };
                    let binding_pid = s(&binding, "principal_id");
                    let verified_ms = retire_channel_warnings(c, id, binding_pid, None)?;
                    verify_delivery(c, id, binding_pid, verified_ms)?;
                    Ok(true)
                }) {
                    Ok(true) => self.emit(json!({"type":"kv"})),
                    Ok(false) => {}
                    Err(error) => result["maintenance_warning"] = json!(error),
                }
            }
            return Ok(result);
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
        // Reap the old credential reader before replacing service identity; retain
        // the live process on re-pair so its destination cooldown is not lost.
        if changed {
            self.stop_process(id)?;
        }
        let result = prepared?;
        if result["ok"] != true {
            return Err(format!("PLUGIN_EXEC_FAILED: {}", s(&result, "error")));
        }
        let instruction = required(&result, "instruction")?;
        self.inner.db.transaction(|c| {
            if changed || hub_patch {
                retire_service_warnings(c, id)?;
                save_channel_service(c, &plugin, &cfg)?;
                reconcile_bindings(c, &cfg, changed)?;
            }
            persist_service_label(c, id, &result)?;
            cancel_pair(c,id,pid)?;
            c.execute("INSERT INTO hub_pairing_codes(code,plugin_id,principal_id,owner_user,created_ms,expires_ms,instruction,pairing_command,pairing_url,bot_username,initial_human_scope) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                &[json!(code),json!(id),caller["principal_id"].clone(),caller["owner_user"].clone(),json!(now_ms()),json!(expires),json!(instruction),result["pairing_command"].clone(),result["pairing_url"].clone(),result["bot_username"].clone(),json!(self.inner.default_human_scope)])?;
            Ok(())
        })?;
        self.emit(json!({"type":"kv"}));
        let mut result = json!({"ok":true,"committed":true,"code":code,"instruction":instruction,"pairing_command":result["pairing_command"],"pairing_url":result["pairing_url"],"bot_username":result["bot_username"],"service_label":result_service_label(&result),"expires_at_ms":expires});
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
        Ok(())
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
            let inbox_paused = find_human(c,caller)?.1.is_some_and(|reg| reg["human_paused"] == true);
            let mut channels=Vec::new();
            let mut bytes = PAGE_OVERHEAD;
            let pid = s(caller,"principal_id");
            for plugin in plugins {
                let (cfg,health) = service(c,&plugin.id)?;
                let binding=channel_binding(c,&plugin.id,pid)?.unwrap_or(Value::Null);
                let user=parse(&binding,"user_config_json");
                let has_hub_service=plugin.fields.iter().any(|f|s(f,"scope")=="hub");
                let can_configure_hub=has_hub_service && is_admin;
                let hub_configured_any=plugin.fields.iter().any(|f|s(f,"scope")=="hub" && configured(&cfg[s(f,"key")]));
                let can_edit_personal=plugin.fields.iter().any(|f|s(f,"scope")=="user");
                let pairing_supported=plugin.channel.pairing["supported"]==true;
                let mode=inbound_mode(&health);
                let pairing_enabled=pairing_supported && mode != "outbound_only";
                let hub_ready=ready(&plugin,&cfg,"hub");
                let user_ready=ready(&plugin,&user,"user");
                let enabled=!binding.is_null() && n(&binding,"enabled")==1 && user_ready;
                let pair=one(c,"SELECT code,instruction,pairing_command,pairing_url,bot_username FROM hub_pairing_codes WHERE principal_id IN (?,?) AND plugin_id=? AND consumed_ms IS NULL AND expires_ms>? ORDER BY created_ms DESC, rowid DESC LIMIT 1", &[json!(pid),json!(alternate_oidc_principal(pid)),json!(plugin.id),json!(now_ms())])?.unwrap_or(Value::Null);
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
                    "hub_ready" if pairing_enabled=>"Service ready · Pair your account with one click".into(),
                    "hub_ready"=>"Service ready · Enter your personal destination fields below".into(),
                    _ if !can_configure_hub=>"Shared service credentials must be configured by the Hub administrator".into(),
                    _=>"Configure service credentials and pair your account".into(),
                };
                let mut warning = false;
                let mut error_origin: Option<&str> = None;
                let warning_since = n(&binding, "updated_ms").max(n(&binding, "delivery_verified_ms"));
                // Legacy successes can still mask older warnings after verification is invalidated.
                if let Some(failed) = one(c,
                    "WITH latest_warning AS (
                         SELECT rowid,status,last_error,delivered_ms FROM hub_channel_deliveries
                         WHERE plugin_id=?1 AND principal_id IN (?2,?3)
                           AND delivered_ms >= ?4
                           AND status IN ('failed','failed_permanent','effect_unknown','in_flight')
                           AND COALESCE(last_error, '') <> ''
                         ORDER BY delivered_ms DESC, rowid DESC LIMIT 1
                     )
                     SELECT status,last_error FROM latest_warning w WHERE NOT EXISTS (
                         SELECT 1 FROM hub_channel_deliveries d WHERE d.plugin_id=?1 AND d.principal_id IN (?2,?3)
                           AND d.status='delivered'
                           AND (d.delivered_ms>w.delivered_ms OR (d.delivered_ms=w.delivered_ms AND d.rowid>w.rowid))
                     )",
                    &[json!(plugin.id), json!(pid), json!(alternate_oidc_principal(pid)), json!(warning_since)],
                )? {
                    warning = true;
                    error_origin = Some("user");
                    detail = if failed["status"] == "effect_unknown" {
                        format!("Delivery outcome unknown; automatic replay stopped to avoid duplicates. {}", s(&failed, "last_error"))
                    } else {
                        s(&failed, "last_error").into()
                    };
                }
                let mut delivery_unverified = false;
                if let Some(since) = health["delivery_unverified_since_ms"].as_i64().filter(|_| enabled && hub_ready) {
                    if n(&binding,"delivery_verified_ms") < since {
                        delivery_unverified = true;
                        let guide = s(&plugin.channel.pairing,"deliveryVerificationHelpText");
                        let label = health["service_label"].as_str().unwrap_or("the new service");
                        let guidance = format!("Delivery unverified for {label}. {guide}");
                        if warning { detail.push(' '); detail.push_str(&guidance); } else { detail = guidance; }
                        warning = true;
                        error_origin = Some("user");
                    }
                }
                if let Some(error) = health["poll_error"].as_str() { warning = true; error_origin = Some("hub"); if delivery_unverified { detail.push(' '); detail.push_str(error); } else { detail = error.into(); } }
                let fs=plugin.fields.iter().map(|f| {
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
                let mut channel = json!({"id":plugin.id,"display_name":plugin.display_name,
                    "sf_symbol":plugin.sf_symbol,"tint":plugin.tint,
                    "has_hub_service":has_hub_service,"hub_ready":hub_ready,"can_configure_hub":can_configure_hub,
                    "can_clear_hub":can_configure_hub&&hub_configured_any,"can_edit_personal":can_edit_personal,
                    "can_pair":pairing_enabled&&(hub_ready||can_configure_hub),
                    "supports_outbound_only":plugin.channel.supports_outbound_only,
                    "inbound_mode":mode,"service_label":health["service_label"],"service_identity":public_service_identity(&plugin,&cfg),
                    "can_test":((hub_ready&&user_ready)||can_edit_personal)&&(hub_ready||can_configure_hub),
                    "can_unpair":(hub_ready&&user_ready)||!pair.is_null(),"error_origin":error_origin,
                    "enabled":enabled,"configured":hub_ready&&user_ready,"inbox_paused":inbox_paused,"delivery_enabled":enabled&&hub_ready&&user_ready&&!inbox_paused,"status":status,"detail":detail,"display_label":binding["display_label"],
                    "hub_provided":hub_ready&&has_hub_service,
                    "setup_guide":plugin.channel.setup_guide,
                    "pairing_help_text":plugin.channel.pairing["helpText"].as_str(),
                    "pairing_supported":pairing_supported,
                    "pairing_action_label":plugin.channel.pairing["actionLabel"].as_str().unwrap_or("Pair Account…"),
                    "warning":warning,"delivery_unverified":delivery_unverified,"pairing_manual_disclosure":plugin.channel.pairing["manualDisclosure"]==true,"pairing_open_url_label":plugin.channel.pairing["openUrlLabel"],"pairing_pending":!pair.is_null(),"pairing_code":pair["code"],"pairing_instruction":pair["instruction"],"pairing_command":pair["pairing_command"],"pairing_url":pair["pairing_url"],"bot_username":pair["bot_username"],"fields":fs});
                for (key, label) in [
                    ("service_save_label", &plugin.channel.service_actions.save),
                    ("service_change_label", &plugin.channel.service_actions.change),
                    ("service_clear_label", &plugin.channel.service_actions.clear),
                ] {
                    if let Some(label) = label {
                        channel[key] = json!(label);
                    }
                }
                bytes += channel.to_string().len() + 1;
                if bytes >= MAX_RPC_BYTES {
                    return Err("RESOURCE_LIMIT: Channel inventory exceeds response frame budget".into());
                }
                channels.push(channel);
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
        let plugins = self
            .inner
            .plugins
            .lock()
            .map_err(|_| "Registry lock poisoned")?
            .clone();
        let active = self.inner.channels_active.load(Ordering::SeqCst);
        if !active {
            // Quiesce children before any fallible SQLite maintenance.
            for plugin in &plugins {
                self.stop_process(&plugin.id)?;
            }
        }
        self.inner.db.transaction(maintain)?;
        if !active {
            return Ok(());
        }
        let mut first_error = None;
        for plugin in plugins {
            let (should_run,retry_at)=self.inner.db.transaction(|c|{let(cfg,state)=service(c,&plugin.id)?;let bound=one(c,"SELECT 1 AS n FROM hub_human_channels WHERE plugin_id=? AND enabled=1 LIMIT 1",&[json!(plugin.id)])?.is_some();let pairing=one(c,"SELECT 1 AS n FROM hub_pairing_codes WHERE plugin_id=? AND consumed_ms IS NULL AND expires_ms>? LIMIT 1",&[json!(plugin.id),json!(now_ms())])?.is_some();Ok((ready(&plugin,&cfg,"hub")&&(cfg.as_object().is_some_and(|m|m.values().any(configured))||bound||pairing),n(&state,"process_retry_at_ms")))})?;
            if !should_run {
                if self
                    .inner
                    .processes
                    .lock()
                    .map_err(|_| "Process lock poisoned")?
                    .get(&plugin.id)
                    .is_some_and(|p| p.has_pending_requests())
                {
                    continue;
                }
                self.stop_process(&plugin.id)?;
                self.set_poll_error(&plugin.id, None)?;
                continue;
            }
            if (1..=CHANNEL_RETRY_DELAY.as_millis() as i64)
                .contains(&retry_at.saturating_sub(now_ms()))
            {
                first_error
                    .get_or_insert_with(|| "Plugin subprocess exited; waiting to restart".into());
                continue;
            }
            if let Err(error) = self
                .ensure_process(&plugin)
                .and_then(|process| self.replay(&plugin, &process))
            {
                if !self.inner.channels_active.load(Ordering::SeqCst) {
                    break;
                }
                self.set_poll_error(&plugin.id, Some(&error))?;
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
    fn replay(&self, plugin: &ChannelPlugin, process: &ChannelProcess) -> Result<(), String> {
        // Reservation expiry measures delivery progress, independently of process liveness.
        let reservations = self.reserve_deliveries(plugin)?;
        let mut pending = reservations.into_iter();
        while let Some(send) = pending.next() {
            // Linearize the final eligibility check and bounded pipe write against
            // Hub inbox changes; the earlier reservation is not delivery authority.
            let handoff = self.inner.db.transaction(|c| {
                let node = s(&send["signal"]["to"], "listener_id");
                let token = &send["delivery_id"];
                let current = one(c, "SELECT s.sender_principal_id,b.principal_id FROM hub_channel_deliveries d JOIN hub_signals s ON s.seq=d.seq JOIN hub_human_channels b ON b.plugin_id=d.plugin_id AND b.principal_id=d.principal_id WHERE d.plugin_id=? AND d.reservation_id=? AND d.status='in_flight' AND b.enabled=1 AND s.acked_at_ms IS NULL", &[json!(plugin.id),token.clone()])?;
                let registration = kv_row(c, node)?;
                let allowed = current.is_some_and(|current| registration.as_ref().is_none_or(|row| {
                    let reg = parse(row, "value");
                    reg["human_paused"] != true && reg["state"] != "UNLISTENED"
                        && principal_ids_equal(s(row,"principal_id"), s(&current,"principal_id"))
                        && (s(row,"scope") != "self" || principal_ids_equal(s(row,"principal_id"), s(&current,"sender_principal_id")))
                }));
                if !allowed {
                    c.execute("UPDATE hub_channel_deliveries SET status='failed',reservation_id=NULL,next_retry_ms=0 WHERE plugin_id=? AND reservation_id=? AND status='in_flight'", &[json!(plugin.id),token.clone()])?;
                    return Ok(Ok(()));
                }
                Ok(process.send_tracked(&send))
            })?;
            if let Err((started, error)) = handoff {
                if started {
                    self.delivery_result(&plugin.id, &json!({"delivery_id":send["delivery_id"],"effect_unknown":true,"error":error}))?;
                }
                for unsent in Some(send).into_iter().filter(|_| !started).chain(pending) {
                    self.delivery_result(&plugin.id, &json!({"delivery_id":unsent["delivery_id"],"retryable":true,"effect_unknown":false,"error":"Delivery stopped before send"}))?;
                }
                return Err(error);
            }
        }
        Ok(())
    }
    fn reserve_deliveries(&self, plugin: &ChannelPlugin) -> Result<Vec<Value>, String> {
        self.inner.db.transaction(|c|{let bindings=c.query("SELECT * FROM hub_human_channels WHERE plugin_id=? AND enabled=1",&[json!(plugin.id)])?;let (service_cfg, _) = service(c, &plugin.id)?;let mut sends=Vec::new();let in_flight=one(c,"SELECT COUNT(*) AS count FROM hub_channel_deliveries WHERE plugin_id=? AND status='in_flight' AND delivered_ms>?",&[json!(plugin.id),json!(now_ms()-60_000)])?.map(|row|n(&row,"count")).unwrap_or(0);let capacity=32usize.saturating_sub(in_flight.max(0) as usize);for b in bindings{if !ready(plugin, &service_cfg, "hub") || !ready(plugin, &parse(&b, "user_config_json"), "user") { continue; }let (node,reg)=find_human(c,&b)?;if reg.as_ref().is_some_and(|reg| reg["human_paused"] == true || reg["state"] == "UNLISTENED") { continue; }let private = reg.as_ref().is_some_and(|reg| reg["scope"] == "self");let rows=c.query(&format!("{SIGNAL_SELECT} WHERE created_ms>=?1 AND subject=?2 AND seq>?3 AND acked_at_ms IS NULL AND signal_id<>'' AND (NOT ?4 OR sender_principal_id IN (?5,?6)) AND NOT EXISTS(SELECT 1 FROM hub_channel_deliveries d WHERE d.plugin_id=?7 AND d.principal_id IN (?5,?6) AND d.signal_id=hub_signals.signal_id AND (d.status IN ('delivered','failed_permanent','effect_unknown') OR d.status='in_flight' AND d.delivered_ms>?8 OR d.status='failed' AND d.next_retry_ms>?9)) ORDER BY seq LIMIT 200"),&[json!(signal_cutoff(c)?),json!(format!("cito.signal.{node}")),b["last_delivered_seq"].clone(),json!(private),b["principal_id"].clone(),json!(alternate_oidc_principal(s(&b,"principal_id"))),json!(plugin.id),json!(now_ms()-60_000),json!(now_ms())])?;for row in rows{if sends.len() >= capacity { break; }let id=s(&row,"signal_id");let data=parse(&row,"data");let token=random_id("delivery_")?;let reserved_ms=now_ms();
            // Retire superseded predecessor warnings before refreshing the reservation timestamp.
            c.execute(
                "INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,from_listener_id,delivered_ms,reservation_id)
                 VALUES(?1,?2,?3,?4,'in_flight',?5,?6,?7)
                 ON CONFLICT(plugin_id,principal_id,signal_id) DO UPDATE SET
                   status='in_flight',delivered_ms=MAX(hub_channel_deliveries.delivered_ms, excluded.delivered_ms),reservation_id=excluded.reservation_id,from_listener_id=excluded.from_listener_id,
                   last_error=CASE
                     WHEN hub_channel_deliveries.delivered_ms < COALESCE((
                       SELECT MAX(ch.updated_ms, COALESCE(ch.delivery_verified_ms, 0))
                       FROM hub_human_channels ch
                       WHERE ch.plugin_id=hub_channel_deliveries.plugin_id AND ch.principal_id IN (?8,?9)
                       ORDER BY ch.principal_id DESC LIMIT 1
                     ), 0) OR EXISTS (
                       SELECT 1 FROM hub_channel_deliveries newer
                       WHERE newer.plugin_id=hub_channel_deliveries.plugin_id AND newer.principal_id IN (?8,?9)
                         AND newer.signal_id<>hub_channel_deliveries.signal_id
                         AND (newer.delivered_ms>hub_channel_deliveries.delivered_ms
                              OR (newer.delivered_ms=hub_channel_deliveries.delivered_ms AND newer.rowid>hub_channel_deliveries.rowid))
                         AND (newer.status='delivered' OR COALESCE(newer.last_error, '') <> '')
                     ) THEN NULL ELSE hub_channel_deliveries.last_error END",
                &[json!(plugin.id),b["principal_id"].clone(),json!(id),row["seq"].clone(),data["from"]["listener_id"].clone(),json!(reserved_ms),json!(token),b["principal_id"].clone(),json!(alternate_oidc_principal(s(&b,"principal_id")))],
            )?;
            let already_sent_ids=c.query("SELECT external_msg_id FROM hub_channel_delivery_msgs WHERE plugin_id=? AND principal_id=? AND signal_id=? ORDER BY rowid",&[json!(plugin.id),b["principal_id"].clone(),json!(id)])?.into_iter().map(|r|r["external_msg_id"].clone()).collect::<Vec<_>>();let mut payload=data.clone();payload["created_at_ms"]=row["created_ms"].clone();payload["to"]=json!({"listener_id":node});sends.push(json!({"op":"deliver","claim_until_ms":reserved_ms+60_000,"already_sent_ids":already_sent_ids,"delivery_id":token,"principal_id":b["principal_id"],"user":b["owner_user"],"config":parse(&b,"user_config_json"),"signal":payload}));}}Ok(sends)})
    }
    fn plugin_frame(&self, id: &str, frame: &Value) -> Option<Value> {
        let result=match s(frame,"op"){
        "cursor_save"=>self.inner.db.transaction(|c|{
            let(_,mut state)=service(c,id)?;
            let numeric = |v:&Value| v.as_u64().or_else(||v.as_str().and_then(|s|s.parse::<u64>().ok()));
            if let Some(cursor) = numeric(&frame["cursor"]).filter(|n|*n <= i64::MAX as u64) {
                let previous = numeric(&state["reply_floor"]).unwrap_or(0);
                let rollover = frame["rollover"] == true;
                let floor = if rollover { cursor } else { cursor.max(previous) };
                state["cursor"]=json!(floor); state["reply_floor"]=json!(floor);
                if cursor >= previous || rollover {
                    if let Some(seen) = frame["last_update_ms"].as_u64() {
                        state["cursor_last_update_ms"] = json!(seen);
                    }
                }
                if rollover {
                    c.execute("DELETE FROM hub_processed_replies WHERE plugin_id=?", &[json!(id)])?;
                } else {
                    c.execute("DELETE FROM hub_processed_replies WHERE plugin_id=? AND update_id!='' AND update_id NOT GLOB '*[^0-9]*' AND CAST(update_id AS INTEGER)<?",&[json!(id),json!(floor)])?;
                }
            } else { state["cursor"]=frame["cursor"].clone(); }
            c.execute("UPDATE hub_plugin_services SET service_state_json=?,updated_ms=? WHERE plugin_id=?",&[json!(state.to_string()),json!(now_ms()),json!(id)])?;Ok(Value::Null)}),
        "health" | "process_exit" => self.inner.db.diagnostic_transaction(|c| {
            let (cfg,mut state)=service(c,id)?;
            let previous = state.clone();
            if frame["op"] == "process_exit" {
                let retry = n(frame,"retry_ms").clamp(0,CHANNEL_RETRY_DELAY.as_millis() as i64);
                state["process_retry_at_ms"] = json!(now_ms()+retry);
            }
            if let Some(error) = frame.get("error") {
                if error.is_null() {
                    if let Some(fields) = state.as_object_mut() {
                        fields.remove("poll_error");
                        fields.remove("process_retry_at_ms");
                    }
                } else if error.is_string() {
                    state["poll_error"] = error.clone();
                }
            }
            if let Some(label) = frame["service_label"].as_str().filter(|s| !s.is_empty()) {
                if frame["delivery_unverified"] == true && state["service_label"] != label {
                    state["delivery_unverified_since_ms"] = json!(now_ms());
                    invalidate_delivery_verification(c, id, None)?;
                }
                state["service_label"] = json!(label);
            }
            let changed = previous != state;
            if changed {
                save_service(c,id,&cfg)?;
                write_service_state(c,id,&state)?;
            }
            Ok(changed)
        }).map(|changed| {
            if changed {
                self.emit(json!({"type":"kv"}));
            }
            Value::Null
        }),
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
            let now = now_ms();
            let pid = s(&row, "principal_id");
            let invalid_ids = f.get("external_msg_ids").is_some_and(|ids| !ids.as_array().is_some_and(|ids| ids.iter().all(Value::is_string)))
                || f.get("external_msg_id").is_some_and(|id| !id.is_null() && !id.is_string());
            if !invalid_ids {
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
                    c.execute("UPDATE hub_channel_deliveries SET delivered_ms=MAX(delivered_ms, ?) WHERE plugin_id=? AND reservation_id=? AND status='in_flight'", &[json!(now),json!(id),json!(token)])?;
                    return Ok(Value::Null);
                }
            }
            let (status, retry_at, error, attempts_spent, reservation) = if invalid_ids {
                ("effect_unknown", 0, json!("Channel returned invalid delivery message IDs"), 0, Some(token))
            } else if f["effect_unknown"] != true && f["retryable"] == true
                && matches!(s(f, "error"), "Outbound queue is full" | "Delivery reservation expired before send" | "Delivery stopped before send")
            {
                ("failed", now + 2000, f["error"].clone(), 0, None)
            } else {
                let (status, retry_at) = delivery_transition(f, n(&row, "attempts").max(0), now);
                let error = match status {
                    "delivered" => Value::Null,
                    "effect_unknown" if f["error"].as_str().is_none() => json!("Channel returned no valid delivery status"),
                    _ => f["error"].clone(),
                };
                (status, retry_at, error, 1, Some(token))
            };
            retire_channel_warnings(c, id, pid, Some(s(&row, "signal_id")))?;
            let delivered_ms = next_outcome_ms(c, id, pid)?;
            // Keep the legacy success tie-breaker, but order new outcomes by settlement, not reservation.
            c.execute("UPDATE hub_channel_deliveries SET rowid=(SELECT MAX(rowid)+1 FROM hub_channel_deliveries),status=?,delivered_ms=?,attempts=attempts+?,next_retry_ms=?,last_error=?,reservation_id=? WHERE plugin_id=? AND reservation_id=?",
                &[json!(status),json!(delivered_ms),json!(attempts_spent),json!(retry_at),error,json!(reservation),json!(id),json!(token)])?;
            if status == "delivered" {
                verify_delivery(c, id, pid, delivered_ms)?;
            }
            Ok(Value::Null)
        })
    }
    fn redeem(&self, id: &str, f: &Value) -> Result<Value, String> {
        let plugin = self.plugin(id)?;
        let (mut result, changed) = self.inner.db.transaction(|c| {
            if f["is_private_chat"] != true {
                return Err("FORBIDDEN: pairing requires a private conversation".into());
            }
            let chat = required(f, "external_chat_id")?;
            let user = required(f, "external_user_id")?;
            let code = required(f, "code")?.trim();
            let row = one(c, "SELECT * FROM hub_pairing_codes WHERE code=? AND plugin_id=?",
                &[json!(code), json!(id)])?.ok_or("INVALID_INPUT: expired or consumed pairing code")?;
            if parse(&row, "redemption_json") == json!([chat, user]) {
                return Ok((json!({"op":"pair_ack","code":code,"ok":true,"duplicate":true,"owner_user":row["owner_user"],"error":null}), false));
            }
            if !row["consumed_ms"].is_null() || n(&row, "expires_ms") <= now_ms() {
                return Err("INVALID_INPUT: expired or consumed pairing code".into());
            }
            validate_config(&plugin, &f["user_config_patch"], "user")?;
            let pid = s(&row,"principal_id");
            let old = channel_binding(c,id,pid)?;
            let binding_pid = old.as_ref().map_or(pid, |b| s(b,"principal_id"));
            let mut config = old.as_ref().map(|b| parse(b, "user_config_json")).unwrap_or(json!({}));
            merge_channel_config(&plugin, "user", &mut config, &f["user_config_patch"], false);
            config["_external_chat_id"] = json!(chat);
            config["_external_user_id"] = json!(user);
            let updated_ms = retire_channel_warnings(c, id, binding_pid, None)?;
            let (service_cfg, _) = service(c, id)?;
            let active = ready(&plugin, &service_cfg, "hub") && ready(&plugin, &config, "user");
            let boundary = if let Some(old) = old.as_ref().filter(|old| n(old,"enabled") != 0) { n(old,"last_delivered_seq") } else { max_seq(c)? };
            c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,enabled,user_config_json,display_label,last_delivered_seq,updated_ms) VALUES(?,?,?,1,?,?,?,?) ON CONFLICT(principal_id,plugin_id) DO UPDATE SET enabled=1,user_config_json=excluded.user_config_json,display_label=excluded.display_label,last_delivered_seq=excluded.last_delivered_seq,updated_ms=MAX(updated_ms,excluded.updated_ms)",
                &[json!(binding_pid), json!(id), row["owner_user"].clone(), json!(channel_config_json(&config)?), f["display_label"].clone(), json!(boundary), json!(updated_ms)])?;
            replace_destination(c, &plugin, binding_pid, old.as_ref().map(|b|parse(b,"user_config_json")).as_ref(), &config)?;
            c.execute("UPDATE hub_channel_deliveries SET status='failed',attempts=0,next_retry_ms=0,last_error=NULL WHERE plugin_id=? AND principal_id IN (?,?) AND status='failed_permanent'", &[json!(id),json!(binding_pid),json!(alternate_oidc_principal(binding_pid))])?;
            verify_delivery(c, id, binding_pid, updated_ms)?;
            c.execute("UPDATE hub_pairing_codes SET consumed_ms=?,redemption_json=? WHERE code=?", &[json!(now_ms()), json!(json!([chat,user]).to_string()), json!(code)])?;
            let binding = one(c, "SELECT * FROM hub_human_channels WHERE principal_id=? AND plugin_id=?",
                &[json!(binding_pid), json!(id)])?.ok_or("Binding missing")?;
            attach(c, &binding, active, s(&row, "initial_human_scope"), Some(&plugin))?;
            Ok((json!({"op":"pair_ack","code":code,"ok":true,"owner_user":row["owner_user"],"error":null}), true))
        })?;
        if !changed {
            return Ok(result);
        }
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
        let mut first_error = None;
        for (id, frame) in frames {
            let proc = self
                .inner
                .processes
                .lock()
                .map_err(|_| "Process lock poisoned")?
                .get(&id)
                .cloned();
            if let Some(proc) = proc {
                if let Err(error) = proc.send(&frame) {
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
    fn inbound_reply(&self, id: &str, f: &Value) -> Result<Value, String> {
        let mut events = Vec::new();
        let mut ack_frames = Vec::new();
        let result=self.inner.db.transaction(|c|{
        let update=required(f,"update_id")?;
        let (_,state) = service(c,id)?;
        if inbound_mode(&state) == "outbound_only" { return Err("INVALID_INPUT: Replies are disabled for this channel".into()); }
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
        if let Err(error) = self.send_ack_frames(ack_frames) {
            eprintln!("Post-commit acknowledgment notification failed: {error}");
        }
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
    // Sender addresses in delivery mappings can also be hub-qualified.
    let raw_target = target.rsplit_once(':').map_or(target, |(_, id)| id);
    let target_row =
        resolve_registration(c, raw_target)?.ok_or("LISTENER_NOT_FOUND: reply target not found")?;
    if s(&target_row, "scope") == "self"
        && !principal_matches(s(&target_row, "principal_id"), sender)
    {
        return Err("FORBIDDEN_TARGET_SCOPE: reply target is private".into());
    }
    let mut registration = parse(&target_row, "value");
    if !matches!(
        authoritative_lease(&mut registration, now_ms() as u64),
        "ACTIVE" | "DISCONNECTED_RETAINED"
    ) {
        return Err("LISTENER_NOT_FOUND: reply target is inactive or expired".into());
    }
    let text = super::identity::sanitize_message(text)?;
    let (human, _) = find_human(c, sender)?;
    let target_id = s(&target_row, "key");
    if human == target_id {
        return Err("SELF_SEND_FORBIDDEN: cannot reply to the same node".into());
    }
    let source_retained = if let Some(source) = in_reply_to {
        if let Some(row) = one(
            c,
            "SELECT subject,CAST(data AS TEXT) AS data FROM hub_signals WHERE signal_id=?",
            &[json!(source)],
        )? {
            let data = parse(&row, "data");
            let from = s(&data["from"], "listener_id");
            let from = from.rsplit_once(':').map_or(from, |(_, id)| id);
            if s(&row, "subject") != format!("cito.signal.{human}")
                || !(super::identity::same_node_id(from, target_id) || from == raw_target)
            {
                return Err("FORBIDDEN: Reply target must be the source signal sender".into());
            }
            true
        } else {
            false
        }
    } else {
        false
    };
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
        .map(|key| hash(&json!([pid, host_id, "human", target_id, key]).to_string()));
    let payload = json!({"signal_id":sig,"to":target_id,"target_generation":registration["binding_generation"],"sender_endpoint_key":format!("{pid}|{host_id}|human|default"),"from":{"principal_id":pid,"user":owner,"host_id":host_id,"host_name":host_name,"harness":"human","listener_id":super::identity::public_listener_id(owner, &human),"title":"Human","about":"Human channel"},"message":text,"in_reply_to":in_reply_to});
    let published = publish_payload(
        c,
        &format!("cito.signal.{target_id}"),
        &payload,
        None,
        msg_id.as_deref().or(Some(&sig)),
        None,
        events,
    )?;
    // A retry must return the committed signal ID, not the newly generated candidate.
    let signal_id = if published["duplicate"] == true {
        let row = one(
            c,
            "SELECT signal_id, CAST(data AS TEXT) AS data FROM hub_signals WHERE seq=?",
            &[published["seq"].clone()],
        )?
        .ok_or("Published reply missing")?;
        if msg_id.is_some() {
            let stored = parse(&row, "data");
            if stored["message"] != payload["message"]
                || stored["in_reply_to"] != payload["in_reply_to"]
            {
                return Err("IDEMPOTENCY_CONFLICT: reply payload differs".into());
            }
        }
        row["signal_id"].clone()
    } else {
        json!(sig)
    };
    let (ack_request, acked_at_ms) = if let Some(source) = in_reply_to {
        let request = json!({"node_id":human,"signal_ids":[source],"channel_id":format!("{host_id}:{channel}")});
        if source_retained {
            let acked = ack(c, &request, None, events)?;
            (Some(request), acked["acked_at_ms"].clone())
        } else {
            (None, json!(now_ms()))
        }
    } else {
        (None, Value::Null)
    };
    Ok((
        json!({"ok":true,"seq":published["seq"],"signal_id":signal_id,"target_display":registration["public_id"],"acked_at_ms":acked_at_ms,"error":null}),
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
fn stream_config(c: &Connection, name: &str) -> Result<Value, String> {
    let row = one(
        c,
        "SELECT json_val FROM hub_meta WHERE key=?",
        &[json!(format!("stream:{name}"))],
    )?;
    if let Some(row) = row {
        Ok(parse(&row, "json_val"))
    } else if matches!(name, SIGNALS | KV_STREAM) {
        Ok(default_stream(name))
    } else {
        Err(format!("10059: stream not found: {name}"))
    }
}
fn stream_info(c: &Connection, name: &str) -> Result<Value, String> {
    let config = stream_config(c, name)?;
    let state = if name == KV_STREAM {
        let count = one(c, "SELECT COUNT(*) AS messages FROM hub_kv", &[])?.unwrap_or(Value::Null);
        let rev =
            one(c, "SELECT int_val FROM hub_meta WHERE key='kv_seq'", &[])?.unwrap_or(Value::Null);
        json!({"messages":count["messages"],"last_seq":rev["int_val"],"first_seq":if n(&count,"messages")==0{0}else{1},"bytes":0,"consumer_count":0})
    } else if name == SIGNALS {
        one(c,"SELECT COUNT(*) AS messages,COALESCE(MAX(seq),0) AS last_seq,COALESCE(MIN(seq),0) AS first_seq,0 AS bytes,(SELECT COUNT(*) FROM hub_consumers) AS consumer_count FROM hub_signals",&[])?.unwrap_or(Value::Null)
    } else {
        json!({"messages":0,"last_seq":0,"first_seq":0,"bytes":0,"consumer_count":0})
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
    let cfg = stream_config(c, SIGNALS)?;
    let age = cfg["max_age"].as_i64().unwrap_or(1_800_000_000_000) / 1_000_000;
    Ok(if age > 0 {
        now_ms().saturating_sub(age)
    } else {
        0
    })
}
fn prune(c: &Connection) -> Result<(), String> {
    let cfg = stream_config(c, SIGNALS)?;
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
        "DELETE FROM hub_processed_replies WHERE processed_at_ms<?",
        &[json!(now - 604_800_000)],
    )?;
    c.execute(
        "DELETE FROM hub_channel_deliveries WHERE delivered_ms<? AND NOT EXISTS(SELECT 1 FROM hub_signals s WHERE s.signal_id=hub_channel_deliveries.signal_id)",
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
    if owns_principal(s(row, "sender_principal_id"), p) {
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
                "SELECT subject,sender_principal_id FROM hub_signals WHERE seq=?",
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
    if super::identity::same_node_id(s(&data["from"], "listener_id"), target) {
        return Err("SELF_SEND_FORBIDDEN: Cannot send to your own node".into());
    }
    let target_row = kv_row(c, target)?;
    if target_row.as_ref().is_some_and(|row| {
        s(row, "scope") == "self"
            && (oidc(caller) || raw.is_some() && data.get("target_generation").is_some())
            && !caller.map_or_else(
                || principal_ids_equal(s(row, "principal_id"), s(&data["from"], "principal_id")),
                |p| principal_matches(s(row, "principal_id"), p),
            )
    }) {
        return Err("FORBIDDEN_TARGET_SCOPE: target is private".into());
    }
    let mut registration = target_row.as_ref().map(|row| parse(row, "value"));
    if registration
        .as_ref()
        .is_some_and(|reg| reg["human_paused"] == true)
    {
        return Err("LISTENER_NOT_FOUND: Human inbox is paused".into());
    }
    if oidc(caller)
        && data.get("from").is_some()
        && !owns_principal(s(&data["from"], "principal_id"), caller)
    {
        return Err("PERMISSIONS_VIOLATION: sender principal mismatch".into());
    }
    let bytes = raw
        .map(std::borrow::Cow::Borrowed)
        .unwrap_or_else(|| std::borrow::Cow::Owned(data.to_string().into_bytes()));
    if bytes.len().div_ceil(3) * 4 + subject.len() * 2 + 4096 > MAX_RPC_BYTES {
        return Err("INVALID_INPUT: Signal exceeds response frame budget".into());
    }
    for key in ["signal_id", "in_reply_to"] {
        if let Some(value) = data
            .get(key)
            .filter(|v| key != "in_reply_to" || !v.is_null())
        {
            if !value
                .as_str()
                .is_some_and(|id| (1..=128).contains(&id.len()))
            {
                return Err(format!(
                    "INVALID_INPUT: {key} must be a string of 1..128 bytes"
                ));
            }
        }
    }
    let signal_id = data["signal_id"].as_str();
    if let Some(id) = signal_id {
        // BEGIN IMMEDIATE serializes admission across all writers, including upgraded databases.
        if let Some(row) = one(c,"SELECT seq,subject,hex(data) AS data_hex FROM hub_signals WHERE signal_id=? ORDER BY seq LIMIT 1", &[json!(id)])? {
            if row["subject"] != subject || stored_bytes(&row,"data")? != bytes.as_ref() {
                return Err("INVALID_INPUT: signal_id already belongs to a different publication".into());
            }
            return Ok(json!({"stream":SIGNALS,"seq":row["seq"],"duplicate":true}));
        }
    }
    if let Some(mid) = msg_id {
        let config = stream_config(c, SIGNALS)?;
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
    // Versioned signals come from discovery and must not cross an attachment replacement.
    if raw.is_some() && data.get("target_generation").is_some() {
        let reg = registration
            .as_mut()
            .ok_or("LISTENER_NOT_FOUND: target registration disappeared")?;
        if !matches!(
            authoritative_lease(reg, now as u64),
            "ACTIVE" | "DISCONNECTED_RETAINED"
        ) {
            return Err("LISTENER_NOT_FOUND: target is inactive or expired".into());
        }
        if !data["target_generation"]
            .as_u64()
            .is_some_and(|generation| {
                generation > 0 && reg["binding_generation"].as_u64() == Some(generation)
            })
        {
            return Err("STALE_GENERATION: target registration was replaced".into());
        }
    }
    c.execute(
        &format!(
            "INSERT INTO hub_signals(subject,msg_id,signal_id,created_ms,sender_principal_id,data) VALUES(?,?,?,?,?,{})",
            blob_literal(&bytes)
        ),
        &[json!(subject), json!(msg_id), json!(signal_id), json!(now), data["from"]["principal_id"].as_str().into()],
    )?;
    let seq =
        one(c, "SELECT last_insert_rowid() AS seq", &[])?.unwrap_or(Value::Null)["seq"].clone();
    audit(c, principal_id(caller), "send", target, &json!({"seq":seq}))?;
    events.push(json!({"type":"signal","seq":seq,"subject":subject,"created_ms":now,"data_b64":B64.encode(&bytes),"acked_at_ms":null}));
    Ok(json!({"stream":SIGNALS,"seq":seq,"duplicate":false}))
}
fn consumer_owner(row: &Value, p: Option<&Value>) -> Result<(), String> {
    if row["stream"] != SIGNALS {
        return Err("INVALID_INPUT: Unsupported consumer stream".into());
    }
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
    budget: usize,
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
            if page_bytes + size > budget || (!items.is_empty() && page_bytes + size > 4 * 1024 * 1024) { return Ok(false); }
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
    if array.len() > 1024 {
        return Err("INVALID_INPUT: signal_ids exceeds 1024 IDs".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for id in array {
        let id = id
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("INVALID_INPUT: signal ID must be a string")?;
        if seen.insert(id) {
            out.push(id.to_owned())
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
    let registration = kv_row(c, node)?.map(|row| parse(&row, "value"));
    let incarnation = registration.as_ref().and_then(|reg| {
        let attachment = reg["channels"].get(channel)?;
        let owner = attachment["owner_token"].as_str()?;
        let generation = attachment
            .get("binding_generation")
            .unwrap_or(&reg["binding_generation"]);
        Some((attachment, owner, generation))
    });
    // Older clients omit the expected incarnation. Still bind their stored lease
    // to the attachment present at admission so its successor cannot reuse it.
    let key = if let Some((attachment, owner, generation)) = incarnation {
        if super::lease::attachment_availability(attachment, now as u64) != "ACTIVE"
            || p.get("owner_token").is_some_and(|token| token != owner)
            || p.get("binding_generation")
                .is_some_and(|expected| expected != generation)
        {
            return Err("STALE_GENERATION: Wake attachment is inactive or was replaced".into());
        }
        format!(
            "incarnation:{}",
            hash(&json!([channel, owner, generation]).to_string())
        )
    } else {
        if p.get("owner_token").is_some() || p.get("binding_generation").is_some() {
            return Err("STALE_GENERATION: Wake attachment not found".into());
        }
        channel.to_owned()
    };
    let mut rows = Vec::new();
    let mut acked = Vec::new();
    let mut conflict = false;
    for id in &ids {
        let row=one(c,"SELECT seq,claimed_by_channel,claim_until_ms,acked_at_ms FROM hub_signals WHERE subject=? AND signal_id=?",&[json!(format!("cito.signal.{node}")),json!(id)])?;
        match row {
            Some(row) if row["acked_at_ms"].is_null() => {
                if !row["claimed_by_channel"].is_null()
                    && s(&row, "claimed_by_channel") != key
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
            &[json!(key), json!(now + lease), row["seq"].clone()],
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
        count+=c.execute("UPDATE hub_signals SET acked_at_ms=COALESCE(acked_at_ms,?),claimed_by_channel=NULL,claim_until_ms=NULL WHERE subject=? AND signal_id=?",&[json!(now),json!(format!("cito.signal.{node}")),json!(id)])?;
    }
    audit(
        c,
        principal_id(caller),
        "ack",
        node,
        &json!({"acked_count":count}),
    )?;
    let data = json!({"type":"node_ack","node_id":node,"signal_ids":ids,"acked_at_ms":now,"channel_id":p["channel_id"]});
    events
        .push(json!({"type":"nc","subject":format!("cito.ack.{node}"),"data_b64":encoded(&data)}));
    Ok(json!({"acked_count":count,"acked_at_ms":now,"signal_ids":ids}))
}

fn configured(v: &Value) -> bool {
    match v {
        Value::String(s) => !s.trim().is_empty(),
        Value::Null => false,
        _ => true,
    }
}
fn ready(plugin: &ChannelPlugin, config: &Value, scope: &str) -> bool {
    plugin
        .fields
        .iter()
        .filter(|f| s(f, "scope") == scope && f["required"] == true)
        .all(|f| configured(&config[s(f, "key")]))
}
// Check patches and their final stored form: merges and pairing metadata can grow either scope.
fn channel_config_json(config: &Value) -> Result<String, String> {
    let fields = config
        .as_object()
        .ok_or("INVALID_INPUT: channel config must be an object")?;
    if fields
        .values()
        .filter_map(Value::as_str)
        .any(|s| s.len() > 64 * 1024)
    {
        return Err("INVALID_INPUT: channel config field exceeds 64 KiB".into());
    }
    let encoded = config.to_string();
    if encoded.len() > 256 * 1024 {
        return Err("INVALID_INPUT: channel config exceeds 256 KiB".into());
    }
    Ok(encoded)
}
fn validate_config(plugin: &ChannelPlugin, patch: &Value, scope: &str) -> Result<(), String> {
    channel_config_json(patch)?;
    let patch = patch
        .as_object()
        .ok_or("INVALID_INPUT: channel config must be an object")?;
    for (key, value) in patch {
        let field = plugin
            .fields
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
    let identity = &plugin.channel.service_identity;
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
fn public_service_identity(plugin: &ChannelPlugin, cfg: &Value) -> Value {
    let identity = &plugin.channel.service_identity;
    let prefix = identity["field"].as_str().and_then(|field| {
        let separator = identity["separator"].as_str().filter(|s| !s.is_empty())?;
        let (prefix, rest) = cfg[field].as_str()?.split_once(separator)?;
        (!prefix.is_empty() && !rest.is_empty()).then_some(prefix)
    });
    json!(prefix)
}

fn inbound_mode(state: &Value) -> &str {
    state["inbound_mode"].as_str().unwrap_or("primary")
}

pub(super) fn channel_display_label(label: &Value) -> Option<&str> {
    label
        .as_str()
        .map(str::trim)
        .filter(|s| (1..=128).contains(&s.len()))
}

fn result_service_label(result: &Value) -> Option<&str> {
    ["service_label", "bot_username"]
        .into_iter()
        .filter_map(|key| result[key].as_str())
        .find(|label| !label.is_empty())
}

fn write_service_state(c: &Connection, id: &str, state: &Value) -> Result<(), String> {
    c.execute(
        "UPDATE hub_plugin_services SET service_state_json=? WHERE plugin_id=?",
        &[json!(state.to_string()), json!(id)],
    )?;
    Ok(())
}

fn persist_service_label(c: &Connection, id: &str, result: &Value) -> Result<bool, String> {
    let Some(label) = result_service_label(result) else {
        return Ok(false);
    };
    let (cfg, mut state) = service(c, id)?;
    if state["service_label"] == label {
        return Ok(false);
    }
    save_service(c, id, &cfg)?;
    state["service_label"] = json!(label);
    write_service_state(c, id, &state)?;
    Ok(true)
}

fn save_channel_service(
    c: &Connection,
    plugin: &ChannelPlugin,
    cfg: &Value,
) -> Result<bool, String> {
    if plugin.channel.service_identity["field"].as_str().is_none() {
        return save_service(c, &plugin.id, cfg);
    }
    let (old, _) = service(c, &plugin.id)?;
    let change = if service_identity(plugin, &old) != service_identity(plugin, cfg) {
        ServiceChange::Identity
    } else {
        ServiceChange::SameIdentity
    };
    let changed = save_service_state(c, &plugin.id, cfg, change)?;
    if changed
        && matches!(change, ServiceChange::Identity)
        && !s(&plugin.channel.pairing, "deliveryVerificationHelpText").is_empty()
        && ready(plugin, &old, "hub")
        && ready(plugin, cfg, "hub")
    {
        let (_, mut state) = service(c, &plugin.id)?;
        state["delivery_unverified_since_ms"] = json!(now_ms());
        write_service_state(c, &plugin.id, &state)?;
    }
    Ok(changed)
}
fn save_service(c: &Connection, id: &str, cfg: &Value) -> Result<bool, String> {
    save_service_state(c, id, cfg, ServiceChange::Configuration)
}
#[derive(Clone, Copy)]
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
    let (old, state) = service(c, id)?;
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
    let state = match change {
        ServiceChange::SameIdentity => state,
        // Keep the inbound role when replacing credentials, too: otherwise a
        // credential edit could silently start a duplicate poller.
        _ => state
            .get("inbound_mode")
            .map_or_else(|| json!({}), |mode| json!({"inbound_mode":mode})),
    };
    c.execute("INSERT INTO hub_plugin_services(plugin_id,config_json,service_state_json,updated_ms) VALUES(?,?,?,?) ON CONFLICT(plugin_id) DO UPDATE SET config_json=excluded.config_json,service_state_json=excluded.service_state_json,updated_ms=excluded.updated_ms", &[json!(id),json!(channel_config_json(cfg)?),json!(state.to_string()),json!(now_ms())])?;
    if matches!(change, ServiceChange::Identity) {
        fence_in_flight(
            c,
            id,
            None,
            "Service identity replaced during delivery; check the previous account",
        )?;
        invalidate_delivery_verification(c, id, None)?;
        c.execute(
            "DELETE FROM hub_channel_delivery_msgs WHERE plugin_id=?",
            &[json!(id)],
        )?;
        c.execute("UPDATE hub_channel_deliveries SET external_msg_id=NULL,from_listener_id=NULL,reservation_id=NULL WHERE plugin_id=?", &[json!(id)])?;
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
            let secret = plugin
                .fields
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
            v["_external_chat_id"].as_str().map(str::to_owned),
            v["_external_user_id"].as_str().map(str::to_owned),
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
                && plugin.channel.pairing["privateIdentityField"]
                    .as_str()
                    .map(|key| old[key] != config[key])
                    .unwrap_or(old != config)
    }) {
        let alias = json!(alternate_oidc_principal(pid));
        c.execute(
            "DELETE FROM hub_channel_delivery_msgs WHERE plugin_id=? AND principal_id IN (?,?)",
            &[json!(id), json!(pid), alias.clone()],
        )?;
        fence_in_flight(
            c,
            id,
            Some(pid),
            "Destination replaced while delivery was in flight; check the previous destination",
        )?;
        invalidate_delivery_verification(c, id, Some(pid))?;
        c.execute("UPDATE hub_channel_deliveries SET external_msg_id=NULL,reservation_id=NULL,from_listener_id=NULL,status=CASE WHEN status='failed_permanent' THEN 'failed' ELSE status END,last_error=CASE WHEN status IN ('failed','failed_permanent') THEN NULL ELSE last_error END,attempts=CASE WHEN status IN ('failed','failed_permanent') THEN 0 ELSE attempts END,next_retry_ms=CASE WHEN status IN ('failed','failed_permanent') THEN 0 ELSE next_retry_ms END WHERE plugin_id=? AND principal_id IN (?,?)", &[json!(id),json!(pid),alias])?;
    }
    Ok(())
}
// Preserve the stored binding key: delivery reservations and reply mappings refer to it.
fn channel_binding(c: &Connection, id: &str, pid: &str) -> Result<Option<Value>, String> {
    one(c, "SELECT * FROM hub_human_channels WHERE plugin_id=? AND principal_id IN (?,?) ORDER BY principal_id DESC LIMIT 1", &[json!(id),json!(pid),json!(alternate_oidc_principal(pid))])
}

// Warning text is independent of the delivery status, which fences replay.
fn retire_channel_warnings(
    c: &Connection,
    id: &str,
    pid: &str,
    exclude_signal: Option<&str>,
) -> Result<i64, String> {
    let args = [
        json!(id),
        json!(pid),
        json!(alternate_oidc_principal(pid)),
        json!(exclude_signal),
    ];
    c.execute("UPDATE hub_channel_deliveries SET last_error=NULL WHERE plugin_id=?1 AND principal_id IN (?2,?3) AND (?4 IS NULL OR signal_id<>?4) AND last_error IS NOT NULL", &args)?;
    Ok(now_ms())
}

fn retire_service_warnings(c: &Connection, id: &str) -> Result<(), String> {
    for row in c.query(
        "SELECT DISTINCT principal_id FROM hub_channel_deliveries WHERE plugin_id=?",
        &[json!(id)],
    )? {
        let pid = s(&row, "principal_id");
        let updated_ms = retire_channel_warnings(c, id, pid, None)?;
        c.execute("UPDATE hub_human_channels SET updated_ms=MAX(updated_ms,?) WHERE plugin_id=? AND principal_id IN (?,?)", &[json!(updated_ms),json!(id),json!(pid),json!(alternate_oidc_principal(pid))])?;
    }
    Ok(())
}

fn next_outcome_ms(c: &Connection, id: &str, pid: &str) -> Result<i64, String> {
    let binding = channel_binding(c, id, pid)?;
    let watermark = binding.map_or(0, |b| {
        n(&b, "updated_ms").max(n(&b, "delivery_verified_ms"))
    });
    Ok(now_ms().max(watermark))
}

fn verify_delivery(c: &Connection, id: &str, pid: &str, verified_ms: i64) -> Result<(), String> {
    c.execute("UPDATE hub_human_channels SET delivery_verified_ms=? WHERE plugin_id=? AND principal_id IN (?,?)", &[json!(verified_ms),json!(id),json!(pid),json!(alternate_oidc_principal(pid))])?;
    Ok(())
}

fn invalidate_delivery_verification(
    c: &Connection,
    id: &str,
    pid: Option<&str>,
) -> Result<(), String> {
    let args = [
        json!(id),
        json!(pid),
        json!(pid.and_then(alternate_oidc_principal)),
    ];
    for binding in c.query("SELECT principal_id,delivery_verified_ms FROM hub_human_channels WHERE plugin_id=?1 AND (?2 IS NULL OR principal_id IN (?2,?3)) AND delivery_verified_ms IS NOT NULL", &args)? {
        let pid = s(&binding, "principal_id");
        // The same binding that hid the old warning must authorize its retirement.
        if alternate_oidc_principal(pid).is_some_and(|alias| alias.as_str() > pid)
            && channel_binding(c, id, pid)?.is_some_and(|current| current["principal_id"] != pid)
        {
            continue;
        }
        c.execute("UPDATE hub_channel_deliveries SET last_error=NULL WHERE plugin_id=? AND principal_id IN (?,?) AND delivered_ms<? AND (status<>'in_flight' OR attempts>0) AND last_error IS NOT NULL", &[json!(id),json!(pid),json!(alternate_oidc_principal(pid)),binding["delivery_verified_ms"].clone()])?;
    }
    c.execute("UPDATE hub_human_channels SET delivery_verified_ms=NULL WHERE plugin_id=?1 AND (?2 IS NULL OR principal_id IN (?2,?3)) AND delivery_verified_ms IS NOT NULL", &args)?;
    Ok(())
}

fn fence_in_flight(c: &Connection, id: &str, pid: Option<&str>, error: &str) -> Result<(), String> {
    let args = [
        json!(id),
        json!(pid),
        json!(pid.and_then(alternate_oidc_principal)),
    ];
    for row in c.query("SELECT DISTINCT principal_id FROM hub_channel_deliveries WHERE plugin_id=?1 AND (?2 IS NULL OR principal_id IN (?2,?3)) AND status='in_flight'", &args)? {
        let pid = s(&row, "principal_id");
        let binding = [json!(id), json!(pid), json!(alternate_oidc_principal(pid))];
        // An alias may already have been fenced in this batch. Keep one warning for both keys.
        let Some(warning) = one(c, "SELECT rowid FROM hub_channel_deliveries WHERE plugin_id=? AND principal_id IN (?,?) AND status='in_flight' ORDER BY rowid DESC LIMIT 1", &binding)? else {
            continue;
        };
        retire_channel_warnings(c, id, pid, None)?;
        let delivered_ms = next_outcome_ms(c, id, pid)?;
        c.execute("UPDATE hub_channel_deliveries SET status='effect_unknown',delivered_ms=?,next_retry_ms=0 WHERE plugin_id=? AND principal_id IN (?,?) AND status='in_flight'",
            &[json!(delivered_ms), binding[0].clone(), binding[1].clone(), binding[2].clone()])?;
        c.execute("UPDATE hub_channel_deliveries SET last_error=?,rowid=(SELECT MAX(rowid)+1 FROM hub_channel_deliveries) WHERE rowid=?", &[json!(error), warning["rowid"].clone()])?;
    }
    Ok(())
}
fn cancel_pair(c: &Connection, id: &str, pid: &str) -> Result<(), String> {
    c.execute("UPDATE hub_pairing_codes SET consumed_ms=? WHERE principal_id IN (?,?) AND plugin_id=? AND consumed_ms IS NULL", &[json!(now_ms()),json!(pid),json!(alternate_oidc_principal(pid)),json!(id)])?;
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
    if frame["effect_unknown"] == true || frame["ok"].as_bool().is_none() {
        ("effect_unknown", 0)
    } else if frame["ok"] == true {
        ("delivered", 0)
    } else if frame.get("retryable").is_some_and(|retry| *retry != true) || attempts >= 5 {
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
                registration::register_endpoint, state::LocalState, transport::HubClient,
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
            let enrolled = register_endpoint(&first, &client, "first-host", &args, &agent)?;
            let id = s(&enrolled, "canonical_listener_id");
            assert_eq!(
                client.kv_get(id)?.unwrap().0["principal_id"],
                "https://id.example.com#alice"
            );
            client.identity = json!({"principal_id":"oidc:https://id.example.com|alice","legacy_principal_id":"https://id.example.com#alice","owner_user":"alice","oidc":true});
            for (state, host) in [(&first, "first-host"), (&second, "second-host")] {
                assert_eq!(
                    register_endpoint(state, &client, host, &args, &agent)?
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
        *hub.inner.plugins.lock().unwrap() = vec![hub_plugins::test_descriptor(
            &dir.join("missing"),
            "missing",
            if blocked_input {
                vec!["/bin/sh".into(), "-c".into(), "exec sleep 30".into()]
            } else {
                vec![dir.join("unavailable").to_string_lossy().into_owned()]
            },
            json!({}),
        )];
        let (entered, boundary) = mpsc::channel();
        let (resume, released) = mpsc::channel();
        *hub.inner.before_worker_wait.lock().unwrap() = Some(Box::new(move || {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(3)).unwrap();
        }));
        let value = if blocked_input {
            "x".repeat(64 * 1024)
        } else {
            "enabled".into()
        };
        hub.inner
            .db
            .transaction(|c| save_service(c, "missing", &json!({"first":value,"second":value})))
            .unwrap();
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
            ].into_iter().map(|(id, command)| hub_plugins::test_descriptor(&dir.join(id), id, command, json!({}))).collect();
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
        let plugin = hub_plugins::test_descriptor(
            &dir,
            "sample",
            vec![],
            json!({"fields":[{"key":"credential","scope":"hub","required":true},
                {"key":"destination","scope":"user","required":true}],"pairing":{"supported":true}}),
        );
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
            let plugin = hub_plugins::test_descriptor(&dir, "fixture", vec![], json!({}));
            let changes = || hub.inner.db.query("SELECT total_changes() AS n", &[]);
            let before = changes()?;
            for _ in 0..8 {
                hub.inner.db.transaction(|c| {
                    assert!(backlog_from(c,&json!({"name":"consumer","filter_subject":"cito.signal.node","start_seq":1}),None,0,MAX_RPC_BYTES)?["backlog"].as_array().is_some_and(Vec::is_empty));
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
            c.execute("INSERT INTO hub_signals(subject,signal_id,created_ms,sender_principal_id,data) VALUES('cito.signal.target','signal',?,?,CAST(? AS BLOB))", &[json!(now_ms()),json!(sender),json!(json!({"from":{"principal_id":sender}}).to_string())])?;
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
        *hub.inner.plugins.lock().map_err(|e| e.to_string())? = vec![hub_plugins::test_descriptor(
            &dir,
            "messenger",
            vec![dir.join("unavailable").to_string_lossy().into_owned()],
            json!({"displayName":"Messenger","fields":[{"key":"destination","scope":"user","type":"text","required":true}]}),
        )];
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
        *hub.inner.plugins.lock().map_err(|e| e.to_string())? = vec![hub_plugins::test_descriptor(
            &dir,
            "messenger",
            vec![],
            json!({"displayName":"Messenger"}),
        )];
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
    write_kv(
        c,
        id,
        now,
        reg.to_string().as_bytes(),
        &reg["principal_id"],
        &reg["scope"],
    )
}

fn reconcile_human_row(
    c: &Connection,
    row: Value,
    events: &mut Vec<Value>,
) -> Result<Value, String> {
    let mut reg = parse(&row, "value");
    // Opaque or malformed legacy records remain readable, without a Human write-back.
    if reg["harness"] != "human" || validate_human(&row, &reg, &row).is_err() {
        return Ok(row);
    }
    let before = reg.clone();
    if reg["human_control_revision"].is_null() {
        reg["human_control_revision"] = json!(1);
    }
    super::lease::reconcile_registration_state(&mut reg, now_ms().max(0) as u64);
    if reg != before {
        events.push(save_human(c, s(&row, "key"), &mut reg)?);
        return kv_row(c, s(&row, "key"))?
            .ok_or_else(|| "STATE_ERROR: Human record disappeared".into());
    }
    Ok(row)
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
    let (id, existing) = find_human(c, caller)?;
    if p.get("node_id").is_some_and(|node| node != &id) {
        return Err("FORBIDDEN: Human operation belongs to another principal".into());
    }
    let mut reg = if let Some(old) = existing {
        old
    } else {
        if !matches!(op, "human_device_attach" | "human_inbox_update") {
            return Err("LISTENER_NOT_FOUND: Human inbox not found".into());
        }
        json!({"listener_id":id,"public_id":super::identity::public_listener_id(s(caller,"owner_user"),&id),"principal_id":pid,"owner_user":caller["owner_user"],"harness":"human","session_id":"default","title":"Human","about":"Human","scope":default_scope,"human_paused":false,"human_control_revision":1,"binding_generation":1,"channels":{}})
    };
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
                || (same && existing["state"] != "ACTIVE")
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

fn validate_human(row: &Value, reg: &Value, principal: &Value) -> Result<(), String> {
    if !reg.is_object() {
        return Err("INVALID_INPUT: Listener must be an object".into());
    }
    if reg["harness"] != "human"
        || reg["session_id"] != "default"
        || !principal_matches(s(row, "principal_id"), principal)
        || !principal_matches(s(reg, "principal_id"), principal)
    {
        return Err("FORBIDDEN: Human registration identity mismatch".into());
    }
    Ok(())
}
fn find_human(c: &Connection, principal: &Value) -> Result<(String, Option<Value>), String> {
    let id = listener_id(required(principal, "principal_id")?, "human", "default");
    let mut principals = Vec::new();
    for key in ["principal_id", "legacy_principal_id"] {
        let value = s(principal, key);
        if value.is_empty() {
            continue;
        }
        principals.push(json!(value));
        if let Some(alias) = super::identity::alternate_oidc_principal(value) {
            principals.push(json!(alias));
        }
    }
    let placeholders = vec!["?"; principals.len()].join(",");
    let mut found = None;
    c.query_while(
        &format!("SELECT key,principal_id,CAST(value AS TEXT) AS value FROM hub_kv WHERE principal_id IN ({placeholders}) ORDER BY revision DESC"),
        &principals, |row| {
            let reg = parse(&row,"value");
            if reg["harness"] == "human" && validate_human(&row,&reg,principal).is_ok() {
                found = Some((s(&row,"key").into(),Some(reg)));
                return Ok(false);
            }
            Ok(true)
        })?;
    if let Some(found) = found {
        return Ok(found);
    }
    if let Some(row) = kv_row(c, &id)? {
        let reg = parse(&row, "value");
        validate_human(&row, &reg, principal)?;
        return Ok((id, Some(reg)));
    }
    Ok((id, None))
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
    let (id, existing) = find_human(c, b)?;
    let mut reg=existing.unwrap_or_else(||json!({"listener_id":id,"public_id":super::identity::public_listener_id(s(b,"owner_user"), &id),"principal_id":pid,"owner_user":b["owner_user"],"host_id":"hub","host_name":"Hub","harness":"human","session_id":"default","cwd":"","repo":null,"branch":null,"title":"Human","about":"Human","human_paused":false,"human_control_revision":1,"scope":default_scope,"state":"ACTIVE","binding_generation":1,"owner_token":"hub","channels":{}}));
    if !reg["channels"].is_object() {
        reg["channels"] = json!({})
    }
    if reg["human_control_revision"].is_null() {
        reg["human_control_revision"] = json!(1);
    }
    let channel = format!("hub:{}", s(b, "plugin_id"));
    reg["channels"][&channel] = json!({"channel_id":channel,"kind":"human","host_id":"hub","host_name":"Hub","display_name":plugin.map(|p|json!(p.display_name)).unwrap_or_else(||b["plugin_id"].clone()),"display_label":b["display_label"],"delivery_mode":"notify_fanout","priority":100,"state":if enabled{"ACTIVE"}else{"UNLISTENED"},"updated_at_ms":now_ms()});
    reg["updated_at_ms"] = json!(now_ms());
    save_human(c, &id, &mut reg)?;
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
        "--name" => "CITO_HUB_NAME",
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
        let setting = cli_setting(key);
        if setting == key {
            return Err(if arg.starts_with('-') {
                format!("Unknown flag: {arg}")
            } else {
                format!("Unexpected argument: {arg}")
            });
        }
        let value = super::cli_value::read(args, &mut i, key, true)
            .map_err(|_| format!("Missing value for {key} ({setting})"))?
            .to_owned();
        if value.trim().is_empty() {
            return Err(format!(
                "INVALID_INPUT: {key} ({setting}) must not be empty"
            ));
        }
        opts.insert(setting.into(), value);
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
    let value = |env: &str| opts.get(env).cloned().or_else(|| env_value(env));
    let mode = value("CITO_AUTH_MODE").unwrap_or_else(|| "none".into());
    let port = value("CITO_HUB_PORT")
        .unwrap_or_else(|| "4291".into())
        .parse::<u16>()
        .map_err(|_| "CITO_HUB_PORT must be an integer in 0..65535")?;
    let bind = value("CITO_HUB_BIND").unwrap_or_else(|| {
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
        value("CITO_HUB_TOKEN"),
        value("CITO_OIDC_ISSUER"),
        value("CITO_OIDC_CLIENT_ID"),
    )?;
    auth.advertise_name(value("CITO_HUB_NAME").as_deref())?;
    auth.redirect_port = value("CITO_OIDC_REDIRECT_PORT")
        .map(|raw| {
            raw.parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .ok_or("INVALID_INPUT: redirect port must be an integer in 1..65535")
        })
        .transpose()?;
    let raw = value("CITO_HUB_DATA_DIR").unwrap_or_else(|| "~/.cito/hub".into());
    let home = env_value("HOME").unwrap_or_else(|| ".".into());
    let data_dir = super::plugins::expand_home_dir(&raw, "CITO_HUB_DATA_DIR", Some(&home))?;
    let engine = HubEngine::open_with_auth_mode(&data_dir, &mode)?;
    let server = engine
        .start_server(&bind, port, auth)
        .inspect_err(|_| engine.shutdown())?;
    eprintln!("cito hub: ready on http://{} (auth={mode})", server.address);
    let url = if server.address.ip().is_unspecified() {
        format!("http://<host>:{}", server.port())
    } else {
        format!("http://{}", server.address)
    };
    let join = if mode == "token" {
        format!("cito setup --url {url} --auth token --token <token>")
    } else {
        format!("cito setup --url {url}")
    };
    eprintln!("cito hub: join with: {join}");
    // The listener and signal handler share the same level-triggered shutdown wake.
    SIGNAL_WRITE.store(server.wake.write_fd(), Ordering::SeqCst);
    // SAFETY: sigaction receives initialized pointers; the handler only stores
    // an atomic and writes to the server's nonblocking wake descriptor.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = hub_signal as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
    let result = server.wait_for_stop(&HUB_STOP);
    SIGNAL_WRITE.store(-1, Ordering::SeqCst);
    engine.shutdown_before(|| server.shutdown());
    result
}
