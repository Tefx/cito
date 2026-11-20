use super::{
    auth::principal_ids_equal,
    database::{Connection, Database},
    identity::{hash, normalize_slug, string},
};
use crate::plugins::now_ms;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

mod activity;

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;

/// Provider activity is source-owned. Effects and malformed revisions never renew leases.
pub(super) fn qualifying_activity(candidate: &Value) -> Option<(u64, u64)> {
    let activity = &candidate["activity"];
    if !matches!(activity["kind"].as_str(), Some("user_turn" | "unknown")) {
        return None;
    }
    Some((activity["revision"].as_u64()?, activity["at_ms"].as_u64()?))
}
pub(super) fn activity_revision(row: &Value, key: &str) -> Option<u64> {
    row[key]
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| row[key].as_u64())
}

#[derive(Clone)]
pub struct LocalState {
    pub db: Database,
    discovery_revisions: Arc<Mutex<HashMap<String, Arc<AtomicU64>>>>,
    registration_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}
impl LocalState {
    pub fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir.join("sockets")).map_err(|e| e.to_string())?;
        for path in [dir.to_path_buf(), dir.join("sockets")] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let db = Database::open(&dir.join("state.sqlite"))?;
        let current = db
            .query("PRAGMA user_version", &[])?
            .first()
            .and_then(|v| v["user_version"].as_i64())
            .unwrap_or(0);
        if current != 7 {
            db.transaction(|c| {
            let version=c.query("PRAGMA user_version",&[])?.first().and_then(|v|v["user_version"].as_i64()).unwrap_or(0);
            if version == 7 { return Ok(()); }
            if version>7 { return Err(format!("Unsupported state schema version {version}")); }
            if version < 4 {
                c.execute("DROP TABLE IF EXISTS local_pairwise_wakes; DROP TABLE IF EXISTS local_signals; DROP TABLE IF EXISTS local_listeners; DROP TABLE IF EXISTS local_sent_idempotency; DROP TABLE IF EXISTS pending_hub_acks; DROP TABLE IF EXISTS pending_hub_detaches; DROP TABLE IF EXISTS wake_retries; DROP TABLE IF EXISTS daemon_owner;", &[])?;
            }
            c.execute("CREATE TABLE IF NOT EXISTS local_listeners (
                hub TEXT NOT NULL DEFAULT 'default', listener_id TEXT NOT NULL, public_id TEXT NOT NULL DEFAULT '', tag TEXT,
                principal_id TEXT NOT NULL, owner_user TEXT NOT NULL, host_id TEXT NOT NULL, host_name TEXT NOT NULL,
                harness TEXT NOT NULL CHECK(length(harness)>0), session_id TEXT NOT NULL, endpoint_pid INTEGER,
                cwd TEXT NOT NULL, repo TEXT, branch TEXT, title TEXT NOT NULL, about TEXT NOT NULL,
                scope TEXT NOT NULL CHECK(scope IN ('self','workspace')),
                status TEXT NOT NULL CHECK(status IN ('ACTIVE','REBINDING','UNLISTENED','FENCED_INACTIVE')),
                binding_generation INTEGER NOT NULL DEFAULT 0, owner_token TEXT NOT NULL DEFAULT '',
                persisted_seq INTEGER NOT NULL DEFAULT 0, consecutive_probe_failures INTEGER NOT NULL DEFAULT 0,
                replay_gap_detected INTEGER NOT NULL DEFAULT 0, replay_gap_range TEXT, dispatcher_token TEXT,
                lease_until_ms INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(hub,listener_id));
                CREATE INDEX IF NOT EXISTS listeners_by_session ON local_listeners(harness,session_id);
                CREATE TABLE IF NOT EXISTS session_custom_titles(harness TEXT NOT NULL,session_id TEXT NOT NULL,title TEXT NOT NULL,PRIMARY KEY(harness,session_id));
                CREATE TABLE IF NOT EXISTS local_signals (
                hub TEXT NOT NULL DEFAULT 'default', signal_id TEXT NOT NULL, listener_id TEXT NOT NULL, seq INTEGER NOT NULL,
                binding_generation INTEGER NOT NULL, from_user TEXT NOT NULL, from_host_name TEXT NOT NULL, from_harness TEXT,
                from_listener_id TEXT, from_title TEXT NOT NULL, from_about TEXT, message TEXT NOT NULL, held_reason TEXT,
                in_reply_to TEXT, created_at TEXT NOT NULL, expires_at_ms INTEGER NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('RECEIVED','DISPATCHING','DISPATCHED','STORED_PASSIVE','EXPIRED_UNSENT')),
                batch_id TEXT, batch_expires_at_ms INTEGER, dispatcher_token TEXT, queue_written INTEGER NOT NULL DEFAULT 0,
                acknowledged_at_ms INTEGER, batch_card TEXT, PRIMARY KEY(hub,signal_id));
                CREATE INDEX IF NOT EXISTS signals_listener ON local_signals(hub,listener_id,state,seq);
                CREATE INDEX IF NOT EXISTS signals_batch ON local_signals(hub,batch_id);
                CREATE TABLE IF NOT EXISTS local_sent_idempotency (
                hub TEXT NOT NULL DEFAULT 'default', dedup_id TEXT NOT NULL, request_hash TEXT NOT NULL,
                signal_id TEXT NOT NULL, seq INTEGER NOT NULL DEFAULT 0, to_listener_id TEXT NOT NULL, status TEXT NOT NULL,
                held_reason TEXT, expires_at_iso TEXT NOT NULL, expires_at_ms INTEGER NOT NULL, PRIMARY KEY(hub,dedup_id));
                CREATE TABLE IF NOT EXISTS daemon_owner(singleton INTEGER PRIMARY KEY CHECK(singleton=1),pid INTEGER NOT NULL,port INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS pending_hub_acks(hub TEXT NOT NULL,node_id TEXT NOT NULL,signal_id TEXT NOT NULL,channel_id TEXT NOT NULL,acked_at_ms INTEGER NOT NULL,PRIMARY KEY(hub,node_id,signal_id));
                CREATE TABLE IF NOT EXISTS pending_hub_detaches(hub TEXT NOT NULL,listener_id TEXT NOT NULL,hub_url TEXT,PRIMARY KEY(hub,listener_id));
                CREATE TABLE IF NOT EXISTS wake_retries(hub TEXT NOT NULL,listener_id TEXT NOT NULL,retry_at_ms INTEGER NOT NULL,PRIMARY KEY(hub,listener_id));
                PRAGMA user_version=7;",&[])?;
            // Additive V4 extension: a successful insertion and an unknown insertion
            // both set queue_written, but only the former may become business-read.
            if !c.query("PRAGMA table_info(local_signals)", &[])?.iter().any(|r| r["name"] == "queue_confirmed") {
                c.execute("ALTER TABLE local_signals ADD COLUMN queue_confirmed INTEGER NOT NULL DEFAULT 0", &[])?;
            }
            // Additive V4 extension binds retained listeners to their original Hub URL.
            if !c.query("PRAGMA table_info(local_listeners)", &[])?.iter().any(|r| r["name"] == "hub_url") {
                c.execute("ALTER TABLE local_listeners ADD COLUMN hub_url TEXT", &[])?;
            }
            // Additive V4 metadata fences cached enrollment after local revocation.
            if !c.query("PRAGMA table_info(local_listeners)", &[])?.iter().any(|r| r["name"] == "updated_at") {
                c.execute("ALTER TABLE local_listeners ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0", &[])?;
            }
            if !c.query("PRAGMA table_info(local_signals)", &[])?.iter().any(|r| r["name"] == "activity_hidden") {
                c.execute("ALTER TABLE local_signals ADD COLUMN activity_hidden INTEGER NOT NULL DEFAULT 0", &[])?;
            }
            if !c.query("PRAGMA table_info(pending_hub_acks)", &[])?.iter().any(|r| r["name"] == "hub_url") {
                c.execute("ALTER TABLE pending_hub_acks ADD COLUMN hub_url TEXT; ALTER TABLE pending_hub_acks ADD COLUMN principal_id TEXT;", &[])?;
                // Legacy rows have no original Hub identity. Keep them unbound:
                // a current listener can already belong to a replacement profile.
            }
            c.execute("CREATE INDEX IF NOT EXISTS pending_hub_acks_binding ON pending_hub_acks(hub,hub_url,principal_id,acked_at_ms)",&[])?;
            if !c.query("PRAGMA table_info(local_listeners)", &[])?.iter().any(|r| r["name"] == "ack_scan_seq") {
                c.execute("ALTER TABLE local_listeners ADD COLUMN ack_scan_seq INTEGER NOT NULL DEFAULT 0", &[])?;
            }
            if !c.query("PRAGMA table_info(local_signals)", &[])?.iter().any(|r| r["name"] == "hub_url") {
                c.execute("ALTER TABLE local_signals ADD COLUMN hub_url TEXT; ALTER TABLE local_signals ADD COLUMN principal_id TEXT;", &[])?;
                // Existing signals have no trustworthy admission identity: a listener
                // may already belong to a replacement profile. Leave them unbound.
            }
            let columns = c.query("PRAGMA table_info(local_listeners)", &[])?;
            for (name, definition) in [
                ("listen_mode", "TEXT NOT NULL DEFAULT 'manual' CHECK(listen_mode IN ('manual','auto'))"),
                ("user_stopped", "INTEGER NOT NULL DEFAULT 0"),
                ("retirement_reason", "TEXT"),
                // TEXT preserves all u64 revisions, including values above SQLite's signed range.
                ("retired_activity_revision", "TEXT"),
                ("last_activity_at_ms", "INTEGER"),
                ("last_activity_revision", "TEXT"),
            ] {
                if !columns.iter().any(|r| r["name"] == name) {
                    c.execute(&format!("ALTER TABLE local_listeners ADD COLUMN {name} {definition}"), &[])?;
                }
            }
            if !c.query("PRAGMA table_info(local_signals)", &[])?.iter().any(|r| r["name"] == "delivered_at_ms") {
                c.execute("ALTER TABLE local_signals ADD COLUMN delivered_at_ms INTEGER", &[])?;
            }
            // Opening another handle must never steal a live dispatch lease. Frozen
            // rows recover after lease expiry, or the daemon's explicit startup hook.
            Ok(())
        })?;
        }
        Ok(Self {
            db,
            discovery_revisions: Arc::new(Mutex::new(HashMap::new())),
            registration_locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    /// Hub authority outlives client replacement, reconnect, and disable/enable.
    pub fn registration_lock(&self, hub: &str) -> Result<Arc<Mutex<()>>, String> {
        let mut locks = self
            .registration_locks
            .lock()
            .map_err(|_| "STATE_ERROR: Registration registry lock poisoned")?;
        Ok(locks.entry(hub.to_owned()).or_default().clone())
    }
    /// Retire only the local incarnation whose remote cleanup just completed.
    pub fn clear_pending_detach_if_current(
        &self,
        hub: &str,
        id: &str,
        generation: u64,
        owner: &str,
    ) -> Result<(), String> {
        self.db.execute(
            "DELETE FROM pending_hub_detaches WHERE hub=?1 AND listener_id=?2 AND EXISTS (SELECT 1 FROM local_listeners WHERE hub=?1 AND listener_id=?2 AND status='UNLISTENED' AND binding_generation=?3 AND COALESCE(owner_token,'')=?4)",
            &[json!(hub),json!(id),json!(generation),json!(owner)],
        )?;
        Ok(())
    }
    /// Share provider invalidation with receiver clones, without sharing worker ownership.
    pub(super) fn discovery_revision(&self, harness: &str) -> Result<Arc<AtomicU64>, String> {
        let mut revisions = self
            .discovery_revisions
            .lock()
            .map_err(|_| "Discovery revision lock poisoned")?;
        Ok(revisions.entry(harness.to_owned()).or_default().clone())
    }
    pub(super) fn invalidate_discovery(&self, harness: &str) -> Result<(), String> {
        let revisions = self
            .discovery_revisions
            .lock()
            .map_err(|_| "Discovery revision lock poisoned")?;
        if let Some(revision) = revisions.get(harness) {
            revision.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
    pub fn custom_title(&self, harness: &str, session: &str) -> Result<Option<String>, String> {
        Ok(self
            .db
            .query(
                "SELECT title FROM session_custom_titles WHERE harness=? AND session_id=?",
                &[json!(harness), json!(session)],
            )?
            .first()
            .and_then(|row| row["title"].as_str())
            .map(str::to_owned))
    }
    pub fn set_custom_title(
        &self,
        harness: &str,
        session: &str,
        title: &str,
    ) -> Result<(), String> {
        super::registration::validate_title(title)?;
        self.db.transaction(|c| {
            c.execute("INSERT INTO session_custom_titles(harness,session_id,title) VALUES(?,?,?) ON CONFLICT(harness,session_id) DO UPDATE SET title=excluded.title", &[json!(harness),json!(session),json!(title)])?;
            c.execute("UPDATE local_listeners SET title=? WHERE harness=? AND session_id=?", &[json!(title),json!(harness),json!(session)])?;
            Ok(())
        })
    }
    pub fn listener(&self, hub: &str, id: &str) -> Result<Option<Value>, String> {
        Ok(self
            .db
            .query(
                "SELECT * FROM local_listeners WHERE hub=? AND listener_id=?",
                &[json!(hub), json!(id)],
            )?
            .into_iter()
            .next())
    }
    pub fn active(&self) -> Result<Vec<Value>, String> {
        self.db
            .query("SELECT * FROM local_listeners WHERE status='ACTIVE'", &[])
    }
    /// Convert the Hub's remaining lifetime once; all local queues and badge
    /// expiry queries then use this local deadline without client clock skew.
    pub(super) fn signal_deadline(created: u64, hub_now: u64, local_now: u64) -> u64 {
        local_now.saturating_add(created.saturating_add(1_800_000).saturating_sub(hub_now))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Preserve the existing typed persistence transition API."
    )]
    pub fn admit(
        &self,
        hub: &str,
        listener: &Value,
        signal: &Value,
        seq: u64,
        created: &str,
        expires: u64,
        acked: &Value,
    ) -> Result<bool, String> {
        self.db.transaction(|c| {
            let current = current_listener(c, hub, string(listener, "listener_id"))?;
            check_listener(&current, listener)?;
            c.execute("UPDATE local_listeners SET persisted_seq=MAX(persisted_seq,?) WHERE hub=? AND listener_id=?", &[json!(seq), json!(hub), listener["listener_id"].clone()])?;
            let previous = c.query("SELECT * FROM local_signals WHERE hub=? AND signal_id=?", &[json!(hub), signal["signal_id"].clone()])?;
            if previous.is_empty() && current["scope"] != "workspace"
                && !principal_ids_equal(string(&signal["from"], "principal_id"), string(&current, "principal_id")) {
                // Privacy drops still advance the durable transport cursor.
                return Ok(false);
            }
            // Human device generations fence attachment ownership, not the shared inbox.
            let stale = current["harness"] != "human"
                && signal["target_generation"] != current["binding_generation"];
            let already_acked = acked.is_number();
            let changed = c.execute("INSERT OR IGNORE INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_harness,from_listener_id,from_title,from_about,message,held_reason,in_reply_to,created_at,expires_at_ms,state,acknowledged_at_ms,hub_url,principal_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", &[
                json!(hub), signal["signal_id"].clone(), listener["listener_id"].clone(), json!(seq), current["binding_generation"].clone(),
                signal["from"]["user"].clone(), signal["from"]["host_name"].clone(), signal["from"]["harness"].clone(), signal["from"]["listener_id"].clone(),
                json!(string(&signal["from"], "title")), signal["from"]["about"].clone(), signal["message"].clone(),
                if stale { json!("stale_generation") } else { Value::Null },
                signal["in_reply_to"].clone(), json!(created), json!(expires), json!(if stale || already_acked { "STORED_PASSIVE" } else { "RECEIVED" }),
                if already_acked { acked.clone() } else { Value::Null },
                current["hub_url"].clone(), current["principal_id"].clone()
            ])?;
            if changed > 0 && !stale && !already_acked && current["harness"] != "human" {
                c.execute("UPDATE local_listeners SET last_activity_at_ms=MAX(COALESCE(last_activity_at_ms,0),?) WHERE hub=? AND listener_id=?", &[json!(now_ms()),json!(hub),listener["listener_id"].clone()])?;
            }
            if already_acked {
                c.execute("UPDATE local_signals SET acknowledged_at_ms=COALESCE(acknowledged_at_ms,?),state=CASE WHEN state='RECEIVED' THEN 'STORED_PASSIVE' ELSE state END WHERE hub=? AND signal_id=?", &[acked.clone(), json!(hub), signal["signal_id"].clone()])?;
            }
            // Only wakeable duplicates recover scheduling. A redelivery must not
            // shorten a losing channel's 30.5-second passive fallback.
            if current["harness"] != "human" {
                c.execute("INSERT INTO wake_retries(hub,listener_id,retry_at_ms) SELECT ?,?,? WHERE EXISTS(SELECT 1 FROM local_signals WHERE hub=? AND signal_id=? AND state='RECEIVED' AND acknowledged_at_ms IS NULL AND held_reason IS NULL) ON CONFLICT(hub,listener_id) DO UPDATE SET retry_at_ms=MIN(retry_at_ms,excluded.retry_at_ms)", &[json!(hub), listener["listener_id"].clone(), json!(now_ms()+1500), json!(hub), signal["signal_id"].clone()])?;
            }
            reconcile_wake_retry(c, hub, string(listener, "listener_id"))?;
            Ok(changed > 0)
        })
    }
    /// Only the daemon holding the exclusive global lock may call this at startup.
    /// Ordinary opens leave active leases intact, including independent DB handles.
    pub fn recover_dispatch_leases(&self) -> Result<(), String> {
        self.db.execute(
            "UPDATE local_listeners SET dispatcher_token=NULL,lease_until_ms=0",
            &[],
        )?;
        Ok(())
    }
    #[cfg(test)]
    pub fn claim_wake_batch(&self, hub: &str, id: &str) -> Result<Vec<Value>, String> {
        let Some(listener) = self.listener(hub, id)? else {
            return Ok(Vec::new());
        };
        self.claim_wake_batch_for(hub, &listener, false)
    }
    pub fn claim_wake_batch_for(
        &self,
        hub: &str,
        captured: &Value,
        qualify_ids: bool,
    ) -> Result<Vec<Value>, String> {
        let id = string(captured, "listener_id");
        self.db.transaction(|c| {
            let listener = current_listener(c, hub, id)?;
            check_listener(&listener, captured)?;
            let now = now_ms();
            if !listener["dispatcher_token"].is_null() && listener["lease_until_ms"].as_u64().unwrap_or(0) > now {
                reconcile_wake_retry(c, hub, id)?;
                return Ok(Vec::new());
            }
            // Batch expiry is the minimum member expiry. Never partially deliver
            // a recovered batch merely because another member lives longer.
            c.execute("UPDATE local_signals SET state='EXPIRED_UNSENT',dispatcher_token=NULL WHERE hub=? AND listener_id=? AND state IN ('RECEIVED','DISPATCHING','STORED_PASSIVE') AND batch_id IS NOT NULL AND batch_expires_at_ms<=?", &[json!(hub), json!(id), json!(now)])?;
            c.execute("UPDATE local_signals SET state='EXPIRED_UNSENT',dispatcher_token=NULL WHERE hub=? AND listener_id=? AND state='RECEIVED' AND batch_id IS NULL AND expires_at_ms<=?", &[json!(hub), json!(id), json!(now)])?;
            c.execute("UPDATE local_signals SET state='STORED_PASSIVE',held_reason='stale_generation',dispatcher_token=NULL WHERE hub=? AND listener_id=? AND binding_generation<>? AND state IN ('RECEIVED','DISPATCHING')", &[json!(hub), json!(id), listener["binding_generation"].clone()])?;
            // Acknowledged members force an explicit re-batch of remaining unread
            // rows. Queue checkpoints belong to rows and survive this transition.
            c.execute("UPDATE local_signals SET state='STORED_PASSIVE',dispatcher_token=NULL WHERE hub=? AND listener_id=? AND state='DISPATCHING' AND acknowledged_at_ms IS NOT NULL", &[json!(hub), json!(id)])?;
            c.execute("UPDATE local_signals SET state='RECEIVED',batch_id=NULL,batch_card=NULL,batch_expires_at_ms=NULL,dispatcher_token=NULL WHERE hub=? AND listener_id=? AND state IN ('RECEIVED','DISPATCHING') AND acknowledged_at_ms IS NULL AND batch_id IN (SELECT batch_id FROM local_signals WHERE hub=? AND listener_id=? AND acknowledged_at_ms IS NOT NULL)", &[json!(hub), json!(id), json!(hub), json!(id)])?;
            let old = c.query("SELECT * FROM local_signals WHERE hub=? AND listener_id=? AND binding_generation=? AND state IN ('DISPATCHING','RECEIVED') AND batch_id IS NOT NULL AND acknowledged_at_ms IS NULL AND held_reason IS NULL ORDER BY seq LIMIT 1", &[json!(hub), json!(id), listener["binding_generation"].clone()])?.into_iter().next();
            let (rows, batch, expires, card) = if let Some(old) = old {
                let rows = c.query("SELECT * FROM local_signals WHERE hub=? AND listener_id=? AND batch_id=? AND acknowledged_at_ms IS NULL ORDER BY seq", &[json!(hub), json!(id), old["batch_id"].clone()])?;
                let card = old["batch_card"].as_str().map(str::to_owned).unwrap_or_else(|| format_wake_card(&listener, &rows, hub, qualify_ids));
                let expires = old["batch_expires_at_ms"].as_u64().unwrap_or_else(|| earliest_expiry(&rows));
                (rows, old["batch_id"].clone(), expires, card)
            } else {
                let mut rows = c.query("SELECT * FROM local_signals WHERE hub=? AND listener_id=? AND binding_generation=? AND state='RECEIVED' AND batch_id IS NULL AND acknowledged_at_ms IS NULL AND held_reason IS NULL AND expires_at_ms>? ORDER BY seq LIMIT 50", &[json!(hub), json!(id), listener["binding_generation"].clone(), json!(now)])?;
                // Plugins receive batch-wide queue flags, so re-batched survivors
                // must never share a request with a different insertion checkpoint.
                if let Some(first) = rows.first() {
                    let checkpoint = (first["queue_written"].clone(), first["queue_confirmed"].clone());
                    rows.retain(|row| row["queue_written"] == checkpoint.0 && row["queue_confirmed"] == checkpoint.1);
                }
                while !rows.is_empty() {
                    let card = format_wake_card(&listener,&rows,hub,qualify_ids);
                    if serde_json::to_vec(&card).map_err(|e| e.to_string())?.len() <= 524_288 { break; }
                    if rows.len() == 1 {
                        c.execute("UPDATE local_signals SET state='STORED_PASSIVE',held_reason='driver_unavailable' WHERE hub=? AND signal_id=?", &[json!(hub),rows[0]["signal_id"].clone()])?;
                    }
                    rows.pop();
                }
                let Some(first) = rows.first() else {
                    reconcile_wake_retry(c, hub, id)?;
                    return Ok(Vec::new());
                };
                let card = format_wake_card(&listener, &rows, hub, qualify_ids);
                // A middle-member acknowledgment can leave first/last seq intact.
                // Include all members and the card so that re-batching cannot reuse
                // an identity for a different immutable payload.
                let members: Vec<_> = rows.iter().map(|r| &r["signal_id"]).collect();
                let fingerprint = hash(&json!([members, card]).to_string());
                let batch = json!(format!("batch_{hub}_{id}_{}_{}_{}_{}", first["seq"], rows[rows.len()-1]["seq"], listener["binding_generation"], &fingerprint[..16]));
                let expires = earliest_expiry(&rows);
                (rows, batch, expires, card)
            };
            let token = json!(crate::plugins::random_uuid_v4());
            c.execute("UPDATE local_listeners SET dispatcher_token=?,lease_until_ms=? WHERE hub=? AND listener_id=?", &[token.clone(), json!(now+30_000), json!(hub), json!(id)])?;
            for row in &rows {
                c.execute("UPDATE local_signals SET state='DISPATCHING',batch_id=?,batch_card=?,batch_expires_at_ms=?,dispatcher_token=? WHERE hub=? AND signal_id=?", &[batch.clone(), json!(card), json!(expires), token.clone(), json!(hub), row["signal_id"].clone()])?;
            }
            c.query("SELECT * FROM local_signals WHERE hub=? AND listener_id=? AND batch_id=? AND dispatcher_token=? ORDER BY seq", &[json!(hub), json!(id), batch, token])
        })
    }
    pub fn check_dispatch(
        &self,
        listener: &Value,
        batch: &Value,
        renew: bool,
    ) -> Result<(), String> {
        self.check_dispatch_state(listener, batch, renew, true)
    }
    /// A business ack may race a single already-submitted effect's completion.
    /// Preserve that completion; ownership loss, expiry and remote renewal loss
    /// still cancel the child. A separate effect needs a fresh unread preflight.
    pub fn check_dispatch_in_flight(
        &self,
        listener: &Value,
        batch: &Value,
        renew: bool,
    ) -> Result<(), String> {
        self.check_dispatch_state(listener, batch, renew, false)
    }
    fn check_dispatch_state(
        &self,
        listener: &Value,
        batch: &Value,
        renew: bool,
        require_unread: bool,
    ) -> Result<(), String> {
        self.db.transaction(|c| {
            let hub = string(listener, "hub");
            let current = current_listener(c, hub, string(listener, "listener_id"))?;
            check_listener(&current, listener)?;
            if current["dispatcher_token"] != batch["dispatcher_token"]
                || current["lease_until_ms"].as_u64().unwrap_or(0) <= now_ms()
                || batch["batch_expires_at_ms"].as_u64().unwrap_or(0) <= now_ms()
                || c.query("SELECT 1 FROM local_signals WHERE hub=? AND batch_id=? AND dispatcher_token=? AND state='DISPATCHING' AND (?=0 OR acknowledged_at_ms IS NULL) LIMIT 1", &[json!(hub), batch["batch_id"].clone(), batch["dispatcher_token"].clone(), json!(require_unread)])?.is_empty() {
                return Err("DISPATCH_STOPPED: Batch expired, acknowledged or ownership lost".into());
            }
            if renew {
                c.execute("UPDATE local_listeners SET lease_until_ms=? WHERE hub=? AND listener_id=? AND dispatcher_token=?", &[json!(now_ms()+30_000), json!(hub), listener["listener_id"].clone(), batch["dispatcher_token"].clone()])?;
            }
            Ok(())
        })
    }
    /// Commit only the owning dispatch's result. A stale child cannot mutate its
    /// successor's rows or clear its lease. Acks always survive completion.
    #[expect(
        clippy::too_many_arguments,
        reason = "Preserve the existing typed persistence transition API."
    )]
    pub fn finish_wake_batch(
        &self,
        listener: &Value,
        batch: &Value,
        state: &str,
        reason: Option<&str>,
        queue_written: bool,
        queue_confirmed: bool,
        delivered: bool,
        re_batch: bool,
    ) -> Result<bool, String> {
        self.db.transaction(|c| {
            let hub = string(listener, "hub");
            let current = current_listener(c, hub, string(listener, "listener_id"))?;
            if current["binding_generation"] != listener["binding_generation"] || current["owner_token"] != listener["owner_token"] || current["dispatcher_token"] != batch["dispatcher_token"] {
                return Ok(false);
            }
            let accepted = state != "DISPATCHED" || current["status"] == "ACTIVE";
            let reason = if !accepted { Some(if current["status"] == "UNLISTENED" { "unlistened_before_dispatch" } else { "fenced_before_dispatch" }) } else { reason };
            let state = if accepted { state } else { "STORED_PASSIVE" };
            let delivered = delivered && accepted;
            c.execute("UPDATE local_signals SET state=CASE WHEN batch_expires_at_ms<=? AND ?<>'DISPATCHED' THEN 'EXPIRED_UNSENT' WHEN acknowledged_at_ms IS NOT NULL AND ?<>'DISPATCHED' THEN 'STORED_PASSIVE' ELSE ? END,held_reason=?,queue_written=MAX(queue_written,?),queue_confirmed=MAX(queue_confirmed,?),acknowledged_at_ms=CASE WHEN ? THEN COALESCE(acknowledged_at_ms,?) ELSE acknowledged_at_ms END,dispatcher_token=NULL WHERE hub=? AND batch_id=? AND dispatcher_token=? AND state='DISPATCHING'", &[json!(now_ms()), json!(state), json!(state), json!(state), json!(reason), json!(queue_written), json!(queue_confirmed), json!(delivered), json!(now_ms()), json!(hub), batch["batch_id"].clone(), batch["dispatcher_token"].clone()])?;
            if accepted && state == "DISPATCHED" {
                // COALESCE makes post-delivery grace a one-time credit per signal.
                c.execute("UPDATE local_signals SET delivered_at_ms=COALESCE(delivered_at_ms,?) WHERE hub=? AND batch_id=? AND state='DISPATCHED'", &[json!(now_ms()),json!(hub),batch["batch_id"].clone()])?;
            }
            if delivered {
                c.execute("INSERT OR IGNORE INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms,hub_url,principal_id) SELECT s.hub,s.listener_id,s.signal_id,?,s.acknowledged_at_ms,s.hub_url,s.principal_id FROM local_signals s WHERE s.hub=? AND s.batch_id=? AND s.acknowledged_at_ms IS NOT NULL", &[json!(format!("{}:{}", string(listener, "host_id"), string(listener, "harness"))), json!(hub), batch["batch_id"].clone()])?;
            }
            if re_batch {
                c.execute("UPDATE local_signals SET batch_id=NULL,batch_card=NULL,batch_expires_at_ms=NULL WHERE hub=? AND batch_id=? AND state='RECEIVED' AND acknowledged_at_ms IS NULL", &[json!(hub), batch["batch_id"].clone()])?;
            }
            c.execute("UPDATE local_listeners SET dispatcher_token=NULL,lease_until_ms=0 WHERE hub=? AND listener_id=? AND dispatcher_token=?", &[json!(hub), listener["listener_id"].clone(), batch["dispatcher_token"].clone()])?;
            reconcile_wake_retry(c, hub, string(listener, "listener_id"))?;
            Ok(accepted)
        })
    }
    pub fn fence_listener_for(&self, listener: &Value, reason: &str) -> Result<bool, String> {
        self.deactivate_listener_for(listener, "FENCED_INACTIVE", reason)
    }
    pub fn pause_listener_for(&self, listener: &Value) -> Result<bool, String> {
        if listener["status"] != "ACTIVE" {
            return Ok(false);
        }
        self.deactivate_listener_for(listener, "UNLISTENED", "unlistened")
    }
    fn deactivate_listener_for(
        &self,
        listener: &Value,
        status: &str,
        reason: &str,
    ) -> Result<bool, String> {
        let changed = self.db.transaction(|c| {
            let hub=string(listener,"hub");
            let current=current_listener(c,hub,string(listener,"listener_id"))?;
            // A retained pause can be fenced for a changed profile, but a stale
            // active receiver must never overwrite a more recent pause or resume.
            if current["status"] != listener["status"] || current["status"] == "FENCED_INACTIVE" || current["binding_generation"] != listener["binding_generation"] || current["owner_token"] != listener["owner_token"] { return Ok(false); }
            c.execute("UPDATE local_listeners SET status=?,updated_at=?,dispatcher_token=NULL,lease_until_ms=0 WHERE hub=? AND listener_id=? AND binding_generation=? AND COALESCE(owner_token,'')=? AND status=?", &[json!(status),json!(now_ms()),json!(hub),listener["listener_id"].clone(),listener["binding_generation"].clone(),listener["owner_token"].clone(),listener["status"].clone()])?;
            c.execute("UPDATE local_signals SET state='STORED_PASSIVE',held_reason=?,dispatcher_token=NULL WHERE hub=? AND listener_id=? AND binding_generation<=? AND state IN ('RECEIVED','DISPATCHING')", &[json!(reason),json!(hub),listener["listener_id"].clone(),listener["binding_generation"].clone()])?;
            c.execute("DELETE FROM wake_retries WHERE hub=? AND listener_id=?", &[json!(hub), listener["listener_id"].clone()])?;
            Ok(true)
        })?;
        if changed {
            self.invalidate_discovery(string(listener, "harness"))?;
        }
        Ok(changed)
    }
    pub fn restore_passive(&self, hub: &str, listener: &Value) -> Result<(), String> {
        self.db.execute("UPDATE local_signals SET state='RECEIVED',held_reason=NULL WHERE hub=? AND listener_id=? AND binding_generation=? AND state='STORED_PASSIVE' AND acknowledged_at_ms IS NULL AND (held_reason IS NULL OR (held_reason='driver_unavailable' AND NOT EXISTS(SELECT 1 FROM wake_retries w WHERE w.hub=local_signals.hub AND w.listener_id=local_signals.listener_id AND w.retry_at_ms>?))) AND EXISTS(SELECT 1 FROM local_listeners WHERE hub=? AND listener_id=? AND status='ACTIVE' AND binding_generation=? AND owner_token=?)", &[json!(hub), listener["listener_id"].clone(), listener["binding_generation"].clone(), json!(now_ms()), json!(hub), listener["listener_id"].clone(), listener["binding_generation"].clone(), listener["owner_token"].clone()])?;
        Ok(())
    }
    pub fn schedule_wake_retry_for(&self, listener: &Value, delay: u64) -> Result<(), String> {
        self.db.transaction(|c| {
            if reconcile_wake_retry(c, string(listener, "hub"), string(listener, "listener_id"))? {
                c.execute("INSERT INTO wake_retries(hub,listener_id,retry_at_ms) SELECT hub,listener_id,? FROM local_listeners WHERE hub=? AND listener_id=? AND status='ACTIVE' AND harness<>'human' AND binding_generation=? AND owner_token=? ON CONFLICT(hub,listener_id) DO UPDATE SET retry_at_ms=excluded.retry_at_ms", &[json!(now_ms()+delay), listener["hub"].clone(), listener["listener_id"].clone(), listener["binding_generation"].clone(), listener["owner_token"].clone()])?;
            }
            Ok(())
        })
    }
    pub fn acknowledge(
        &self,
        hub: &str,
        node: &str,
        ids: &[Value],
        channel: &str,
    ) -> Result<(), String> {
        self.db.transaction(|c| {
            let listener = current_listener(c, hub, node)?;
            for id in ids {
            let rows = c.query("SELECT hub_url,principal_id FROM local_signals WHERE hub=? AND listener_id=? AND signal_id=?", &[json!(hub),json!(node),id.clone()])?;
            if !rows.first().is_some_and(|signal| signal_matches_listener(signal, &listener)) { continue; }
            acknowledge_current(c, hub, node, id, channel)?;
        }
        reconcile_wake_retry(c, hub, node)?;
        Ok(()) })
    }
    pub fn isolate_stale_signals(&self, listener: &Value) -> Result<(), String> {
        self.db.transaction(|db| {
            let current = current_listener(db, string(listener,"hub"), string(listener,"listener_id"))?;
            for signal in db.query("SELECT hub,signal_id,listener_id,seq,state,held_reason,batch_id,hub_url,principal_id FROM local_signals WHERE hub=? AND listener_id=? AND state IN ('RECEIVED','DISPATCHING')", &[listener["hub"].clone(),listener["listener_id"].clone()])? {
                if !signal_matches_listener(&signal, &current) {
                    db.execute("UPDATE local_signals SET state='STORED_PASSIVE',held_reason='authority_mismatch',dispatcher_token=NULL WHERE hub=? AND signal_id=?", &[signal["hub"].clone(),signal["signal_id"].clone()])?;
                }
            }
            Ok(())
        })
    }
    /// Unread identity and dispatch metadata; payloads stay in paginated inbox reads.
    pub fn current_signal_metadata(
        &self,
        hub: &str,
        node: &str,
        after_seq: u64,
        limit: usize,
        match_id: Option<&str>,
    ) -> Result<Vec<Value>, String> {
        let Some(listener) = self.listener(hub, node)? else {
            return Ok(Vec::new());
        };
        let mut rows = Vec::new();
        self.db.transaction(|db| db.query_while(
            "SELECT hub,signal_id,listener_id,seq,state,held_reason,batch_id,hub_url,principal_id FROM local_signals WHERE hub=? AND listener_id=? AND acknowledged_at_ms IS NULL AND (?=0 OR seq>?) AND (? IS NULL OR signal_id=? OR batch_id=?) ORDER BY seq,signal_id",
            &[json!(hub),json!(node),json!(after_seq),json!(after_seq),json!(match_id),json!(match_id),json!(match_id)],
            |row| {
                if signal_matches_listener(&row, &listener) { rows.push(row); }
                Ok(rows.len() < limit)
            },
        ))?;
        Ok(rows)
    }

    pub fn unread_count(&self, hub: &str, node: &str, live_only: bool) -> Result<u64, String> {
        let Some(listener) = self.listener(hub, node)? else {
            return Ok(0);
        };
        let mut count = 0;
        self.db.transaction(|db| db.query_each(
            "SELECT hub_url,principal_id FROM local_signals WHERE hub=? AND listener_id=? AND acknowledged_at_ms IS NULL AND (?=0 OR (expires_at_ms>? AND state!='EXPIRED_UNSENT'))",
            &[json!(hub),json!(node),json!(live_only),json!(now_ms())],
            |row| { if signal_matches_listener(&row, &listener) { count += 1; } },
        ))?;
        Ok(count)
    }

    pub fn current_signals(
        &self,
        hub: &str,
        node: &str,
        limit: usize,
    ) -> Result<Vec<Value>, String> {
        let Some(listener) = self.listener(hub, node)? else {
            return Ok(Vec::new());
        };
        let limit = limit.clamp(1, 1000);
        let mut rows = Vec::new();
        self.db.transaction(|db| db.query_while("SELECT * FROM local_signals WHERE hub=? AND listener_id=? AND acknowledged_at_ms IS NULL ORDER BY created_at,seq", &[json!(hub),json!(node)], |row| {
            if signal_matches_listener(&row, &listener) { rows.push(row); }
            Ok(rows.len() < limit)
        }))?;
        Ok(rows)
    }
    pub fn activity(&self) -> Result<(Vec<Value>, u64), String> {
        let (rows, _, unread) = self.query_activity("all", "")?;
        Ok((rows, unread))
    }
    /// Count visible current history without loading message payloads.
    pub fn activity_counts(&self) -> Result<(u64, u64), String> {
        let rows = self.db.query("SELECT s.principal_id AS source,l.principal_id AS target,count(*) AS total,sum(s.acknowledged_at_ms IS NULL) AS unread FROM local_signals s JOIN local_listeners l ON l.hub=s.hub AND l.listener_id=s.listener_id WHERE s.activity_hidden=0 AND l.status!='FENCED_INACTIVE' AND l.hub_url!='' AND s.hub_url=l.hub_url GROUP BY s.principal_id,l.principal_id", &[])?;
        Ok(rows
            .iter()
            .filter(|row| {
                super::auth::principal_ids_equal(string(row, "source"), string(row, "target"))
            })
            .fold((0, 0), |(total, unread), row| {
                (
                    total + row["total"].as_u64().unwrap_or(0),
                    unread + row["unread"].as_u64().unwrap_or(0),
                )
            }))
    }
    pub fn query_activity(
        &self,
        filter: &str,
        query: &str,
    ) -> Result<(Vec<Value>, u64, u64), String> {
        let predicate = " FROM local_signals s LEFT JOIN local_listeners l ON l.hub=s.hub AND l.listener_id=s.listener_id WHERE s.activity_hidden=0 AND (?!='me' OR l.harness='human') AND (?!='unread' OR s.acknowledged_at_ms IS NULL)";
        let query = query.trim().to_lowercase();
        let params = [json!(filter), json!(filter)];
        self.db.transaction(|c| {
            let mut rows = Vec::new();
            let (mut total, mut unread) = (0, 0);
            // Apply address normalization before pagination and counts, streaming
            // history so a broad search retains at most the visible page in memory.
            c.query_each(&format!("SELECT s.*, l.hub_url AS target_hub_url,l.principal_id AS target_principal_id,l.status AS target_status,l.owner_user AS target_user, l.harness AS target_harness, l.title AS target_title{predicate} ORDER BY s.created_at DESC,s.signal_id ASC"), &params, |mut row| {
                if !joined_signal_is_current(&row) || !matches_activity(&row, &query) { return; }
                total += 1;
                unread += u64::from(row["acknowledged_at_ms"].is_null());
                if rows.len() < 80 {
                    if let Some(fields) = row.as_object_mut() {
                        for key in ["target_user", "target_harness", "target_title", "target_hub_url", "target_principal_id", "target_status"] { fields.remove(key); }
                    }
                    rows.push(row);
                }
            })?;
            Ok((rows, total, unread))
        })
    }
}

