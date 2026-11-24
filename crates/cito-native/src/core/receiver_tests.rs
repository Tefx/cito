use super::super::{
    database::Database,
    hub::{HubEngine, TestDirectory, TestEngine},
    registration::register_endpoint,
};
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
    fn assert_aborted(self, f: &Fixture, timeout: Duration) -> Result<(), String> {
        wait_until(|| self.is_finished(), timeout)?;
        assert!(self.join()?.is_err());
        child_gone(f)
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
    state: LocalState,
    engine: TestEngine,
    client: HubClient,
    row: Value,
    dir: TestDirectory,
}
impl Fixture {
    fn new() -> Result<Self, String> {
        let dir = TestDirectory(std::env::temp_dir().join(format!(
            "cito-receiver-{}",
            crate::plugins::random_uuid_v4()
        )));
        std::fs::create_dir_all(dir.join("home")).map_err(|e| e.to_string())?;
        let state = LocalState::open(&dir)?;
        let engine = TestEngine(HubEngine::open(&dir.join("hub"))?);
        engine.set_plugin_roots(&[])?;
        let client = HubClient::new(
            "local".into(),
            json!({"url":"local://"}),
            "alice",
            Some(Arc::new(engine.clone())),
        );
        let registration = register_endpoint(
            &state,
            &client,
            "host",
            &json!({"about":"Test receiver","scope":"workspace"}),
            &json!({"harness":"fixture","session_id":"session","title":"Target","cwd":dir.as_path(),"endpoint_pid":null,"repo":null,"branch":null}),
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
    fn listener_id(&self) -> &str {
        string(&self.row, "listener_id")
    }
    fn listener(&self) -> Result<Value, String> {
        self.state
            .listener("local", self.listener_id())?
            .ok_or_else(|| "Missing local listener".into())
    }
    fn binding(&self) -> Result<(Value, u64), String> {
        self.client
            .kv_get(self.listener_id())?
            .ok_or_else(|| "Missing remote binding".into())
    }
    fn retries(&self) -> Result<Vec<Value>, String> {
        self.state.db.query("SELECT * FROM wake_retries", &[])
    }
    fn seeded(ids: &[&str]) -> Result<Self, String> {
        let f = Self::new()?;
        for id in ids {
            f.seed(id)?;
        }
        Ok(f)
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
            pid_bound: true,
            auto_listen: Default::default(),
            plugin_dir: self.dir.clone(),
            is_builtin: false,
            command: vec!["/usr/bin/python3".into(), "-c".into(), script],
            channel_env: None,
            fields: vec![],
            post_setup_hint: None,
            post_disconnect_hint: None,
        }
    }
    fn probe_result(&self, plugin: &PluginDescriptor, row: &Value) -> Value {
        plugin_request(plugin, &json!({"op":"probe","session_id":row["session_id"],"endpoint_pid":row["endpoint_pid"],"process_identity":row["process_identity"],"cwd":row["cwd"],"state_dir":self.dir.as_path()}), None, Some((&self.state, row)))
            .unwrap_or_else(|error| json!({"ok":false,"error":error}))
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
    fn start_worker(&self, body: &str) -> Result<Worker, String> {
        let plugin = self.plugin(body);
        let (state, client, row, dir) = (
            self.state.clone(),
            self.client.clone(),
            self.row.clone(),
            self.dir.clone(),
        );
        let worker = Worker {
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
        };
        wait_until(|| self.dir.join("entered").exists(), Duration::from_secs(3)).map_err(|e| {
            format!(
                "Waiting for child to start: {e}; worker_finished={}; rows={:?}",
                worker.is_finished(),
                self.rows()
            )
        })?;
        Ok(worker)
    }
}
#[test]
fn transient_probe_revocation_respects_dispatch_and_lease() -> Result<(), String> {
    for definitive in [false, true] {
        let f = Fixture::seeded(&["in-flight"])?;
        let mut row = f.row.clone();
        row["consecutive_probe_failures"] = json!(2);
        f.state.db.execute(
            "UPDATE local_listeners SET lease_until_ms=?",
            &[json!(now_ms() + 60_000)],
        )?;
        apply_probe(&f.state, &f.client, &row, &json!({"ok":false}))?;
        assert_eq!(f.listener()?["status"], "ACTIVE");
        f.state
            .db
            .execute("UPDATE local_listeners SET lease_until_ms=0", &[])?;
        f.state.db.execute(
            "UPDATE local_signals SET state='DISPATCHING',dispatcher_token='worker'",
            &[],
        )?;
        apply_probe(&f.state, &f.client, &row, &json!({"ok":false}))?;
        assert_eq!(f.listener()?["status"], "ACTIVE");
        if !definitive {
            f.state
                .db
                .execute("UPDATE local_signals SET dispatcher_token=NULL", &[])?;
        }
        apply_probe(
            &f.state,
            &f.client,
            &row,
            &json!({"ok":definitive,"status":"closed_definitive"}),
        )?;
        assert_eq!(f.listener()?["status"], "UNLISTENED");
        assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    }
    Ok(())
}

#[test]
fn local_registration_failure_compensates_only_new_remote_attachments() -> Result<(), String> {
    let f = Fixture::new()?;
    f.state.db.execute("CREATE TRIGGER reject_enrollment BEFORE INSERT ON local_listeners BEGIN SELECT RAISE(ABORT,'enrollment rejected'); END", &[])?;
    for (harness, session, expected) in [
        ("fixture", "session", "ACTIVE"),
        ("fixture", "other", "UNLISTENED"),
        ("human", "default", "UNLISTENED"),
    ] {
        let error = register_endpoint(
            &f.state,
            &f.client,
            "host",
            &json!({"about":"Receiver"}),
            &json!({"harness":harness,"session_id":session}),
        )
        .unwrap_err();
        assert_eq!(error, "enrollment rejected");
        let id = crate::core::identity::listener_id(
            string(&f.client.identity, "principal_id"),
            harness,
            session,
        );
        let (remote, _) = f.client.kv_get(&id)?.ok_or("Missing remote registration")?;
        assert_eq!(
            remote["channels"][format!("host:{harness}")]["state"],
            expected
        );
    }
    Ok(())
}

#[test]
fn poll_settles_over_budget_delivery_and_admits_the_following_signal() -> Result<(), String> {
    let f = Fixture::new()?;
    for (id, message) in [
        ("oversize", "\"".repeat(1_900_000)),
        ("healthy", "Hello".into()),
    ] {
        let signal = json!({"signal_id":id,"to":f.listener_id(),"target_generation":f.row["binding_generation"],"from":{"user":"bob","host_name":"desktop"},"message":message});
        f.client.rpc(
            "js_publish",
            &json!({"subject":format!("cito.signal.{}", f.listener_id()),"data":signal}),
        )?;
    }
    let mut admitted = Vec::new();
    // The oversized item fills one Hub backlog page; the next poll must progress.
    for _ in 0..2 {
        poll_listener(&f.state, &f.client, &f.row, None, &f.dir, |s| {
            admitted.push(s["signal_id"].clone())
        })?;
    }
    assert_eq!(admitted, [json!("healthy")]);
    assert_eq!(f.state.unread_count("local", f.listener_id(), false)?, 1);
    let backlog = f.client.rpc(
        "consumer_backlog",
        &json!({"name":format!("host_{}", f.listener_id())}),
    )?;
    assert_eq!(backlog["backlog"], json!([]));
    Ok(())
}

#[test]
fn poll_fallback_preserves_the_original_hub_clock_observation() -> Result<(), String> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    let f = Fixture::new()?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let id = f.row["listener_id"].clone();
    let generation = f.row["binding_generation"].clone();
    let remote = f.client.kv_get(string(&f.row, "listener_id"))?.unwrap().0;
    let server = thread::spawn(move || {
        for (index, expected) in [
            "consumer_add",
            "consumer_backlog",
            "kv_get",
            "consumer_ack",
            "consumer_ack",
            "consumer_ack",
            "consumer_ack",
            "stream_get_message",
        ]
        .into_iter()
        .enumerate()
        {
            let mut stream = crate::core::http_boundary::accept_test_client(&listener).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let mut length = 0;
            loop {
                line.clear();
                assert_ne!(
                    reader.read_line(&mut line).unwrap(),
                    0,
                    "Truncated fixture request"
                );
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':') {
                    if key.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["op"], expected);
            let now = 10_000_000u64; // Deliberately unrelated to the local wall clock.
            let body = if expected == "consumer_add" {
                let signal = json!({"signal_id":"expiring","to":id,"target_generation":generation,"from":{"principal_id":"none#alice","user":"alice","host_name":"desktop"},"message":"near expiry"});
                json!({"ok":true,"result":{"server_now_ms":now,"backlog":[{}, {"seq":1,"data_b64":"!"},{"seq":2,"data_b64":STANDARD.encode(b"not JSON")},{"seq":3,"data_b64":STANDARD.encode(json!({"to":"c_0000000000000000"}).to_string())},{"seq":4,"created_ms":now-1_800_000+50,"data_b64":STANDARD.encode(signal.to_string())}]}})
            } else if expected == "kv_get" {
                json!({"ok":true,"result":{"revision":1,"value_b64":STANDARD.encode(remote.to_string())}})
            } else if expected == "consumer_backlog" {
                thread::sleep(Duration::from_millis(150));
                json!({"ok":false,"error":{"code":"UNAVAILABLE","message":"Backlog failed"}})
            } else {
                if expected == "consumer_ack" { assert_eq!(request["params"]["seq"], index - 2); }
                json!({"ok":true,"result":{}})
            }.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let client = HubClient::new(
        "local".into(),
        json!({"url":format!("http://{address}")}),
        "alice",
        None,
    );
    let mut row = f.row.clone();
    row["hub_url"] = client.config["url"].clone();
    f.state.db.execute(
        "UPDATE local_listeners SET hub_url=?",
        &[client.config["url"].clone()],
    )?;
    let mut admitted_at = 0;
    poll_listener(&f.state, &client, &row, None, &f.dir, |_| {
        admitted_at = now_ms()
    })?;
    server.join().map_err(|_| "Clock server panicked")?;
    let rows = f.rows()?;
    assert_eq!(rows.len(), 1);
    // Admission clamps already elapsed lifetimes to zero. Re-anchoring the add
    // response after the failed RPC incorrectly leaves another 50ms of lifetime.
    let created = rows[0]["created_at"].as_str().unwrap();
    assert!(created.contains("1970"), "{rows:?}");
    assert!(
        rows[0]["expires_at_ms"].as_u64().unwrap() <= admitted_at,
        "{rows:?}"
    );
    Ok(())
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
fn probe_transport_and_negative_responses_count_toward_retirement() -> Result<(), String> {
    for body in [
        "print('invalid json')",
        "print(json.dumps({'ok':False,'error':'database unavailable'}))",
    ] {
        let f = Fixture::seeded(&["probe-retry"])?;
        let plugin = f.plugin(body);
        f.state.schedule_wake_retry_for(&f.row, 5000)?;
        assert_eq!(f.retries()?.len(), 1);
        for failures in 1..=3 {
            let row = f.listener()?;
            apply_probe(&f.state, &f.client, &row, &f.probe_result(&plugin, &row))?;
            let current = f.listener()?;
            assert_eq!(current["consecutive_probe_failures"], failures);
            assert_eq!(
                current["status"],
                if failures == 3 {
                    "UNLISTENED"
                } else {
                    "ACTIVE"
                }
            );
        }
        assert!(f.retries()?.is_empty());
    }
    Ok(())
}

#[test]
fn hard_exit_rebind_restores_only_unread_current_authority_work() -> Result<(), String> {
    for stopped in [false, true] {
        let f = Fixture::new()?;
        for id in ["pending", "batch-peer", "mismatch", "expired", "read"] {
            f.seed(id)?;
        }
        apply_probe(
            &f.state,
            &f.client,
            &f.row,
            &json!({"ok":true,"status":"closed_definitive"}),
        )?;
        f.state.db.execute("UPDATE local_signals SET batch_id='retained',batch_expires_at_ms=?,queue_written=1,queue_confirmed=1 WHERE signal_id IN ('pending','batch-peer')", &[json!(now_ms()+60_000)])?;
        f.state.db.execute(
            "UPDATE local_signals SET held_reason='authority_mismatch' WHERE signal_id='mismatch'",
            &[],
        )?;
        f.state.db.execute(
            "UPDATE local_signals SET expires_at_ms=0 WHERE signal_id='expired'",
            &[],
        )?;
        f.state.db.execute(
            "UPDATE local_signals SET acknowledged_at_ms=1 WHERE signal_id='read'",
            &[],
        )?;
        if stopped {
            f.state
                .db
                .execute("UPDATE local_listeners SET user_stopped=1", &[])?;
        }
        register_endpoint(
            &f.state,
            &f.client,
            "host",
            &json!({"about":"Restored receiver"}),
            &f.row,
        )?;
        let current = f
            .state
            .listener("local", string(&f.row, "listener_id"))?
            .unwrap();
        assert!(current["binding_generation"].as_u64() > f.row["binding_generation"].as_u64());
        let rows = f
            .state
            .db
            .query("SELECT * FROM local_signals ORDER BY signal_id", &[])?;
        for row in rows {
            let restored =
                !stopped && matches!(string(&row, "signal_id"), "pending" | "batch-peer");
            assert_eq!(
                row["state"],
                if restored {
                    "RECEIVED"
                } else {
                    "STORED_PASSIVE"
                },
                "{row}"
            );
            if restored {
                assert_eq!(row["binding_generation"], current["binding_generation"]);
                assert_eq!(row["queue_written"], 1);
                assert_eq!(row["queue_confirmed"], 1);
                assert_eq!(row["batch_id"], "retained");
                assert!(row["held_reason"].is_null());
            }
        }
        assert_eq!(
            f.state.db.query("SELECT * FROM wake_retries", &[])?.len(),
            usize::from(!stopped)
        );
    }
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
        // Seed each malformed historical value without replacing an existing tuple.
        f.client.rpc("kv_delete", &json!({"key":id}))?;
        f.client.kv_save(id, &remote, None)?;
        let error = detach_channel_expected(&f.client, id, "host:fixture", &f.row)
            .expect_err("Malformed registrations must reject detach");
        assert!(error.starts_with("INVALID_INPUT: Listener"));
        assert_eq!(
            f.client.kv_get(id)?.ok_or("Missing registration")?.0,
            remote
        );
        if !remote.is_object() {
            assert_eq!(
                register_endpoint(
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
    let f = Fixture::seeded(&["s"])?;
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
    assert!(f.listener()?["dispatcher_token"].is_null());
    assert_eq!(f.retries()?.len(), 1);
    f.dispatch(&f.plugin(DELIVER))?;
    let requests = f.requests()?;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["batch_id"], failed[0]["batch_id"]);
    assert_eq!(requests[0]["text"], failed[0]["batch_card"]);
    assert_eq!(
        requests[0]["text"],
        "Cito · bob@other · Sender [signal_id: s] · auto-acknowledged on delivery\n\nmessage &lt;s&gt;"
    );
    assert_eq!(f.rows()?[0]["state"], "DISPATCHED");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_number());
    Ok(())
}

#[test]
fn missing_and_acked_claim_members_reconcile_without_partially_claiming_unread(
) -> Result<(), String> {
    let f = Fixture::seeded(&["missing", "alreadyread", "remaining"])?;
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
    assert_eq!(f.retries()?.len(), 1);
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
fn terminal_claim_loss_and_expiry_drop_retries_without_dropping_unread_survivors(
) -> Result<(), String> {
    for transition in ["initial_ack", "renewal_ack", "expired"] {
        let f = Fixture::seeded(&["s"])?;
        let plugin = f.plugin(DELIVER);
        match transition {
            "initial_ack" => {
                f.client.rpc("signal_ack", &json!({"node_id":f.row["listener_id"],"signal_ids":["s"],"channel_id":"sibling:fixture"}))?;
                f.dispatch(&plugin)?;
                assert!(f.requests()?.is_empty());
                assert!(f.rows()?[0]["acknowledged_at_ms"].is_number());
            }
            "renewal_ack" => {
                let worker = f.start_worker(WAIT)?;
                f.client.rpc("signal_ack", &json!({"node_id":f.row["listener_id"],"signal_ids":["s"],"channel_id":"sibling:fixture"}))?;
                worker.assert_aborted(&f, Duration::from_secs(3))?;
                assert!(f.rows()?[0]["acknowledged_at_ms"].is_number());
            }
            _ => {
                f.state
                    .db
                    .execute("UPDATE local_signals SET expires_at_ms=1", &[])?;
                f.dispatch(&plugin)?;
                assert_eq!(f.rows()?[0]["state"], "EXPIRED_UNSENT");
                assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
            }
        }
        assert!(f.retries()?.is_empty(), "{transition}");
    }
    Ok(())
}

#[test]
fn losing_channel_stays_unread_until_fallback_then_can_recover_without_business_ack(
) -> Result<(), String> {
    let f = Fixture::seeded(&["s"])?;
    f.client.rpc("signal_claim_dispatch", &json!({"node_id":f.row["listener_id"],"signal_ids":["s"],"channel_id":"sibling:fixture","lease_ms":30_000}))?;
    let start = now_ms();
    let plugin = f.plugin(DELIVER);
    f.dispatch(&plugin)?;
    assert!(f.requests()?.is_empty());
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    let retry = f.retries()?[0]["retry_at_ms"]
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
    let f = Fixture::seeded(&["first"])?;
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
        let f = Fixture::seeded(&["read", "unread"])?;
        let plugin = f.plugin(&format!(
            "if not r['queue_written']:\n with open(os.path.join(root,'insertions'),'a') as f: f.write(json.dumps([s['signal_id'] for s in r['signals']])+'\\n')\nn=sum(1 for _ in open(os.path.join(root,'requests.jsonl')))\nprint(json.dumps({{'ok':True,'outcome':'driver_unavailable' if n==1 else 'delivered_unconfirmed','queue_written':True,'insertion_unknown':{},'open_confirmed':True}}))",
            if confirmed { "False" } else { "True" }
        ));
        f.dispatch(&plugin)?;
        f.state
            .acknowledge("local", f.listener_id(), &[json!("read")], "other")?;
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
    let f = Fixture::seeded(&["first"])?;
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
    let f = Fixture::seeded(&["s"])?;
    let batch = f.state.claim_wake_batch_for("local", &f.row, false)?;
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
    let f = Fixture::seeded(&["s"])?;
    let (mut remote, revision) = f.binding()?;
    remote["channels"]["sibling:fixture"] = json!({"channel_id":"sibling:fixture","kind":"fixture","host_id":"sibling","host_name":"sibling","delivery_mode":"wake_exclusive","priority":100,"state":"ACTIVE","updated_at_ms":now_ms()});
    remote["owner_token"] = json!("legacy-sibling-owner");
    f.client
        .kv_save(string(&f.row, "listener_id"), &remote, Some(revision))?;
    f.state
        .db
        .execute("UPDATE local_listeners SET updated_at=1", &[])?;
    let dispatched_at = now_ms();
    let worker = f.start_worker(&WAIT.replace("'delivered'", "'session_switched'"))?;
    f.seed("unread-survivor")?;
    std::fs::write(f.dir.join("release"), b"complete").map_err(|e| e.to_string())?;
    worker.join()??;
    assert!(
        f.retries()?.is_empty(),
        "Session retirement must stop scheduling, including newly admitted work"
    );
    let rows = f.rows()?;
    for signal in ["s", "unread-survivor"] {
        let row = rows
            .iter()
            .find(|row| row["signal_id"] == signal)
            .ok_or("Missing retired signal")?;
        assert_eq!(row["state"], "STORED_PASSIVE");
        assert_eq!(row["held_reason"], "session_switched");
        assert!(row["dispatcher_token"].is_null());
        assert!(row["acknowledged_at_ms"].is_null());
    }
    let revoked = f.listener()?;
    assert!(revoked["updated_at"]
        .as_u64()
        .is_some_and(|stamp| stamp > 1 && stamp >= dispatched_at));
    let (after, _) = f.binding()?;
    assert_eq!(after["channels"]["host:fixture"]["state"], "UNLISTENED");
    assert_eq!(after["channels"]["sibling:fixture"]["state"], "ACTIVE");
    assert_eq!(after["state"], "ACTIVE");
    assert_eq!(after["binding_generation"], remote["binding_generation"]);
    assert_eq!(after["owner_token"], remote["owner_token"]);
    assert_eq!(f.listener()?["status"], "UNLISTENED");
    Ok(())
}

#[test]
fn in_flight_rebind_aborts_and_reaps_real_child_without_touching_new_owner() -> Result<(), String> {
    let terminals = [
        (
            true,
            "print(json.dumps({'ok':True,'status':'closed_definitive'}))",
        ),
        (
            false,
            "print(json.dumps({'ok':True,'outcome':'session_switched'}))",
        ),
    ];
    for (probe, response) in terminals {
        let f = Fixture::seeded(&["serialized"])?;
        let body = WAIT.replace(DELIVER, response);
        let plugin = f.plugin(&body);
        let id = string(&f.row, "listener_id");
        let writer = f.hub_db()?;
        let replacement_client = HubClient::new(
            "local".into(),
            json!({"url":"local://"}),
            "alice",
            Some(Arc::new(f.engine.clone())),
        );
        let registration = f.state.registration_lock("local")?;
        std::thread::scope(|scope| -> Result<(), String> {
            let terminal = scope.spawn(|| {
                if probe {
                    apply_probe(
                        &f.state,
                        &f.client,
                        &f.row,
                        &f.probe_result(&plugin, &f.row),
                    )
                } else {
                    f.dispatch(&plugin)
                }
            });
            wait_until(|| f.dir.join("entered").exists(), Duration::from_secs(3))?;
            // Block the remote detach after local revocation, without holding any
            // local-state or registration mutex in this controlling thread.
            let registering=writer.transaction(|_| {
                std::fs::write(f.dir.join("release"),b"revoke").map_err(|e|e.to_string())?;
                wait_until(||f.state.listener("local",id).ok().flatten().is_some_and(|row|row["status"]=="UNLISTENED"),Duration::from_secs(3))?;
                assert!(matches!(registration.try_lock(),Err(std::sync::TryLockError::WouldBlock)),"Terminal revocation must hold registration authority through remote detach; probe={probe}");
                Ok(scope.spawn(|| {
                    register_endpoint(&f.state,&replacement_client,"host",&json!({"about":"After terminal revocation"}),&json!({"harness":"fixture","session_id":"session","title":"Target","cwd":f.dir.as_path(),"endpoint_pid":null,"repo":null,"branch":null}))
                }))
            })?;
            terminal.join().map_err(|_| "Terminal worker panicked")??;
            let replacement = registering
                .join()
                .map_err(|_| "Registration worker panicked")??;
            let row = f.listener()?;
            assert_eq!(row["status"], "ACTIVE");
            assert!(
                replacement["binding_generation"].as_u64() > f.row["binding_generation"].as_u64()
            );
            assert_ne!(row["owner_token"], f.row["owner_token"]);
            let (remote, _) = f.binding()?;
            assert_eq!(remote["channels"]["host:fixture"]["state"], "ACTIVE");
            assert_eq!(
                remote["channels"]["host:fixture"]["owner_token"],
                row["owner_token"]
            );
            Ok(())
        })?;
        child_gone(&f)?;
    }
    for (probe, response) in terminals {
        for cleanup in ["success", "retry", "newer", "enqueue_failure"] {
            let f = Fixture::seeded(&["durable-revocation"])?;
            let id = f.listener_id();
            let (mut remote, revision) = f.binding()?;
            remote["owner_token"] = json!("sibling-owner");
            remote["channels"]["sibling:fixture"] = json!({"channel_id":"sibling:fixture","kind":"fixture","host_id":"sibling","state":"ACTIVE","owner_token":"sibling-owner","priority":100,"updated_at_ms":now_ms()});
            f.client.kv_save(id, &remote, Some(revision))?;
            let (remote, _) = f.binding()?;
            let fail = matches!(cleanup, "retry" | "newer");
            if cleanup == "enqueue_failure" {
                f.state.db.execute("CREATE TRIGGER reject_detach BEFORE INSERT ON pending_hub_detaches BEGIN SELECT RAISE(ABORT,'fixture queue failure'); END",&[])?;
            }
            let fault = if fail {
                "import sqlite3\nwith sqlite3.connect(os.path.join(root,'hub','hub.sqlite')) as db: db.execute('ALTER TABLE hub_kv RENAME TO unavailable_hub_kv')\n"
            } else {
                ""
            };
            let plugin=f.plugin(&format!("with open(os.path.join(root,'entered'),'w') as f: f.write(str(os.getpid()))\n{fault}{response}"));
            let result = if probe {
                apply_probe(
                    &f.state,
                    &f.client,
                    &f.row,
                    &f.probe_result(&plugin, &f.row),
                )
            } else {
                f.dispatch(&plugin)
            };
            // Restore the sandbox backend before inspecting even a failing result.
            if fail {
                f.hub_db()?
                    .execute("ALTER TABLE unavailable_hub_kv RENAME TO hub_kv", &[])?;
            }
            child_gone(&f)?;
            let row = f.listener()?;
            let detaches = || {
                f.state.db.query(
                    "SELECT * FROM pending_hub_detaches WHERE hub='local' AND listener_id=?",
                    &[json!(id)],
                )
            };
            let pending = detaches()?;
            if cleanup == "enqueue_failure" {
                assert!(result.is_err_and(|error| error.contains("fixture queue failure")));
                assert_eq!(
                    row["status"], "ACTIVE",
                    "Revocation must roll back when retry intent cannot commit"
                );
                assert_eq!(row["owner_token"], f.row["owner_token"]);
                assert!(pending.is_empty());
                assert_eq!(
                    f.binding()?.0["channels"]["host:fixture"]["state"],
                    "ACTIVE"
                );
                continue;
            }
            assert_eq!(
                result.is_ok(),
                probe || !fail,
                "Keep probe/dispatch cleanup error semantics"
            );
            assert_eq!(row["status"], "UNLISTENED");
            assert_eq!(row["binding_generation"], f.row["binding_generation"]);
            assert_eq!(row["owner_token"], f.row["owner_token"]);
            if fail {
                assert_eq!(
                    pending.len(),
                    1,
                    "Failed detach must retain retry intent; probe={probe}"
                );
                assert_eq!(pending[0]["hub_url"], "local://");
                assert_eq!(
                    f.binding()?.0["channels"]["host:fixture"]["state"],
                    "ACTIVE"
                );
                if cleanup == "newer" {
                    // A genuine replacement can exist remotely while this state handle
                    // still retains the old UNLISTENED owner and retry intent.
                    let replacement = LocalState::open(&f.dir.join("replacement"))?;
                    register_endpoint(
                        &replacement,
                        &f.client,
                        "host",
                        &json!({"about":"New owner"}),
                        &json!({"harness":"fixture","session_id":"session","title":"Target","cwd":f.dir.as_path(),"endpoint_pid":null}),
                    )?;
                    let before = f.binding()?;
                    assert_ne!(
                        before.0["channels"]["host:fixture"]["owner_token"],
                        f.row["owner_token"]
                    );
                    super::super::daemon::DaemonService::reconcile_unlistened(
                        &f.state, &f.client, "host",
                    )?;
                    assert_eq!(
                        f.binding()?,
                        before,
                        "Old retry intent cannot detach a newer owner"
                    );
                } else {
                    super::super::daemon::DaemonService::reconcile_unlistened(
                        &f.state, &f.client, "host",
                    )?;
                }
            } else {
                assert!(
                    pending.is_empty(),
                    "Successful immediate detach must clear retry intent"
                );
            }
            assert!(detaches()?.is_empty());
            let (after, _) = f.binding()?;
            assert_eq!(
                after["channels"]["host:fixture"]["state"],
                if cleanup == "newer" {
                    "ACTIVE"
                } else {
                    "UNLISTENED"
                }
            );
            assert_eq!(
                after["channels"]["sibling:fixture"],
                remote["channels"]["sibling:fixture"]
            );
            assert_eq!(after["owner_token"], remote["owner_token"]);
        }
    }
    let f = Fixture::seeded(&["s"])?;
    let worker = f.start_worker(&WAIT.replace("'delivered'", "'session_switched'"))?;
    let id = f.listener_id();
    detach_channel_expected(&f.client, id, "host:fixture", &f.row)?;
    assert_eq!(f.listener()?["status"], "ACTIVE");
    assert_eq!(
        f.binding()?.0["channels"]["host:fixture"]["state"],
        "UNLISTENED"
    );
    let replacement = register_endpoint(
        &f.state,
        &f.client,
        "host",
        &json!({"about":"Replacement"}),
        &json!({"harness":"fixture","session_id":"session","title":"Target","cwd":f.dir.as_path(),"endpoint_pid":null,"repo":null,"branch":null}),
    )?;
    assert!(replacement["binding_generation"].as_u64() > f.row["binding_generation"].as_u64());
    let successor = f.listener()?;
    assert_ne!(
        successor["owner_token"], f.row["owner_token"],
        "A remotely detached binding cannot reuse the active local owner"
    );
    std::fs::write(f.dir.join("release"), b"complete old session switch")
        .map_err(|e| e.to_string())?;
    worker.assert_aborted(&f, Duration::from_secs(3))?;
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    let after = f.listener()?;
    assert_eq!(after["status"], "ACTIVE");
    assert_eq!(after["owner_token"], successor["owner_token"]);
    assert_eq!(after["binding_generation"], successor["binding_generation"]);
    assert_eq!(
        f.binding()?.0["channels"]["host:fixture"]["state"],
        "ACTIVE"
    );
    Ok(())
}

#[test]
fn closed_hub_aborts_owned_child_promptly() -> Result<(), String> {
    let f = Fixture::seeded(&["s"])?;
    let worker = f.start_worker(WAIT)?;
    f.client
        .closed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    worker.assert_aborted(&f, Duration::from_secs(3))?;
    assert_eq!(f.rows()?[0]["state"], "DISPATCHING");
    Ok(())
}

#[test]
fn single_effect_completion_stays_dispatched_and_preserves_concurrent_inbox_ack(
) -> Result<(), String> {
    let f = Fixture::seeded(&["s"])?;
    let worker = f.start_worker(WAIT)?;
    f.state
        .acknowledge("local", f.listener_id(), &[json!("s")], "other")?;
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
    let f = Fixture::seeded(&["s"])?;
    let worker = f.start_worker(WAIT)?;
    let db = f.hub_db()?;
    let id = string(&f.row, "listener_id");
    let channel = "host:fixture";
    let (mut registration, _) = f.binding()?;
    let initial_heartbeat = now_ms();
    // Model a wake approaching the channel's TTL without a minute-long sleep.
    registration["channels"][channel]["updated_at_ms"] = json!(initial_heartbeat - 55_000);
    registration["updated_at_ms"] = json!(initial_heartbeat - 55_000);
    registration["channels"]["sibling:fixture"] = json!({"state":"ACTIVE","updated_at_ms":1});
    registration["owner_token"] = json!("legacy-sibling-owner");
    let sibling = registration["channels"]["sibling:fixture"].clone();
    // Age persisted Hub timestamps directly; a normal write renews on the Hub clock.
    db.execute(
        "UPDATE hub_kv SET value=CAST(? AS BLOB) WHERE key=?",
        &[json!(registration.to_string()), json!(id)],
    )?;
    let initial = db.query("SELECT claim_until_ms FROM hub_signals", &[])?[0]["claim_until_ms"]
        .as_u64()
        .ok_or("Missing claim deadline")?;
    let renewed = wait_until(
        || {
            db.query("SELECT claim_until_ms FROM hub_signals", &[])
                .ok()
                .is_some_and(|r| r[0]["claim_until_ms"].as_u64().unwrap_or(0) >= initial + 100)
                && f.listener()
                    .ok()
                    .is_some_and(|r| r["lease_until_ms"].as_u64().unwrap_or(0) >= initial + 100)
                && f.binding().ok().is_some_and(|(r, _)| {
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
    let f = Fixture::seeded(&["s"])?;
    let worker = f.start_worker(WAIT)?;
    f.hub_db()?.execute(
        "UPDATE hub_signals SET claimed_by_channel='winner:fixture',claim_until_ms=?",
        &[json!(now_ms() + 60_000)],
    )?;
    worker.assert_aborted(&f, Duration::from_millis(900))?;
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    let retry = f.retries()?;
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
    let f = Fixture::seeded(&["s"])?;
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
    let f = Fixture::seeded(&["s"])?;
    let worker = f.start_worker(WAIT)?;
    f.state
        .db
        .execute("UPDATE local_listeners SET status='UNLISTENED'", &[])?;
    worker.assert_aborted(&f, Duration::from_secs(3))?;
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    assert_eq!(f.rows()?[0]["held_reason"], "unlistened_before_dispatch");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    Ok(())
}

#[test]
fn batch_expiry_aborts_owned_child_and_expires_every_member() -> Result<(), String> {
    let f = Fixture::new()?;
    // Keep cold system-interpreter initialization outside the one-second expiry budget.
    plugin_request(
        &f.plugin(DELIVER),
        &json!({"state_dir": f.dir.as_path()}),
        None,
        None,
    )?;
    f.seed("early")?;
    f.seed("late")?;
    f.state.db.execute(
        "UPDATE local_signals SET expires_at_ms=? WHERE signal_id='early'",
        &[json!(now_ms() + 1000)],
    )?;
    let worker = f.start_worker(WAIT)?;
    worker.assert_aborted(&f, Duration::from_secs(3))?;
    assert!(f
        .rows()?
        .iter()
        .all(|r| r["state"] == "EXPIRED_UNSENT" && r["acknowledged_at_ms"].is_null()));
    Ok(())
}

#[test]
fn concurrent_intent_survives_the_registration_commit() -> Result<(), String> {
    for stale in [false, true] {
        let f = Fixture::new()?;
        let writer = Database::open(&f.dir.join("state.sqlite"))?;
        let outcome = thread::scope(|scope| -> Result<_, String> {
            let job = writer.transaction(|db| {
                let job = scope.spawn(|| register_endpoint(&f.state,&f.client,"host",&json!({"about":"Concurrent registration","title":"Registration title","discovered_at":now_ms()}),&f.row));
                wait_until(|| f.binding().is_ok_and(|(r,_)| r["about"] == "Concurrent registration"),Duration::from_secs(3))?;
                LocalState::write_custom_title(db,"fixture","session","Newer title")?;
                if stale { db.execute("UPDATE local_listeners SET status='UNLISTENED', retirement_reason='idle', retired_activity_revision='123',user_stopped=1,updated_at=?", &[json!(now_ms())])?; }
                Ok(job)
            })?;
            job.join().map_err(|_| "Registration panicked".to_owned())
        })?;
        let row = f.listener()?;
        assert_eq!(row["title"], "Newer title");
        assert_eq!(
            f.state.custom_title("fixture", "session")?.as_deref(),
            Some("Newer title")
        );
        if stale {
            assert!(outcome.is_err_and(|e| e.starts_with("STALE_DISCOVERY:")));
            assert_eq!(row["status"], "UNLISTENED");
            assert_eq!(row["retirement_reason"], "idle");
            assert_eq!(row["retired_activity_revision"], "123");
            assert_eq!(row["user_stopped"], 1);
        } else {
            assert_eq!(outcome?["title"], "Newer title");
            assert_eq!(f.binding()?.0["title"], "Newer title");
        }
    }
    Ok(())
}

#[test]
fn inactive_title_sync_does_not_publish_to_hub() -> Result<(), String> {
    for inactive in ["local", "node", "channel"] {
        let f = Fixture::new()?;
        f.state.set_custom_title("fixture", "session", "Renamed")?;
        if inactive == "local" {
            f.state.pause_listener_for(&f.row)?;
        } else {
            let (mut remote, rev) = f.binding()?;
            if inactive == "node" {
                remote["state"] = json!("UNLISTENED");
            } else {
                remote["channels"]["host:fixture"]["state"] = json!("UNLISTENED");
            }
            f.client.kv_save(f.listener_id(), &remote, Some(rev))?;
        }
        let before = f.binding()?;
        super::super::registration::sync_custom_title(&f.state, &f.client, "host", &f.row)?;
        assert_eq!(f.binding()?, before, "{inactive}");
    }
    Ok(())
}

#[test]
fn process_identity_round_trips_through_probe_and_fences_reused_pid() -> Result<(), String> {
    let f = Fixture::new()?;
    let args = json!({"about":"Receiver"});
    let mut caller = f.row.clone();
    caller["endpoint_pid"] = json!(42);
    caller["process_identity"] = json!({"pid":42,"started_at":"first"});
    register_endpoint(&f.state, &f.client, "host", &args, &caller)?;
    let first = f.listener()?;
    let plugin = f.plugin("print(json.dumps({'ok':False}))");
    apply_probe(
        &f.state,
        &f.client,
        &first,
        &f.probe_result(&plugin, &first),
    )?;
    assert_eq!(
        f.requests()?[0]["process_identity"],
        caller["process_identity"]
    );
    register_endpoint(&f.state, &f.client, "host", &args, &caller)?;
    assert_eq!(f.listener()?["consecutive_probe_failures"], 1);
    caller["process_identity"]["started_at"] = json!("second");
    register_endpoint(&f.state, &f.client, "host", &args, &caller)?;
    apply_probe(
        &f.state,
        &f.client,
        &first,
        &json!({"ok":true,"status":"closed_definitive"}),
    )?;
    let current = LocalState::open(&f.dir)?
        .active()?
        .pop()
        .ok_or("Lost replacement")?;
    assert_eq!(current["consecutive_probe_failures"], 0);
    assert_eq!(current["process_identity"], caller["process_identity"]);
    Ok(())
}

#[test]
fn same_channel_refresh_retains_owner_and_replacement_fences_without_siblings() -> Result<(), String>
{
    let f = Fixture::new()?;
    let args = json!({"about":"Refreshed receiver"});
    register_endpoint(&f.state, &f.client, "host", &args, &f.row)?;
    let refreshed = f.listener()?;
    assert_eq!(refreshed["binding_generation"], f.row["binding_generation"]);
    assert_eq!(refreshed["owner_token"], f.row["owner_token"]);
    let mut caller = f.row.clone();
    caller["endpoint_pid"] = json!(42);
    register_endpoint(&f.state, &f.client, "host", &args, &caller)?;
    let replacement = f.listener()?;
    assert_eq!(
        replacement["binding_generation"].as_u64(),
        f.row["binding_generation"].as_u64().map(|v| v + 1)
    );
    assert_ne!(replacement["owner_token"], f.row["owner_token"]);
    let (remote, _) = f.binding()?;
    assert_eq!(remote["owner_token"], replacement["owner_token"]);
    assert_eq!(
        remote["channels"]["host:fixture"]["owner_token"],
        replacement["owner_token"]
    );
    Ok(())
}

#[test]
fn channel_takeover_blocks_stale_detach_and_fences_before_spawn() -> Result<(), String> {
    let f = Fixture::seeded(&["s"])?;
    let id = f.listener_id();
    let (mut remote, revision) = f.binding()?;
    remote["channels"]["host:fixture"]["owner_token"] = json!("winner-owner");
    remote["channels"]["sibling:fixture"] = json!({"state":"ACTIVE"});
    f.client.kv_save(id, &remote, Some(revision))?;
    let (remote, _) = f.binding()?;
    let error = detach_channel_expected(&f.client, id, "host:fixture", &f.row)
        .expect_err("Stale owner must not detach the winner");
    assert!(error.starts_with("STALE_GENERATION:"));
    assert_eq!(f.binding()?.0, remote);
    assert!(f.dispatch(&f.plugin(DELIVER)).is_err());
    assert!(f.requests()?.is_empty());
    assert_eq!(f.listener()?["status"], "FENCED_INACTIVE");
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    assert_eq!(f.rows()?[0]["held_reason"], "fenced_before_dispatch");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    Ok(())
}

#[test]
fn remote_preflight_generation_mismatch_fences_captured_listener_before_spawn() -> Result<(), String>
{
    let f = Fixture::seeded(&["s"])?;
    let (mut remote, revision) = f.binding()?;
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
    assert_eq!(f.listener()?["status"], "FENCED_INACTIVE");
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_null());
    Ok(())
}

#[test]
fn reply_addresses_qualify_non_default_or_named_hubs() {
    let config = Some(r#"{"default_hub":"local","hubs":{"local":{},"default":{"enabled":false}}}"#);
    assert!(!qualify_on_hub("local", config, None));
    assert!(qualify_on_hub("default", config, None));
    assert!(qualify_on_hub("work", config, Some("work")));
    assert!(qualify_on_hub("local", config, Some("default")));
    assert!(!qualify_on_hub("default", config, Some("default")));
}

#[test]
fn colon_bearing_wake_references_round_trip_without_changing_hub_ack_keys() -> Result<(), String> {
    let f = Fixture::seeded(&["local:part:two"])?;
    f.state.db.execute(
        "UPDATE local_signals SET in_reply_to='other:one',from_listener_id='sender:node'",
        &[],
    )?;
    f.dispatch(&f.plugin(DELIVER))?;
    let request = &f.requests()?[0];
    assert!(string(request, "text").contains("[↩ local:sender:node]"));
    let signal = &request["signals"][0];
    assert_eq!(
        string(signal, "signal_id").split_once(':'),
        Some(("local", "local:part:two"))
    );
    assert_eq!(signal["in_reply_to"], "local:other:one");
    let acks = f
        .hub_db()?
        .query("SELECT signal_id,acked_at_ms FROM hub_signals", &[])?;
    assert_eq!(acks[0]["signal_id"], "local:part:two");
    assert!(acks[0]["acked_at_ms"].is_number());
    Ok(())
}

#[test]
fn durable_remote_ack_reconciliation_works_after_transport_cursor_has_advanced(
) -> Result<(), String> {
    let f = Fixture::seeded(&["s"])?;
    poll_listener(&f.state, &f.client, &f.row, None, &f.dir, |_| {})?;
    f.client.rpc(
        "signal_ack",
        &json!({"node_id":f.row["listener_id"],"signal_ids":["s"],"channel_id":"sibling:fixture"}),
    )?;
    // A steady tail of new arrivals must not starve the earlier unread page.
    for index in 0..130 {
        f.seed(&format!("new-{index}"))?;
    }
    for index in 0..3 {
        f.seed(&format!("tail-{index}"))?;
        poll_listener(&f.state, &f.client, &f.row, None, &f.dir, |_| {})?;
    }
    assert!(f.rows()?[0]["acknowledged_at_ms"].is_number());
    assert_eq!(f.rows()?[0]["state"], "STORED_PASSIVE");
    assert!(f.requests()?.is_empty());
    Ok(())
}
