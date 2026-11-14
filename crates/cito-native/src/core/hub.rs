//! One persisted HubEngine for embedded and standalone HTTP deployments.
use super::auth::{env_value, local_principal, principal_ids_equal, principal_matches, HubAuth};
use super::database::{Connection, Database};
use super::hub_plugins::{self, ChannelPlugin, ChannelProcess};
use super::identity::{hash, listener_id};
use super::lease::evaluate_node_lease;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
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
    identity: Arc<()>,
}
pub struct Subscription {
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
struct Inner {
    db: Database,
    user_plugins_dir: PathBuf,
    closed: AtomicBool,
    subscribers: Mutex<Vec<Subscriber>>,
    plugins: Mutex<Vec<ChannelPlugin>>,
    processes: Mutex<HashMap<String, Arc<ChannelProcess>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    lifecycle: Mutex<()>,
    remote: AtomicBool,
}
#[derive(Clone)]
pub struct HubEngine {
    inner: Arc<Inner>,
}
impl HubEngine {
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        Self::open_with_remote(data_dir, false)
    }
    pub fn open_with_remote(data_dir: &Path, remote: bool) -> Result<Self, String> {
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
            for(table,column,ty)in [("hub_signals","signal_id","TEXT"),("hub_signals","claimed_by_channel","TEXT"),("hub_signals","claim_until_ms","INTEGER"),("hub_signals","acked_at_ms","INTEGER"),("hub_pairing_codes","instruction","TEXT"),("hub_pairing_codes","pairing_command","TEXT"),("hub_pairing_codes","pairing_url","TEXT"),("hub_pairing_codes","bot_username","TEXT"),("hub_channel_deliveries","reservation_id","TEXT"),("hub_channel_deliveries","attempts","INTEGER NOT NULL DEFAULT 0"),("hub_channel_deliveries","next_retry_ms","INTEGER NOT NULL DEFAULT 0"),("hub_channel_deliveries","last_error","TEXT")]{let columns=c.query(&format!("PRAGMA table_info({table})"),&[])?;if !columns.iter().any(|v|s(v,"name")==column){c.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"),&[])?;}}
            for sql in ["CREATE INDEX IF NOT EXISTS idx_hub_signals_subject_seq ON hub_signals(subject,seq)","CREATE INDEX IF NOT EXISTS idx_hub_signals_signal_id ON hub_signals(signal_id)","CREATE INDEX IF NOT EXISTS idx_hub_channel_deliveries_ext_msg ON hub_channel_deliveries(plugin_id,principal_id,external_msg_id)","CREATE INDEX IF NOT EXISTS idx_hub_channel_deliveries_recent ON hub_channel_deliveries(plugin_id,principal_id,delivered_ms DESC)"]{c.execute(sql,&[])?;}
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
                plugins: Mutex::new(Vec::new()),
                processes: Mutex::new(HashMap::new()),
                worker: Mutex::new(None),
                lifecycle: Mutex::new(()),
                remote: AtomicBool::new(remote),
            }),
        };
        engine.set_plugin_roots(&hub_plugins::default_roots(data_dir))?;
        engine.restore_attachments()?;
        let weak = Arc::downgrade(&engine.inner);
        let worker = thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(250));
            let Some(inner) = weak.upgrade() else { break };
            if inner.closed.load(Ordering::SeqCst) {
                break;
            }
            let e = HubEngine { inner };
            if let Err(err) = e.tick() {
                eprintln!("cito hub: {err}");
            }
        });
        *engine
            .inner
            .worker
            .lock()
            .map_err(|_| "Hub worker lock poisoned")? = Some(worker);
        Ok(engine)
    }
    pub fn set_plugin_roots(&self, roots: &[PathBuf]) -> Result<(), String> {
        let plugins = hub_plugins::discover(roots, Some(&self.inner.user_plugins_dir))?;
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
        let identity = Arc::new(());
        self.inner
            .subscribers
            .lock()
            .map_err(|_| "Subscriber lock poisoned")?
            .push(Subscriber {
                principal,
                tx,
                identity: identity.clone(),
            });
        Ok(Subscription {
            receiver,
            owner: Arc::downgrade(&self.inner),
            identity,
        })
    }
    pub fn shutdown(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(mut subs) = self.inner.subscribers.lock() {
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
    fn emit(&self, frame: Value) {
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
                    Ok(()) => true,
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
            self.notify_ack(params)?;
        }
        if let Some(request) = ack_request {
            self.notify_ack(&request)?;
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
        let result = match op {
            "human_device_attach"
            | "human_device_heartbeat"
            | "human_device_detach"
            | "human_inbox_update" => human_operation(c, op, p, caller, events),
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
                let rows = c.query(&format!("{KV_SELECT} ORDER BY revision"), &[])?;
                let rows: Vec<_> = rows
                    .into_iter()
                    .filter(|r| visible(r, caller) && matches(s(r, "key"), s(p, "key")))
                    .collect();
                if op == "kv_keys" {
                    Ok(json!({"keys":rows.iter().map(|r|r["key"].clone()).collect::<Vec<_>>()}))
                } else {
                    let rows = rows
                        .into_iter()
                        .map(|row| reconcile_human_row(c, row))
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(
                        json!({"entries":rows.iter().map(kv_wire).collect::<Result<Vec<_>,_>>()?,"server_now_ms":now_ms()}),
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
                let raw = if op == "kv_delete" {
                    Vec::new()
                } else {
                    decode_payload(p)?
                };
                let data = payload_metadata(&raw);
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
                    json!({"name":name,"stream_name":stream,"config":cfg,"backlog":backlog["backlog"]}),
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
                consumer(c, name, caller)?;
                if op == "consumer_delete" {
                    c.execute("DELETE FROM hub_consumers WHERE name=?", &[json!(name)])?;
                    c.execute("DELETE FROM hub_consumer_acks WHERE name=?", &[json!(name)])?;
                    Ok(json!({"deleted":true}))
                } else {
                    c.execute(
                        "INSERT OR IGNORE INTO hub_consumer_acks(name,seq) VALUES(?,?)",
                        &[json!(name), p["seq"].clone()],
                    )?;
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
                save_service(c, &plugin.id, &cfg)?;
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
                    self.inner.remote.load(Ordering::SeqCst),
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
        let process = Arc::new(ChannelProcess::spawn(plugin, callback)?);
        processes.insert(plugin.id.clone(), process.clone());
        drop(processes);
        self.configure_process(&plugin.id, &process)?;
        Ok(process)
    }
    fn configure_process(&self, id: &str, process: &ChannelProcess) -> Result<(), String> {
        let frame=self.inner.db.transaction(|c|{let(cfg,state)=service(c,id)?;let bindings=c.query("SELECT principal_id,owner_user,user_config_json FROM hub_human_channels WHERE plugin_id=? AND enabled=1",&[json!(id)])?.iter().map(|b|json!({"principal_id":b["principal_id"],"user":b["owner_user"],"config":parse(b,"user_config_json")})).collect::<Vec<_>>();Ok(json!({"op":"configure","hub_config":cfg,"bindings":bindings,"cursor":state["cursor"]}))})?;
        process.send(&frame)
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
                if changed { save_service(c, id, &cfg)?; }
                if reset_hub {
                    c.execute("UPDATE hub_plugin_services SET service_state_json='{}' WHERE plugin_id=?", &[json!(id)])?;
                    c.execute("UPDATE hub_pairing_codes SET consumed_ms=? WHERE plugin_id=? AND consumed_ms IS NULL", &[json!(now_ms()), json!(id)])?;
                }
                if reset_user || p["cancel_pair"] == true {
                    cancel_pair(c, id, caller)?;
                }
                if reset_hub {
                    for b in c.query("SELECT * FROM hub_human_channels WHERE plugin_id=?", &[json!(id)])? {
                        attach(c, &b, false, self.inner.remote.load(Ordering::SeqCst), Some(&plugin))?;
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
                    let boundary = if enabled && old.as_ref().is_none_or(|b| n(b,"enabled")==0 && !identity_changed) { max_seq(c)? } else { old.as_ref().map(|b| n(b,"last_delivered_seq")).unwrap_or(0) };
                    c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,enabled,user_config_json,display_label,last_delivered_seq,updated_ms) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(principal_id,plugin_id) DO UPDATE SET owner_user=excluded.owner_user,enabled=excluded.enabled,user_config_json=excluded.user_config_json,display_label=excluded.display_label,last_delivered_seq=excluded.last_delivered_seq,updated_ms=excluded.updated_ms",
                        &[caller["principal_id"].clone(),json!(id),caller["owner_user"].clone(),json!(if enabled{1}else{0}),json!(config.to_string()),label,json!(boundary),json!(now_ms())])?;
                }
                if changed || p.get("user_config").is_some() {
                    c.execute("UPDATE hub_channel_deliveries SET status='failed',attempts=0,next_retry_ms=0,last_error=NULL WHERE plugin_id=? AND status IN ('failed','failed_permanent') AND (? OR principal_id=?)", &[json!(id),json!(changed),caller["principal_id"].clone()])?;
                }
                // Reconcile every binding when service readiness changes, including other principals.
                for b in c.query("SELECT * FROM hub_human_channels WHERE plugin_id=?", &[json!(id)])? {
                    let active = n(&b,"enabled")==1 && ready(&plugin,&cfg,"hub") && ready(&plugin,&parse(&b,"user_config_json"),"user");
                    attach(c,&b,active,self.inner.remote.load(Ordering::SeqCst), Some(&plugin))?;
                }
                audit(c,s(caller,"principal_id"),"channel_configure",id,&json!({"enabled":p["enabled"]}))?;
                Ok(())
            })?;
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
            self.tick_channels()?;
            return self.channel_list(caller);
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
            self.inner.db.transaction(|c| save_service(c, id, &cfg))?;
        }
        let process = self.ensure_process(&plugin)?;
        self.configure_process(id, &process)?;
        let code = pair_code()?;
        let expires = now_ms() + 600_000;
        let result = process.request(json!({"op":"format_pair","code":code,"hub_config":cfg}))?;
        if result["ok"] != true {
            return Err(format!("PLUGIN_EXEC_FAILED: {}", s(&result, "error")));
        }
        let instruction = required(&result, "instruction")?;
        self.inner.db.transaction(|c| {
            cancel_pair(c,id,caller)?;
            c.execute("INSERT INTO hub_pairing_codes(code,plugin_id,principal_id,owner_user,created_ms,expires_ms,instruction,pairing_command,pairing_url,bot_username) VALUES(?,?,?,?,?,?,?,?,?,?)",
                &[json!(code),json!(id),caller["principal_id"].clone(),caller["owner_user"].clone(),json!(now_ms()),json!(expires),json!(instruction),result["pairing_command"].clone(),result["pairing_url"].clone(),result["bot_username"].clone()])?;
            Ok(())
        })?;
        Ok(
            json!({"code":code,"instruction":instruction,"pairing_command":result["pairing_command"],"pairing_url":result["pairing_url"],"bot_username":result["bot_username"],"expires_at_ms":expires}),
        )
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
            let inbox_paused = find_human(c,s(caller,"principal_id"))?.and_then(|id| kv_row(c,&id).ok().flatten()).is_some_and(|row| parse(&row,"value")["human_paused"] == true);
            let mut channels=Vec::new();
            for plugin in plugins {
                let (cfg,health) = service(c,&plugin.id)?;
                let binding=one(c,"SELECT * FROM hub_human_channels WHERE principal_id=? AND plugin_id=?", &[caller["principal_id"].clone(),json!(plugin.id)])?.unwrap_or(Value::Null);
                let user=parse(&binding,"user_config_json");
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
                    "ready"=>label.map(|l|format!("Connected as {l} · Ready for notifications")).unwrap_or("Connected and ready for notifications".into()),
                    "paused"=>label.map(|l|format!("Paused ({l}) · Enable to resume notifications")).unwrap_or("Paused · Enable to resume notifications".into()),
                    "pairing_pending"=>"Pairing in progress · Send the pairing command below to finish linking".into(),
                    "hub_ready"=>"Service ready · Pair your account or enter your destination below".into(),
                    _=>"Configure service credentials and pair your account".into(),
                };
                let mut warning = false;
                if let Some(failed) = one(c,"SELECT status,last_error FROM hub_channel_deliveries WHERE plugin_id=? AND principal_id=? AND (delivered_ms>=? OR status='effect_unknown') ORDER BY (status='effect_unknown') DESC,delivered_ms DESC,rowid DESC LIMIT 1",&[json!(plugin.id),caller["principal_id"].clone(),json!(n(&binding,"updated_ms"))])? {
                    if matches!(s(&failed,"status"),"failed"|"failed_permanent"|"effect_unknown") && !s(&failed,"last_error").is_empty() { warning = true; detail = if failed["status"] == "effect_unknown" { format!("Delivery outcome unknown; automatic replay stopped to avoid duplicates. {}", s(&failed,"last_error")) } else { s(&failed,"last_error").into() };  }
                }
                if let Some(error) = health["poll_error"].as_str() { warning = true; detail = error.into(); }
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
        let mut first_error = None;
        for plugin in plugins {
            let should_run=self.inner.db.transaction(|c|{let(cfg,_)=service(c,&plugin.id)?;let bound=one(c,"SELECT 1 AS n FROM hub_human_channels WHERE plugin_id=? AND enabled=1 LIMIT 1",&[json!(plugin.id)])?.is_some();let pairing=one(c,"SELECT 1 AS n FROM hub_pairing_codes WHERE plugin_id=? AND consumed_ms IS NULL AND expires_ms>? LIMIT 1",&[json!(plugin.id),json!(now_ms())])?.is_some();Ok(ready(&plugin,&cfg,"hub")&&(cfg.as_object().is_some_and(|m|m.values().any(configured))||bound||pairing))})?;
            if !should_run {
                let process = self
                    .inner
                    .processes
                    .lock()
                    .map_err(|_| "Process lock poisoned")?
                    .remove(&plugin.id);
                if let Some(p) = process {
                    p.shutdown()
                }
                continue;
            }
            if let Err(error) = self
                .ensure_process(&plugin)
                .and_then(|process| self.replay(&plugin, &process))
            {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
    fn replay(&self, plugin: &ChannelPlugin, process: &ChannelProcess) -> Result<(), String> {
        // A live process owns its entire queue, including multipart network delivery.
        // Renew queued reservations together; process replacement releases them above.
        if process.alive() {
            self.inner.db.execute(
                "UPDATE hub_channel_deliveries SET delivered_ms=? WHERE plugin_id=? AND status='in_flight'",
                &[json!(now_ms()), json!(plugin.id)],
            )?;
        }
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
        self.inner.db.transaction(|c|{prune(c)?;let bindings=c.query("SELECT * FROM hub_human_channels WHERE plugin_id=? AND enabled=1",&[json!(plugin.id)])?;let (service_cfg, _) = service(c, &plugin.id)?;let mut sends=Vec::new();for b in bindings{if !ready(plugin, &service_cfg, "hub") || !ready(plugin, &parse(&b, "user_config_json"), "user") { continue; }let node=find_human(c,s(&b,"principal_id"))?.unwrap_or_else(||listener_id(s(&b,"principal_id"), "human", "default"));if let Some(reg) = kv_row(c,&node)?.map(|r|parse(&r,"value")) { if reg["human_paused"] == true || reg["state"] == "UNLISTENED" { continue; } }let rows=c.query(&format!("{SIGNAL_SELECT} WHERE subject=? AND seq>? AND acked_at_ms IS NULL AND NOT EXISTS(SELECT 1 FROM hub_channel_deliveries d WHERE d.plugin_id=? AND d.principal_id=? AND d.signal_id=hub_signals.signal_id AND (d.status IN ('delivered','failed_permanent','effect_unknown') OR d.status='in_flight' AND d.delivered_ms>? OR d.status='failed' AND d.next_retry_ms>?)) ORDER BY seq LIMIT 200"),&[json!(format!("cito.signal.{node}")),b["last_delivered_seq"].clone(),json!(plugin.id),b["principal_id"].clone(),json!(now_ms()-60_000),json!(now_ms())])?;for row in rows{let id=s(&row,"signal_id");if id.is_empty(){continue}let data=parse(&row,"data");let token=random_id("delivery_")?;c.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,from_listener_id,from_user,from_title,delivered_ms,reservation_id) VALUES(?,?,?,?,'in_flight',?,?,?,?,?) ON CONFLICT(plugin_id,principal_id,signal_id) DO UPDATE SET status='in_flight',delivered_ms=excluded.delivered_ms,reservation_id=excluded.reservation_id,from_listener_id=excluded.from_listener_id",&[json!(plugin.id),b["principal_id"].clone(),json!(id),row["seq"].clone(),data["from"]["listener_id"].clone(),data["from"]["user"].clone(),data["from"]["title"].clone(),json!(now_ms()),json!(token)])?;let already_sent_ids=c.query("SELECT external_msg_id FROM hub_channel_delivery_msgs WHERE plugin_id=? AND principal_id=? AND signal_id=? ORDER BY rowid",&[json!(plugin.id),b["principal_id"].clone(),json!(id)])?.into_iter().map(|r|r["external_msg_id"].clone()).collect::<Vec<_>>();let mut payload=data.clone();payload["created_at_ms"]=row["created_ms"].clone();payload["to"]=json!({"listener_id":node});sends.push(json!({"op":"deliver","already_sent_ids":already_sent_ids,"delivery_id":token,"principal_id":b["principal_id"],"user":b["owner_user"],"config":parse(&b,"user_config_json"),"signal":payload}));}}Ok(sends)})
    }
    fn plugin_frame(&self, id: &str, frame: &Value) -> Option<Value> {
        let result=match s(frame,"op"){
        "cursor_save"=>self.inner.db.transaction(|c|{let(_,mut state)=service(c,id)?;state["cursor"]=frame["cursor"].clone();c.execute("UPDATE hub_plugin_services SET service_state_json=?,updated_ms=? WHERE plugin_id=?",&[json!(state.to_string()),json!(now_ms()),json!(id)])?;Ok(Value::Null)}),
        "health"=>self.inner.db.transaction(|c|{let(_,mut state)=service(c,id)?;state["poll_error"]=frame["error"].clone();c.execute("UPDATE hub_plugin_services SET service_state_json=? WHERE plugin_id=?",&[json!(state.to_string()),json!(id)])?;Ok(Value::Null)}),
        "delivered"=>self.delivery_result(id,frame),
        "pair_redeem"=>self.redeem(id,frame),
        "inbound_reply"=>self.inbound_reply(id,frame),
        _=>Ok(Value::Null),
    };
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
            if f["progress"] == true { return Ok(Value::Null); }
            let attempts = n(&row, "attempts").max(0);
            let (status, retry_at) = delivery_transition(f, attempts, now_ms());
            c.execute("UPDATE hub_channel_deliveries SET status=?,external_msg_id=?,delivered_ms=?,attempts=attempts+1,next_retry_ms=?,last_error=? WHERE plugin_id=? AND reservation_id=?",
                &[json!(status),f["external_msg_id"].clone(),json!(now_ms()),json!(retry_at),f["error"].clone(),json!(id),json!(token)])?;
            Ok(Value::Null)
        })
    }
    fn redeem(&self, id: &str, f: &Value) -> Result<Value, String> {
        let plugin = self.plugin(id)?;
        let result = self.inner.db.transaction(|c| {
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
            let boundary = if let Some(old) = old.as_ref() { n(old,"last_delivered_seq") } else { max_seq(c)? };
            c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,enabled,user_config_json,display_label,last_delivered_seq,updated_ms) VALUES(?,?,?,1,?,?,?,?) ON CONFLICT(principal_id,plugin_id) DO UPDATE SET enabled=1,user_config_json=excluded.user_config_json,display_label=excluded.display_label,last_delivered_seq=excluded.last_delivered_seq,updated_ms=excluded.updated_ms",
                &[row["principal_id"].clone(), json!(id), row["owner_user"].clone(), json!(config.to_string()), f["display_label"].clone(), json!(boundary), json!(now_ms())])?;
            c.execute("UPDATE hub_channel_deliveries SET status='failed',attempts=0,next_retry_ms=0,last_error=NULL WHERE plugin_id=? AND principal_id=? AND status IN ('failed','failed_permanent')", &[json!(id),row["principal_id"].clone()])?;
            c.execute("UPDATE hub_pairing_codes SET consumed_ms=? WHERE code=?", &[json!(now_ms()), json!(code)])?;
            let binding = one(c, "SELECT * FROM hub_human_channels WHERE principal_id=? AND plugin_id=?",
                &[row["principal_id"].clone(), json!(id)])?.ok_or("Binding missing")?;
            attach(c, &binding, active, self.inner.remote.load(Ordering::SeqCst), Some(&plugin))?;
            Ok(json!({"op":"pair_ack","code":code,"ok":true,"owner_user":row["owner_user"],"error":null}))
        })?;
        // The callback runs on the child's reader thread; configure is a one-way write.
        let process = self
            .inner
            .processes
            .lock()
            .ok()
            .and_then(|p| p.get(id).cloned());
        if let Some(p) = process {
            self.configure_process(id, &p)?;
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
        let update=required(f,"update_id")?;if one(c,"SELECT 1 AS n FROM hub_processed_replies WHERE plugin_id=? AND update_id=?",&[json!(id),json!(update)])?.is_some(){return Ok(json!({"op":"reply_ack","update_id":update,"ok":true,"duplicate":true,"error":null}))}
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
    let human = find_human(c, pid)?.unwrap_or_else(|| listener_id(pid, "human", "default"));
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
        .map(|key| hash(&format!("{pid}|{host_id}|human|{target_id}|{key}")));
    let payload = json!({"signal_id":sig,"to":target_id,"target_generation":registration["binding_generation"],"sender_endpoint_key":format!("{pid}|{host_id}|human|default"),"from":{"principal_id":pid,"user":owner,"host_id":host_id,"host_name":host_name,"harness":"human","listener_id":super::identity::public_listener_id(owner, &human),"title":format!("@{owner} (Me)"),"about":"Human channel"},"message":text,"in_reply_to":in_reply_to});
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
    prune(c)?;
    if let Some(mid) = msg_id {
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
    prune(c)?;
    let rows=c.query(&format!("{SIGNAL_SELECT} WHERE seq>=? AND seq NOT IN(SELECT seq FROM hub_consumer_acks WHERE name=?) ORDER BY seq"),&[json!(min_seq),row["name"].clone()])?;
    let mut items = Vec::new();
    let mut page_bytes = 0;
    for item in rows {
        if matches(s(&item, "subject"), s(row, "filter_subject"))
            && can_consume(c, subject_target(s(&item, "subject")).unwrap_or(""), p)?
        {
            let wire = signal_wire(&item)?;
            let size = wire.to_string().len() + 1;
            // Apply the caller's sequence floor before byte pagination so unread pages
            // cannot hide later items from sequential consumers.
            if !items.is_empty() && page_bytes + size > 4 * 1024 * 1024 {
                break;
            }
            page_bytes += size;
            items.push(wire);
        }
    }
    Ok(json!({"filterSubject":row["filter_subject"],"startSeq":row["start_seq"],"backlog":items}))
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
fn save_service(c: &Connection, id: &str, cfg: &Value) -> Result<bool, String> {
    let (old, _) = service(c, id)?;
    let changed = old != *cfg;
    c.execute("INSERT INTO hub_plugin_services(plugin_id,config_json,service_state_json,updated_ms) VALUES(?,?,'{}',?) ON CONFLICT(plugin_id) DO UPDATE SET config_json=excluded.config_json,service_state_json=CASE WHEN ? THEN '{}' ELSE service_state_json END,updated_ms=excluded.updated_ms", &[json!(id),json!(cfg.to_string()),json!(now_ms()),json!(changed)])?;
    if changed {
        c.execute(
            "DELETE FROM hub_processed_replies WHERE plugin_id=?",
            &[json!(id)],
        )?;
        // Credential edits retain confirmed chunk IDs and terminal unknown effects.
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
            v["_external_chat_id"].clone(),
            v["_external_user_id"].clone(),
        )
    };
    let next = identity(config);
    if next.0.is_string() && next.1.is_string() {
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
            || next.0.is_null()
                && next.1.is_null()
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
            now + (2_000_i64 << attempts.clamp(0, 5)).min(60_000),
        )
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

/// Each operation owns one explicit write set under the Hub database transaction.
fn human_operation(
    c: &Connection,
    op: &str,
    p: &Value,
    caller: Option<&Value>,
    events: &mut Vec<Value>,
) -> Result<Value, String> {
    let caller = caller.ok_or("FORBIDDEN: Authenticated principal required")?;
    let pid = required(caller, "principal_id")?;
    let id = find_human(c, pid)?.unwrap_or_else(|| listener_id(pid, "human", "default"));
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
        json!({"listener_id":id,"public_id":super::identity::public_listener_id(s(caller,"owner_user"),&id),"principal_id":pid,"owner_user":caller["owner_user"],"harness":"human","session_id":"default","title":"Human","about":"Human","scope":if oidc(Some(caller)) || caller["auth_mode"]=="token" {"self"}else{"workspace"},"human_paused":false,"human_control_revision":1,"binding_generation":1,"channels":{}})
    };
    if !reg.is_object() {
        return Err("STATE_ERROR: Human record must be an object".into());
    }
    if reg["human_control_revision"].is_null() {
        reg["human_control_revision"] = json!(1);
    }
    let now = now_ms();
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
            if !existing.is_null()
                && !same
                && generation <= existing["binding_generation"].as_u64().unwrap_or(0)
            {
                return Err("STALE_GENERATION: Human device attachment superseded".into());
            }
            reg["channels"][channel] = json!({"channel_id":channel,"host_id":host,"host_name":p["host_name"],"kind":"human","delivery_mode":"notify_fanout","priority":100,"state":"ACTIVE","updated_at_ms":now,"binding_generation":generation,"owner_token":token});
        } else {
            if !same {
                return Err("STALE_GENERATION: Human device attachment superseded".into());
            }
            if op == "human_device_heartbeat" {
                reg["channels"][channel]["updated_at_ms"] = json!(now);
            } else {
                reg["channels"][channel]["state"] = json!("UNLISTENED");
            }
        }
    }
    events.push(save_human(c, &id, &mut reg)?);
    audit(
        c,
        pid,
        op,
        &id,
        &json!({"human_control_revision":reg["human_control_revision"]}),
    )?;
    Ok(json!({"record":reg,"server_now_ms":now}))
}

fn find_human(c: &Connection, pid: &str) -> Result<Option<String>, String> {
    for row in c.query(&format!("{KV_SELECT} ORDER BY revision DESC"), &[])? {
        if !principal_ids_equal(s(&row, "principal_id"), pid) {
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
    remote: bool,
    plugin: Option<&ChannelPlugin>,
) -> Result<(), String> {
    let pid = required(b, "principal_id")?;
    let id = find_human(c, pid)?.unwrap_or_else(|| listener_id(pid, "human", "default"));
    let old = kv_row(c, &id)?;
    let mut reg=old.as_ref().map(|r|parse(r,"value")).unwrap_or_else(||json!({"listener_id":id,"public_id":super::identity::public_listener_id(s(b,"owner_user"), &id),"principal_id":pid,"owner_user":b["owner_user"],"host_id":"hub","host_name":"Hub","harness":"human","session_id":"default","cwd":"","repo":null,"branch":null,"title":format!("@{} (Me)",s(b,"owner_user")),"about":"Human","human_paused":false,"human_control_revision":1,"scope":if remote{"self"}else{"workspace"},"state":"ACTIVE","binding_generation":1,"owner_token":"hub","channels":{}}));
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

/// Stopping the listener never closes its shared engine. The daemon closes the engine last.
pub struct HubServer {
    pub address: SocketAddr,
    pub(super) auth: Arc<HubAuth>,
    stop: Arc<AtomicBool>,
    sockets: Arc<Mutex<HashMap<u64, TcpStream>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
impl HubServer {
    pub fn shutdown(&self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(sockets) = self.sockets.lock() {
            for socket in sockets.values() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
        if let Ok(mut thread) = self.thread.lock() {
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
    }
    pub fn port(&self) -> u16 {
        self.address.port()
    }
}
impl Drop for HubServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}
impl HubEngine {
    pub fn start_server(
        &self,
        bind: &str,
        port: u16,
        auth: impl Into<Arc<HubAuth>>,
    ) -> Result<HubServer, String> {
        let listener = TcpListener::bind((bind, port)).map_err(|e| e.to_string())?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let sockets = Arc::new(Mutex::new(HashMap::<u64, TcpStream>::new()));
        let auth = auth.into();
        let (engine, st, ss, worker_auth) =
            (self.clone(), stop.clone(), sockets.clone(), auth.clone());
        let handle = thread::spawn(move || {
            let mut workers: Vec<JoinHandle<()>> = Vec::new();
            let mut next_socket = 0u64;
            while !st.load(Ordering::SeqCst) {
                for i in (0..workers.len()).rev() {
                    if workers[i].is_finished() {
                        let _ = workers.swap_remove(i).join();
                    }
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        if st.load(Ordering::SeqCst) {
                            let _ = stream.shutdown(Shutdown::Both);
                            break;
                        }
                        if workers.len() >= 256 {
                            let mut stream = stream;
                            let _ = http_json(
                                &mut stream,
                                503,
                                &json!({"ok":false,"error":{"code":"UNAVAILABLE","message":"Too many connections"}}),
                            );
                            continue;
                        }
                        next_socket += 1;
                        let socket_id = next_socket;
                        if let Ok(clone) = stream.try_clone() {
                            if let Ok(mut list) = ss.lock() {
                                list.insert(socket_id, clone);
                            }
                        }
                        let (e, a, s, connections) =
                            (engine.clone(), worker_auth.clone(), st.clone(), ss.clone());
                        workers.push(thread::spawn(move || {
                            handle_http(stream, e, a, s);
                            if let Ok(mut list) = connections.lock() {
                                list.remove(&socket_id);
                            }
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) => {
                        eprintln!("cito hub: accept: {e}");
                        break;
                    }
                }
            }
            if let Ok(sockets) = ss.lock() {
                for socket in sockets.values() {
                    let _ = socket.shutdown(Shutdown::Both);
                }
            }
            for worker in workers {
                let _ = worker.join();
            }
        });
        Ok(HubServer {
            address,
            auth,
            stop,
            sockets,
            thread: Mutex::new(Some(handle)),
        })
    }
}
fn http_json(stream: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let bytes = body.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        _ => "Service Unavailable",
    };
    write!(stream,"HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",bytes.len(),bytes)?;
    stream.flush()
}
fn http_error(stream: &mut TcpStream, err: &str) {
    let code = err.split(':').next().unwrap_or("INTERNAL_ERROR");
    let status = match code {
        "FORBIDDEN" | "PERMISSIONS_VIOLATION" | "FORBIDDEN_TARGET_SCOPE" => 403,
        "AUTHORIZATION_VIOLATION" => 401,
        "10037" | "10059" | "NOT_FOUND" | "LISTENER_NOT_FOUND" => 404,
        "10071" => 409,
        "AUTH_UNAVAILABLE" | "UNAVAILABLE" => 503,
        _ => 400,
    };
    let mut error = json!({"code":code,"message":err});
    if let Ok(number) = code.parse::<u64>() {
        error["api_error"] = json!({"err_code":number});
    }
    let _ = http_json(stream, status, &json!({"ok":false,"error":error}));
}
fn handle_http(
    mut stream: TcpStream,
    engine: HubEngine,
    auth: Arc<HubAuth>,
    stop: Arc<AtomicBool>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let result = (|| -> Result<(), String> {
        // Darwin accept() inherits the listener's O_NONBLOCK. Client workers use
        // blocking reads with timeouts, including between fragmented headers.
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let mut line = String::new();
        reader
            .by_ref()
            .take(8193)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if line.len() > 8192 {
            return Err("INVALID_INPUT: request line too long".into());
        }
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() != 3 {
            return Err("INVALID_INPUT: malformed HTTP request".into());
        }
        let method = parts[0].to_owned();
        let path = parts[1].split('?').next().unwrap_or("/").to_owned();
        let mut headers = HashMap::new();
        let mut size = 0;
        loop {
            let mut line = String::new();
            let bytes = reader
                .by_ref()
                .take(65537)
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            size += bytes;
            if bytes == 0 || size > 65536 {
                return Err("INVALID_INPUT: truncated or oversized HTTP headers".into());
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            let (k, v) = line
                .split_once(':')
                .ok_or("INVALID_INPUT: malformed HTTP header")?;
            let key = k.trim().to_ascii_lowercase();
            if headers.insert(key.clone(), v.trim().to_owned()).is_some() && key == "content-length"
            {
                return Err("INVALID_INPUT: duplicate content-length".into());
            }
        }
        if path == "/" && method == "GET" {
            let body = "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Cito Hub</title><style>body{background:#12151c;color:#f3f4f6;font:18px system-ui;max-width:42rem;margin:12vh auto;padding:2rem}h1{color:#7dd3fc}code{color:#7dd3fc}</style><body><h1>Cito Hub</h1><p>This Hub is running. On another Mac, open Cito → Settings… → Hubs &amp; Network → Join from Clipboard to connect using a copied invite.</p><p>Your shared inbox connects all attached Macs and channels. Incoming messages appear in the menu bar unread badge and Activity, where you can reply in one line.</p></body></html>";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        if path == "/health" && method == "GET" {
            http_json(&mut stream,200,&json!({"ok":true,"pid":std::process::id(),"version":env!("CARGO_PKG_VERSION"),"auth_mode":auth.mode})).map_err(|e|e.to_string())?;
            return Ok(());
        }
        let principal = auth.authenticate(&headers)?;
        if path == "/v1/events" && method == "GET" {
            let events = engine.subscribe(Some(principal))?;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache, no-transform\r\nConnection: keep-alive\r\n\r\n:ok\n\n").map_err(|e|e.to_string())?;
            while !stop.load(Ordering::SeqCst) && !engine.inner.closed.load(Ordering::SeqCst) {
                match events.recv_timeout(Duration::from_secs(1)) {
                    Ok(frame) => {
                        write!(stream, "data: {frame}\n\n").map_err(|e| e.to_string())?;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        stream
                            .write_all(b":keepalive\n\n")
                            .map_err(|e| e.to_string())?;
                    }
                    Err(_) => break,
                }
                stream.flush().map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        if path != "/v1/rpc" || method != "POST" {
            return Err("NOT_FOUND: Not found".into());
        }
        if headers.contains_key("transfer-encoding") {
            return Err("INVALID_INPUT: transfer-encoding is unsupported".into());
        }
        let length = headers
            .get("content-length")
            .map(|s| s.parse::<usize>())
            .transpose()
            .map_err(|_| "INVALID_INPUT: invalid content-length")?
            .unwrap_or(0);
        if length > 8 * 1024 * 1024 {
            return Err("INVALID_INPUT: RPC body exceeds 8 MiB".into());
        }
        let mut body = vec![0u8; length];
        reader
            .read_exact(&mut body)
            .map_err(|e| format!("INVALID_INPUT: incomplete body: {e}"))?;
        let body: Value =
            serde_json::from_slice(&body).map_err(|_| "INVALID_INPUT: Invalid JSON body")?;
        let op = required(&body, "op")?;
        let params = body.get("params").unwrap_or(&body);
        let result = engine.rpc(op, params, Some(&principal))?;
        http_json(&mut stream, 200, &json!({"ok":true,"result":result}))
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(err) = result {
        http_error(&mut stream, &err);
    }
    let _ = stream.shutdown(Shutdown::Both);
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
        "--data-dir" => "CITO_HUB_DATA_DIR",
        _ => flag,
    }
}
fn cli_options(args: &[String]) -> Result<HashMap<String, String>, String> {
    let mut opts = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (key, inline) = arg
            .split_once('=')
            .map(|(k, v)| (k, Some(v)))
            .unwrap_or((arg.as_str(), None));
        if ![
            "--port",
            "--bind",
            "--auth",
            "--token",
            "--issuer",
            "--client-id",
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
        let value = if let Some(v) = inline {
            v.to_owned()
        } else {
            i += 1;
            args.get(i)
                .filter(|s| !s.starts_with('-'))
                .cloned()
                .ok_or_else(|| format!("Missing value for {key} ({})", cli_setting(key)))?
        };
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
    let auth = HubAuth::new(
        &mode,
        value("--token", "CITO_HUB_TOKEN"),
        value("--issuer", "CITO_OIDC_ISSUER"),
        value("--client-id", "CITO_OIDC_CLIENT_ID"),
    )?;
    let raw = value("--data-dir", "CITO_HUB_DATA_DIR").unwrap_or_else(|| "~/.cito/hub".into());
    let home = env_value("HOME").unwrap_or_else(|| ".".into());
    let data_dir = if raw == "~" {
        PathBuf::from(home)
    } else if let Some(rest) = raw.strip_prefix("~/") {
        PathBuf::from(home).join(rest)
    } else {
        PathBuf::from(raw)
    };
    let engine = HubEngine::open_with_remote(&data_dir, true)?;
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
            libc::poll(&mut pfd, 1, 250);
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
