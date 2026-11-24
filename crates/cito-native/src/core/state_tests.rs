use super::*;
use std::sync::{Arc, Barrier};

struct Fixture {
    state: LocalState,
    dir: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Result<Self, String> {
        let dir =
            std::env::temp_dir().join(format!("cito-state-{}", crate::plugins::random_uuid_v4()));
        let state = LocalState::open(&dir)?;
        state.db.execute("INSERT INTO local_listeners(hub,listener_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status,binding_generation,owner_token,hub_url) VALUES('local','c_1','none#alice','alice','host','host','agent','s','','Target','','workspace','ACTIVE',1,'owner','local://')", &[])?;
        Ok(Self { state, dir })
    }
    fn listener(&self) -> Result<Value, String> {
        self.state
            .listener("local", "c_1")?
            .ok_or_else(|| "Missing fixture listener".into())
    }
    fn claim_wake_batch(&self) -> Result<Vec<Value>, String> {
        self.state
            .claim_wake_batch_for("local", &self.listener()?, false)
    }
    fn seed(&self, id: &str, seq: usize) -> Result<(), String> {
        self.state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,hub_url,principal_id) VALUES('local',?,'c_1',?,1,'alice','host.local','','message','2026-01-01T00:00:00.000Z',?,'RECEIVED','local://','none#alice')", &[json!(id), json!(seq), json!(now_ms()+60_000)])?;
        Ok(())
    }
    fn rows(&self) -> Result<Vec<Value>, String> {
        self.state
            .db
            .query("SELECT * FROM local_signals ORDER BY seq", &[])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
#[test]
fn passive_restore_and_claim_fence_foreign_rows_before_freezing_a_card() -> Result<(), String> {
    let f = Fixture::new()?;
    let l = f.listener()?;
    for (i, id) in [
        "active-foreign",
        "passive-foreign",
        "driver-foreign",
        "local",
    ]
    .iter()
    .enumerate()
    {
        f.seed(id, i)?;
    }
    f.state.db.execute(
        "UPDATE local_signals SET hub_url='other' WHERE signal_id LIKE '%-foreign'",
        &[],
    )?;
    f.state.db.execute("UPDATE local_signals SET state='STORED_PASSIVE' WHERE signal_id IN ('passive-foreign','driver-foreign','local')",&[])?;
    f.state.db.execute("UPDATE local_signals SET held_reason='driver_unavailable' WHERE signal_id='driver-foreign'",&[])?;
    f.state.restore_passive("local", &l)?;
    assert!(f.rows()?[..3]
        .iter()
        .all(|r| r["held_reason"] == "authority_mismatch"));
    f.seed("after-restore", 4)?;
    f.state.db.execute(
        "UPDATE local_signals SET principal_id='none#bob' WHERE signal_id='after-restore'",
        &[],
    )?;
    let rows = f.state.claim_wake_batch_for("local", &l, false)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["signal_id"], "local");
    Ok(())
}

#[test]
fn partially_read_batch_stops_before_new_effect_and_releases_unread_survivor() -> Result<(), String>
{
    let f = Fixture::new()?;
    let l = f.listener()?;
    f.seed("read", 1)?;
    f.seed("unread", 2)?;
    let old = f.state.claim_wake_batch_for("local", &l, false)?;
    f.state
        .acknowledge("local", "c_1", &[json!("read")], "host:agent")?;
    f.state.check_dispatch_in_flight(&l, &old[0], false)?;
    assert!(f
        .state
        .check_dispatch(&l, &old[0], false)
        .unwrap_err()
        .starts_with("DISPATCH_STOPPED:"));
    let new = f.state.claim_wake_batch_for("local", &l, false)?;
    assert_eq!(new.len(), 1);
    assert_eq!(new[0]["signal_id"], "unread");
    assert_ne!(new[0]["batch_id"], old[0]["batch_id"]);
    Ok(())
}

#[test]
fn unread_counts_exclude_foreign_expired_and_fenced_work() -> Result<(), String> {
    let f = Fixture::new()?;
    for index in 0..140 {
        f.seed(&format!("signal-{index:03}"), index / 2)?;
    }
    f.state.db.execute(
        "UPDATE local_signals SET hub_url='https://other.example.com' WHERE signal_id='signal-001'",
        &[],
    )?;
    assert_eq!(f.state.unread_count("local", "c_1", false)?, 139);
    f.state.db.execute(
        "UPDATE local_signals SET expires_at_ms=0 WHERE signal_id='signal-002'",
        &[],
    )?;
    assert_eq!(f.state.unread_count("local", "c_1", true)?, 138);
    f.state
        .db
        .execute("UPDATE local_listeners SET status='FENCED_INACTIVE'", &[])?;
    assert_eq!(f.state.unread_count("local", "c_1", false)?, 0);
    Ok(())
}

#[test]
fn signal_reads_and_acknowledgments_isolate_origin_and_accept_principal_aliases(
) -> Result<(), String> {
    let f = Fixture::new()?;
    f.state.db.execute(
        "UPDATE local_listeners SET principal_id='oidc:https://id.example.com|alice'",
        &[],
    )?;
    for id in [
        "current",
        "legacy-principal",
        "other-principal",
        "other-url",
        "unbound",
    ] {
        f.seed(id, 1)?;
    }
    f.state.db.execute("UPDATE local_signals SET principal_id='oidc:https://id.example.com|alice'; UPDATE local_signals SET principal_id='https://id.example.com#alice' WHERE signal_id='legacy-principal'; UPDATE local_signals SET principal_id='none#other' WHERE signal_id='other-principal'; UPDATE local_signals SET hub_url='https://other.example.com' WHERE signal_id='other-url'; UPDATE local_signals SET hub_url=NULL,principal_id=NULL WHERE signal_id='unbound'",&[])?;
    let (rows, total, unread) = f.state.query_activity("all", "")?;
    assert_eq!((rows.len(), total, unread), (2, 2, 2));
    f.state.schedule_wake_retry_for(&f.listener()?, 5000)?;
    f.state.restore_passive("local", &f.listener()?)?;
    for row in f.rows()? {
        if !["current", "legacy-principal"].contains(&string(&row, "signal_id")) {
            assert_eq!(row["state"], "STORED_PASSIVE");
            assert_eq!(row["held_reason"], "authority_mismatch");
        }
    }
    f.state.acknowledge(
        "local",
        "c_1",
        &[
            json!("current"),
            json!("legacy-principal"),
            json!("other-principal"),
            json!("other-url"),
            json!("unbound"),
        ],
        "host:agent",
    )?;
    let acks = f.state.db.query(
        "SELECT signal_id FROM pending_hub_acks ORDER BY signal_id",
        &[],
    )?;
    assert_eq!(
        acks,
        vec![
            json!({"signal_id":"current"}),
            json!({"signal_id":"legacy-principal"})
        ]
    );
    assert!(f
        .state
        .db
        .query("SELECT * FROM wake_retries", &[])?
        .is_empty());
    assert_eq!(f.state.query_activity("all", "")?.2, 0);
    Ok(())
}