/// A retry belongs to current, unexpired, wakeable unread work, never merely
/// to a previous attempt. Use inside the same transaction as terminal updates.
pub(super) fn reconcile_wake_retry(c: &Connection, hub: &str, id: &str) -> Result<bool, String> {
    let listener = c
        .query(
            "SELECT * FROM local_listeners WHERE hub=? AND listener_id=?",
            &[json!(hub), json!(id)],
        )?
        .into_iter()
        .next();
    let retryable = if let Some(listener) =
        listener.filter(|row| row["status"] == "ACTIVE" && row["harness"] != "human")
    {
        c.query("SELECT hub,signal_id,listener_id,seq,state,held_reason,batch_id,hub_url,principal_id FROM local_signals WHERE hub=? AND listener_id=? AND binding_generation=? AND acknowledged_at_ms IS NULL AND state IN ('RECEIVED','DISPATCHING','STORED_PASSIVE') AND (held_reason IS NULL OR held_reason='driver_unavailable') AND expires_at_ms>? AND (batch_expires_at_ms IS NULL OR batch_expires_at_ms>?)",
            &[json!(hub), json!(id), listener["binding_generation"].clone(), json!(now_ms()), json!(now_ms())])?
            .iter().any(|signal| signal_matches_listener(signal, &listener))
    } else {
        false
    };
    if !retryable {
        c.execute(
            "DELETE FROM wake_retries WHERE hub=? AND listener_id=?",
            &[json!(hub), json!(id)],
        )?;
    }
    Ok(retryable)
}

