use super::super::{database::Database, hub::HubEngine, registration::register_listener};
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{path::PathBuf, sync::Arc, thread, time::Instant};

struct Worker {
    thread: Option<thread::JoinHandle<Result<(), String>>>,
    release: PathBuf,
}
impl Worker {
    fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
    fn join(mut self) -> Result<Result<(), String>, String> {
        self.thread
            .take()
            .ok_or_else(|| "Missing worker".to_string())?
            .join()
            .map_err(|_| "Worker panicked".into())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = std::fs::write(&self.release, b"cleanup");
            let _ = thread.join();
        }
    }
}

struct Fixture {
    dir: PathBuf,
    state: LocalState,
    engine: Arc<HubEngine>,
    client: HubClient,
    row: Value,
}
impl Fixture {
    fn new() -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!(
            "cito-receiver-{}",
            crate::plugins::random_uuid_v4()
        ));
        std::fs::create_dir_all(dir.join("home")).map_err(|e| e.to_string())?;
        let state = LocalState::open(&dir)?;
        let engine = Arc::new(HubEngine::open(&dir.join("hub"))?);
        engine.set_plugin_roots(&[])?;
        let client = HubClient::new(
            "local".into(),
            json!({"url":"local://"}),
            "alice",
            Some(engine.clone()),
        );
        let registration = register_listener(
            &state,
            &client,
            "host",
            &json!({"about":"Test receiver","scope":"workspace"}),
            &json!({"harness":"fixture","session_id":"session","title":"Target","cwd":dir,"endpoint_pid":null,"repo":null,"branch":null}),
        )?;
        let row = state
            .listener("local", string(&registration, "canonical_listener_id"))?
            .ok_or("Missing listener")?;
        Ok(Self {
            dir,
            state,
            engine,
            client,
            row,
        })
    }
    fn seed(&self, id: &str) -> Result<(), String> {
        let signal = json!({"signal_id":id,"to":self.row["listener_id"],"target_generation":self.row["binding_generation"],"sender_endpoint_key":"fixture","from":{"principal_id":"none#bob","user":"bob","host_id":"other","host_name":"other.local","harness":"fixture","listener_id":null,"title":"Sender","about":null},"message":format!("message <{id}>"),"in_reply_to":null});
        let published = self.client.rpc("js_publish", &json!({"subject":format!("cito.signal.{}", string(&self.row,"listener_id")),"data_b64":STANDARD.encode(signal.to_string()),"opts":{"msgID":id}}))?;
        self.state.admit(
            "local",
            &self.row,
            &signal,
            published["seq"].as_u64().ok_or("Missing sequence")?,
            "2026-01-01T00:00:00.000Z",
            now_ms() + 120_000,
            &Value::Null,
        )?;
        Ok(())
    }
    fn plugin(&self, body: &str) -> PluginDescriptor {
        let script = format!("import json,sys,os,time\nr=json.loads(sys.stdin.readline())\nroot=r['state_dir']\nos.environ['HOME']=os.path.join(root,'home')\nwith open(os.path.join(root,'requests.jsonl'),'a') as f: f.write(json.dumps(r)+'\\n')\n{body}\n");
        PluginDescriptor {
            id: "fixture".into(),
            kind: "agent-harness".into(),
            display_name: "Fixture".into(),
            tier: "test".into(),
            sf_symbol: String::new(),
            tint: String::new(),
            cli_flag: false,
            plugin_dir: self.dir.clone(),
            is_builtin: false,
            command: vec!["/usr/bin/python3".into(), "-c".into(), script],
            channel_env: None,
            fields: vec![],
        }
    }
    fn dispatch(&self, plugin: &PluginDescriptor) -> Result<(), String> {
        dispatch_listener(&self.state, &self.client, &self.row, plugin, &self.dir)
    }
    fn rows(&self) -> Result<Vec<Value>, String> {
        self.state
            .db
            .query("SELECT * FROM local_signals ORDER BY seq", &[])
    }
    fn requests(&self) -> Result<Vec<Value>, String> {
        match std::fs::read_to_string(self.dir.join("requests.jsonl")) {
            Ok(s) => s
                .lines()
                .map(|line| serde_json::from_str(line).map_err(|e| e.to_string()))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.to_string()),
        }
    }
    fn hub_db(&self) -> Result<Database, String> {
        Database::open(&self.dir.join("hub/hub.sqlite"))
    }
    fn worker(&self, plugin: PluginDescriptor) -> Worker {
        let (state, client, row, dir) = (
            self.state.clone(),
            self.client.clone(),
            self.row.clone(),
            self.dir.clone(),
        );
        Worker {
            release: self.dir.join("release"),
            thread: Some(thread::spawn(move || {
                dispatch_listener(&state, &client, &row, &plugin, &dir)
            })),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn wait_until(mut predicate: impl FnMut() -> bool, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        if Instant::now() >= deadline {
            return Err("Timed out waiting for fixture transition".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
const DELIVER: &str = "print(json.dumps({'ok':True,'outcome':'delivered'}))";
const WAIT: &str = "with open(os.path.join(root,'entered'),'w') as f: f.write(str(os.getpid()))\nwhile not os.path.exists(os.path.join(root,'release')): time.sleep(.01)\nprint(json.dumps({'ok':True,'outcome':'delivered'}))";
fn child_gone(f: &Fixture) -> Result<(), String> {
    let pid: i32 = std::fs::read_to_string(f.dir.join("entered"))
        .map_err(|e| e.to_string())?
        .parse()
        .map_err(|e| format!("Invalid child PID: {e}"))?;
    // SAFETY: signal zero only inspects the already-reaped fixture child's PID.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    Ok(())
}

#[test]
fn rpc_error_fails_closed_even_with_one_channel_and_preserves_frozen_retry() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    drop(listener);
    let client = HubClient::new(
        "local".into(),
        json!({"url":format!("http://{address}")}),
        "alice",
        None,
    );
    assert!(dispatch_listener(&f.state, &client, &f.row, &f.plugin(DELIVER), &f.dir).is_err());
    assert!(f.requests()?.is_empty());
    let failed = f.rows()?;
    assert_eq!(failed[0]["state"], "RECEIVED");
    assert!(!failed[0]["batch_card"].is_null());
    assert!(f
        .state
        .listener("local", string(&f.row, "listener_id"))?
        .ok_or("Missing listener")?["dispatcher_token"]
        .is_null());
    assert_eq!(
        f.state.db.query("SELECT * FROM wake_retries", &[])?.len(),
        1
    );
    f.dispatch(&f.plugin(DELIVER))?;
    let requests = f.requests()?;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["batch_id"], failed[0]["batch_id"]);
    assert_eq!(requests[0]["text"], failed[0]["batch_card"]);
    assert_eq!(
        requests[0]["text"],
        "🔔 Cito · bob@other · Sender\n\nmessage &lt;s&gt;"
    );
    assert_eq!(f.rows()?[0]["state"], "DISPATCHED");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_number());
    Ok(())
}

#[test]
fn missing_and_acked_claim_members_reconcile_without_partially_claiming_unread(
) -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("missing")?;
    f.seed("alreadyread")?;
    f.seed("remaining")?;
    f.client.rpc("signal_ack", &json!({"node_id":f.row["listener_id"],"signal_ids":["alreadyread"],"channel_id":"sibling:fixture"}))?;
    f.hub_db()?
        .execute("DELETE FROM hub_signals WHERE signal_id='missing'", &[])?;
    let plugin = f.plugin(DELIVER);
    f.dispatch(&plugin)?;
    assert!(f.requests()?.is_empty());
    let rows = f.rows()?;
    for r in &rows[..2] {
        assert_eq!(r["state"], "STORED_PASSIVE");
        assert!(r["acknowledged_at_ms"].is_number());
    }
    assert_eq!(rows[2]["state"], "RECEIVED");
    assert!(rows[2]["batch_id"].is_null());
    assert!(f.hub_db()?.query(
        "SELECT claimed_by_channel FROM hub_signals WHERE signal_id='remaining'",
        &[]
    )?[0]["claimed_by_channel"]
        .is_null());
    f.dispatch(&plugin)?;
    let requests = f.requests()?;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]["signals"]
            .as_array()
            .ok_or("Missing signals")?
            .len(),
        1
    );
    assert_eq!(requests[0]["signals"][0]["signal_id"], "remaining");
    Ok(())
}