#[test]
fn hub_clock_remaining_lifetime_survives_client_skew_at_admission() -> Result<(), String> {
    for skew in [-1_860_000i64, 1_860_000] {
        let f = Fixture::new()?;
        let local_now = now_ms();
        let hub_now = (local_now as i64 + skew) as u64;
        let created = hub_now - 60_000;
        let expires = LocalState::signal_deadline(created, hub_now, local_now);
        assert_eq!(expires, local_now + 1_740_000);
        let signal = signal("skewed", "none#bob", 1);
        f.state.admit(
            "local",
            &f.listener()?,
            &signal,
            1,
            "2026-01-01T00:00:00Z",
            expires,
            &Value::Null,
        )?;
        let rows = f.claim_wake_batch()?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["signal_id"], "skewed");
        assert!(rows[0]["batch_expires_at_ms"].as_u64().unwrap() > local_now);
        assert_eq!(
            LocalState::signal_deadline(hub_now - 1_800_001, hub_now, local_now),
            local_now
        );
    }
    Ok(())
}
#[test]
fn streaming_state_reads_see_committed_rows_while_another_connection_writes() -> Result<(), String>
{
    let f = Fixture::new()?;
    f.seed("visible", 1)?;
    let writer = Database::open(&f.dir.join("state.sqlite"))?;
    f.state.db.execute("PRAGMA busy_timeout=0", &[])?;
    writer.transaction(|connection| {
        connection.execute("UPDATE local_signals SET acknowledged_at_ms=1", &[])?;
        assert_eq!(f.state.unread_count("local", "c_1", false)?, 1);
        let (page, total, unread) = f.state.query_activity("unread", "")?;
        assert_eq!((total, unread), (1, 1));
        assert_eq!(page[0]["signal_id"], "visible");
        Ok(())
    })?;
    assert_eq!(f.state.unread_count("local", "c_1", false)?, 0);
    Ok(())
}

#[test]
fn current_schema_open_does_not_acquire_a_write_transaction() -> Result<(), String> {
    let f = Fixture::new()?;
    let writer = Database::open(&f.dir.join("state.sqlite"))?;
    let (locked, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let owner = std::thread::spawn(move || {
        writer.transaction(|_| {
            locked.send(()).map_err(|e| e.to_string())?;
            wait.recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|e| e.to_string())?;
            Ok(())
        })
    });
    ready
        .recv_timeout(std::time::Duration::from_secs(1))
        .map_err(|e| e.to_string())?;
    let dir = f.dir.clone();
    let (opened, result) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let _ = opened.send(LocalState::open(&dir));
    });
    let fast = result.recv_timeout(std::time::Duration::from_millis(150));
    let _ = release.send(());
    owner.join().map_err(|_| "Writer panicked")??;
    reader.join().map_err(|_| "Reader panicked")?;
    assert!(
        fast.is_ok(),
        "A verified schema must open while another connection holds write authority"
    );
    let reopened = fast.map_err(|e| e.to_string())??;
    assert!(reopened.listener("local", "c_1")?.is_some());
    Ok(())
}

fn signal(id: &str, principal: &str, generation: i64) -> Value {
    json!({"signal_id":id,"to":"c_1","target_generation":generation,"from":{"principal_id":principal,"user":"bob","host_name":"other.local","harness":"agent","listener_id":null,"title":"Sender","about":null},"message":"hello <world>","in_reply_to":null})
}
fn admit(f: &Fixture, s: &Value, seq: u64) -> Result<bool, String> {
    f.state.admit(
        "local",
        &f.listener()?,
        s,
        seq,
        "2026-01-01T00:00:00.000Z",
        now_ms() + 60_000,
        &Value::Null,
    )
}

#[test]
fn existing_v7_listener_gains_nullable_process_identity() -> Result<(), String> {
    let f = Fixture::new()?;
    f.state.db.execute(
        "ALTER TABLE local_listeners DROP COLUMN process_identity",
        &[],
    )?;
    let reopened = LocalState::open(&f.dir)?;
    let row = reopened.listener("local", "c_1")?.unwrap();
    assert_eq!(row.get("process_identity"), Some(&Value::Null));
    assert_eq!(row["owner_token"], "owner");
    assert_eq!(
        reopened.db.query("PRAGMA user_version", &[])?[0]["user_version"],
        7
    );
    Ok(())
}

