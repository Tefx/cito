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
            order: i64::MAX,
            sf_symbol: String::new(),
            tint: String::new(),
            cli_flag: false,
            plugin_dir: self.dir.clone(),
            is_builtin: false,
            command: vec!["/usr/bin/python3".into(), "-c".into(), script],
            channel_env: None,
            fields: vec![],
            post_setup_hint: None,
            post_disconnect_hint: None,
        }
    }
    fn dispatch(&self, plugin: &PluginDescriptor) -> Result<(), String> {
        dispatch_listener(
            &self.state,
            &self.client,
            &self.row,
            plugin,
            &self.dir,
            Duration::from_secs(10),
        )
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
                dispatch_listener(
                    &state,
                    &client,
                    &row,
                    &plugin,
                    &dir,
                    Duration::from_millis(150),
                )
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
fn malformed_remote_registrations_return_errors_without_overwriting_kv() -> Result<(), String> {
    let f = Fixture::new()?;
    let id = string(&f.row, "listener_id");
    let mut malformed_channel = f.client.kv_get(id)?.ok_or("Missing registration")?.0;
    malformed_channel["channels"]["host:fixture"] = json!([]);
    for remote in [
        json!([]),
        json!(42),
        json!(true),
        json!("invalid"),
        Value::Null,
        malformed_channel,
    ] {
        let revision = f.client.kv_get(id)?.ok_or("Missing registration")?.1;
        f.client.kv_save(id, &remote, Some(revision))?;
        let error = detach_channel_expected(&f.client, id, "host:fixture", &f.row)
            .expect_err("Malformed registrations must reject detach");
        assert!(error.starts_with("INVALID_INPUT: Listener"));
        assert_eq!(
            f.client.kv_get(id)?.ok_or("Missing registration")?.0,
            remote
        );
        if !remote.is_object() {
            assert_eq!(
                register_listener(
                    &f.state,
                    &f.client,
                    "host",
                    &json!({"about":"Test receiver"}),
                    &json!({"harness":"fixture","session_id":"session"}),
                ),
                Err("INVALID_INPUT: Listener must be an object".into()),
            );
        }
    }
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
    assert!(dispatch_listener(
        &f.state,
        &client,
        &f.row,
        &f.plugin(DELIVER),
        &f.dir,
        Duration::from_secs(10)
    )
    .is_err());
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
fn partially_acked_checkpoint_keeps_plugin_retry_separate_from_fresh_arrivals() -> Result<(), String>
{
    for confirmed in [false, true] {
        let f = Fixture::new()?;
        f.seed("read")?;
        f.seed("unread")?;
        let plugin = f.plugin(&format!(
            "if not r['queue_written']:\n with open(os.path.join(root,'insertions'),'a') as f: f.write(json.dumps([s['signal_id'] for s in r['signals']])+'\\n')\nn=sum(1 for _ in open(os.path.join(root,'requests.jsonl')))\nprint(json.dumps({{'ok':True,'outcome':'driver_unavailable' if n==1 else 'delivered_unconfirmed','queue_written':True,'insertion_unknown':{},'open_confirmed':True}}))",
            if confirmed { "False" } else { "True" }
        ));
        f.dispatch(&plugin)?;
        f.state.acknowledge(
            "local",
            string(&f.row, "listener_id"),
            &[json!("read")],
            "other",
        )?;
        f.client.rpc(
            "signal_ack",
            &json!({"node_id":f.row["listener_id"],"signal_ids":["read"],"channel_id":"other"}),
        )?;
        f.seed("fresh")?;
        f.dispatch(&plugin)?;
        let requests = f.requests()?;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1]["queue_written"], true);
        assert_eq!(requests[1]["queue_confirmed"], confirmed);
        assert_eq!(requests[1]["signals"][0]["signal_id"], "unread");
        assert_eq!(requests[1]["signals"].as_array().map(Vec::len), Some(1));
        let rows = f.rows()?;
        assert_eq!(rows[1]["queue_written"], 1);
        assert_eq!(rows[1]["queue_confirmed"], i64::from(confirmed));
        assert_eq!(rows[1]["acknowledged_at_ms"].is_number(), confirmed);
        assert_eq!(rows[2]["state"], "RECEIVED");
        f.dispatch(&plugin)?;
        let fresh = &f.requests()?[2];
        assert_eq!(fresh["queue_written"], false);
        assert_eq!(fresh["queue_confirmed"], false);
        assert_eq!(fresh["signals"][0]["signal_id"], "fresh");
        assert_eq!(fresh["signals"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            std::fs::read_to_string(f.dir.join("insertions")).map_err(|e| e.to_string())?,
            "[\"read\", \"unread\"]\n[\"fresh\"]\n"
        );
    }
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
    dispatch_listener(&reopened,&f.client,&f.row,&f.plugin("print(json.dumps({'ok':True,'outcome':'delivered_unconfirmed','queue_written':True,'open_confirmed':True}))"),&f.dir,Duration::from_secs(10))?;
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
    remote["owner_token"] = json!("legacy-sibling-owner");
    f.client
        .kv_save(string(&f.row, "listener_id"), &remote, Some(revision))?;
    f.state
        .db
        .execute("UPDATE local_listeners SET updated_at=1", &[])?;
    let dispatched_at = now_ms();
    f.dispatch(&f.plugin("print(json.dumps({'ok':True,'outcome':'session_switched'}))"))?;
    let revoked = f
        .state
        .listener("local", string(&f.row, "listener_id"))?
        .ok_or("Missing revoked listener")?;
    assert!(revoked["updated_at"]
        .as_u64()
        .is_some_and(|stamp| stamp > 1 && stamp >= dispatched_at));
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
    let worker = f.worker(f.plugin(&WAIT.replace("'delivered'", "'session_switched'")));
    wait_until(|| f.dir.join("entered").exists(), Duration::from_secs(3))?;
    let id = string(&f.row, "listener_id");
    detach_channel_expected(&f.client, id, "host:fixture", &f.row)?;
    assert_eq!(
        f.state
            .listener("local", id)?
            .ok_or("Missing local binding")?["status"],
        "ACTIVE"
    );
    assert_eq!(
        f.client
            .kv_get(id)?
            .ok_or("Missing detached registration")?
            .0["channels"]["host:fixture"]["state"],
        "UNLISTENED"
    );
    let replacement = register_listener(
        &f.state,
        &f.client,
        "host",
        &json!({"about":"Replacement"}),
        &json!({"harness":"fixture","session_id":"session","title":"Target","cwd":f.dir,"endpoint_pid":null,"repo":null,"branch":null}),
    )?;
    assert!(replacement["binding_generation"].as_u64() > f.row["binding_generation"].as_u64());
    let successor = f
        .state
        .listener("local", id)?
        .ok_or("Missing replacement")?;
    assert_ne!(
        successor["owner_token"], f.row["owner_token"],
        "A remotely detached binding cannot reuse the active local owner"
    );
    std::fs::write(f.dir.join("release"), b"complete old session switch")
        .map_err(|e| e.to_string())?;
    wait_until(|| worker.is_finished(), Duration::from_secs(3))?;
    assert!(worker.join().map_err(|_| "Worker panicked")?.is_err());
    child_gone(&f)?;
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    let after = f.state.listener("local", id)?.ok_or("Missing listener")?;
    assert_eq!(after["status"], "ACTIVE");
    assert_eq!(after["owner_token"], successor["owner_token"]);
    assert_eq!(after["binding_generation"], successor["binding_generation"]);
    assert_eq!(
        f.client.kv_get(id)?.ok_or("Missing active replacement")?.0["channels"]["host:fixture"]
            ["state"],
        "ACTIVE"
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
fn ten_second_renewal_extends_leases_and_channel_heartbeat_while_real_child_is_owned(
) -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    let worker = f.worker(f.plugin(WAIT));
    wait_until(|| f.dir.join("entered").exists(), Duration::from_secs(3))?;
    let db = f.hub_db()?;
    let id = string(&f.row, "listener_id");
    let channel = "host:fixture";
    let (mut registration, revision) = f.client.kv_get(id)?.ok_or("Missing registration")?;
    let initial_heartbeat = now_ms();
    // Model a wake approaching the channel's TTL without a minute-long sleep.
    registration["channels"][channel]["updated_at_ms"] = json!(initial_heartbeat - 55_000);
    registration["updated_at_ms"] = json!(initial_heartbeat - 55_000);
    registration["channels"]["sibling:fixture"] = json!({"state":"ACTIVE","updated_at_ms":1});
    registration["owner_token"] = json!("legacy-sibling-owner");
    let sibling = registration["channels"]["sibling:fixture"].clone();
    f.client.kv_save(id, &registration, Some(revision))?;
    let initial = db.query("SELECT claim_until_ms FROM hub_signals", &[])?[0]["claim_until_ms"]
        .as_u64()
        .ok_or("Missing claim deadline")?;
    let renewed = wait_until(
        || {
            db.query("SELECT claim_until_ms FROM hub_signals", &[])
                .ok()
                .is_some_and(|r| r[0]["claim_until_ms"].as_u64().unwrap_or(0) >= initial + 100)
                && f.state
                    .listener("local", string(&f.row, "listener_id"))
                    .ok()
                    .flatten()
                    .is_some_and(|r| r["lease_until_ms"].as_u64().unwrap_or(0) >= initial + 100)
                && f.client.kv_get(id).ok().flatten().is_some_and(|(r, _)| {
                    r["channels"][channel]["updated_at_ms"]
                        .as_u64()
                        .unwrap_or(0)
                        >= initial_heartbeat + 100
                        && r["updated_at_ms"] == r["channels"][channel]["updated_at_ms"]
                        && r["channels"]["sibling:fixture"] == sibling
                        && r["binding_generation"] == registration["binding_generation"]
                        && r["owner_token"] == registration["owner_token"]
                        && super::super::lease::evaluate_node_lease(&r, now_ms()) == "ACTIVE"
                })
        },
        Duration::from_millis(900),
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
    let stopped = wait_until(|| worker.is_finished(), Duration::from_millis(900));
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
    f.state
        .db
        .execute("UPDATE wake_retries SET retry_at_ms=0", &[])?;
    let before = now_ms();
    f.dispatch(&f.plugin(
        "print(json.dumps({'ok':True,'outcome':'driver_unavailable','queue_written':False}))",
    ))?;
    let row = &f.rows()?[0];
    assert_eq!(row["state"], "STORED_PASSIVE");
    assert_eq!(row["held_reason"], "driver_unavailable");
    assert_eq!(row["queue_written"], 0);
    assert!(row["acknowledged_at_ms"].is_null());
    let retries = f.state.db.query(
        "SELECT retry_at_ms FROM wake_retries WHERE hub='local' AND listener_id=?",
        &[f.row["listener_id"].clone()],
    )?;
    assert!(
        retries[0]["retry_at_ms"]
            .as_u64()
            .ok_or("Missing retry deadline")?
            >= before + 1500
    );
    poll_listener(&f.state, &f.client, &f.row, None, &f.dir, |_| {})?;
    assert_eq!(f.requests()?.len(), 1);
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    f.state
        .db
        .execute("UPDATE wake_retries SET retry_at_ms=0", &[])?;
    poll_listener(&f.state, &f.client, &f.row, None, &f.dir, |_| {})?;
    assert_eq!(f.rows()?[0]["state"], "RECEIVED");
    assert!(f.rows()?[0]["held_reason"].is_null());
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    for reason in ["stale_generation", "session_switched", "unlistened"] {
        f.state.db.execute(
            "UPDATE local_signals SET state='STORED_PASSIVE',held_reason=?",
            &[json!(reason)],
        )?;
        f.state.restore_passive("local", &f.row)?;
        assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    }
    f.state.db.execute(
        "UPDATE local_signals SET state='RECEIVED',held_reason=NULL",
        &[],
    )?;
    let plugin = f.plugin(DELIVER);
    poll_listener(&f.state, &f.client, &f.row, Some(&plugin), &f.dir, |_| {})?;
    assert_eq!(f.requests()?.len(), 2);
    assert_eq!(f.rows()?[0]["state"], "DISPATCHED");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_number());
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
    // Keep cold system-interpreter initialization outside the one-second expiry budget.
    plugin_request(&f.plugin(DELIVER), &json!({"state_dir": f.dir}), None, None)?;
    f.seed("early")?;
    f.seed("late")?;
    f.state.db.execute(
        "UPDATE local_signals SET expires_at_ms=? WHERE signal_id='early'",
        &[json!(now_ms() + 1000)],
    )?;
    let worker = f.worker(f.plugin(WAIT));
    wait_until(|| f.dir.join("entered").exists(), Duration::from_secs(3)).map_err(|e| {
        format!(
            "Waiting for expiring batch child to start: {e}; worker_finished={}; rows={:?}",
            worker.is_finished(),
            f.rows()
        )
    })?;
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
fn same_channel_refresh_retains_owner_and_replacement_fences_without_siblings() -> Result<(), String>
{
    let f = Fixture::new()?;
    let id = string(&f.row, "listener_id");
    let args = json!({"about":"Refreshed receiver"});
    register_listener(&f.state, &f.client, "host", &args, &f.row)?;
    let refreshed = f
        .state
        .listener("local", id)?
        .ok_or("Missing refreshed row")?;
    assert_eq!(refreshed["binding_generation"], f.row["binding_generation"]);
    assert_eq!(refreshed["owner_token"], f.row["owner_token"]);
    let mut caller = f.row.clone();
    caller["endpoint_pid"] = json!(42);
    register_listener(&f.state, &f.client, "host", &args, &caller)?;
    let replacement = f
        .state
        .listener("local", id)?
        .ok_or("Missing replacement row")?;
    assert_eq!(
        replacement["binding_generation"].as_u64(),
        f.row["binding_generation"].as_u64().map(|v| v + 1)
    );
    assert_ne!(replacement["owner_token"], f.row["owner_token"]);
    let remote = f.client.kv_get(id)?.ok_or("Missing registration")?.0;
    assert_eq!(remote["owner_token"], replacement["owner_token"]);
    assert_eq!(
        remote["channels"]["host:fixture"]["owner_token"],
        replacement["owner_token"]
    );
    Ok(())
}

#[test]
fn channel_takeover_blocks_stale_detach_and_fences_before_spawn() -> Result<(), String> {
    let f = Fixture::new()?;
    f.seed("s")?;
    let id = string(&f.row, "listener_id");
    let (mut remote, revision) = f.client.kv_get(id)?.ok_or("Missing registration")?;
    remote["channels"]["host:fixture"]["owner_token"] = json!("winner-owner");
    remote["channels"]["sibling:fixture"] = json!({"state":"ACTIVE"});
    f.client.kv_save(id, &remote, Some(revision))?;
    let error = detach_channel_expected(&f.client, id, "host:fixture", &f.row)
        .expect_err("Stale owner must not detach the winner");
    assert!(error.starts_with("STALE_GENERATION:"));
    assert_eq!(
        f.client.kv_get(id)?.ok_or("Missing registration")?.0,
        remote
    );
    assert!(f.dispatch(&f.plugin(DELIVER)).is_err());
    assert!(f.requests()?.is_empty());
    assert_eq!(
        f.state.listener("local", id)?.ok_or("Missing listener")?["status"],
        "FENCED_INACTIVE"
    );
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    assert_eq!(f.rows()?[0]["held_reason"], "fenced_before_dispatch");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
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
fn disabled_legacy_hubs_do_not_qualify_single_local_reply_ids() -> Result<(), String> {
    if std::env::var_os("CITO_REPLY_IDS_WORKER").is_none() {
        let output = std::process::Command::new(
            std::env::current_exe().map_err(|e| e.to_string())?,
        )
        .args([
            "--exact",
            "core::receiver::tests::disabled_legacy_hubs_do_not_qualify_single_local_reply_ids",
        ])
        .env("CITO_REPLY_IDS_WORKER", "1")
        .env_remove("CITO_CONFIG_PATH")
        .output()
        .map_err(|e| e.to_string())?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        return Ok(());
    }
    let f = Fixture::new()?;
    std::fs::write(f.dir.join("config.json"), json!({"defaultHub":"local","hubs":{"local":{"url":"local://"},"work":{"url":"https://hub.example.com","disabled":true}}}).to_string()).map_err(|e| e.to_string())?;
    assert!(!qualify_reply_ids(&f.dir, "local"));
    std::fs::write(f.dir.join("config.json"), json!({"defaultHub":"local","hubs":{"local":{"url":"local://"},"work":{"url":"https://hub.example.com","enabled":true}}}).to_string()).map_err(|e| e.to_string())?;
    assert!(qualify_reply_ids(&f.dir, "local"));
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
