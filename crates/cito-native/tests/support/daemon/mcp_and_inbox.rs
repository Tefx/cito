use super::*;

#[test]
fn stdio_transport_provenance_allows_real_thread_provider_but_fences_bridge_claims() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let source =
        cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite")).unwrap();
    source.execute("CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,recency_at_ms INTEGER); INSERT INTO threads VALUES('thread-a','/repo',0,1)", &[]).unwrap();
    let plugin = f.state.join("plugins/codex");
    fs::create_dir_all(&plugin).unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(root().join("plugins/codex/plugin.json")).unwrap())
            .unwrap();
    manifest["command"] = json!([CODEX]);
    fs::write(plugin.join("plugin.json"), manifest.to_string()).unwrap();
    // Pin all real providers. Fixture::new sets the builtin directory but does
    // not substitute a provider; do not rely on staged binaries for this seam.
    let pi = f.state.join("plugins/pi");
    fs::create_dir_all(&pi).unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(root().join("plugins/pi/plugin.json")).unwrap()).unwrap();
    manifest["command"] = json!([PI]);
    fs::write(pi.join("plugin.json"), manifest.to_string()).unwrap();
    let claude = f.state.join("plugins/claude");
    fs::create_dir_all(&claude).unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(root().join("plugins/claude/plugin.json")).unwrap())
            .unwrap();
    manifest["command"] = json!([CLAUDE]);
    fs::write(claude.join("plugin.json"), manifest.to_string()).unwrap();
    let app = f.dir.join("claude.mjs");
    fs::write(&app, "console.log('ready'); setInterval(()=>{},1000)").unwrap();
    let endpoint = f.spawn("python3", &["-c", "import os,sys; m,s=os.openpty(); os.dup2(s,0); os.set_inheritable(m,True); os.close(s); os.execvp('node',['node',sys.argv[1]])", app.to_str().unwrap()], &[]);
    wait(|| f.output(endpoint, false).contains("ready"));
    let claude_pid = f.pid(endpoint);
    let sidecar = f.spawn(CLAUDE, &[], &[]);
    let mut env = f.env.clone();
    env.insert("CLAUDE_SESSION_ID".into(), "claude-session".into());
    f.send(sidecar, json!({"op":"stdio_sidecar","state_dir":f.state,"env":env,"endpoint_pid":claude_pid,"cwd":"/repo"}));
    wait(|| f.frames(sidecar).iter().any(|row| row["op"] == "ready"));
    let request = json!({"op":"resolve_caller","meta":{"endpoint_pid":std::process::id(),"threadId":"thread-a"}});
    let pi_rejection = f.plugin(PI, request.clone());
    assert_eq!(
        pi_rejection["error_code"], "BINDING_REQUIRED",
        "{pi_rejection}"
    );
    let codex_rejection = f.plugin(CODEX, request);
    assert_eq!(
        codex_rejection["error_code"], "INVALID_INPUT",
        "{codex_rejection}"
    );
    for (meta, code) in [
        (json!({"threadId":"thread-a"}), "INVALID_INPUT"),
        (
            json!({"expected_harness":"codex","expected_session_id":"thread-a"}),
            "BINDING_REQUIRED",
        ),
    ] {
        let reply = f.plugin(
            CODEX,
            json!({"op":"resolve_caller","endpoint_pid":std::process::id(),"meta":meta}),
        );
        assert_eq!(reply["ok"], false, "{reply}");
        assert_eq!(reply["error_code"], code, "{reply}");
    }
    let expected = json!({"expected_harness":"codex","endpoint_pid":std::process::id(),"expected_session_id":"thread-a"});
    let direct = f.plugin(
        CODEX,
        json!({"op":"resolve_caller","explicit_session_id":"thread-a","meta":expected}),
    );
    assert_eq!(direct["error_code"], "BINDING_REQUIRED", "{direct}");
    let claude_meta =
        json!({"endpoint_pid":claude_pid,"expected_session_id":"claude-session","cwd":"/repo"});
    let direct = f.plugin(
        CODEX,
        json!({"op":"resolve_caller","explicit_session_id":"claude-session","meta":claude_meta}),
    );
    assert_eq!(direct["error_code"], "BINDING_REQUIRED", "{direct}");
    let direct = f.plugin(
        CLAUDE,
        json!({"op":"resolve_caller","explicit_session_id":"claude-session","meta":claude_meta}),
    );
    assert_eq!(direct["caller"]["harness"], "claude", "{direct}");
    f.daemon(&[]);
    let rejected = f.tool_result("cito_status", json!({}), expected);
    assert_eq!(rejected["isError"], true, "{rejected}");
    assert!(
        rejected["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("BINDING_REQUIRED"),
        "{rejected}"
    );
    let routed = f.tool_result(
        "cito_listen",
        json!({"about":"Cross-provider expected session routing"}),
        claude_meta,
    );
    assert_ne!(routed["isError"], true, "{routed}");
    let routed: Value =
        serde_json::from_str(routed["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(routed["ok"], true, "{routed}");
    let rows = f.rows("SELECT * FROM local_listeners WHERE harness='claude'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["session_id"], "claude-session");
    for thread in [false, true] {
        let mut meta = json!({"_stdio":{"endpoint_pid":std::process::id()},"cwd":"/repo"});
        if thread {
            meta["threadId"] = json!("thread-a");
        }
        let result = f.tool(
            "cito_listen",
            json!({"about":"Transport identity regression"}),
            meta,
        );
        assert_eq!(result["ok"], true, "{result}");
        let rows = f.rows("SELECT * FROM local_listeners WHERE harness='codex'");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["session_id"], "thread-a");
    }
    let explicit = f.tool("cito_listen", json!({"about":"New thread before database projection"}),
        json!({"_stdio":{"endpoint_pid":std::process::id()},"threadId":"not-yet-indexed","cwd":"/repo"}));
    assert_eq!(explicit["ok"], true);
    assert_eq!(
        f.rows("SELECT * FROM local_listeners WHERE session_id='not-yet-indexed'")
            .len(),
        1
    );
    let rejected = f.tool_result(
        "cito_status",
        json!({}),
        json!({"endpoint_pid":std::process::id(),"threadId":"thread-a"}),
    );
    assert_eq!(rejected["isError"], true, "{rejected}");
    assert!(
        rejected["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("INVALID_INPUT"),
        "{rejected}"
    );
    let rejected = f.tool_result(
        "cito_status",
        json!({}),
        json!({"expected_harness":"codex","endpoint_pid":std::process::id(),"threadId":"thread-a"}),
    );
    assert_eq!(rejected["isError"], true, "{rejected}");
    assert!(
        rejected["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("INVALID_INPUT"),
        "{rejected}"
    );
    f.eof(sidecar);
    wait(|| f.exited(sidecar));
}

#[test]
fn long_lived_stdio_recovers_after_daemon_restart_and_configured_port_change() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    f.env.remove("CITO_DAEMON_PORT");
    fs::write(&f.config, json!({"daemonPort":f.port}).to_string()).unwrap();
    let first = f.daemon(&[]);
    let stdio = f.spawn(NATIVE, &["mcp-stdio"], &[]);
    f.send(
        stdio,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    wait(|| f.frames(stdio).iter().any(|r| r["id"] == 1));
    assert!(f
        .frames(stdio)
        .iter()
        .find(|r| r["id"] == 1)
        .unwrap()
        .get("result")
        .is_some());
    f.stop(first);
    f.port = port();
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    config["daemonPort"] = json!(f.port);
    fs::write(&f.config, config.to_string()).unwrap();
    f.daemon(&[]);
    f.send(
        stdio,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    );
    wait(|| f.frames(stdio).iter().any(|r| r["id"] == 2));
    let response = f.frames(stdio).into_iter().find(|r| r["id"] == 2).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    assert!(
        response["result"]["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "{response}"
    );
    f.eof(stdio);
    wait(|| f.exited(stdio));
}
#[test]
fn pending_business_ack_requires_original_url_and_principal() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    let human = f
        .rows("SELECT * FROM local_listeners WHERE hub='local' AND harness='human'")
        .remove(0);
    let sent = f.tool(
        "cito_send",
        json!({"to":human["public_id"],"message":"Keep unread"}),
        json!({}),
    );
    let id = sent["signal_id"].clone();
    let delivered = f.tool(
        "cito_send",
        json!({"to":human["public_id"],"message":"Admitted before replacement"}),
        json!({}),
    )["signal_id"]
        .clone();
    let state = f.db("state.sqlite");
    let hub = f.db("hub.sqlite");
    wait(|| {
        !state
            .query(
                "SELECT 1 FROM local_signals WHERE signal_id=?",
                std::slice::from_ref(&id),
            )
            .unwrap()
            .is_empty()
    });
    wait(|| {
        !state
            .query(
                "SELECT 1 FROM local_signals WHERE signal_id=?",
                std::slice::from_ref(&delivered),
            )
            .unwrap()
            .is_empty()
    });
    state.execute("UPDATE local_listeners SET hub_url=\'https://replacement.example.com\',principal_id=\'none#bob\' WHERE listener_id=?", std::slice::from_ref(&human["listener_id"])).unwrap();
    let local = cito_native::core::state::LocalState::open(&f.state).unwrap();
    local
        .acknowledge(
            "local",
            human["listener_id"].as_str().unwrap(),
            std::slice::from_ref(&delivered),
            "device:human",
        )
        .unwrap();
    let ack = state
        .query(
            "SELECT * FROM pending_hub_acks WHERE signal_id=?",
            std::slice::from_ref(&delivered),
        )
        .unwrap()
        .remove(0);
    assert_eq!(ack["hub_url"], "local://");
    assert_eq!(ack["principal_id"], human["principal_id"]);
    f.tool("cito_inbox", json!({"ack_all":false}), meta.clone());
    assert!(state
        .query(
            "SELECT * FROM pending_hub_acks WHERE signal_id=?",
            std::slice::from_ref(&delivered)
        )
        .unwrap()
        .is_empty());
    assert!(!hub
        .query(
            "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
            std::slice::from_ref(&delivered)
        )
        .unwrap()[0]["acked_at_ms"]
        .is_null());
    state.execute("INSERT INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms,hub_url,principal_id) VALUES('local',?,?,'device:human',1,'https://other.example.com','none#bob')",&[human["listener_id"].clone(),id.clone()]).unwrap();
    for (url, principal) in [
        ("local://", "none#bob"),
        (
            "https://other.example.com",
            human["principal_id"].as_str().unwrap(),
        ),
    ] {
        state
            .execute(
                "UPDATE pending_hub_acks SET hub_url=?,principal_id=?",
                &[json!(url), json!(principal)],
            )
            .unwrap();
        f.tool("cito_inbox", json!({"ack_all":false}), meta.clone());
        assert_eq!(
            state
                .query("SELECT * FROM pending_hub_acks", &[])
                .unwrap()
                .len(),
            1
        );
        assert!(hub
            .query(
                "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
                std::slice::from_ref(&id)
            )
            .unwrap()[0]["acked_at_ms"]
            .is_null());
    }
    state.transaction(|c| {
        for i in 0..256 { c.execute("INSERT INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms) VALUES('local',?,?,'device:human',0)",&[human["listener_id"].clone(),json!(format!("unbound-{i}"))])?; }
        c.execute("UPDATE pending_hub_acks SET hub_url='local://',principal_id=? WHERE signal_id=?",&[human["principal_id"].clone(),id.clone()])?;
        Ok(())
    }).unwrap();
    f.tool("cito_inbox", json!({"ack_all":false}), meta);
    let retained = state.query("SELECT * FROM pending_hub_acks", &[]).unwrap();
    assert_eq!(
        retained.len(),
        256,
        "Unbound history must not starve a valid later ACK"
    );
    assert!(retained
        .iter()
        .all(|r| r["hub_url"].is_null() && r["principal_id"].is_null()));
    assert!(!hub
        .query(
            "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
            &[id]
        )
        .unwrap()[0]["acked_at_ms"]
        .is_null());
}
#[test]
fn inbox_ordered_pages_explicit_ack_union_and_no_background_wake() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    enroll(&f, meta.clone());
    let row = f.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    let db = f.db("state.sqlite");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    db.execute(
        "INSERT OR REPLACE INTO wake_retries(hub,listener_id,retry_at_ms) VALUES('local',?,?)",
        &[row["listener_id"].clone(), json!(now + 60_000)],
    )
    .unwrap();
    db.transaction(|c|{for i in (1..=55).rev(){c.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) VALUES('local',?,?,?,?,'bob','laptop','Sender','Message','2030-01-01T00:00:00.000Z',?,'RECEIVED')",&[json!(format!("sig-{i}")),row["listener_id"].clone(),json!(i),row["binding_generation"].clone(),json!(now+60_000)])?;}Ok(())}).unwrap();
    let selected = f.tool("cito_inbox", json!({"ack_ids":["sig-55"]}), meta.clone());
    assert_eq!(selected["acknowledged_count"], 1);
    assert_eq!(
        selected["signals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        (1..=50).collect::<Vec<_>>()
    );
    assert_eq!(
        f.tool("cito_inbox", json!({}), meta.clone())["acknowledged_count"],
        50
    );
    let union = f.tool(
        "cito_inbox",
        json!({"ack_ids":["sig-55"],"ack_all":true}),
        meta.clone(),
    );
    assert_eq!(union["acknowledged_count"], 4);
    assert_eq!(union["signals"].as_array().unwrap().len(), 4);
    assert_eq!(f.tool("cito_inbox", json!({}), meta)["signals"], json!([]));
    assert_eq!(
        f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NOT NULL")
            .len(),
        55
    );
    let calls = fs::read_to_string(f.state.join("plugins/fixture/calls.jsonl")).unwrap();
    assert!(!calls
        .lines()
        .any(|l| serde_json::from_str::<Value>(l).unwrap()["op"] == "wake"));
}
#[test]
fn pending_publish_constraint_failure_recovers_exact_id_and_dedup_sequence() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta);
    let db = f.db("hub.sqlite");
    db.execute("CREATE TRIGGER reject_publish BEFORE INSERT ON hub_signals BEGIN SELECT RAISE(ABORT,'injected failure'); END",&[]).unwrap();
    let args = json!({"to":target["listener_id"],"message":"Recovery","idempotency_key":"same"});
    assert_eq!(send_human_result(&f, args.clone())["http_status"], 400);
    let row = f.rows("SELECT * FROM local_sent_idempotency")[0].clone();
    assert_eq!(row["status"], "PENDING_PUBLISH");
    db.execute("DROP TRIGGER reject_publish", &[]).unwrap();
    let recovered = send_human(&f, args.clone());
    assert_eq!(recovered["signal_id"], row["signal_id"]);
    assert_eq!(send_human(&f, args)["seq"], recovered["seq"]);
    assert_eq!(
        db.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
            .unwrap()[0]["n"],
        1
    );
}
#[test]
fn shared_hub_private_scope_is_invisible_and_workspace_scope_is_deliverable() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (_, p) = host.hub();
    let config = json!({"hubs":{"work":{"url":format!("http://127.0.0.1:{p}"),"user":"alice"}},"defaultHub":"work"});
    let a = Fixture::new(config);
    let meta = a.provider("delivered_unconfirmed");
    a.daemon(&[]);
    let target = a.tool(
        "cito_listen",
        json!({"about":"Private target","scope":"self"}),
        meta.clone(),
    );
    let mut b = Fixture::new(
        json!({"hubs":{"work":{"url":format!("http://127.0.0.1:{p}"),"user":"bob"}},"defaultHub":"work"}),
    );
    b.env.insert("CITO_USER".into(), "bob".into());
    b.daemon(&[]);
    assert_eq!(
        b.tool("cito_find", json!({"harness":"fixture"}), json!({}))["listeners"],
        json!([])
    );
    let denied = send_human_result(&b, json!({"to":target["listener_id"],"message":"Private"}));
    assert_eq!(denied["http_status"], 400);
    assert!(denied.to_string().contains("FORBIDDEN_TARGET_SCOPE"));
    let public = a.tool(
        "cito_listen",
        json!({"about":"Workspace target","scope":"workspace"}),
        meta,
    );
    send_human(
        &b,
        json!({"to":public["listener_id"],"message":"Workspace"}),
    );
    wait(|| {
        a.rows("SELECT * FROM local_signals WHERE state='DISPATCHED'")
            .len()
            == 1
    });
    assert_eq!(
        a.rows("SELECT from_user FROM local_signals")[0]["from_user"],
        "bob"
    );
}
#[test]
fn same_channel_takeover_fences_old_owner_and_preserves_winner() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (_, p) = host.hub();
    let config =
        json!({"hubs":{"work":{"url":format!("http://127.0.0.1:{p}")}},"defaultHub":"work"});
    let a = Fixture::new(config.clone());
    let ma = a.provider("delivered_unconfirmed");
    a.daemon(&[]);
    let old = enroll(&a, ma);
    let sibling = Fixture::new(config.clone());
    let sibling_meta = sibling.provider("delivered_unconfirmed");
    sibling.daemon(&[]);
    let attached = enroll(&sibling, sibling_meta);
    assert_eq!(attached["binding_generation"], old["binding_generation"]);
    wait(|| {
        [&a, &sibling].iter().all(|fixture| {
            fixture
                .rows("SELECT status FROM local_listeners WHERE hub=\'work\' AND harness=\'human\'")
                .iter()
                .any(|row| row["status"] == "ACTIVE")
        })
    });
    let device =
        a.rows("SELECT * FROM local_listeners WHERE hub='work' AND harness='human'")[0].clone();
    let sibling_device = sibling
        .rows("SELECT * FROM local_listeners WHERE hub='work' AND harness='human'")[0]
        .clone();
    assert_eq!(
        a.action(
            json!({"op":"unlisten_session","hub":"work","harness":"human","session_id":"default"})
        )
        .0,
        200
    );
    assert_eq!(
        a.rows("SELECT status FROM local_listeners WHERE hub='work' AND harness='human'")[0]
            ["status"],
        "UNLISTENED"
    );
    let retained = sibling
        .rows("SELECT * FROM local_listeners WHERE hub='work' AND harness='human'")[0]
        .clone();
    assert_eq!(retained["status"], "ACTIVE");
    assert_eq!(retained["owner_token"], sibling_device["owner_token"]);
    assert_eq!(
        a.action(json!({"op":"get_snapshot"})).1["hubs"][0]["human_paused"],
        false
    );
    assert_eq!(
        a.action(json!({"op":"cli_human_device","hub":"work","attach":true}))
            .0,
        200
    );
    let reattached =
        a.rows("SELECT * FROM local_listeners WHERE hub='work' AND harness='human'")[0].clone();
    assert_eq!(reattached["status"], "ACTIVE");
    assert!(reattached["binding_generation"].as_u64() > device["binding_generation"].as_u64());
    assert_ne!(reattached["owner_token"], device["owner_token"]);
    let unchanged = sibling
        .rows("SELECT * FROM local_listeners WHERE hub='work' AND harness='human'")[0]
        .clone();
    assert_eq!(unchanged["scope"], sibling_device["scope"]);
    assert_eq!(unchanged["owner_token"], sibling_device["owner_token"]);
    let sibling_row =
        sibling.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    // Hold only the sibling's dispatch lease; its receiver must keep polling unfenced.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    sibling.db("state.sqlite").execute(
        "UPDATE local_listeners SET dispatcher_token='test-held',lease_until_ms=? WHERE listener_id=?",
        &[json!(now + 60_000), sibling_row["listener_id"].clone()],
    ).unwrap();
    let b = Fixture::new(config);
    b.provider("delivered_unconfirmed");
    fs::write(
        b.state.join("host-id"),
        fs::read(a.state.join("host-id")).unwrap(),
    )
    .unwrap();
    b.daemon(&[]);
    let winner = enroll(
        &b,
        json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
    );
    // Heartbeat fencing must work even before another signal arrives.
    wait(|| {
        a.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"]
            == "FENCED_INACTIVE"
    });
    let sent = send_human(&b, json!({"to":winner["listener_id"],"message":"Winner"}));
    wait(|| {
        b.rows("SELECT * FROM local_signals WHERE state='DISPATCHED'")
            .len()
            == 1
            && sibling.rows("SELECT * FROM local_signals").len() == 1
    });
    assert!(sibling.rows("SELECT held_reason FROM local_signals")[0]["held_reason"].is_null());
    assert_eq!(
        b.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "ACTIVE"
    );
    assert_eq!(winner["binding_generation"], old["binding_generation"]);
    assert_eq!(
        sibling.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "ACTIVE"
    );
    let winner_row = b.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    let old_row = a.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    assert_ne!(winner_row["owner_token"], old_row["owner_token"]);
    let assert_channels_active = || {
        let remote: Value = serde_json::from_str(
            host.db("hub/hub.sqlite")
                .query(
                    "SELECT CAST(value AS TEXT) AS value FROM hub_kv WHERE key=?",
                    &[winner_row["listener_id"].clone()],
                )
                .unwrap()[0]["value"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(remote["state"], "ACTIVE");
        for row in [&winner_row, &sibling_row] {
            let channel = format!("{}:fixture", row["host_id"].as_str().unwrap());
            assert_eq!(remote["channels"][&channel]["state"], "ACTIVE");
            assert_eq!(
                remote["channels"][&channel]["owner_token"],
                row["owner_token"]
            );
        }
    };
    assert_channels_active();
    let mut sent_signals = vec![sent];
    for operation in ["unlisten", "reload"] {
        if operation == "unlisten" {
            assert_eq!(
                a.tool(
                    "cito_unlisten",
                    json!({"hub":"work"}),
                    json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
                )["status"],
                "not_listening"
            );
            assert_eq!(
                a.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
                "UNLISTENED"
            );
        } else {
            // Recreate a reload that captured A's ACTIVE row before heartbeat fencing.
            // Defer background fencing so the reload must exercise remote detach.
            let db = a.db("state.sqlite");
            db.execute(
                "CREATE TRIGGER defer_fencing BEFORE UPDATE OF status ON local_listeners WHEN OLD.harness='fixture' AND NEW.status='FENCED_INACTIVE' BEGIN SELECT RAISE(IGNORE); END",
                &[],
            ).unwrap();
            db.execute(
                "UPDATE local_listeners SET status='ACTIVE' WHERE harness='fixture'",
                &[],
            )
            .unwrap();
            let mut config: Value = serde_json::from_slice(&fs::read(&a.config).unwrap()).unwrap();
            let url = format!("http://localhost:{p}");
            config["hubs"]["work"]["url"] = json!(url);
            fs::write(&a.config, config.to_string()).unwrap();
            let (status, snapshot) = a.action(json!({"op":"get_snapshot"}));
            // Finish the local fence deferred by the fixture's race injection.
            db.execute(
                "DROP TRIGGER defer_fencing; UPDATE local_listeners SET status='FENCED_INACTIVE' WHERE harness='fixture'",
                &[],
            ).unwrap();
            assert_eq!(status, 200, "{snapshot}");
            assert_eq!(snapshot["settings"]["hubs"][0]["url"], url);
        }
        assert_channels_active();
        sent_signals.push(send_human(
            &b,
            json!({"to":winner["listener_id"],"message":format!("Winner after {operation}")}),
        ));
        wait(|| {
            b.rows("SELECT * FROM local_signals WHERE state='DISPATCHED'")
                .len()
                == sent_signals.len()
        });
        assert_eq!(
            b.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
            "ACTIVE"
        );
    }
    for (fixture, expected) in [(&a, 0), (&b, sent_signals.len()), (&sibling, 0)] {
        let calls = fs::read_to_string(fixture.state.join("plugins/fixture/calls.jsonl")).unwrap();
        let wakes: Vec<Value> = calls
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|call| call["op"] == "wake")
            .collect();
        assert_eq!(wakes.len(), expected);
        for (wake, sent) in wakes.iter().zip(&sent_signals) {
            assert_eq!(
                wake["signals"][0]["signal_id"],
                sent["signal_id"]
                    .as_str()
                    .unwrap()
                    .rsplit(':')
                    .next()
                    .unwrap()
            );
        }
    }
}
#[test]
fn http_origin_guard_strict_tool_schemas_and_singleton_port_ownership() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let daemon = f.daemon(&[]);
    let response = ureq::get(&format!("http://127.0.0.1:{}/health", f.port))
        .set("Origin", "https://foreign.example")
        .call();
    assert!(matches!(response, Err(ureq::Error::Status(403, _))));
    let tools = f.mcp("tools/list", json!({}));
    for tool in tools["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        let name = tool["name"].as_str().unwrap();
        let result = f.tool_result(name, json!({"extra":true}), json!({}));
        assert_eq!(result["isError"], true);
        assert!(result.to_string().contains("INVALID_INPUT"));
    }
    assert!(f.command(NATIVE, &["daemon"], "").0);
    f.env.insert("CITO_DAEMON_PORT".into(), port().to_string());
    assert!(!f.command(NATIVE, &["daemon"], "").0);
    f.stop(daemon);
    f.env.insert("CITO_DAEMON_PORT".into(), f.port.to_string());
    f.daemon(&[]);
}
#[test]
fn cli_formats_real_mcp_error_envelopes_without_json_or_internal_codes() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.provider("delivered_unconfirmed");
    let _daemon = f.daemon(&[]);
    let (ok, _, error) = f.command(
        NATIVE,
        &[
            "send",
            "--harness",
            "fixture",
            "--to",
            "c_0000000000000000",
            "--message",
            "hello",
        ],
        "",
    );
    assert!(!ok, "Unknown targets must fail");
    assert!(error.contains("Target listener does not exist"), "{error}");
    assert!(
        !error.contains("error_code")
            && !error.contains("{")
            && !error.contains("LISTENER_NOT_FOUND"),
        "{error}"
    );
}
#[test]
fn sqlite_pre_v4_upgrade_and_v4_reopen_preserve_outbound_recovery_rows() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let db = f.db("state.sqlite");
    db.execute("CREATE TABLE local_listeners(legacy TEXT);CREATE TABLE local_pairwise_wakes(legacy TEXT);PRAGMA user_version=3",&[]).unwrap();
    let daemon = f.daemon(&[]);
    assert_eq!(
        db.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        7
    );
    assert!(db
        .query(
            "SELECT name FROM sqlite_master WHERE name='local_pairwise_wakes'",
            &[]
        )
        .unwrap()
        .is_empty());
    db.execute("INSERT INTO local_sent_idempotency(hub,dedup_id,request_hash,signal_id,to_listener_id,status,expires_at_iso,expires_at_ms) VALUES('work','d','p','s','c_abcdefgh2345','PENDING_PUBLISH','now',0)",&[]).unwrap();
    f.stop(daemon);
    f.daemon(&[]);
    assert_eq!(
        db.query(
            "SELECT signal_id FROM local_sent_idempotency WHERE hub='work' AND dedup_id='d'",
            &[]
        )
        .unwrap(),
        vec![json!({"signal_id":"s"})]
    );
}