#[test]
fn listener_lifecycle_upgrade_backfills_versions_four_five_and_six_without_losing_work(
) -> Result<(), String> {
    for version in [4, 5, 6] {
        let f = Fixture::new()?;
        f.seed("retained", 1)?;
        f.state
            .db
            .execute("INSERT INTO wake_retries VALUES('local','c_1',99)", &[])?;
        for column in [
            "listen_mode",
            "user_stopped",
            "retirement_reason",
            "retired_activity_revision",
            "last_activity_at_ms",
            "last_activity_revision",
        ] {
            f.state.db.execute(
                &format!("ALTER TABLE local_listeners DROP COLUMN {column}"),
                &[],
            )?;
        }
        f.state
            .db
            .execute(&format!("PRAGMA user_version={version}"), &[])?;
        for _ in 0..2 {
            let state = LocalState::open(&f.dir)?;
            let row = state
                .listener("local", "c_1")?
                .ok_or("Missing migrated listener")?;
            assert_eq!(row["listen_mode"], "manual");
            assert_eq!(row["user_stopped"], 0);
            for key in [
                "retirement_reason",
                "retired_activity_revision",
                "last_activity_at_ms",
            ] {
                assert!(row[key].is_null(), "{key}");
            }
            assert_eq!(row["binding_generation"], 1);
            assert_eq!(state.db.query("SELECT * FROM local_signals", &[])?.len(), 1);
            assert_eq!(
                state
                    .db
                    .query("SELECT retry_at_ms FROM wake_retries", &[])?[0]["retry_at_ms"],
                99
            );
            assert_eq!(
                state.db.query("PRAGMA user_version", &[])?[0]["user_version"],
                7
            );
        }
    }
    Ok(())
}
#[test]
fn inbound_activity_is_credited_once_per_signal_and_dispatch_grace_once_per_delivery(
) -> Result<(), String> {
    let f = Fixture::new()?;
    assert!(admit(&f, &signal("one", "none#alice", 1), 1)?);
    let initial = f.listener()?["last_activity_at_ms"]
        .as_u64()
        .ok_or("Missing activity credit")?;
    f.state.db.execute(
        "UPDATE local_listeners SET last_activity_at_ms=?",
        &[json!(initial - 100)],
    )?;
    assert!(!admit(&f, &signal("one", "none#alice", 1), 1)?);
    assert_eq!(f.listener()?["last_activity_at_ms"], initial - 100);
    let listener = f.listener()?;
    let batch = f.claim_wake_batch()?;
    assert!(!batch.is_empty());
    f.state.finish_wake_batch(
        &listener,
        &batch[0],
        "DISPATCHED",
        None,
        true,
        true,
        false,
        false,
    )?;
    let delivered = f.rows()?[0]["delivered_at_ms"].clone();
    assert!(delivered.is_number());
    f.state.finish_wake_batch(
        &listener,
        &batch[0],
        "DISPATCHED",
        None,
        true,
        true,
        false,
        false,
    )?;
    assert_eq!(f.rows()?[0]["delivered_at_ms"], delivered);
    Ok(())
}
#[test]
fn v4_pending_ack_migration_never_guesses_original_hub_identity() -> Result<(), String> {
    let f = Fixture::new()?;
    f.state.db.execute("UPDATE local_listeners SET hub_url='local://'; DROP TABLE pending_hub_acks; CREATE TABLE pending_hub_acks(hub TEXT NOT NULL,node_id TEXT NOT NULL,signal_id TEXT NOT NULL,channel_id TEXT NOT NULL,acked_at_ms INTEGER NOT NULL,PRIMARY KEY(hub,node_id,signal_id)); INSERT INTO pending_hub_acks VALUES('local','c_1','legacy','host:agent',1); PRAGMA user_version=4;",&[])?;
    let upgraded = LocalState::open(&f.dir)?;
    let rows = upgraded.db.query("SELECT * FROM pending_hub_acks", &[])?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["signal_id"], "legacy");
    assert!(rows[0]["hub_url"].is_null() && rows[0]["principal_id"].is_null());
    assert_eq!(
        upgraded.db.query("PRAGMA user_version", &[])?[0]["user_version"],
        7
    );
    Ok(())
}
#[test]
fn signal_origin_upgrade_leaves_legacy_rows_unbound_and_new_admissions_immutable(
) -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("legacy", 1)?;
    f.state.db.execute("ALTER TABLE local_signals DROP COLUMN hub_url; ALTER TABLE local_signals DROP COLUMN principal_id; PRAGMA user_version=5", &[])?;
    let state = LocalState::open(&f.dir)?;
    assert!(state.db.query("SELECT * FROM local_signals", &[])?[0]["hub_url"].is_null());
    state
        .db
        .execute("UPDATE local_listeners SET hub_url=\'local://\'", &[])?;
    admit(&f, &signal("origin", "none#alice", 1), 2)?;
    state.db.execute("UPDATE local_listeners SET hub_url=\'https://other.example.com\',principal_id=\'none#bob\'", &[])?;
    state.acknowledge(
        "local",
        "c_1",
        &[json!("origin"), json!("legacy")],
        "host:agent",
    )?;
    let rows = state
        .db
        .query("SELECT * FROM pending_hub_acks ORDER BY signal_id", &[])?;
    assert!(
        rows.is_empty(),
        "A changed principal cannot acknowledge retained or legacy signals"
    );
    let signals = state
        .db
        .query("SELECT * FROM local_signals ORDER BY signal_id", &[])?;
    assert!(signals[0]["hub_url"].is_null() && signals[0]["principal_id"].is_null());
    assert_eq!(signals[1]["hub_url"], "local://");
    assert_eq!(signals[1]["principal_id"], "none#alice");
    assert!(signals
        .iter()
        .all(|signal| signal["acknowledged_at_ms"].is_null()));
    Ok(())
}
#[test]
fn inbox_admission_bounds_the_double_escaped_wire_signal_including_metadata() -> Result<(), String>
{
    let f = Fixture::new()?;
    let mut s = signal("local:s", "none#alice", 1);
    s["in_reply_to"] = json!("reply:parent");
    // Quotes need four bytes inside MCP's JSON-encoded text. Include sender
    // metadata and both fully qualified references in the single-signal limit.
    s["from"]["about"] = json!("\"".repeat(1_000));
    let mut wire = json!({"hub":"local","signal_id":"local:local:s","seq":1,"from":s["from"],"in_reply_to":"local:reply:parent","message":"","created_at":"2026-01-01T00:00:00.000Z","held_reason":"unlistened_before_dispatch"});
    wire["from"].as_object_mut().unwrap().remove("principal_id");
    let remaining = 7_339_008 - serde_json::to_vec(&wire.to_string()).unwrap().len();
    let mut message = "\"".repeat(remaining / 4) + &"x".repeat(remaining % 4);
    message.push('x');
    s["message"] = json!(message);
    assert!(!admit(&f, &s, 1)?);
    assert_eq!(f.state.unread_count("local", "c_1", false)?, 0);
    message.pop();
    s["message"] = json!(message);
    assert!(admit(&f, &s, 1)?);
    assert_eq!(f.state.unread_count("local", "c_1", false)?, 1);
    Ok(())
}