#[test]
fn losing_channel_stays_unread_until_fallback_then_can_recover_without_business_ack(
) -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    f.client.rpc("signal_claim_dispatch", &json!({"node_id":f.row["listener_id"],"signal_ids":["s"],"channel_id":"sibling:fixture","lease_ms":30_000}))?;
    let start = now_ms();
    let plugin = f.plugin(DELIVER);
    f.dispatch(&plugin)?;
    assert!(f.requests()?.is_empty());
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    let retry = f
        .state
        .db
        .query("SELECT retry_at_ms FROM wake_retries", &[])?[0]["retry_at_ms"]
        .as_u64()
        .ok_or("Missing retry")?;
    assert!(retry >= start + 30_500 && retry <= now_ms() + 30_500);
    poll_listener(&f.state, &f.client, &f.row, Some(&plugin), &f.dir, |_| {})?;
    assert!(f.requests()?.is_empty());
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    // Advance persisted deadlines instead of spending 30 seconds of wall time.
    f.hub_db()?
        .execute("UPDATE hub_signals SET claim_until_ms=1", &[])?;
    f.state
        .db
        .execute("UPDATE wake_retries SET retry_at_ms=0", &[])?;
    poll_listener(&f.state, &f.client, &f.row, Some(&plugin), &f.dir, |_| {})?;
    assert_eq!(f.requests()?.len(), 1);
    assert_eq!(f.rows()?[0]["state"], "DISPATCHED");
    Ok(())
}