#[test]
fn agent_and_human_sends_share_unicode_control_sanitization() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta.clone());
    let hub = f.db("hub.sqlite");
    let error = "INVALID_INPUT: message must contain 1..65,536 characters";
    let agent = f.tool_result(
        "cito_send",
        json!({"to":"@self","message":"\u{0081}"}),
        meta.clone(),
    );
    assert_eq!(agent["isError"], true);
    let agent_error: Value =
        serde_json::from_str(agent["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        agent_error,
        json!({"ok":false,"error":{"code":"INVALID_INPUT","message":"message must contain 1..65,536 characters","retryable":false}})
    );
    let (status, human) = f.action(
        json!({"op":"send_reply_from_human","to_listener_id":target["listener_id"],"text":"\u{0081}"}),
    );
    assert_eq!(status, 400);
    assert_eq!(human["error"], error);
    assert_eq!(
        hub.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
            .unwrap(),
        vec![json!({"n":0})]
    );
    for (message, expected) in [
        ("a\u{0081}b", "ab"),
        (" \ta\u{0081}b\u{2003}", "ab"),
        (
            "a\n\r\t\u{000b}\u{000c}\u{0085}b",
            "a\n\r\t\u{000b}\u{000c}\u{0085}b",
        ),
    ] {
        let agent = f.tool(
            "cito_send",
            json!({"to":"@self","message":message}),
            meta.clone(),
        );
        let (status, human) = f.action(
            json!({"op":"send_reply_from_human","to_listener_id":target["listener_id"],"text":message}),
        );
        assert_eq!(status, 200, "{human}");
        for sent in [agent, human] {
            let id = sent["signal_id"]
                .as_str()
                .unwrap()
                .rsplit(':')
                .next()
                .unwrap();
            assert_eq!(
                hub.query(
                    "SELECT json_extract(CAST(data AS TEXT),'$.message') AS message FROM hub_signals WHERE signal_id=?",
                    &[json!(id)],
                ).unwrap(),
                vec![json!({"message":expected})]
            );
        }
    }
}