#[test]
fn oversized_single_card_stays_readable_without_retrying_or_blocking_its_peer() -> Result<(), String>
{
    let f = Fixture::new()?;
    let mut large = signal("large", "none#alice", 1);
    large["message"] = json!("<".repeat(131_072));
    assert!(admit(&f, &large, 1)?);
    assert!(admit(&f, &signal("healthy", "none#alice", 1), 2)?);
    let batch = f.claim_wake_batch()?;
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0]["signal_id"], "healthy");
    f.state.finish_wake_batch(
        &f.listener()?,
        &batch[0],
        "DISPATCHED",
        None,
        true,
        true,
        true,
        false,
    )?;
    f.state.restore_passive("local", &f.listener()?)?;
    f.state.schedule_wake_retry_for(&f.listener()?, 0)?;
    assert!(f.claim_wake_batch()?.is_empty());
    assert!(f
        .state
        .db
        .query("SELECT 1 FROM wake_retries", &[])?
        .is_empty());
    let (page, _, unread) = f.state.query_activity("unread", "")?;
    assert_eq!(unread, 1);
    assert_eq!(page[0]["message"], large["message"]);
    assert_eq!(page[0]["state"], "STORED_PASSIVE");
    assert_eq!(page[0]["held_reason"], "card_oversize");
    assert!(page[0]["dispatcher_token"].is_null() && page[0]["batch_id"].is_null());
    Ok(())
}

#[test]
fn oversized_only_candidate_releases_an_expired_predecessor_lease() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("predecessor", 1)?;
    assert_eq!(f.claim_wake_batch()?.len(), 1);
    f.state.db.execute("UPDATE local_listeners SET lease_until_ms=1; UPDATE local_signals SET batch_expires_at_ms=1", &[])?;
    let mut large = signal("large", "none#alice", 1);
    large["message"] = json!("\"".repeat(262_144));
    assert!(admit(&f, &large, 2)?);
    assert!(f.claim_wake_batch()?.is_empty());
    assert_eq!(f.rows()?[1]["held_reason"], "card_oversize");
    let listener = f.listener()?;
    assert!(listener["dispatcher_token"].is_null());
    assert_eq!(listener["lease_until_ms"], 0);
    Ok(())
}

#[test]
fn wake_retry_stops_stepping_at_the_first_current_signal() -> Result<(), String> {
    let f = Fixture::new()?;
    for (seq, id) in ["foreign", "current", "unvisited"].iter().enumerate() {
        f.seed(id, seq)?;
    }
    f.state.db.execute(
        "UPDATE local_signals SET hub_url='other' WHERE seq=0;
        CREATE TEMP VIEW local_signals AS SELECT * FROM main.local_signals WHERE seq=0
        UNION ALL SELECT * FROM main.local_signals WHERE seq=1
        UNION ALL SELECT * FROM main.local_signals WHERE seq=2 AND abs(-9223372036854775808)",
        &[],
    )?;
    let retry = || {
        f.state
            .db
            .transaction(|db| reconcile_wake_retry(db, "local", "c_1"))
    };
    assert!(retry()?);
    // Without a match SQLite must step the last arm, exposing its sentinel error.
    f.state.db.execute(
        "UPDATE main.local_signals SET acknowledged_at_ms=1 WHERE seq=1",
        &[],
    )?;
    assert!(retry().unwrap_err().contains("integer overflow"));
    Ok(())
}

#[test]
fn maximum_unicode_message_reaches_dispatch_within_the_wake_envelope_budget() -> Result<(), String>
{
    let f = Fixture::new()?;
    f.seed("maximum", 1)?;
    f.state.db.execute(
        "UPDATE local_signals SET message=? WHERE signal_id='maximum'",
        &[json!("😀".repeat(65_536))],
    )?;
    let rows = f.claim_wake_batch()?;
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["held_reason"].is_null());
    assert_eq!(rows[0]["state"], "DISPATCHING");
    let bytes = serde_json::to_vec(&rows[0]["batch_card"]).map_err(|e| e.to_string())?;
    assert!(bytes.len() > 262_144 && bytes.len() <= 524_288);
    Ok(())
}

#[test]
fn hidden_activity_keeps_agent_dispatch_checkpoints_and_full_search_fields() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("pending", 1)?;
    f.state.db.execute("UPDATE local_signals SET from_user='bob',from_harness='sender-app',from_listener_id='bob-abcdef1234567890',from_title='Review'",&[])?;
    for query in [
        "@bob",
        "sender-app",
        "agent",
        "bob-abcdef1234567890",
        "bob/sender-app#abcd",
        "c_1",
    ] {
        assert_eq!(f.state.query_activity("all", query)?.1, 1, "{query}");
    }
    f.state.db.execute("UPDATE local_listeners SET hub='work'; UPDATE local_signals SET hub='work',from_user='Alice.Smith',from_harness='fixture',from_listener_id='alice-smith-abcdef1234567890'", &[])?;
    for query in [
        "alice-smith/fixture#abcd",
        "work:alice-smith/fixture#abcd",
        "work:alice-smith/fixture#abcdef1234567890",
    ] {
        let (rows, total, unread) = f.state.query_activity("all", query)?;
        assert_eq!((rows.len(), total, unread), (1, 1, 1), "{query}");
    }
    f.state.db.execute(
        "UPDATE local_listeners SET hub='local'; UPDATE local_signals SET hub='local'",
        &[],
    )?;
    let batch = f.claim_wake_batch()?;
    f.state.finish_wake_batch(
        &f.listener()?,
        &batch[0],
        "DISPATCHING",
        None,
        true,
        true,
        false,
        false,
    )?;
    let batch = f.claim_wake_batch()?;
    f.state.clear_activity_history(
        &json!({"hub":"local","query":"work:missing"}),
        "all",
        "host",
    )?;
    assert_eq!(f.state.activity_counts()?, (1, 1));
    f.state.clear_activity_history(
        &json!({"hub":"local","query":"local:alice-smith/fixture#abcd"}),
        "all",
        "host",
    )?;
    assert_eq!(f.state.activity_counts()?.1, 0);
    assert_eq!(f.state.query_activity("all", "")?.1, 0);
    let rows = f.rows()?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["queue_written"], 1);
    assert_eq!(rows[0]["queue_confirmed"], 1);
    assert!(rows[0]["acknowledged_at_ms"].is_null());
    f.state.finish_wake_batch(
        &f.listener()?,
        &batch[0],
        "DISPATCHED",
        None,
        false,
        false,
        false,
        false,
    )?;
    assert_eq!(f.rows()?[0]["state"], "DISPATCHED");
    Ok(())
}
#[test]
fn clear_human_history_acknowledges_only_the_selected_unread_rows() -> Result<(), String> {
    let f = Fixture::new()?;
    f.state.db.execute("UPDATE local_listeners SET harness='human'; CREATE INDEX visible_unread ON local_signals(activity_hidden, acknowledged_at_ms)", &[])?;
    for (seq, id) in [
        "match-one",
        "match-two",
        "match-read",
        "unrelated",
        "match-foreign",
    ]
    .iter()
    .enumerate()
    {
        f.seed(id, seq)?;
    }
    f.state.db.execute("UPDATE local_signals SET hub_url='other' WHERE signal_id='match-foreign'; UPDATE local_signals SET acknowledged_at_ms=1 WHERE signal_id='match-read'", &[])?;
    f.state.clear_activity_history(
        &json!({"hub":"local","query":"match","filter":"unread"}),
        "all",
        "host",
    )?;
    let rows = f.rows()?;
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row["activity_hidden"], usize::from(i < 2));
        assert_eq!(row["acknowledged_at_ms"].is_number(), i < 3);
    }
    assert_eq!(
        f.state.db.query(
            "SELECT signal_id FROM pending_hub_acks ORDER BY signal_id",
            &[]
        )?,
        [
            json!({"signal_id":"match-one"}),
            json!({"signal_id":"match-two"})
        ]
    );
    Ok(())
}