#[test]
fn final_response_checkpoint_survives_protocol_retry_before_later_arrivals() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("first")?;
    let plugin=f.plugin("if not r['queue_written']:\n with open(os.path.join(root,'insertions'),'a') as f: f.write('inserted\\n')\nn=sum(1 for _ in open(os.path.join(root,'requests.jsonl')))\nif n==1: print(json.dumps({'ok':True,'outcome':'driver_unavailable','queue_written':True,'queue_confirmed':True}))\nelse: print(json.dumps({'ok':True,'outcome':'delivered_unconfirmed','queue_written':True,'open_confirmed':True}))");
    f.dispatch(&plugin)?;
    assert_eq!(f.rows()?[0]["state"], "DISPATCHING");
    assert_eq!(f.rows()?[0]["queue_written"], 1);
    assert_eq!(f.rows()?[0]["queue_confirmed"], 1);
    f.seed("later")?;
    f.dispatch(&plugin)?;
    let requests = f.requests()?;
    assert_eq!(requests.len(), 2);
    assert_eq!(
        std::fs::read_to_string(f.dir.join("insertions")).map_err(|e| e.to_string())?,
        "inserted\n"
    );
    assert_eq!(requests[0]["queue_written"], false);
    assert_eq!(requests[1]["queue_written"], true);
    assert_eq!(requests[1]["queue_confirmed"], true);
    assert_eq!(requests[0]["batch_id"], requests[1]["batch_id"]);
    assert_eq!(requests[0]["text"], requests[1]["text"]);
    assert_eq!(
        requests[1]["signals"]
            .as_array()
            .ok_or("Missing signals")?
            .len(),
        1
    );
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_number());
    assert_eq!(f.rows()?[1]["state"], "RECEIVED");
    Ok(())
}