pub(super) fn signal_matches_listener(signal: &Value, listener: &Value) -> bool {
    listener["status"] != "FENCED_INACTIVE"
        && !string(listener, "hub_url").is_empty()
        && signal["hub_url"] == listener["hub_url"]
        && !string(signal, "principal_id").is_empty()
        && !string(listener, "principal_id").is_empty()
        && super::auth::principal_ids_equal(
            string(signal, "principal_id"),
            string(listener, "principal_id"),
        )
}

pub(super) fn joined_signal_is_current(row: &Value) -> bool {
    signal_matches_listener(
        row,
        &json!({"hub_url":row["target_hub_url"],"principal_id":row["target_principal_id"],"status":row["target_status"]}),
    )
}

pub(super) fn matches_activity(row: &Value, query: &str) -> bool {
    let address = |user: &str, harness: &str, raw_id: &str| {
        let id = super::identity::canonical_node_id(raw_id).unwrap_or_default();
        let suffix = id.strip_prefix("c_").unwrap_or("");
        if harness == "human" {
            format!("@{user}")
        } else if suffix.is_empty() {
            raw_id.to_owned()
        } else {
            format!("{}/{harness}#{suffix}", normalize_slug(user))
        }
    };
    let source = address(
        string(row, "from_user"),
        string(row, "from_harness"),
        string(row, "from_listener_id"),
    );
    let target = address(
        string(row, "target_user"),
        string(row, "target_harness"),
        string(row, "listener_id"),
    );
    let mut searchable = format!(
        "{}:{source} {}:{target} @{} @{}",
        string(row, "hub"),
        string(row, "hub"),
        string(row, "from_user"),
        string(row, "target_user")
    );
    for field in [
        "message",
        "from_user",
        "from_host_name",
        "from_title",
        "target_title",
        "signal_id",
        "listener_id",
        "from_harness",
        "target_harness",
        "from_listener_id",
    ] {
        searchable.push(' ');
        searchable.push_str(string(row, field));
    }
    if row["target_harness"] == "human" {
        searchable.push_str(" you");
    }
    searchable
        .to_lowercase()
        .contains(&query.trim().to_lowercase())
}