#[test]
fn activity_filters_before_paging_and_counts_full_matching_set() -> Result<(), String> {
    let f = Fixture::new()?;
    for i in 0..100 {
        f.seed(&format!("new{i}"), i)?;
    }
    f.state.db.execute(
        "UPDATE local_signals SET acknowledged_at_ms=1,created_at='2030-01-01'",
        &[],
    )?;
    f.seed("old-match", 101)?;
    f.state.db.execute(
        "UPDATE local_signals SET message='unique needle' WHERE signal_id='old-match'",
        &[],
    )?;
    let (rows, total, unread) = f.state.query_activity("unread", "needle")?;
    assert_eq!((rows.len(), total, unread), (1, 1, 1));
    assert_eq!(rows[0]["signal_id"], "old-match");
    f.state.db.execute("UPDATE local_signals SET from_host_name='unique-machine.local' WHERE signal_id='old-match'", &[])?;
    for query in ["unique-machine", "old-match"] {
        let (rows, total, unread) = f.state.query_activity("all", query)?;
        assert_eq!((rows.len(), total, unread), (1, 1, 1), "{query}");
        assert_eq!(rows[0]["signal_id"], "old-match");
    }
    f.state.db.execute(
        "UPDATE local_signals SET from_host_name='remote-mac-42' WHERE signal_id='old-match'",
        &[],
    )?;
    for query in ["  remote-mac-42  ", "\tREMOTE-MAC-42\n"] {
        let (rows, total, unread) = f.state.query_activity("all", query)?;
        assert_eq!((rows.len(), total, unread), (1, 1, 1));
        assert_eq!(rows[0]["signal_id"], "old-match");
    }
    assert_eq!(f.state.query_activity("all", "   ")?.1, 101);
    assert_eq!(f.state.query_activity("all", "you")?.1, 0);
    assert_eq!(f.state.query_activity("all", "c_1")?.1, 101);
    assert_eq!(f.state.query_activity("me", "")?.1, 0);
    f.state
        .db
        .execute("UPDATE local_listeners SET harness='human'", &[])?;
    let (rows, total, unread) = f.state.query_activity("me", "")?;
    assert_eq!((rows.len(), total, unread), (80, 101, 1));
    assert_eq!(f.state.query_activity("all", "you")?.1, 101);
    Ok(())
}

#[test]
fn full_unread_count_and_non_destructive_v4_reopen() -> Result<(), String> {
    let f = Fixture::new()?;
    for i in 0..100 {
        f.seed(&format!("s{i}"), i)?;
    }
    let (rows, _, unread) = f.state.query_activity("all", "")?;
    assert_eq!((rows.len(), unread), (80, 100));
    let batch = f.claim_wake_batch()?;
    let before = f.listener()?;
    let second = LocalState::open(&f.dir)?;
    assert_eq!(second.activity_counts()?.1, 100);
    assert_eq!(
        second.db.query("PRAGMA user_version", &[])?[0]["user_version"],
        7
    );
    assert_eq!(
        second.listener("local", "c_1")?.ok_or("Missing listener")?["dispatcher_token"],
        before["dispatcher_token"]
    );
    assert_eq!(f.rows()?[0]["batch_id"], batch[0]["batch_id"]);
    assert_eq!(f.rows()?[0]["state"], "DISPATCHING");
    assert!(second
        .claim_wake_batch_for("local", &before, false)?
        .is_empty());
    Ok(())
}

#[test]
fn independent_sqlite_handles_have_one_atomic_winner() -> Result<(), String> {
    let f = Fixture::new()?;
    for i in 0..10 {
        f.seed(&format!("s{i}"), i)?;
    }
    let listener = f.listener()?;
    let handles = [LocalState::open(&f.dir)?, LocalState::open(&f.dir)?];
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = handles
        .into_iter()
        .map(|state| {
            let barrier = barrier.clone();
            let listener = listener.clone();
            std::thread::spawn(move || {
                barrier.wait();
                state
                    .claim_wake_batch_for("local", &listener, false)
                    .map(|v| v.len())
            })
        })
        .collect();
    barrier.wait();
    let mut counts = Vec::new();
    for t in threads {
        counts.push(t.join().map_err(|_| "Worker panicked")??);
    }
    counts.sort();
    assert_eq!(counts, vec![0, 10]);
    assert_eq!(
        f.rows()?
            .iter()
            .map(|r| &r["dispatcher_token"])
            .collect::<std::collections::HashSet<_>>()
            .len(),
        1
    );
    Ok(())
}

