use super::support::*;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use std::fs;

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
    assert_eq!(
        f.tool_result("cito_send", args.clone(), human())["isError"],
        true
    );
    let row = f.rows("SELECT * FROM local_sent_idempotency")[0].clone();
    assert_eq!(row["status"], "PENDING_PUBLISH");
    db.execute("DROP TRIGGER reject_publish", &[]).unwrap();
    let recovered = f.tool("cito_send", args.clone(), human());
    assert_eq!(recovered["signal_id"], row["signal_id"]);
    assert_eq!(f.tool("cito_send", args, human())["seq"], recovered["seq"]);
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
        b.tool("cito_find", json!({"harness":"fixture"}), human())["listeners"],
        json!([])
    );
    let denied = b.tool_result(
        "cito_send",
        json!({"to":target["listener_id"],"message":"Private"}),
        human(),
    );
    assert_eq!(denied["isError"], true);
    assert!(denied.to_string().contains("FORBIDDEN_TARGET_SCOPE"));
    let public = a.tool(
        "cito_listen",
        json!({"about":"Workspace target","scope":"workspace"}),
        meta,
    );
    b.tool(
        "cito_send",
        json!({"to":public["listener_id"],"message":"Workspace"}),
        human(),
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
    b.tool(
        "cito_send",
        json!({"to":winner["listener_id"],"message":"Winner"}),
        human(),
    );
    wait(|| {
        a.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"]
            == "FENCED_INACTIVE"
    });
    assert_eq!(
        b.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "ACTIVE"
    );
    assert!(
        winner["binding_generation"].as_u64().unwrap()
            > old["binding_generation"].as_u64().unwrap()
    );
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
fn sqlite_pre_v4_upgrade_and_v4_reopen_preserve_outbound_recovery_rows() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let db = f.db("state.sqlite");
    db.execute("CREATE TABLE local_listeners(legacy TEXT);CREATE TABLE local_pairwise_wakes(legacy TEXT);PRAGMA user_version=3",&[]).unwrap();
    let daemon = f.daemon(&[]);
    assert_eq!(
        db.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        4
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
fn ui_ipc_actions_mutate_real_state_and_only_sandbox_host_services() {
    let _lock = serial();
    let owner = Fixture::new(json!({}));
    let manifest = json!({"id":"channel","kind":"human-channel","scope":"hub","displayName":"Fixture Channel",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],
        "pairing":{"supported":true},"fields":[
            {"key":"credential","scope":"hub","type":"secret","required":true},
            {"key":"address","scope":"user","type":"string","required":true}]});
    let remote_plugin = owner.state.join("hub/plugins/channel");
    fs::create_dir_all(&remote_plugin).unwrap();
    fs::write(remote_plugin.join("plugin.json"), manifest.to_string()).unwrap();
    let (_, hub_port) = owner.hub();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":format!("http://127.0.0.1:{hub_port}")}},"defaultHub":"local"}),
    );
    f.provider("delivered_unconfirmed");
    let plugin = f.state.join("plugins/channel");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"), manifest.to_string()).unwrap();
    let ui = f.recording_ui();
    let daemon = f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    for op in [
        "refresh_available",
        "open_settings",
        "close_settings",
        "open_directory",
        "query_directory",
        "close_directory",
    ] {
        assert_eq!(f.action(json!({"op":op})).0, 200, "{op}");
    }
    assert_eq!(f.action(json!({"op":"listen_session","harness":"fixture","session_id":"session-1","about":"IPC receiver","scope":"workspace"})).0,200);
    let row = f.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    let id = row["listener_id"].clone();
    assert_eq!(
        f.action(json!({"op":"switch_hub","from_hub":"local","to_hub":"work","listener_id":id}))
            .0,
        200
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE hub='local' AND harness='fixture'")[0]
            ["status"],
        "UNLISTENED"
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE hub='work' AND harness='fixture'")[0]
            ["status"],
        "ACTIVE"
    );
    let hub_db = owner.db("hub/hub.sqlite");
    assert_eq!(
        f.rows("SELECT scope FROM local_listeners WHERE hub='work' AND harness='fixture'")[0]
            ["scope"],
        "workspace"
    );
    assert_eq!(
        hub_db
            .query("SELECT scope FROM hub_kv WHERE key=?", &[id.clone()])
            .unwrap()[0]["scope"],
        "workspace"
    );
    for scope in ["self", "workspace"] {
        assert_eq!(
            f.action(json!({"op":"set_scope","hub":"work","listener_id":id,"scope":scope}))
                .0,
            200
        );
        assert_eq!(
            f.rows("SELECT scope FROM local_listeners WHERE hub='work' AND harness='fixture'")[0]
                ["scope"],
            scope
        );
        let registration = hub_db
            .query(
                "SELECT scope,CAST(value AS TEXT) AS value FROM hub_kv WHERE key=?",
                &[id.clone()],
            )
            .unwrap();
        assert_eq!(registration[0]["scope"], scope);
        let value: Value =
            serde_json::from_str(registration[0]["value"].as_str().unwrap()).unwrap();
        assert_eq!(value["scope"], scope);
    }
    for op in ["setup_apply", "setup_remove_app"] {
        assert_eq!(f.action(json!({"op":op,"harness":"fixture"})).0, 200);
    }
    let calls = fs::read_to_string(f.state.join("plugins/fixture/calls.jsonl")).unwrap();
    assert!(calls
        .lines()
        .any(|line| serde_json::from_str::<Value>(line).unwrap()["op"] == "setup_remove"));
    for op in ["channel_save", "channel_test", "channel_pair"] {
        let (s, v) = f.action(json!({"op":op,"channel_id":"missing"}));
        assert_eq!(s, 400);
        assert_eq!(
            v["error"],
            "INVALID_INPUT: Unknown human channel plugin: missing"
        );
    }
    let (status, saved) = f.action(
        json!({"op":"channel_save","hub":"work","channel_id":"channel",
        "enabled":true,"values":{"credential":"fixture-secret","address":"fixture-account"}}),
    );
    assert_eq!(status, 200, "{saved}");
    let channel = saved["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "channel")
        .unwrap();
    assert_eq!(channel["configured"], true);
    let service = hub_db
        .query(
            "SELECT config_json FROM hub_plugin_services WHERE plugin_id='channel'",
            &[],
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(service[0]["config_json"].as_str().unwrap()).unwrap(),
        json!({"credential":"fixture-secret"})
    );
    let binding = hub_db.query("SELECT enabled,user_config_json FROM hub_human_channels WHERE plugin_id='channel' AND principal_id='none#alice'", &[]).unwrap();
    assert_eq!(binding[0]["enabled"], 1);
    assert_eq!(
        serde_json::from_str::<Value>(binding[0]["user_config_json"].as_str().unwrap()).unwrap(),
        json!({"address":"fixture-account"})
    );
    let (status, tested) =
        f.action(json!({"op":"channel_test","hub":"work","channel_id":"channel"}));
    assert_eq!(status, 200, "{tested}");
    assert_eq!(tested["ok"], true);
    assert_eq!(tested["detail"], "Fixture test succeeded");
    let (status, paired) =
        f.action(json!({"op":"channel_pair","hub":"work","channel_id":"channel"}));
    assert_eq!(status, 200, "{paired}");
    let code = paired["code"].as_str().unwrap();
    assert!(code.starts_with("cito-"));
    assert_eq!(
        paired["instruction"],
        format!("Send {code} to fixture-account")
    );
    let pairing = hub_db.query("SELECT code,instruction,expires_ms,consumed_ms FROM hub_pairing_codes WHERE plugin_id='channel' AND principal_id='none#alice'", &[]).unwrap();
    assert_eq!(pairing[0]["code"], paired["code"]);
    assert_eq!(pairing[0]["instruction"], paired["instruction"]);
    assert_eq!(pairing[0]["expires_ms"], paired["expires_at_ms"]);
    assert!(pairing[0]["consumed_ms"].is_null());
    wait(|| {
        let frames: Vec<Value> = fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        frames.iter().any(|v| {
            v["op"] == "channel_test_result"
                && v["hub"] == "work"
                && v["channel_id"] == "channel"
                && v["ok"] == true
                && v["detail"] == "Fixture test succeeded"
        }) && frames.iter().any(|v| {
            v["op"] == "pair_code"
                && v["hub"] == "work"
                && v["channel_id"] == "channel"
                && v["code"] == paired["code"]
                && v["instruction"] == paired["instruction"]
                && v["expires_at_ms"] == paired["expires_at_ms"]
        })
    });
    assert_eq!(
        f.action(json!({"op":"set_launch_at_login","enabled":true}))
            .0,
        200
    );
    assert!(f
        .home
        .join("Library/LaunchAgents/com.tefx.cito.daemon.plist")
        .exists());
    assert_eq!(
        f.action(json!({"op":"set_launch_at_login","enabled":false}))
            .0,
        200
    );
    assert!(!f
        .home
        .join("Library/LaunchAgents/com.tefx.cito.daemon.plist")
        .exists());
    assert_eq!(
        f.action(json!({"op":"set_desktop_banner","enabled":false}))
            .0,
        200
    );
    let p = port();
    assert_eq!(f.action(json!({"op":"set_host_hub","enabled":true,"port":p,"bind":"127.0.0.1","auth_mode":"none"})).0,200);
    assert!(rpc(p, "kv_snapshot", json!({}))["entries"].is_array());
    let configured: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    let token = configured["host_hub"]["token"].as_str().unwrap();
    assert_eq!(token.len(), 64);
    assert!(token
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":false})).0,
        200
    );
    let sent = f.action(json!({"op":"send_signal","hub":"work","to":id,"message":"Agent inbox"}));
    assert_eq!(sent.0, 200, "{}", sent.1);
    wait(|| !f.rows("SELECT * FROM local_signals").is_empty());
    f.tool(
        "cito_inbox",
        json!({"hub":"work"}),
        json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
    );
    enroll(&f, human());
    f.tool(
        "cito_send",
        json!({"hub":"local","to":"@self","message":"Human inbox"}),
        json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
    );
    wait(|| {
        !f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL")
            .is_empty()
    });
    assert_eq!(
        f.action(json!({"op":"ack_signals","hub":"local","ack_all":true}))
            .0,
        200
    );
    assert!(f
        .rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL")
        .is_empty());
    assert_eq!(
        f.action(json!({"op":"unlisten","hub":"work","listener_id":id}))
            .0,
        200
    );
    assert_eq!(f.action(json!({"op":"unlisten_all"})).0, 200);
    assert_eq!(
        f.action(json!({"op":"setup_save_hub","name":"extra","url":"local://","auth_mode":"none"}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"setup_toggle_hub","hub":"extra","enabled":false}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"setup_remove_hub","hub":"extra"})).0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"hub_action","hub":"work","action":"reconnect"}))
            .0,
        200
    );
    assert_eq!(f.action(json!({"op":"quit"})).0, 200);
    wait(|| f.exited(daemon));
    let g = Fixture::new(json!({}));
    fs::write(g.state.join("retain-data"), "keep").unwrap();
    let daemon = g.daemon(&[]);
    assert_eq!(
        g.action(json!({"op":"setup_uninstall_all","purge_data":false}))
            .0,
        200
    );
    wait(|| g.exited(daemon));
    assert_eq!(
        fs::read_to_string(g.state.join("retain-data")).unwrap(),
        "keep"
    );
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
        4
    );
    assert_eq!(
        db.query("SELECT COUNT(*) AS n FROM local_signals", &[])
            .unwrap()[0]["n"],
        100
    );
}
fn human() -> Value {
    serde_json::from_str(HUMAN).unwrap()
}
fn enroll(f: &Fixture, meta: Value) -> Value {
    f.tool(
        "cito_listen",
        json!({"about":"Native receiver","scope":"workspace"}),
        meta,
    )
}
fn rpc(p: u16, op: &str, params: Value) -> Value {
    let mut v = params;
    v["op"] = json!(op);
    let (s, r) = http(p, "/v1/rpc", Some(&v));
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["ok"], true, "{r}");
    r["result"].clone()
}
#[test]
fn mcp_http_initialize_tool_schemas_and_malformed_boundaries() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let init=f.mcp("initialize",json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}));
    assert_eq!(init["serverInfo"]["version"], "0.19.0");
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
            "cito_listen",
            "cito_unlisten",
            "cito_find",
            "cito_send",
            "cito_inbox"
        ]
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
fn local_mcp_listen_find_send_inbox_ack_idempotency_and_channel_detach() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta.clone());
    assert_eq!(target["hub"], "local");
    assert_eq!(target["scope"], "workspace");
    let find = f.tool("cito_find", json!({"query":"Native receiver"}), human());
    assert!(find["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["listener_id"] == target["listener_id"]));
    let long = "Context payload segment ".repeat(100).trim().to_string();
    let args = json!({"to":target["listener_id"],"message":long,"idempotency_key":"stable"});
    let sent = f.tool("cito_send", args.clone(), human());
    assert_eq!(sent["status"], "wake_accepted");
    let again = f.tool("cito_send", args, human());
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
        f.tool("cito_unlisten", json!({}), meta)["status"],
        "unlistened"
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "UNLISTENED"
    );
}
#[test]
fn confirmed_delivery_business_ack_and_pending_hub_ack_reconciliation() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    let target = enroll(&f, meta);
    f.tool(
        "cito_send",
        json!({"to":target["listener_id"],"message":"confirmed"}),
        human(),
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
    assert_eq!(http(p, "/health", None).1["version"], "0.19.0");
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
        json!({"hubs":{"personal":{"url":format!("http://127.0.0.1:{pa}"),"user":"alice"},"work":{"url":format!("http://127.0.0.1:{pb}"),"user":"alice"}},"defaultHub":"personal"}),
    );
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    for hub in ["personal", "work"] {
        let target = f.tool(
            "cito_listen",
            json!({"hub":hub,"about":"Multi hub","scope":"workspace"}),
            meta.clone(),
        );
        let sent = f.tool(
            "cito_send",
            json!({"to":target["listener_id"],"message":hub}),
            human(),
        );
        assert!(sent["signal_id"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{hub}:")));
    }
    wait(|| f.rows("SELECT * FROM local_signals").len() == 2);
    a.stop(ha);
    b.stop(hb);
    let inbox = f.tool("cito_inbox", json!({}), meta.clone());
    assert_eq!(inbox["signals"].as_array().unwrap().len(), 2);
    assert_eq!(f.rows("SELECT * FROM pending_hub_acks").len(), 2);
    f.tool("cito_unlisten", json!({}), meta);
    assert!(f
        .rows("SELECT status FROM local_listeners WHERE harness='fixture'")
        .iter()
        .all(|v| v["status"] == "UNLISTENED"));
}
#[test]
fn live_user_reload_and_auth_mode_canonicalization_survive_restart() {
    let _lock = serial();
    let mut f =
        Fixture::new(json!({"user":"alice","hubs":{"local":{"url":"local://","authMode":"NONE"}}}));
    f.env.remove("CITO_USER");
    let daemon = f.daemon(&[]);
    let a = enroll(&f, human());
    assert!(a["listener_id"].as_str().unwrap().starts_with("alice-"));
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    config["user"] = json!("bob");
    fs::write(&f.config, config.to_string()).unwrap();
    let b = enroll(&f, human());
    assert!(
        b["listener_id"].as_str().unwrap().starts_with("bob-"),
        "{b}"
    );
    f.stop(daemon);
    f.daemon(&[]);
    assert!(enroll(&f, human())["listener_id"]
        .as_str()
        .unwrap()
        .starts_with("bob-"));
}
#[test]
fn ui_invites_and_config_mutations_validate_before_persistent_effects() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let before = fs::read(&f.config).unwrap();
    for invite in ["[]", "42", "true", "\"invite\"", "null"] {
        let (s, v) = f.action(json!({"op":"paste_hub_invite","invite":invite}));
        assert_eq!(s, 400);
        assert_eq!(v["error"], "INVALID_INPUT: Invalid hub invite");
        assert_eq!(fs::read(&f.config).unwrap(), before);
    }
    assert_eq!(f.action(json!({"op":"paste_hub_invite","invite":"{\"name\":\"work\",\"url\":\"local://\",\"authMode\":\"none\"}"})).0,200);
    assert_eq!(
        f.action(json!({"op":"set_desktop_banner","enabled":false}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"setup_save_hub","name":"other","url":"local://","auth_mode":"none"}))
            .0,
        200
    );
    let config: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    assert_eq!(config["desktopBannerEnabled"], false);
    assert_eq!(config["hubs"]["other"]["url"], "local://");
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
#[test]
fn cli_validation_dry_run_and_selective_harness_configuration() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for args in [
        vec!["setup", "--bogus"],
        vec!["listen", "--session", "orphan"],
        vec!["listen", "--harness", "missing"],
    ] {
        assert!(!f.command(NATIVE, &args, "").0);
    }
    let before = fs::read(&f.config).unwrap();
    assert!(
        f.command(
            NATIVE,
            &[
                "setup",
                "--enable",
                "pi",
                "--dry-run",
                "--json",
                "--no-login-item"
            ],
            ""
        )
        .0
    );
    assert_eq!(fs::read(&f.config).unwrap(), before);
    assert!(!f.home.join(".pi").exists());
    assert!(
        f.command(
            NATIVE,
            &["setup", "--enable", "pi", "--json", "--no-login-item"],
            ""
        )
        .0
    );
    assert!(f.home.join(".pi/agent/settings.json").exists());
    assert!(!f.home.join(".codex/config.toml").exists());
    assert!(!f.home.join(".claude/settings.json").exists());
    assert_eq!(
        f.command(
            f.state.join("bin/cito").to_str().unwrap(),
            &["--version"],
            ""
        )
        .1
        .trim(),
        "cito v0.19.0"
    );
}