#[test]
fn snapshot_unread_count_precedes_activity_limit_and_utf8_frame_bounding() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let daemon = f.daemon(&[]);
    enroll(&f, human());
    let row = f.rows("SELECT * FROM local_listeners WHERE harness='human'")[0].clone();
    f.stop(daemon);
    let db = f.db("state.sqlite");
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 60_000;
    db.transaction(|c|{for i in 0..100 {c.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) VALUES('local',?,?,?,?,'bob','laptop','Sender',?,'2030-01-01T00:00:00.000Z',?,'STORED_PASSIVE')",&[json!(format!("sig-{i}")),row["listener_id"].clone(),json!(i),row["binding_generation"].clone(),json!("🦀".repeat(10000)),json!(expires)])?;}Ok(())}).unwrap();
    f.daemon(&[]);
    let (s, frame) = f.action(json!({"op":"get_snapshot"}));
    assert_eq!(s, 200);
    assert_eq!(frame["unread_count"], 100);
    assert_eq!(frame["activity"].as_array().unwrap().len(), 80);
    assert!(serde_json::to_vec(&frame).unwrap().len() + 1 < 256 * 1024);
    assert!(frame["activity"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["message"].as_str().unwrap().chars().count() <= 512));
    assert_eq!(
        db.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        7
    );
    assert_eq!(
        db.query("SELECT COUNT(*) AS n FROM local_signals", &[])
            .unwrap()[0]["n"],
        100
    );
}
#[test]
fn mcp_http_initialize_tool_schemas_and_malformed_boundaries() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let init=f.mcp("initialize",json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}));
    assert_eq!(init["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    let list = f.mcp("tools/list", json!({}));
    let names: Vec<_> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "cito_status",
            "cito_listen",
            "cito_unlisten",
            "cito_find",
            "cito_send",
            "cito_inbox"
        ]
    );
    let listen = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "cito_listen")
        .unwrap();
    assert_eq!(
        listen["inputSchema"]["properties"]["title"],
        json!({"type":"string","minLength":1,"maxLength":128})
    );
    let find = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "cito_find")
        .unwrap();
    assert_eq!(
        find["inputSchema"]["properties"]["harness"]["pattern"],
        "^[a-z0-9][a-z0-9_-]{0,31}$"
    );
    assert_eq!(
        f.tool("cito_find", json!({"harness":"1agent"}), json!({}))["listeners"],
        json!([])
    );
    for v in [
        json!([]),
        json!(42),
        json!(true),
        json!("request"),
        Value::Null,
    ] {
        for path in ["/ui", "/control", "/action", "/mcp"] {
            assert_eq!(http(f.port, path, Some(&v)).0, 400);
        }
        assert_eq!(f.mcp("tools/call", v.clone())["isError"], true);
        assert_ne!(f.tool_result("cito_find", json!({}), v)["isError"], true);
    }
    assert_eq!(f.mcp("ping", json!({})), json!({}));
}
#[test]
fn v29_readable_aliases_reject_ambiguous_live_prefixes_even_for_one_principal() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta);
    let canonical = target["canonical_listener_id"].as_str().unwrap();
    let prefix = &canonical[2..6];
    for length in [8, 12, 16] {
        let query = format!("alice/fixture#{}", &canonical[2..2 + length]);
        let found = f.tool("cito_find", json!({"query":query}), json!({}));
        assert_eq!(found["matched_count"], 1, "{found}");
        assert_eq!(found["listeners"][0]["listener_id"], canonical);
    }
    for to in [
        format!("alice/fixture#{prefix}"),
        format!("fixture#{prefix}"),
        format!("alice#{prefix}"),
        format!("local:alice/fixture#{prefix}"),
    ] {
        assert_eq!(
            send_human(&f, json!({"to":to,"message":"Alias routing"}))["to"],
            target["listener_id"]
        );
    }
    let duplicate = format!("c_{prefix}ffffffffffff");
    let row = json!({"listener_id":duplicate,"public_id":format!("alice-{}",&duplicate[2..]),"owner_user":"alice","principal_id":"none#alice","harness":"fixture","state":"ACTIVE","scope":"workspace","updated_at_ms":cito_native::core::plugins::now_ms(),"binding_generation":1});
    f.db("hub.sqlite").execute("INSERT INTO hub_kv(key,revision,created_ms,value,principal_id,scope) VALUES(?,1,?,CAST(? AS BLOB),'none#alice','workspace')",&[json!(duplicate),json!(cito_native::core::plugins::now_ms()),json!(row.to_string())]).unwrap();
    wait(|| {
        f.tool("cito_find", json!({"harness":"fixture"}), json!({}))["listeners"]
            .as_array()
            .unwrap()
            .len()
            == 2
    });
    for to in [
        format!("alice/fixture#{prefix}"),
        format!("fixture#{prefix}"),
        format!("alice#{prefix}"),
    ] {
        let result = send_human_result(&f, json!({"to":to,"message":"Must disambiguate"}));
        assert_eq!(result["http_status"], 400, "{result}");
        assert_eq!(
            result["error"],
            "INVALID_INPUT: Ambiguous target; use full listener_id"
        );
    }
}