#[test]
fn recovered_frozen_batch_precedes_new_rows_and_keeps_card_queue_and_deadline() -> Result<(), String>
{
    let f = Fixture::new()?;
    f.seed("old", 1)?;
    let batch = f.claim_wake_batch()?;
    let captured = f.listener()?;
    f.state.finish_wake_batch(
        &captured,
        &batch[0],
        "DISPATCHING",
        None,
        true,
        true,
        false,
        false,
    )?;
    f.seed("later", 2)?;
    f.state
        .db
        .execute("UPDATE local_listeners SET title='Changed title'", &[])?;
    f.state.db.execute(
        "UPDATE local_signals SET from_title='Changed sender' WHERE signal_id='old'",
        &[],
    )?;
    let recovered = LocalState::open(&f.dir)?.claim_wake_batch_for("local", &captured, false)?;
    assert_eq!(recovered.len(), 1);
    for field in ["batch_id", "batch_card", "batch_expires_at_ms"] {
        assert_eq!(recovered[0][field], batch[0][field]);
    }
    assert_ne!(
        recovered[0]["dispatcher_token"],
        batch[0]["dispatcher_token"]
    );
    assert_eq!(recovered[0]["queue_written"], 1);
    assert_eq!(recovered[0]["queue_confirmed"], 1);
    assert_eq!(f.rows()?[1]["state"], "RECEIVED");
    assert!(f.rows()?[1]["batch_id"].is_null());
    Ok(())
}

#[test]
fn explicit_locked_startup_recovery_preserves_frozen_rows() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s1", 1)?;
    let batch = f.claim_wake_batch()?;
    let reopened = LocalState::open(&f.dir)?;
    reopened.recover_dispatch_leases()?;
    let recovered = reopened.claim_wake_batch_for("local", &f.listener()?, false)?;
    assert_eq!(recovered[0]["batch_id"], batch[0]["batch_id"]);
    assert_eq!(recovered[0]["batch_card"], batch[0]["batch_card"]);
    assert_ne!(
        recovered[0]["dispatcher_token"],
        batch[0]["dispatcher_token"]
    );
    Ok(())
}

#[test]
fn whole_batch_expires_at_earliest_member_not_lease_deadline() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("early", 1)?;
    f.seed("late", 2)?;
    let early = now_ms() + 45_000;
    f.state.db.execute(
        "UPDATE local_signals SET expires_at_ms=? WHERE signal_id='early'",
        &[json!(early)],
    )?;
    let batch = f.claim_wake_batch()?;
    assert!(batch.iter().all(|r| r["batch_expires_at_ms"] == early));
    f.state.db.execute("UPDATE local_listeners SET lease_until_ms=0; UPDATE local_signals SET batch_expires_at_ms=1 WHERE batch_id IS NOT NULL", &[])?;
    f.seed("new", 3)?;
    let next = f.claim_wake_batch()?;
    assert_eq!(next.len(), 1);
    assert_eq!(next[0]["signal_id"], "new");
    assert!(f.rows()?[..2]
        .iter()
        .all(|r| r["state"] == "EXPIRED_UNSENT" && r["acknowledged_at_ms"].is_null()));
    Ok(())
}

#[test]
fn expired_received_never_wakes() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s", 1)?;
    f.state
        .db
        .execute("UPDATE local_signals SET expires_at_ms=1", &[])?;
    assert!(f.claim_wake_batch()?.is_empty());
    assert_eq!(f.rows()?[0]["state"], "EXPIRED_UNSENT");
    Ok(())
}

#[test]
fn private_signal_is_dropped_and_legacy_principal_equality_is_admitted() -> Result<(), String> {
    let f = Fixture::new()?;
    f.state.db.execute(
        "UPDATE local_listeners SET scope='self',principal_id='oidc:https://id.example.com|alice'",
        &[],
    )?;
    assert!(!admit(&f, &signal("private", "none#bob", 1), 7)?);
    assert!(f.rows()?.is_empty());
    assert!(admit(
        &f,
        &signal("own", "https://id.example.com#alice", 1),
        8
    )?);
    assert_eq!(f.rows()?[0]["state"], "RECEIVED");
    assert!(f.rows()?[0]["held_reason"].is_null());
    Ok(())
}

#[test]
fn workspace_peers_wake_normally_and_unequal_generations_are_passive() -> Result<(), String> {
    let f = Fixture::new()?;
    assert!(admit(&f, &signal("peer", "none#bob", 1), 1)?);
    assert!(admit(&f, &signal("old", "none#bob", 0), 2)?);
    assert!(admit(&f, &signal("future", "none#bob", 2), 3)?);
    let rows = f.rows()?;
    assert_eq!(rows[0]["state"], "RECEIVED");
    assert!(rows[0]["held_reason"].is_null());
    for r in &rows[1..] {
        assert_eq!(r["state"], "STORED_PASSIVE");
        assert_eq!(r["held_reason"], "stale_generation");
    }
    let batch = f.claim_wake_batch()?;
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0]["signal_id"], "peer");
    Ok(())
}

#[test]
fn duplicate_received_recovers_schedule_but_passive_redelivery_keeps_fallback() -> Result<(), String>
{
    let f = Fixture::new()?;
    let s = signal("s", "none#bob", 1);
    assert!(admit(&f, &s, 1)?);
    f.state.db.execute("DELETE FROM wake_retries", &[])?;
    assert!(!admit(&f, &s, 1)?);
    assert_eq!(
        f.state.db.query("SELECT * FROM wake_retries", &[])?.len(),
        1
    );
    let batch = f.claim_wake_batch()?;
    f.state.finish_wake_batch(
        &f.listener()?,
        &batch[0],
        "STORED_PASSIVE",
        None,
        false,
        false,
        false,
        false,
    )?;
    f.state.schedule_wake_retry_for(&f.listener()?, 30_500)?;
    let fallback = f
        .state
        .db
        .query("SELECT retry_at_ms FROM wake_retries", &[])?[0]["retry_at_ms"]
        .clone();
    assert!(!admit(&f, &s, 1)?);
    assert_eq!(
        f.state
            .db
            .query("SELECT retry_at_ms FROM wake_retries", &[])?[0]["retry_at_ms"],
        fallback
    );
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    Ok(())
}