#[test]
fn failed_subprocess_retries_frozen_batch_without_mixing_new_arrivals() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("first")?;
    let plugin=f.plugin("n=sum(1 for _ in open(os.path.join(root,'requests.jsonl')))\nif n==1: sys.exit(2)\nprint(json.dumps({'ok':True,'outcome':'delivered_unconfirmed'}))");
    assert!(f.dispatch(&plugin).is_err());
    assert_eq!(f.rows()?[0]["state"], "DISPATCHING");
    assert!(f.rows()?[0]["dispatcher_token"].is_null());
    f.seed("later")?;
    f.dispatch(&plugin)?;
    let requests = f.requests()?;
    assert_eq!(requests[0]["batch_id"], requests[1]["batch_id"]);
    assert_eq!(requests[0]["text"], requests[1]["text"]);
    assert_eq!(f.rows()?[0]["state"], "DISPATCHED");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    assert_eq!(f.rows()?[1]["state"], "RECEIVED");
    Ok(())
}

#[test]
fn frozen_restart_preserves_queue_written_and_does_not_ack_unknown_insertion() -> Result<(), String>
{
    let f = Fixture::new()?;
    f.seed("s")?;
    let batch = f
        .state
        .claim_wake_batch("local", string(&f.row, "listener_id"))?;
    f.state.db.execute(
        "UPDATE local_signals SET queue_written=1,batch_card='exact persisted card'",
        &[],
    )?;
    let reopened = LocalState::open(&f.dir)?;
    reopened.recover_dispatch_leases()?;
    dispatch_listener(&reopened,&f.client,&f.row,&f.plugin("print(json.dumps({'ok':True,'outcome':'delivered_unconfirmed','queue_written':True,'open_confirmed':True}))"),&f.dir)?;
    let request = &f.requests()?[0];
    assert_eq!(request["batch_id"], batch[0]["batch_id"]);
    assert_eq!(request["text"], "exact persisted card");
    assert_eq!(request["queue_written"], true);
    assert_eq!(request["queue_confirmed"], false);
    assert_eq!(f.rows()?[0]["queue_written"], 1);
    assert_eq!(f.rows()?[0]["queue_confirmed"], 0);
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    Ok(())
}

#[test]
fn session_switch_detaches_only_local_channel_and_preserves_sibling_generation(
) -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    let (mut remote, revision) = f
        .client
        .kv_get(string(&f.row, "listener_id"))?
        .ok_or("Missing registration")?;
    remote["channels"]["sibling:fixture"] = json!({"channel_id":"sibling:fixture","kind":"fixture","host_id":"sibling","host_name":"sibling","delivery_mode":"wake_exclusive","priority":100,"state":"ACTIVE","updated_at_ms":now_ms()});
    f.client
        .kv_save(string(&f.row, "listener_id"), &remote, Some(revision))?;
    f.dispatch(&f.plugin("print(json.dumps({'ok':True,'outcome':'session_switched'}))"))?;
    let (after, _) = f
        .client
        .kv_get(string(&f.row, "listener_id"))?
        .ok_or("Missing registration")?;
    assert_eq!(after["channels"]["host:fixture"]["state"], "UNLISTENED");
    assert_eq!(after["channels"]["sibling:fixture"]["state"], "ACTIVE");
    assert_eq!(after["state"], "ACTIVE");
    assert_eq!(after["binding_generation"], remote["binding_generation"]);
    assert_eq!(after["owner_token"], remote["owner_token"]);
    assert_eq!(
        f.state
            .listener("local", string(&f.row, "listener_id"))?
            .ok_or("Missing listener")?["status"],
        "UNLISTENED"
    );
    Ok(())
}

#[test]
fn in_flight_rebind_aborts_and_reaps_real_child_without_touching_new_owner() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    let worker = f.worker(f.plugin(WAIT));
    wait_until(|| f.dir.join("entered").exists(), Duration::from_secs(3))?;
    f.state.db.execute("UPDATE local_listeners SET status='REBINDING',binding_generation=binding_generation+1,owner_token='successor'", &[])?;
    wait_until(|| worker.is_finished(), Duration::from_secs(3))?;
    assert!(worker.join().map_err(|_| "Worker panicked")?.is_err());
    child_gone(&f)?;
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    assert_eq!(
        f.state
            .listener("local", string(&f.row, "listener_id"))?
            .ok_or("Missing listener")?["owner_token"],
        "successor"
    );
    Ok(())
}

