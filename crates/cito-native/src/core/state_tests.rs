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
        state.db.execute("INSERT INTO local_listeners(hub,listener_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status,binding_generation,owner_token) VALUES('local','c_1','none#alice','alice','host','host','agent','s','','Target','','workspace','ACTIVE',1,'owner')", &[])?;
        Ok(Self { state, dir })
    }
    fn listener(&self) -> Result<Value, String> {
        self.state
            .listener("local", "c_1")?
            .ok_or_else(|| "Missing fixture listener".into())
    }
    fn seed(&self, id: &str, seq: usize) -> Result<(), String> {
        self.state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) VALUES('local',?,'c_1',?,1,'alice','host.local','','message','2026-01-01T00:00:00.000Z',?,'RECEIVED')", &[json!(id), json!(seq), json!(now_ms()+60_000)])?;
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
fn full_unread_count_and_non_destructive_v4_reopen() -> Result<(), String> {
    let f = Fixture::new()?;
    for i in 0..100 {
        f.seed(&format!("s{i}"), i)?;
    }
    let (rows, unread) = f.state.activity()?;
    assert_eq!((rows.len(), unread), (80, 100));
    let batch = f.state.claim_wake_batch("local", "c_1")?;
    let before = f.listener()?;
    let second = LocalState::open(&f.dir)?;
    assert_eq!(second.activity()?.1, 100);
    assert_eq!(
        second.db.query("PRAGMA user_version", &[])?[0]["user_version"],
        4
    );
    assert_eq!(
        second.listener("local", "c_1")?.ok_or("Missing listener")?["dispatcher_token"],
        before["dispatcher_token"]
    );
    assert_eq!(f.rows()?[0]["batch_id"], batch[0]["batch_id"]);
    assert_eq!(f.rows()?[0]["state"], "DISPATCHING");
    assert!(second.claim_wake_batch("local", "c_1")?.is_empty());
    Ok(())
}

#[test]
fn independent_sqlite_handles_have_one_atomic_winner() -> Result<(), String> {
    let f = Fixture::new()?;
    for i in 0..10 {
        f.seed(&format!("s{i}"), i)?;
    }
    let handles = [LocalState::open(&f.dir)?, LocalState::open(&f.dir)?];
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = handles
        .into_iter()
        .map(|state| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                state.claim_wake_batch("local", "c_1").map(|v| v.len())
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
    let batch = f.state.claim_wake_batch("local", "c_1")?;
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
    let recovered = LocalState::open(&f.dir)?.claim_wake_batch("local", "c_1")?;
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
    let batch = f.state.claim_wake_batch("local", "c_1")?;
    let reopened = LocalState::open(&f.dir)?;
    reopened.recover_dispatch_leases()?;
    let recovered = reopened.claim_wake_batch("local", "c_1")?;
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
    let batch = f.state.claim_wake_batch("local", "c_1")?;
    assert!(batch.iter().all(|r| r["batch_expires_at_ms"] == early));
    f.state.db.execute("UPDATE local_listeners SET lease_until_ms=0; UPDATE local_signals SET batch_expires_at_ms=1 WHERE batch_id IS NOT NULL", &[])?;
    f.seed("new", 3)?;
    let next = f.state.claim_wake_batch("local", "c_1")?;
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
    assert!(f.state.claim_wake_batch("local", "c_1")?.is_empty());
    assert_eq!(f.rows()?[0]["state"], "EXPIRED_UNSENT");
    Ok(())
}

#[test]
fn private_drop_advances_cursor_and_legacy_principal_equality_is_admitted() -> Result<(), String> {
    let f = Fixture::new()?;
    f.state.db.execute(
        "UPDATE local_listeners SET scope='self',principal_id='oidc:https://id.example.com|alice'",
        &[],
    )?;
    assert!(!admit(&f, &signal("private", "none#bob", 1), 7)?);
    assert!(f.rows()?.is_empty());
    assert_eq!(f.listener()?["persisted_seq"], 7);
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
    let batch = f.state.claim_wake_batch("local", "c_1")?;
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
    let batch = f.state.claim_wake_batch("local", "c_1")?;
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
        let original = f.state.claim_wake_batch("local", "c_1")?;
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
        let next = f.state.claim_wake_batch("local", "c_1")?;
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
        let fresh = f.state.claim_wake_batch("local", "c_1")?;
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
        let batch = f.state.claim_wake_batch("local", "c_1")?;
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
    let original = f.state.claim_wake_batch("local", "c_1")?;
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
    let next = f.state.claim_wake_batch("local", "c_1")?;
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
    let old = f.state.claim_wake_batch("local", "c_1")?;
    f.state.db.execute(
        "UPDATE local_listeners SET owner_token='successor',lease_until_ms=0",
        &[],
    )?;
    assert!(f
        .state
        .claim_wake_batch_for("local", &old_listener, false)
        .is_err());
    let next = f.state.claim_wake_batch("local", "c_1")?;
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
    let batch = f.state.claim_wake_batch("local", "c_1")?;
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
    let batch = f.state.claim_wake_batch("local", "c_1")?;
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
    let batch = f.state.claim_wake_batch("local", "c_1")?;
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
    Ok(())
}

#[test]
fn native_card_matches_baseline_escaping_sender_and_multi_signal_layout() {
    let target = json!({"title":" <Target> "});
    let one = json!({"from_user":"alice","from_host_name":"host.LOCAL","from_title":"Alice Smith","from_listener_id":"alice-smith-abcdefghijkl","message":"<x> & hello"});
    assert_eq!(
        format_wake_card(&target, std::slice::from_ref(&one), "cloud", true),
        "🔔 Cito · alice@host [↩ cloud:alice-smith-abcdefghijkl]\n\n&lt;x&gt; & hello"
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
    assert_eq!(format_wake_card(&target, &[one, two], "local", false), "🔔 Cito · 2 notifications → &lt;Target&gt;\n\n[1] alice@host [↩ alice-smith-abcdefghijkl]\n&lt;x&gt; & hello\n\n[2] bob@other · Sender &lt;B&gt;\nsecond");
}

#[test]
fn six_hundred_messages_paginate_into_twelve_fifty_member_batches() -> Result<(), String> {
    let f = Fixture::new()?;
    for i in 0..600 {
        f.seed(&format!("s{i}"), i)?;
    }
    let mut batches = Vec::new();
    loop {
        let batch = f.state.claim_wake_batch("local", "c_1")?;
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
    assert_eq!(f.state.activity()?.1, 600);
    Ok(())
}

#[test]
fn additive_queue_confirmation_upgrade_keeps_v4_data_and_indexes() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s", 1)?;
    f.state.db.execute("ALTER TABLE local_signals DROP COLUMN queue_confirmed; INSERT INTO daemon_owner VALUES(1,123,456)", &[])?;
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
        4
    );
    Ok(())
}