#[test]
fn acked_members_rebatch_remaining_unread_without_losing_queue_checkpoint() -> Result<(), String> {
    for confirmed in [false, true] {
        let f = Fixture::new()?;
        f.seed("read", 1)?;
        f.seed("unread", 2)?;
        let original = f.claim_wake_batch()?;
        f.state.finish_wake_batch(
            &f.listener()?,
            &original[0],
            "DISPATCHING",
            None,
            true,
            confirmed,
            false,
            false,
        )?;
        f.state
            .acknowledge("local", "c_1", &[json!("read")], "other")?;
        f.seed("fresh", 3)?;
        let next = f.claim_wake_batch()?;
        assert_eq!(next.len(), 1);
        assert_eq!(next[0]["signal_id"], "unread");
        assert_ne!(next[0]["batch_id"], original[0]["batch_id"]);
        assert_ne!(next[0]["batch_card"], original[0]["batch_card"]);
        assert_eq!(next[0]["queue_written"], 1);
        assert_eq!(next[0]["queue_confirmed"], i64::from(confirmed));
        let fresh = &f.rows()?[2];
        assert_eq!(fresh["state"], "RECEIVED");
        assert!(fresh["batch_id"].is_null());
        f.state.finish_wake_batch(
            &f.listener()?,
            &next[0],
            "DISPATCHED",
            None,
            false,
            false,
            false,
            false,
        )?;
        let fresh = f.claim_wake_batch()?;
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0]["signal_id"], "fresh");
        assert_eq!(fresh[0]["queue_written"], 0);
        assert_eq!(fresh[0]["queue_confirmed"], 0);
    }
    Ok(())
}

#[test]
fn new_batches_separate_persisted_queue_confirmation_checkpoints() -> Result<(), String> {
    let f = Fixture::new()?;
    for (seq, id) in ["unknown-a", "confirmed", "fresh", "unknown-b"]
        .iter()
        .enumerate()
    {
        f.seed(id, seq + 1)?;
    }
    f.state.db.execute(
        "UPDATE local_signals SET queue_written=1,queue_confirmed=CASE WHEN signal_id='confirmed' THEN 1 ELSE 0 END WHERE signal_id<>'fresh'",
        &[],
    )?;
    for (ids, written, confirmed) in [
        (vec!["unknown-a", "unknown-b"], 1, 0),
        (vec!["confirmed"], 1, 1),
        (vec!["fresh"], 0, 0),
    ] {
        let batch = f.claim_wake_batch()?;
        assert_eq!(
            batch
                .iter()
                .map(|row| string(row, "signal_id"))
                .collect::<Vec<_>>(),
            ids
        );
        for row in &batch {
            assert_eq!(row["queue_written"], written);
            assert_eq!(row["queue_confirmed"], confirmed);
        }
        f.state.finish_wake_batch(
            &f.listener()?,
            &batch[0],
            "DISPATCHED",
            None,
            false,
            false,
            false,
            false,
        )?;
    }
    Ok(())
}

#[test]
fn middle_member_ack_cannot_reuse_batch_identity_for_a_different_card() -> Result<(), String> {
    let f = Fixture::new()?;
    for i in 1..=3 {
        f.seed(&format!("s{i}"), i)?;
    }
    let original = f.claim_wake_batch()?;
    f.state.finish_wake_batch(
        &f.listener()?,
        &original[0],
        "DISPATCHING",
        None,
        false,
        false,
        false,
        false,
    )?;
    f.state
        .acknowledge("local", "c_1", &[json!("s2")], "other")?;
    let next = f.claim_wake_batch()?;
    assert_eq!(next.len(), 2);
    assert_eq!(next[0]["seq"], original[0]["seq"]);
    assert_eq!(next[1]["seq"], original[2]["seq"]);
    assert_ne!(next[0]["batch_id"], original[0]["batch_id"]);
    assert_ne!(next[0]["batch_card"], original[0]["batch_card"]);
    assert_eq!(f.rows()?[1]["state"], "STORED_PASSIVE");
    Ok(())
}

#[test]
fn stale_owner_cannot_claim_or_complete_successors_batch() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s", 1)?;
    let old_listener = f.listener()?;
    let old = f.claim_wake_batch()?;
    f.state.db.execute(
        "UPDATE local_listeners SET owner_token='successor',lease_until_ms=0",
        &[],
    )?;
    assert!(f
        .state
        .claim_wake_batch_for("local", &old_listener, false)
        .is_err());
    let next = f.claim_wake_batch()?;
    f.state.schedule_wake_retry_for(&f.listener()?, 30_500)?;
    let retry = f
        .state
        .db
        .query("SELECT retry_at_ms FROM wake_retries", &[])?[0]["retry_at_ms"]
        .clone();
    f.state.schedule_wake_retry_for(&old_listener, 1500)?;
    assert_eq!(
        f.state
            .db
            .query("SELECT retry_at_ms FROM wake_retries", &[])?[0]["retry_at_ms"],
        retry
    );
    assert!(!f.state.finish_wake_batch(
        &old_listener,
        &old[0],
        "DISPATCHED",
        None,
        true,
        true,
        true,
        false
    )?);
    assert_eq!(
        f.rows()?[0]["dispatcher_token"],
        next[0]["dispatcher_token"]
    );
    assert_eq!(f.rows()?[0]["state"], "DISPATCHING");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    Ok(())
}

#[test]
fn local_lease_renewal_and_ack_cancellation_are_token_fenced() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s", 1)?;
    let batch = f.claim_wake_batch()?;
    let listener = f.listener()?;
    f.state.db.execute(
        "UPDATE local_listeners SET lease_until_ms=?",
        &[json!(now_ms() + 1000)],
    )?;
    f.state.check_dispatch(&listener, &batch[0], true)?;
    assert!(
        f.listener()?["lease_until_ms"]
            .as_u64()
            .ok_or("Missing lease")?
            > now_ms() + 28_000
    );
    f.state.acknowledge("local", "c_1", &[json!("s")], "ch")?;
    assert!(f.state.check_dispatch(&listener, &batch[0], true).is_err());
    Ok(())
}