#[test]
fn closed_hub_aborts_owned_child_promptly() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    let worker = f.worker(f.plugin(WAIT));
    wait_until(|| f.dir.join("entered").exists(), Duration::from_secs(3))?;
    f.client
        .closed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    wait_until(|| worker.is_finished(), Duration::from_secs(3))?;
    assert!(worker.join().map_err(|_| "Worker panicked")?.is_err());
    child_gone(&f)?;
    assert_eq!(f.rows()?[0]["state"], "DISPATCHING");
    Ok(())
}

#[test]
fn single_effect_completion_stays_dispatched_and_preserves_concurrent_inbox_ack(
) -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    let worker = f.worker(f.plugin(WAIT));
    wait_until(|| f.dir.join("entered").exists(), Duration::from_secs(3))?;
    f.state.acknowledge(
        "local",
        string(&f.row, "listener_id"),
        &[json!("s")],
        "other",
    )?;
    let ack = f.rows()?[0]["acknowledged_at_ms"].clone();
    std::fs::write(f.dir.join("release"), b"complete").map_err(|e| e.to_string())?;
    wait_until(|| worker.is_finished(), Duration::from_secs(3))?;
    worker.join()??;
    child_gone(&f)?;
    assert_eq!(f.rows()?[0]["acknowledged_at_ms"], ack);
    assert_eq!(f.rows()?[0]["state"], "DISPATCHED");
    Ok(())
}

#[test]
fn ten_second_renewal_extends_hub_and_local_leases_while_real_child_is_owned() -> Result<(), String>
{
    let f = Fixture::new()?;
    f.seed("s")?;
    let worker = f.worker(f.plugin(WAIT));
    wait_until(|| f.dir.join("entered").exists(), Duration::from_secs(3))?;
    let db = f.hub_db()?;
    let initial = db.query("SELECT claim_until_ms FROM hub_signals", &[])?[0]["claim_until_ms"]
        .as_u64()
        .ok_or("Missing claim deadline")?;
    let renewed = wait_until(
        || {
            db.query("SELECT claim_until_ms FROM hub_signals", &[])
                .ok()
                .is_some_and(|r| r[0]["claim_until_ms"].as_u64().unwrap_or(0) >= initial + 9000)
                && f.state
                    .listener("local", string(&f.row, "listener_id"))
                    .ok()
                    .flatten()
                    .is_some_and(|r| r["lease_until_ms"].as_u64().unwrap_or(0) >= initial + 9000)
        },
        Duration::from_secs(13),
    );
    // Release and join even if the renewal assertion fails, so no child escapes.
    std::fs::write(f.dir.join("release"), b"done").map_err(|e| e.to_string())?;
    let result = worker.join().map_err(|_| "Worker panicked")?;
    renewed?;
    result?;
    assert_eq!(f.rows()?[0]["state"], "DISPATCHED");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_number());
    assert_eq!(f.requests()?.len(), 1);
    Ok(())
}

#[test]
fn renewal_claim_loss_aborts_unfinished_real_child_and_recovers_retry_schedule(
) -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    let worker = f.worker(f.plugin(WAIT));
    wait_until(|| f.dir.join("entered").exists(), Duration::from_secs(3))?;
    f.hub_db()?.execute(
        "UPDATE hub_signals SET claimed_by_channel='winner:fixture',claim_until_ms=?",
        &[json!(now_ms() + 60_000)],
    )?;
    let stopped = wait_until(|| worker.is_finished(), Duration::from_secs(13));
    if stopped.is_err() {
        std::fs::write(f.dir.join("release"), b"done").map_err(|e| e.to_string())?;
    }
    let result = worker.join().map_err(|_| "Worker panicked")?;
    stopped?;
    assert!(result.is_err());
    child_gone(&f)?;
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    let retry = f
        .state
        .db
        .query("SELECT retry_at_ms FROM wake_retries", &[])?;
    assert_eq!(retry.len(), 1);
    assert!(
        retry[0]["retry_at_ms"]
            .as_u64()
            .ok_or("Missing retry deadline")?
            >= now_ms() + 30_000
    );
    Ok(())
}