fn acknowledge_current(
    c: &Connection,
    hub: &str,
    node: &str,
    id: &Value,
    channel: &str,
) -> Result<(), String> {
    let at = json!(now_ms());
    c.execute("UPDATE local_signals SET acknowledged_at_ms=COALESCE(acknowledged_at_ms,?),state=CASE WHEN state='RECEIVED' THEN 'STORED_PASSIVE' ELSE state END WHERE hub=? AND listener_id=? AND signal_id=?",&[at.clone(),json!(hub),json!(node),id.clone()])?;
    c.execute("INSERT OR REPLACE INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms,hub_url,principal_id) SELECT hub,listener_id,signal_id,?,?,hub_url,principal_id FROM local_signals WHERE hub=? AND listener_id=? AND signal_id=?",&[json!(channel),at,json!(hub),json!(node),id.clone()])?;
    Ok(())
}

fn current_listener(c: &Connection, hub: &str, id: &str) -> Result<Value, String> {
    c.query(
        "SELECT * FROM local_listeners WHERE hub=? AND listener_id=?",
        &[json!(hub), json!(id)],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| "STALE_GENERATION: Listener removed".into())
}
fn check_listener(current: &Value, captured: &Value) -> Result<(), String> {
    if current["status"] != "ACTIVE"
        || current["binding_generation"] != captured["binding_generation"]
        || current["owner_token"] != captured["owner_token"]
    {
        Err("STALE_GENERATION: Listener changed during dispatch".into())
    } else {
        Ok(())
    }
}
fn earliest_expiry(rows: &[Value]) -> u64 {
    rows.iter()
        .filter_map(|r| r["expires_at_ms"].as_u64())
        .min()
        .unwrap_or(0)
}
fn escape_card(raw: &str) -> String {
    raw.replace('<', "&lt;").replace('>', "&gt;")
}
fn format_sender(signal: &Value, hub: &str, qualify_ids: bool) -> String {
    let raw_host = string(signal, "from_host_name");
    let host = if raw_host.to_ascii_lowercase().ends_with(".local") && raw_host.len() > 6 {
        &raw_host[..raw_host.len() - 6]
    } else {
        raw_host
    };
    let raw_id = string(signal, "from_listener_id");
    let scoped = if qualify_ids && !raw_id.is_empty() {
        format!("{hub}:{raw_id}")
    } else {
        raw_id.into()
    };
    let title = string(signal, "from_title").trim();
    // Match the baseline public-ID title suppression, including legacy base32 IDs.
    let slug = raw_id
        .rsplit_once(':')
        .map(|(_, id)| id)
        .unwrap_or(raw_id)
        .rsplit_once('-')
        .filter(|(slug, suffix)| {
            !slug.is_empty()
                && slug.split('-').all(|p| {
                    !p.is_empty()
                        && p.bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                })
                && (suffix.len() == 16 && suffix.bytes().all(|b| b.is_ascii_hexdigit())
                    || suffix.len() == 12
                        && suffix
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)))
        })
        .map(|(slug, _)| slug);
    let title_part = if !title.is_empty() && slug != Some(normalize_slug(title).as_str()) {
        format!(" · {}", escape_card(title))
    } else {
        String::new()
    };
    let reply = if scoped.is_empty() {
        String::new()
    } else {
        format!(" [↩ {}]", escape_card(&scoped))
    };
    format!(
        "{}@{}{title_part}{reply}",
        escape_card(string(signal, "from_user")),
        escape_card(host)
    )
}
pub fn format_wake_card(target: &Value, signals: &[Value], hub: &str, qualify_ids: bool) -> String {
    if signals.len() == 1 {
        return format!(
            "Cito · {}\n\n{}",
            format_sender(&signals[0], hub, qualify_ids),
            escape_card(string(&signals[0], "message"))
        );
    }
    let title = string(target, "title").trim();
    let target_part = if title.is_empty() {
        String::new()
    } else {
        format!(" → {}", escape_card(title))
    };
    let body = signals
        .iter()
        .enumerate()
        .map(|(i, s)| {
            format!(
                "[{}] {}\n{}",
                i + 1,
                format_sender(s, hub, qualify_ids),
                escape_card(string(s, "message"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "Cito · {} notifications{target_part}\n\n{body}",
        signals.len()
    )
}