#[test]
fn completion_preserves_concurrent_ack_and_commits_pending_hub_acks_atomically(
) -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s", 1)?;
    let batch = f.claim_wake_batch()?;
    f.state
        .db
        .execute("UPDATE local_signals SET acknowledged_at_ms=123", &[])?;
    f.state.finish_wake_batch(
        &f.listener()?,
        &batch[0],
        "DISPATCHED",
        None,
        true,
        true,
        true,
        false,
    )?;
    let row = &f.rows()?[0];
    assert_eq!(row["acknowledged_at_ms"], 123);
    assert_eq!(row["state"], "DISPATCHED");
    assert_eq!(row["queue_written"], 1);
    assert_eq!(row["queue_confirmed"], 1);
    let pending = f.state.db.query("SELECT * FROM pending_hub_acks", &[])?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["acked_at_ms"], 123);
    assert_eq!(pending[0]["channel_id"], "host:agent");
    assert!(f.listener()?["dispatcher_token"].is_null());
    Ok(())
}

#[test]
fn unlisten_racing_final_completion_cannot_business_ack_or_resurrect_dispatch() -> Result<(), String>
{
    let f = Fixture::new()?;
    f.seed("s", 1)?;
    let listener = f.listener()?;
    let batch = f.claim_wake_batch()?;
    f.state
        .db
        .execute("UPDATE local_listeners SET status='UNLISTENED'", &[])?;
    assert!(!f.state.finish_wake_batch(
        &listener,
        &batch[0],
        "DISPATCHED",
        None,
        true,
        true,
        true,
        false
    )?);
    let row = &f.rows()?[0];
    assert_eq!(row["state"], "STORED_PASSIVE");
    assert_eq!(row["held_reason"], "unlistened_before_dispatch");
    assert_eq!(row["queue_written"], 1);
    assert!(row["acknowledged_at_ms"].is_null());
    assert!(f
        .state
        .db
        .query("SELECT * FROM pending_hub_acks", &[])?
        .is_empty());
    assert!(f.listener()?["dispatcher_token"].is_null());
    f.state.db.execute(
        "UPDATE local_listeners SET status='ACTIVE',updated_at=1",
        &[],
    )?;
    let active = f.listener()?;
    assert!(f.state.pause_listener_for(&active)?);
    let paused = f.listener()?;
    assert_eq!(paused["status"], "UNLISTENED");
    assert!(paused["updated_at"].as_u64().is_some_and(|stamp| stamp > 1));
    assert!(!f
        .state
        .fence_listener_for(&active, "fenced_before_dispatch")?);
    assert_eq!(
        f.listener()?["updated_at"],
        paused["updated_at"],
        "A stale captured status cannot restamp a newer pause"
    );
    assert!(f
        .state
        .fence_listener_for(&paused, "fenced_before_dispatch")?);
    assert_eq!(
        f.listener()?["status"],
        "FENCED_INACTIVE",
        "Profile changes must still fence a retained pause"
    );
    Ok(())
}

#[test]
fn native_card_matches_baseline_escaping_sender_and_multi_signal_layout() {
    let target = json!({"title":" <Target> "});
    let one = json!({"from_user":"alice","from_host_name":"host.LOCAL","from_title":"Alice Smith","from_listener_id":"alice-smith-abcdefghijkl","message":"<x> & hello"});
    assert_eq!(
        format_wake_card(&target, std::slice::from_ref(&one), "cloud", true),
        "Cito · alice@host [↩ cloud:alice-smith-abcdefghijkl]\n\n&lt;x&gt; & hello"
    );
    for suffix in ["abcdefghijkl", "0123456789abcdef"] {
        let mut sender = one.clone();
        sender["from_listener_id"] = json!(format!("alice-smith-{suffix}"));
        assert!(!format_sender(&sender, "local", false).contains("Alice Smith"));
    }
    let mut distinct = one.clone();
    distinct["from_title"] = json!("Different title");
    assert!(format_sender(&distinct, "local", false).contains("Different title"));
    let two = json!({"from_user":"bob","from_host_name":"other.local","from_title":" Sender <B> ","from_listener_id":null,"message":"second"});
    assert_eq!(format_wake_card(&target, &[one, two], "local", false), "Cito · 2 notifications → &lt;Target&gt;\n\n[1] alice@host [↩ alice-smith-abcdefghijkl]\n&lt;x&gt; & hello\n\n[2] bob@other · Sender &lt;B&gt;\nsecond");
}

#[test]
fn six_hundred_messages_paginate_into_twelve_fifty_member_batches() -> Result<(), String> {
    let f = Fixture::new()?;
    for i in 0..600 {
        f.seed(&format!("s{i}"), i)?;
    }
    let mut batches = Vec::new();
    loop {
        let batch = f.claim_wake_batch()?;
        if batch.is_empty() {
            break;
        }
        batches.push(batch.len());
        f.state.finish_wake_batch(
            &f.listener()?,
            &batch[0],
            "DISPATCHED",
            None,
            false,
            false,
            false,
            false,
        )?;
    }
    assert_eq!(batches, vec![50; 12]);
    assert_eq!(f.state.activity_counts()?.1, 600);
    Ok(())
}

#[test]
fn additive_queue_confirmation_upgrade_keeps_v4_data_and_indexes() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s", 1)?;
    f.state.db.execute("ALTER TABLE local_signals DROP COLUMN queue_confirmed; PRAGMA user_version=6; INSERT INTO daemon_owner VALUES(1,123,456)", &[])?;
    let reopened = LocalState::open(&f.dir)?;
    assert_eq!(
        reopened
            .db
            .query("SELECT queue_confirmed,message FROM local_signals", &[])?[0],
        json!({"queue_confirmed":0,"message":"message"})
    );
    assert_eq!(
        reopened.db.query("SELECT pid FROM daemon_owner", &[])?[0]["pid"],
        123
    );
    assert_eq!(reopened.db.query("SELECT name FROM sqlite_master WHERE type='index' AND name IN ('signals_listener','signals_batch')", &[])?.len(), 2);
    assert_eq!(
        reopened.db.query("PRAGMA user_version", &[])?[0]["user_version"],
        7
    );
    Ok(())
}