#[test]
fn unavailable_driver_keeps_signal_passive_unread_without_fabricating_queue_insertion(
) -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    f.dispatch(&f.plugin(
        "print(json.dumps({'ok':True,'outcome':'driver_unavailable','queue_written':False}))",
    ))?;
    let row = &f.rows()?[0];
    assert_eq!(row["state"], "STORED_PASSIVE");
    assert_eq!(row["held_reason"], "driver_unavailable");
    assert_eq!(row["queue_written"], 0);
    assert!(row["acknowledged_at_ms"].is_null());
    Ok(())
}

#[test]
fn unlisten_aborts_child_and_preserves_unlistened_passive_reason() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    let worker = f.worker(f.plugin(WAIT));
    wait_until(|| f.dir.join("entered").exists(), Duration::from_secs(3))?;
    f.state
        .db
        .execute("UPDATE local_listeners SET status='UNLISTENED'", &[])?;
    wait_until(|| worker.is_finished(), Duration::from_secs(3))?;
    assert!(worker.join()?.is_err());
    child_gone(&f)?;
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    assert_eq!(f.rows()?[0]["held_reason"], "unlistened_before_dispatch");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    Ok(())
}

#[test]
fn batch_expiry_aborts_owned_child_and_expires_every_member() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("early")?;
    f.seed("late")?;
    f.state.db.execute(
        "UPDATE local_signals SET expires_at_ms=? WHERE signal_id='early'",
        &[json!(now_ms() + 1000)],
    )?;
    let worker = f.worker(f.plugin(WAIT));
    wait_until(|| f.dir.join("entered").exists(), Duration::from_secs(3))?;
    wait_until(|| worker.is_finished(), Duration::from_secs(3))?;
    assert!(worker.join()?.is_err());
    child_gone(&f)?;
    assert!(f
        .rows()?
        .iter()
        .all(|r| r["state"] == "EXPIRED_UNSENT" && r["acknowledged_at_ms"].is_null()));
    Ok(())
}

#[test]
fn remote_preflight_generation_mismatch_fences_captured_listener_before_spawn() -> Result<(), String>
{
    let f = Fixture::new()?;
    f.seed("s")?;
    let (mut remote, revision) = f
        .client
        .kv_get(string(&f.row, "listener_id"))?
        .ok_or("Missing registration")?;
    remote["binding_generation"] = json!(
        remote["binding_generation"]
            .as_u64()
            .ok_or("Missing generation")?
            + 1
    );
    remote["owner_token"] = json!(crate::plugins::random_uuid_v4());
    f.client
        .kv_save(string(&f.row, "listener_id"), &remote, Some(revision))?;
    assert!(f.dispatch(&f.plugin(DELIVER)).is_err());
    assert!(f.requests()?.is_empty());
    assert_eq!(
        f.state
            .listener("local", string(&f.row, "listener_id"))?
            .ok_or("Missing listener")?["status"],
        "FENCED_INACTIVE"
    );
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    Ok(())
}

#[test]
fn durable_remote_ack_reconciliation_works_after_transport_cursor_has_advanced(
) -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    poll_listener(&f.state, &f.client, &f.row, None, &f.dir, |_| {})?;
    f.client.rpc(
        "signal_ack",
        &json!({"node_id":f.row["listener_id"],"signal_ids":["s"],"channel_id":"sibling:fixture"}),
    )?;
    poll_listener(&f.state, &f.client, &f.row, None, &f.dir, |_| {})?;
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_number());
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    assert!(f.requests()?.is_empty());
    Ok(())
}