#[test]
fn incoming_messages_update_unread_state_and_activity_until_read() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    let recorder = f.recording_ui();
    f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", recorder.to_str().unwrap()),
    ]);
    let frames = || {
        fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect::<Vec<_>>()
    };
    for message in ["Review requested", "Build completed"] {
        let sent = f.tool(
            "cito_send",
            json!({"to":"@self","message":message}),
            meta.clone(),
        );
        assert_eq!(sent["ok"], true, "{sent}");
    }
    wait(|| {
        frames()
            .iter()
            .any(|frame| frame["op"] == "state" && frame["unread_count"] == 2)
    });
    assert_eq!(
        f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL")
            .len(),
        2
    );
    let (status, activity) = f.action(json!({"op":"query_directory","filter":"me"}));
    assert_eq!(status, 200);
    assert_eq!(activity["activity_total"], 2);
    assert_eq!(
        f.action(json!({"op":"get_snapshot"})).1["hubs"][0]["human_paused"],
        false
    );
    assert_eq!(
        f.action(json!({"op":"ack_signals","filter":"me","ack_all":true}))
            .0,
        200
    );
    wait(|| {
        f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL")
            .is_empty()
    });
    wait(|| {
        frames()
            .iter()
            .rev()
            .find(|frame| frame["op"] == "state")
            .is_some_and(|frame| frame["unread_count"] == 0)
    });
}
#[test]
fn activity_filter_clear_and_read_preserve_hidden_unconfirmed_agent_obligations() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let agent = f.tool("cito_listen", json!({"about":"Pending work"}), meta.clone());
    send_human(
        &f,
        json!({"to":agent["listener_id"],"message":"Pending agent work"}),
    );
    f.tool(
        "cito_send",
        json!({"to":"@self","message":"Human notice"}),
        meta,
    );
    wait(|| f.rows("SELECT * FROM local_signals").len() == 2);
    let (status, me) = f.action(json!({"op":"query_directory","filter":"me"}));
    assert_eq!(status, 200);
    assert_eq!(me["activity_total"], 1);
    assert_eq!(me["activity_all_total"], 2);
    assert_eq!(
        f.action(json!({"op":"ack_signals","filter":"me","ack_all":true}))
            .0,
        200
    );
    assert!(
        f.rows("SELECT * FROM local_signals WHERE message='Pending agent work'")[0]
            ["acknowledged_at_ms"]
            .is_null()
    );
    assert_eq!(
        f.action(json!({"op":"clear_activity_history","filter":"me"}))
            .0,
        200
    );
    assert_eq!(
        f.rows("SELECT * FROM local_signals WHERE activity_hidden=0")
            .len(),
        1
    );
    assert_eq!(
        f.action(json!({"op":"clear_activity_history","filter":"all"}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"ack_signals","filter":"all","ack_all":true}))
            .0,
        200
    );
    let pending = f
        .rows("SELECT * FROM local_signals WHERE message='Pending agent work'")
        .remove(0);
    assert_eq!(pending["activity_hidden"], 1);
    assert!(pending["acknowledged_at_ms"].is_null());
    let ledger = f
        .db("hub.sqlite")
        .query(
            "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
            &[pending["signal_id"].clone()],
        )
        .unwrap();
    assert!(ledger[0]["acked_at_ms"].is_null());
    assert_eq!(f.action(json!({"op":"get_snapshot"})).1["unread_count"], 0);
}
#[test]
fn v29_sender_auto_enrollment_readable_reply_and_clear_close_the_pathway() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let sent = f.tool(
        "cito_send",
        json!({"to":"@self","message":"Agent initiated"}),
        meta.clone(),
    );
    assert_eq!(sent["ok"], true, "{sent}");
    let sender = f.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    assert_eq!(sender["status"], "ACTIVE");
    wait(|| {
        f.rows("SELECT * FROM local_signals WHERE message='Agent initiated'")
            .len()
            == 1
    });
    let early = f.action(json!({"op":"get_snapshot"})).1;
    assert!(
        early["activity"][0]["from_display_address"]
            .as_str()
            .unwrap()
            .contains("/fixture#"),
        "{early}"
    );
    let (status, snapshot) = f.action(json!({"op":"query_directory","filter":"all"}));
    assert_eq!(status, 200, "{snapshot}");
    let activity = &snapshot["activity"][0];
    assert_eq!(activity["from_scoped_id"], sender["public_id"]);
    assert!(activity["from_display_address"]
        .as_str()
        .unwrap()
        .contains("/fixture#"));
    let reply = send_human(
        &f,
        json!({"to":activity["from_display_address"],"message":"Human reply","in_reply_to":activity["signal_id"]}),
    );
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["to"], sender["public_id"]);
    wait(|| f.rows("SELECT * FROM local_signals").len() == 2);
    // Clearing hides Activity rows without deleting the delivery ledger.
    assert_eq!(
        f.action(json!({"op":"clear_activity_history","hub":"local","signal_id":"missing"}))
            .0,
        200
    );
    assert_eq!(f.rows("SELECT * FROM local_signals").len(), 2);
    assert_eq!(
        f.action(
            json!({"op":"clear_activity_history","hub":"local","signal_id":activity["signal_id"]})
        )
        .0,
        200
    );
    let remaining = f.rows("SELECT * FROM local_signals WHERE activity_hidden=0");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["message"], "Human reply");
    assert_eq!(f.action(json!({"op":"clear_activity_history"})).0, 200);
    assert!(f
        .rows("SELECT * FROM local_signals WHERE activity_hidden=0")
        .is_empty());
    assert_eq!(f.rows("SELECT * FROM local_signals").len(), 2);
    f.tool("cito_unlisten", json!({}), meta.clone());
    assert_eq!(
        f.tool(
            "cito_send",
            json!({"to":"@self","message":"Explicitly paused"}),
            meta
        )["ok"],
        true
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "UNLISTENED"
    );
}

#[test]
fn local_mcp_listen_find_send_inbox_ack_idempotency_and_channel_detach() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta.clone());
    assert_eq!(target["hub"], "local");
    assert_eq!(target["scope"], "workspace");
    let status = f.tool("cito_status", json!({}), meta.clone());
    assert_eq!(status["registered"], true);
    assert_eq!(status["title"], "Native fixture");
    assert_eq!(status["unread_count"], 0);
    assert_eq!(status["hubs"][0]["is_default"], true);
    assert_eq!(
        status
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "about",
            "display_address",
            "hubs",
            "ok",
            "registered",
            "scope",
            "title",
            "unread_count"
        ]
    );
    for hub in status["hubs"].as_array().unwrap() {
        assert_eq!(hub.as_object().unwrap().len(), 3);
    }
    for forbidden in [
        "cito_setup",
        "cito_rename",
        "cito_unlisten_all",
        "cito_join_hub",
        "cito_channel_save",
        "cito_disconnect_all",
    ] {
        assert_eq!(
            f.tool_result(forbidden, json!({}), meta.clone())["isError"],
            true
        );
    }
    for title in ["".to_owned(), " ".to_owned(), "x".repeat(129)] {
        assert_eq!(
            f.tool_result(
                "cito_listen",
                json!({"about":"Native receiver","title":title}),
                meta.clone()
            )["isError"],
            true
        );
    }
    f.tool(
        "cito_listen",
        json!({"about":"Native receiver","title":"Backend API"}),
        meta.clone(),
    );
    f.tool(
        "cito_listen",
        json!({"about":"Native receiver"}),
        meta.clone(),
    );
    assert_eq!(
        f.tool("cito_status", json!({}), meta.clone())["title"],
        "Backend API"
    );
    let (ok, out, err) = f.command(
        NATIVE,
        &[
            "rename",
            "Release audit",
            "--harness",
            "fixture",
            "--session",
            "session-1",
            "--json",
        ],
        "",
    );
    assert!(ok, "{out} {err}");
    assert_eq!(
        f.tool("cito_status", json!({}), meta.clone())["title"],
        "Release audit"
    );
    let (ok, out, err) = f.command(NATIVE, &["status", "--json"], "");
    assert!(ok, "{out} {err}");
    let snapshot: Value = serde_json::from_str(&out).unwrap();
    assert!(snapshot["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["title"] == "Release audit"));
    assert!(f
        .command(NATIVE, &["status"], "")
        .1
        .contains("AVAILABLE SESSIONS"));
    let find = f.tool("cito_find", json!({"query":"Native receiver"}), json!({}));
    assert!(find["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["scoped_id"] == target["listener_id"]
            && v["listener_id"] == target["canonical_listener_id"]));
    let long = "Context payload segment ".repeat(100).trim().to_string();
    let args = json!({"to":target["listener_id"],"message":long,"idempotency_key":"stable"});
    let sent = send_human(&f, args.clone());
    assert_eq!(sent["status"], "wake_accepted");
    let again = send_human(&f, args);
    assert_eq!(sent["signal_id"], again["signal_id"]);
    wait(|| f.rows("SELECT * FROM local_signals").len() == 1);
    assert!(
        f.rows("SELECT acknowledged_at_ms FROM local_signals")[0]["acknowledged_at_ms"].is_null()
    );
    let inbox = f.tool("cito_inbox", json!({}), meta.clone());
    assert_eq!(inbox["signals"][0]["message"], long);
    assert_eq!(inbox["acknowledged_count"], 1);
    assert_eq!(
        f.tool("cito_inbox", json!({}), meta.clone())["signals"],
        json!([])
    );
    assert_eq!(
        f.tool("cito_unlisten", json!({}), meta.clone())["status"],
        "unlistened"
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "UNLISTENED"
    );
    assert_eq!(
        f.tool("cito_status", json!({}), meta.clone())["registered"],
        false
    );
    let (ok, out, err) = f.command(
        NATIVE,
        &[
            "listen",
            "--title",
            "Billing API",
            "--harness",
            "fixture",
            "--session",
            "session-1",
            "Native receiver",
            "--json",
        ],
        "",
    );
    assert!(ok, "{out} {err}");
    assert_eq!(
        f.tool("cito_status", json!({}), meta)["title"],
        "Billing API"
    );
    let (ok, out, err) = f.command(NATIVE, &["unlisten", "--all", "--json"], "");
    assert!(ok, "{out} {err}");
    let stopped: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(stopped["removed"], 1);
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
        .is_empty());
    let options_path = f.state.join("plugins/fixture/options.json");
    let mut options: Value = serde_json::from_slice(&fs::read(&options_path).unwrap()).unwrap();
    options["session_id"] = json!("session-home");
    options["cwd"] = json!(f.home);
    options["title"] = json!("alice");
    fs::write(options_path, options.to_string()).unwrap();
    let (ok, out, err) = f.command(
        NATIVE,
        &[
            "listen",
            "--harness",
            "fixture",
            "--session",
            "session-home",
            "Auth refactor",
            "--json",
        ],
        "",
    );
    assert!(ok, "{out} {err}");
    let home: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(home["title"], "Auth refactor");
    assert_eq!(home["about"], "Auth refactor");
    assert_eq!(
        f.rows("SELECT title FROM session_custom_titles WHERE session_id='session-home'")[0]
            ["title"],
        "Auth refactor"
    );
}
#[test]
fn confirmed_delivery_business_ack_and_pending_hub_ack_reconciliation() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    let target = enroll(&f, meta);
    send_human(
        &f,
        json!({"to":target["listener_id"],"message":"confirmed"}),
    );
    wait(|| {
        f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NOT NULL")
            .len()
            == 1
    });
    wait(|| {
        f.db("hub.sqlite")
            .query(
                "SELECT * FROM hub_signals WHERE acked_at_ms IS NOT NULL",
                &[],
            )
            .unwrap()
            .len()
            == 1
    });
}
#[test]
fn standalone_http_hub_dedup_atomic_batch_claims_and_business_ack() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let (_, p) = f.hub();
    assert_eq!(
        http(p, "/health", None).1["version"],
        env!("CARGO_PKG_VERSION")
    );
    let id = "c_1111111111111111";
    rpc(
        p,
        "kv_put",
        json!({"key":id,"data_b64":B64.encode(json!({"listener_id":id,"principal_id":"none#alice","owner_user":"alice","harness":"fixture","session_id":"s","scope":"workspace","state":"ACTIVE","binding_generation":1,"channels":{"a":{"state":"ACTIVE"},"b":{"state":"ACTIVE"}}}).to_string())}),
    );
    for signal in ["one", "two", "three"] {
        let publish = json!({"subject":format!("cito.signal.{id}"),"data_b64":B64.encode(json!({"signal_id":signal,"to":id,"target_generation":1,"message":"hello"}).to_string()),"opts":{"msgID":signal}});
        assert_eq!(
            rpc(p, "js_publish", publish.clone())["seq"],
            rpc(p, "js_publish", publish)["seq"]
        );
    }
    assert_eq!(
        rpc(
            p,
            "signal_claim_dispatch",
            json!({"node_id":id,"channel_id":"b","signal_ids":["two"]})
        )["claimed"],
        true
    );
    let conflict = rpc(
        p,
        "signal_claim_dispatch",
        json!({"node_id":id,"channel_id":"a","signal_ids":["one","two"]}),
    );
    assert_eq!(conflict["claimed"], false);
    assert_eq!(conflict["claimed_ids"], json!([]));
    let db = f.db("hub/hub.sqlite");
    assert!(db
        .query(
            "SELECT claimed_by_channel FROM hub_signals WHERE signal_id='one'",
            &[]
        )
        .unwrap()[0]["claimed_by_channel"]
        .is_null());
    rpc(
        p,
        "signal_ack",
        json!({"node_id":id,"channel_id":"b","signal_ids":["two"]}),
    );
    assert_eq!(
        rpc(
            p,
            "signal_claim_dispatch",
            json!({"node_id":id,"channel_id":"a","signal_ids":["three","two"]})
        )["acked_ids"],
        json!(["two"])
    );
    assert!(db
        .query(
            "SELECT claimed_by_channel FROM hub_signals WHERE signal_id='three'",
            &[]
        )
        .unwrap()[0]["claimed_by_channel"]
        .is_null());
}
#[test]
fn two_remote_hubs_isolate_identifiers_and_offline_inbox_remains_readable() {
    let _lock = serial();
    let a = Fixture::new(json!({}));
    let b = Fixture::new(json!({}));
    let (ha, pa) = a.hub();
    let (hb, pb) = b.hub();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://","user":"alice"},"personal":{"url":format!("http://127.0.0.1:{pa}"),"user":"alice"},"work":{"url":format!("http://127.0.0.1:{pb}"),"user":"alice"}},"defaultHub":"personal"}),
    );
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    wait(|| {
        f.action(json!({"op":"get_snapshot"})).1["available"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["harness"] == "fixture" && row["session_id"] == "session-1")
    });
    for hub in ["personal", "work"] {
        let target = f.tool(
            "cito_listen",
            json!({"hub":hub,"about":"Multi hub","scope":"workspace"}),
            meta.clone(),
        );
        let sent = send_human(&f, json!({"to":target["listener_id"],"message":hub}));
        assert!(sent["signal_id"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{hub}:")));
    }
    assert_eq!(f.action(json!({"op":"rename_session","harness":"fixture","session_id":"session-1","title":"alice"})).0, 200);
    // Still available on local: the cached candidate must agree immediately,
    // without waiting for discovery or an eventual-consistency retry.
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    let candidate = snapshot["available"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["harness"] == "fixture" && row["session_id"] == "session-1")
        .unwrap();
    assert_eq!(candidate["title"], "alice");
    assert_eq!(candidate["custom_title"], true);
    assert!(snapshot["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["harness"] == "fixture")
        .all(|row| row["title"] == candidate["title"] && row["custom_title"] == true));
    f.tool(
        "cito_listen",
        json!({"hub":"local","about":"Multi hub","scope":"workspace"}),
        meta.clone(),
    );
    let binding = f.rows("SELECT endpoint_pid,binding_generation,owner_token FROM local_listeners WHERE hub=\'work\' AND harness=\'fixture\'")[0].clone();
    f.db("state.sqlite")
        .execute(
            "UPDATE local_listeners SET endpoint_pid=? WHERE hub=\'work\' AND harness=\'fixture\'",
            &[json!(i32::MAX)],
        )
        .unwrap();
    f.tool(
        "cito_listen",
        json!({"hub":"personal","about":"Multi hub"}),
        meta.clone(),
    );
    let retained = f.rows("SELECT endpoint_pid,binding_generation,owner_token FROM local_listeners WHERE hub=\'work\' AND harness=\'fixture\'")[0].clone();
    assert_eq!(
        retained["endpoint_pid"],
        i32::MAX,
        "Registering on personal must not rewrite work endpoint metadata"
    );
    assert_eq!(
        retained["binding_generation"],
        binding["binding_generation"]
    );
    assert_eq!(retained["owner_token"], binding["owner_token"]);
    f.db("state.sqlite")
        .execute(
            "UPDATE local_listeners SET endpoint_pid=? WHERE hub=\'work\' AND harness=\'fixture\'",
            &[binding["endpoint_pid"].clone()],
        )
        .unwrap();
    let all = f.tool("cito_find", json!({"harness":"fixture"}), json!({}));
    assert_eq!(all["listeners"].as_array().unwrap().len(), 3);
    assert_eq!(all["matched_count"], all["online_count"]);
    assert_eq!(all["matched_count"], 3);
    let hubs: Vec<_> = all["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["hub"].as_str().unwrap())
        .collect();
    assert_eq!(hubs, vec!["local", "personal", "work"]);
    assert!(all["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["title"] == "alice" && r["custom_title"] == true));
    let local = f.tool(
        "cito_find",
        json!({"hub":"local","harness":"fixture"}),
        json!({}),
    );
    assert_eq!(local["listeners"].as_array().unwrap().len(), 1);
    assert_eq!(local["listeners"][0]["title"], "alice");
    for hub in ["personal", "work"] {
        let stopped = if hub == "personal" {
            f.tool("cito_unlisten", json!({"hub":hub}), meta.clone())
        } else {
            let (ok, out, err) = f.command(
                NATIVE,
                &[
                    "unlisten",
                    "--hub",
                    hub,
                    "--harness",
                    "fixture",
                    "--session",
                    "session-1",
                    "--json",
                ],
                "",
            );
            assert!(ok, "{out} {err}");
            serde_json::from_str(&out).unwrap()
        };
        assert_eq!(stopped["removed"], 1);
        let remaining = f.action(json!({"op":"get_snapshot"})).1;
        assert!(remaining["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["hub"] == "local"
                && row["harness"] == "fixture"
                && row["title"] == "alice"
                && row["custom_title"] == true));
        assert_eq!(f.rows("SELECT title FROM session_custom_titles WHERE harness='fixture' AND session_id='session-1'")[0]["title"], "alice");
        // Re-enrollment must retain the shared custom title too.
        f.tool(
            "cito_listen",
            json!({"hub":hub,"about":"Multi hub"}),
            meta.clone(),
        );
        assert_eq!(
            f.tool("cito_status", json!({"hub":hub}), meta.clone())["title"],
            "alice"
        );
    }
    assert_eq!(f.action(json!({"op":"unlisten_session","hub":"local","harness":"fixture","session_id":"session-1"})).0, 200);
    let scoped = f.rows("SELECT hub,status FROM local_listeners WHERE harness='fixture'");
    assert!(scoped
        .iter()
        .any(|v| v["hub"] == "local" && v["status"] == "UNLISTENED"));
    assert!(scoped
        .iter()
        .filter(|v| v["hub"] != "local")
        .all(|v| v["status"] == "ACTIVE"));
    f.tool(
        "cito_listen",
        json!({"hub":"local","about":"Multi hub"}),
        meta.clone(),
    );
    wait(|| f.rows("SELECT * FROM local_signals").len() == 2);
    a.stop(ha);
    b.stop(hb);
    let (status, directory) = f.action(json!({"op":"query_directory","query":"","scope":"all","harness":"all","request_id":"offline-query"}));
    assert_eq!(status, 200, "{directory}");
    let warnings = directory["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|w| w.starts_with("personal:"))),
        "{directory}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|w| w.starts_with("work:"))),
        "{directory}"
    );
    let inbox = f.tool("cito_inbox", json!({}), meta.clone());
    assert_eq!(inbox["signals"].as_array().unwrap().len(), 2);
    assert_eq!(f.rows("SELECT * FROM pending_hub_acks").len(), 2);
    assert_eq!(
        f.action(json!({"op":"unlisten_session","harness":"fixture","session_id":"session-1"}))
            .0,
        400
    );
    assert!(f
        .rows("SELECT status FROM local_listeners WHERE harness='fixture'")
        .iter()
        .all(|v| v["status"] == "UNLISTENED"));
}
#[test]
fn stdio_signal_handlers_exit_cleanly_and_daemon_health_owns_sandbox() {
    let _lock = serial();
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let f = Fixture::new(json!({}));
        f.daemon(&[]);
        let p = f.spawn(NATIVE, &["mcp-stdio"], &[]);
        f.send(p,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}));
        wait(|| !f.frames(p).is_empty());
        f.signal(p, signal);
        wait(|| f.exited(p));
        f.stop(p);
        assert_eq!(http(f.port, "/health", None).1["stateDir"], json!(f.state));
    }
}
