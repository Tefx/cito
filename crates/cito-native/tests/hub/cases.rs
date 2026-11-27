use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

fn test_engine(directory: &Path) -> TestEngine {
    TestEngine(HubEngine::open(directory).unwrap())
}

#[test]
fn detached_device_requires_a_new_generation() {
    let dir = temp();
    let e = test_engine(&dir);
    let owner = principal("alice");
    let mut req = json!({"host_id":"device","channel_id":"device:human","owner_token":"token","binding_generation":2});
    for op in [
        "human_device_attach",
        "human_device_attach",
        "human_device_detach",
    ] {
        e.rpc(op, &req, Some(&owner)).unwrap();
    }
    assert!(e
        .rpc("human_device_attach", &req, Some(&owner))
        .unwrap_err()
        .starts_with("STALE_GENERATION:"));
    req["binding_generation"] = json!(3);
    assert_eq!(
        e.rpc("human_device_attach", &req, Some(&owner)).unwrap()["record"]["channels"]
            ["device:human"]["state"],
        "ACTIVE"
    );
}

#[test]
fn raw_publish_and_kv_enforce_node_and_wire_limits() {
    let dir = temp();
    let e = test_engine(&dir);
    let id = "c_0123456789abcdef";
    let subject = format!("cito.signal.{id}");
    for from in [id, "alice-0123456789abcdef"] {
        let data = json!({"from":{"listener_id":from}});
        assert!(e
            .rpc(
                "stream_publish",
                &json!({"subject":subject,"data":data}),
                None
            )
            .unwrap_err()
            .starts_with("SELF_SEND_FORBIDDEN:"));
    }
    assert!(e
        .rpc(
            "stream_publish",
            &json!({"subject":subject,"data":"x".repeat(6*1024*1024)}),
            None
        )
        .unwrap_err()
        .starts_with("INVALID_INPUT:"));
    assert!(e
        .inner
        .db
        .query("SELECT seq FROM hub_signals", &[])
        .unwrap()
        .is_empty());
    let key = "a".repeat(1024);
    e.rpc("kv_put", &json!({"key":key,"data":"record"}), None)
        .unwrap();
    assert!(e
        .rpc("kv_create", &json!({"key":format!("{key}b")}), None)
        .unwrap_err()
        .starts_with("INVALID_INPUT:"));
}

#[test]
fn final_kv_values_fit_a_single_snapshot_entry_or_leave_the_prior_revision() {
    let dir = temp();
    let e = test_engine(&dir);
    e.pause_worker();
    let key = "\"".repeat(1024);
    let empty =
        json!({"key":key,"value_b64":"","revision":1,"created_ms":now_ms(),"operation":"PUT"});
    let max_raw =
        (2 * 1024 * 1024 - 128 - empty.to_string().len() - 1 - json!(key).to_string().len()) / 4
            * 3;
    let valid = "a".repeat(max_raw);
    let invalid = format!("{valid}b");
    for op in ["kv_create", "kv_update", "kv_put"] {
        let current = e.rpc("kv_get", &json!({"key":key}), None).unwrap();
        let error = e
            .rpc(
                op,
                &json!({"key":key,"revision":current["revision"],"data":invalid}),
                None,
            )
            .unwrap_err();
        assert!(error.starts_with("INVALID_INPUT:"), "{op}: {error}");
        let new = e
            .rpc(
                op,
                &json!({"key":key,"revision":current["revision"],"data":valid}),
                None,
            )
            .unwrap();
        assert_eq!(
            new["revision"].as_i64(),
            Some(current["revision"].as_i64().unwrap_or(0) + 1)
        );
        let page = e.rpc("kv_snapshot", &json!({}), None).unwrap();
        assert_eq!(page["entries"][0]["value_b64"], B64.encode(&valid));
        assert!(page.to_string().len() <= 2 * 1024 * 1024);
    }
    // A compact input fits before lease stamping, then grows past the budget.
    let mut reg = json!({"pad":"","channels":{"local":{"state":"ACTIVE"}}});
    reg["pad"] = json!("a".repeat(max_raw - reg.to_string().len()));
    assert!(e
        .rpc("kv_put", &json!({"key":key,"data":reg}), None)
        .unwrap_err()
        .starts_with("INVALID_INPUT:"));
    let owner = principal("alice");
    let invalid = "x".repeat(2 * 1024 * 1024);
    assert!(e.rpc("human_device_attach", &json!({"channel_id":"mac:human","host_id":"mac","host_name":invalid,"binding_generation":1,"owner_token":"token"}),Some(&owner)).unwrap_err().starts_with("INVALID_INPUT:"));
    assert!(e.inner.db.transaction(|c| attach(c,&json!({"principal_id":"none#alice","owner_user":"alice","plugin_id":"sample","display_label":invalid}),true,"workspace",None)).unwrap_err().starts_with("INVALID_INPUT:"));
    assert_eq!(
        e.rpc("kv_keys", &json!({}), None).unwrap()["keys"],
        json!([key])
    );
}

#[test]
fn bounded_receipts_and_key_pages_handle_legacy_rows_and_escaped_ids() {
    let dir = temp();
    let e = test_engine(&dir);
    e.pause_worker();
    let id = "\u{0001}".repeat(128);
    let publish = |subject, id: &str, text| {
        e.rpc(
            "js_publish",
            &json!({"subject":subject,"msg_id":"one","data":{"signal_id":id,"message":text}}),
            None,
        )
    };
    assert_eq!(
        publish("cito.signal.target", &id, "a").unwrap()["duplicate"],
        false
    );
    assert_eq!(
        publish("cito.signal.target", &id, "a").unwrap()["duplicate"],
        true
    );
    let replay = e
        .rpc(
            "js_publish",
            &json!({"subject":"cito.signal.target","data":{"signal_id":id,"message":"a"}}),
            None,
        )
        .unwrap();
    assert_eq!(replay, json!({"stream":SIGNALS,"seq":1,"duplicate":true}));
    for (subject, id, text) in [
        ("cito.signal.target", format!("{id}x"), "a"),
        ("cito.signal.target", id.clone(), "b"),
        ("cito.signal.other", id.clone(), "a"),
    ] {
        assert!(publish(subject, &id, text)
            .unwrap_err()
            .starts_with("INVALID_INPUT:"));
    }
    // These pre-admission-limit rows are individually readable. Their combined
    // escaped strings cross the frame cap, even though raw UTF-8 sizes fit.
    let legacy = "\u{0001}".repeat(750_000);
    for prefix in ["a", "b"] {
        let key = format!("{prefix}{legacy}");
        e.inner.db.execute("INSERT INTO hub_signals(subject,signal_id,created_ms,data) VALUES('cito.signal.target',?,?,X'00')",&[json!(key),json!(now_ms())]).unwrap();
        e.inner
            .db
            .execute(
                "INSERT INTO hub_kv(key,revision,created_ms,value) VALUES(?,1,1,X'00')",
                &[json!(format!("{prefix}{}", "\u{0001}".repeat(475_000)))],
            )
            .unwrap();
    }
    let batch = |seqs: Value| {
        e.rpc(
            "stream_get_message",
            &json!({"stream":SIGNALS,"seqs":seqs}),
            None,
        )
    };
    assert_eq!(batch(json!([1])).unwrap()["messages"][0]["signal_id"], id);
    assert!(batch(json!([2, 3]))
        .unwrap_err()
        .starts_with("INVALID_INPUT:"));
    for seq in [2, 3] {
        let page = batch(json!([seq])).unwrap();
        assert_eq!(page["messages"][0]["seq"], seq);
        assert!(json!({"ok":true,"result":page}).to_string().len() <= 8 * 1024 * 1024);
    }
    let first = e.rpc("kv_keys", &json!({}), None).unwrap();
    assert_eq!(first["keys"].as_array().unwrap().len(), 1);
    assert_eq!(first["next_cursor"], first["keys"][0]);
    let last = e
        .rpc("kv_keys", &json!({"after_key":first["next_cursor"]}), None)
        .unwrap();
    assert!(last["keys"][0].as_str().unwrap().starts_with('b'));
    assert!(last["next_cursor"].is_null());
    for page in [first, last] {
        assert!(json!({"ok":true,"result":page}).to_string().len() <= 8 * 1024 * 1024);
    }
}

#[test]
fn signal_references_share_a_raw_utf8_byte_budget() {
    let dir = temp();
    let e = test_engine(&dir);
    let id = "é".repeat(64);
    let publish = |data: Value| {
        e.rpc(
            "js_publish",
            &json!({"subject":"cito.signal.target","data":data}),
            None,
        )
    };
    for field in ["signal_id", "in_reply_to"] {
        for invalid in [json!(""), json!(format!("{id}x")), json!(42)] {
            assert!(publish(json!({field:invalid}))
                .unwrap_err()
                .starts_with("INVALID_INPUT:"));
        }
    }
    let added = publish(json!({"signal_id":id,"in_reply_to":id})).unwrap();
    assert_eq!(added["seq"], 1);
    let read = e
        .rpc(
            "stream_get_message",
            &json!({"stream":SIGNALS,"seq":1}),
            None,
        )
        .unwrap();
    let data: Value = serde_json::from_slice(&B64.decode(s(&read, "data_b64")).unwrap()).unwrap();
    assert_eq!(data, json!({"signal_id":id,"in_reply_to":id}));
}

#[test]
fn terminal_delivery_and_opaque_reply_retention_have_independent_clocks() {
    let dir = temp();
    let e = test_engine(&dir);
    e.pause_worker();
    e.rpc(
        "stream_update",
        &json!({"stream":SIGNALS,"patch":{"max_age":0}}),
        None,
    )
    .unwrap();
    let old = json!(now_ms() - 604_800_001);
    for status in ["delivered", "effect_unknown", "failed_permanent"] {
        send(&e, "target", status);
        e.inner.db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,delivered_ms) VALUES('fixture','owner',?,1,?,?)",&[json!(status),json!(status),old.clone()]).unwrap();
    }
    e.inner.db.execute("INSERT INTO hub_processed_replies VALUES('fixture','opaque:expired',?),('fixture','opaque:fresh',?)",&[old,json!(now_ms())]).unwrap();
    e.inner.db.transaction(prune).unwrap();
    assert_eq!(
        e.inner
            .db
            .query("SELECT status FROM hub_channel_deliveries", &[])
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        e.inner
            .db
            .query("SELECT update_id FROM hub_processed_replies", &[])
            .unwrap(),
        vec![json!({"update_id":"opaque:fresh"})]
    );
    e.inner.db.execute("DELETE FROM hub_signals", &[]).unwrap();
    e.inner.db.transaction(prune).unwrap();
    assert!(e
        .inner
        .db
        .query("SELECT status FROM hub_channel_deliveries", &[])
        .unwrap()
        .is_empty());
}

#[test]
fn human_mutations_fence_expired_devices_without_prior_reads() {
    for op in [
        "human_device_attach",
        "human_device_heartbeat",
        "human_device_detach",
    ] {
        let dir = temp();
        let e = test_engine(&dir);
        e.pause_worker();
        let owner = principal("alice");
        let request = json!({"channel_id":"device:human","host_id":"device","binding_generation":3,"owner_token":"old"});
        let mut row = e
            .rpc("human_device_attach", &request, Some(&owner))
            .unwrap()["record"]
            .clone();
        row["channels"]["device:human"]["updated_at_ms"] = json!(1);
        e.inner
            .db
            .execute(
                "UPDATE hub_kv SET value=CAST(? AS BLOB) WHERE key=?",
                &[json!(row.to_string()), row["listener_id"].clone()],
            )
            .unwrap();
        assert!(
            e.rpc(op, &request, Some(&owner))
                .unwrap_err()
                .starts_with("STALE_GENERATION:"),
            "{op}"
        );
        let mut fresh = request.clone();
        fresh["binding_generation"] = json!(4);
        fresh["owner_token"] = json!("new");
        let reg = e.rpc("human_device_attach", &fresh, Some(&owner)).unwrap()["record"].clone();
        assert_eq!(reg["device_generation_fences"]["device:human"], 3);
        assert_eq!(reg["channels"]["device:human"]["binding_generation"], 4);
    }
}

#[test]
fn current_reply_keys_reject_conflicting_payloads() {
    let dir = temp();
    let e = test_engine(&dir);
    e.pause_worker();
    let owner = principal("alice");
    put(&e, "target", "none#bob", "workspace");
    let req = json!({"to":"target","message":"Approved","in_reply_to":"source","msg_id":"once"});
    let first = e.rpc("send_reply_from_human", &req, Some(&owner)).unwrap();
    for (field, value) in [("message", "Rejected"), ("in_reply_to", "other")] {
        let mut conflict = req.clone();
        conflict[field] = json!(value);
        assert_eq!(
            e.rpc("send_reply_from_human", &conflict, Some(&owner))
                .unwrap_err(),
            "IDEMPOTENCY_CONFLICT: reply payload differs"
        );
    }
    assert_eq!(
        e.rpc("send_reply_from_human", &req, Some(&owner)).unwrap()["seq"],
        first["seq"]
    );
}

#[test]
fn human_replies_resolve_stored_ids_before_hub_qualifiers() {
    let dir = temp();
    let e = test_engine(&dir);
    let owner = principal("alice");
    let human = listener_id("none#alice", "human", "default");
    put(&e, "target", "none#bob", "workspace");
    for id in ["1", "event:1", "thread:source"] {
        send_from(&e, &human, id, "none#bob", "target");
    }
    for (field, supplied, expected) in [
        ("in_reply_to", "work:thread:source", "thread:source"),
        ("in_reply_to", "event:1", "event:1"),
        ("in_reply_to", "work:thread:missing", "thread:missing"),
        ("in_reply_to", "plain-missing", "plain-missing"),
        ("signal_id", "work:thread:missing", "thread:missing"),
        ("signal_id", "event:1", "event:1"),
        ("signal_id", "Not-a-hub:1", "Not-a-hub:1"),
    ] {
        let reply = e
            .rpc(
                "send_reply_from_human",
                &json!({"to":"target","message":"Approved",field:supplied}),
                Some(&owner),
            )
            .unwrap();
        let row = e
            .rpc(
                "stream_get_message",
                &json!({"stream":SIGNALS,"seq":reply["seq"]}),
                None,
            )
            .unwrap();
        let data: Value =
            serde_json::from_slice(&B64.decode(s(&row, "data_b64")).unwrap()).unwrap();
        assert_eq!(data["in_reply_to"], expected);
    }
    let receipts = e
        .rpc(
            "stream_get_message",
            &json!({"stream":SIGNALS,"seqs":[1,2,3]}),
            None,
        )
        .unwrap();
    assert!(receipts["messages"][0]["acked_at_ms"].is_null());
    for row in &receipts["messages"].as_array().unwrap()[1..] {
        assert!(row["acked_at_ms"].is_number());
    }
}

#[test]
fn consumer_ack_proves_sequence_filter_and_target_ownership_before_receipts() {
    let dir = temp();
    let e = test_engine(&dir);
    e.pause_worker();
    let alice = json!({"principal_id":"oidc:https://id.example.com|alice","owner_user":"alice","oidc":true});
    let bob =
        json!({"principal_id":"oidc:https://id.example.com|bob","owner_user":"bob","oidc":true});
    put(&e, "alice-node", s(&alice, "principal_id"), "workspace");
    put(&e, "bob-node", s(&bob, "principal_id"), "workspace");
    send(&e, "alice-node", "a");
    send(&e, "bob-node", "b");
    e.rpc(
        "consumer_add",
        &json!({"config":{"durable_name":"one","filter_subject":"cito.signal.alice-node"}}),
        Some(&alice),
    )
    .unwrap();
    e.rpc(
        "consumer_add",
        &json!({"config":{"durable_name":"wide","filter_subject":"cito.signal.*"}}),
        Some(&alice),
    )
    .unwrap();
    let events = e.subscribe(None).unwrap();
    for (name, seq) in [("one", 999), ("one", 2), ("wide", 2)] {
        assert!(e
            .rpc(
                "consumer_ack",
                &json!({"name":name,"seq":seq}),
                Some(&alice)
            )
            .is_err());
    }
    assert!(e
        .inner
        .db
        .query("SELECT * FROM hub_consumer_acks", &[])
        .unwrap()
        .is_empty());
    assert!(events.try_recv().is_err());
    e.rpc("consumer_ack", &json!({"name":"one","seq":1}), Some(&alice))
        .unwrap();
    assert_eq!(
        e.inner
            .db
            .query("SELECT * FROM hub_consumer_acks", &[])
            .unwrap()
            .len(),
        1
    );
}

fn temp() -> TestDirectory {
    let dir = TestDirectory(std::env::temp_dir().join(random_id("cito-hub-test-").unwrap()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
#[test]
fn committed_ack_and_human_reply_survive_notification_failure() {
    let dir = temp();
    let e = test_engine(&dir);
    e.pause_worker();
    let owner = principal("alice");
    let human = e.rpc("human_device_attach", &json!({"channel_id":"host:human","host_id":"host","host_name":"Host","binding_generation":1,"owner_token":"owner"}), Some(&owner)).unwrap()["record"]["listener_id"].as_str().unwrap().to_owned();
    e.inner.db.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,enabled) VALUES('none#alice','fixture',1)", &[]).unwrap();
    put(&e, "target", "none#bob", "workspace");
    send(&e, &human, "ack-source");
    send_from(&e, &human, "reply-source", "none#bob", "target");
    let root = fixture(&dir);
    let mut plugin = hub_plugins::discover(&[(root, false)], None)
        .unwrap()
        .remove(0);
    // Close the pipe after accepting configure; the cursor confirms stdin has no reader.
    plugin.command = vec!["/bin/sh".into(), "-c".into(), "read -r line; exec 0<&-; printf '%s\\n' '{\"op\":\"cursor_save\",\"cursor\":1}'; exec sleep 30".into()];
    e.ensure_process(&plugin).unwrap();
    wait_for(|| e.inner.db.transaction(|c| service(c, "fixture")).unwrap().1["cursor"] == 1);
    let ack = json!({"node_id":human,"signal_ids":["ack-source"]});
    assert!(
        e.notify_ack(&ack).is_err(),
        "The post-commit notification seam must actually fail"
    );
    let committed = e.rpc("signal_ack", &ack, Some(&owner)).unwrap();
    assert!(committed["acked_at_ms"].is_number());
    let request = json!({"to":"target","message":"Approved","in_reply_to":"reply-source","msg_id":"reply-once"});
    let reply = e
        .rpc("send_reply_from_human", &request, Some(&owner))
        .unwrap();
    assert_eq!(reply["ok"], true);
    assert!(reply["acked_at_ms"].is_number());
    let retried = e
        .rpc("send_reply_from_human", &request, Some(&owner))
        .unwrap();
    assert_eq!(retried["signal_id"], reply["signal_id"]);
    assert_eq!(
        e.inner
            .db
            .query("SELECT signal_id FROM hub_signals", &[])
            .unwrap()
            .len(),
        3
    );
    assert!(e.inner.db.query("SELECT signal_id FROM hub_signals WHERE signal_id IN ('ack-source','reply-source') AND acked_at_ms IS NULL", &[]).unwrap().is_empty());
    e.inner
        .db
        .execute(
            "UPDATE hub_human_channels SET user_config_json=?",
            &[json!(
                json!({"_external_chat_id":" room ","_external_user_id":"user"}).to_string()
            )],
        )
        .unwrap();
    send_from(&e, &human, "thread:source", "none#bob", "target");
    e.inner.db.execute("INSERT INTO hub_channel_delivery_msgs VALUES('fixture','none#alice','chunk','thread:source','target')",&[]).unwrap();
    let mut frame = json!({"op":"inbound_reply","external_chat_id":"room","external_user_id":"user","reply_to_external_msg_id":"chunk","update_id":"opaque","text":"Reply"});
    assert_eq!(e.plugin_frame("fixture", &frame).unwrap()["ok"], false);
    frame["external_chat_id"] = json!(" room ");
    let committed = e.plugin_frame("fixture", &frame).unwrap();
    assert_eq!(committed["ok"], true, "{committed}");
    assert_eq!(
        e.plugin_frame("fixture", &frame).unwrap()["duplicate"],
        true
    );
    assert!(e
        .inner
        .db
        .query(
            "SELECT acked_at_ms FROM hub_signals WHERE signal_id='thread:source'",
            &[]
        )
        .unwrap()[0]["acked_at_ms"]
        .is_number());
}

#[test]
fn silent_exit_keeps_its_health_warning_and_backs_off_relaunch() {
    let dir = temp();
    let hub = test_engine(&dir);
    hub.pause_worker();
    let root = fixture(&dir);
    let plugin_dir = root.join("fixture");
    let path = plugin_dir.join("plugin.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["command"] = json!([
        "/bin/sh",
        "-c",
        "echo started >> starts; while read -r line; do case $line in *finish*) exit;; esac; done"
    ]);
    std::fs::write(path, manifest.to_string()).unwrap();
    hub.set_plugin_roots(&[root]).unwrap();
    hub.rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","hub_config":{"credential":"armed"}}),
        Some(&principal("alice")),
    )
    .unwrap();
    let process = hub.ensure_process(&hub.plugin("fixture").unwrap()).unwrap();
    process.send(&json!({"op":"finish"})).unwrap();
    let health = || {
        hub.inner
            .db
            .transaction(|c| service(c, "fixture"))
            .unwrap()
            .1
    };
    wait_for(|| health()["poll_error"] == "Plugin subprocess exited");
    assert!(hub.configure_process("fixture", &process).is_err());
    assert_eq!(health()["poll_error"], "Plugin subprocess exited");
    assert!(hub
        .tick_channels()
        .unwrap_err()
        .contains("waiting to restart"));
    assert_eq!(
        std::fs::read_to_string(plugin_dir.join("starts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn failed_initial_configuration_evicts_process_before_retry() {
    let dir = temp();
    let e = test_engine(&dir);
    e.pause_worker();
    let root = fixture(&dir);
    let plugin = hub_plugins::discover(&[(root, false)], None)
        .unwrap()
        .remove(0);
    e.inner
        .db
        .execute("DROP TABLE hub_plugin_services", &[])
        .unwrap();
    assert!(e.ensure_process(&plugin).is_err());
    assert!(!e.inner.processes.lock().unwrap().contains_key(&plugin.id));
    e.inner.db.execute("CREATE TABLE hub_plugin_services(plugin_id TEXT PRIMARY KEY,config_json TEXT NOT NULL DEFAULT '{}',service_state_json TEXT NOT NULL DEFAULT '{}',updated_ms INTEGER NOT NULL DEFAULT 0)", &[]).unwrap();
    let process = e.ensure_process(&plugin).unwrap();
    assert!(process.alive());
    assert!(Arc::ptr_eq(
        &process,
        e.inner.processes.lock().unwrap().get(&plugin.id).unwrap()
    ));
    e.shutdown();
    assert!(!process.alive());
}

#[test]
fn processed_reply_pruning_uses_a_monotonic_replay_floor_and_resets_with_credentials() {
    let dir = temp();
    let e = test_engine(&dir);
    e.set_plugin_roots(&[]).unwrap();
    e.inner
        .db
        .transaction(|c| {
            save_service(c, "messenger", &json!({"credential":"first"}))?;
            for id in ["1", "2", "3", "4", "opaque"] {
                c.execute(
                    "INSERT INTO hub_processed_replies VALUES('messenger',?,1)",
                    &[json!(id)],
                )?;
            }
            Ok(())
        })
        .unwrap();
    e.plugin_frame("messenger", &json!({"op":"cursor_save","cursor":"3"}));
    e.plugin_frame("messenger", &json!({"op":"cursor_save","cursor":"2"}));
    let (_, state) = e.inner.db.transaction(|c| service(c, "messenger")).unwrap();
    assert_eq!(state["reply_floor"], 3);
    assert_eq!(state["cursor"], 3);
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT update_id FROM hub_processed_replies ORDER BY update_id",
                &[]
            )
            .unwrap()
            .into_iter()
            .map(|r| r["update_id"].clone())
            .collect::<Vec<_>>(),
        vec![json!("3"), json!("4"), json!("opaque")]
    );
    let ack = e
        .inbound_reply("messenger", &json!({"update_id":"2"}))
        .unwrap();
    assert_eq!(ack["duplicate"], true);
    e.plugin_frame("messenger", &json!({"op":"cursor_save","cursor":"5"}));
    assert_eq!(
        e.inbound_reply("messenger", &json!({"update_id":"4"}))
            .unwrap()["duplicate"],
        true
    );
    assert!(e
        .inner
        .db
        .query("SELECT * FROM hub_signals", &[])
        .unwrap()
        .is_empty());
    e.inner
        .db
        .transaction(|c| {
            save_service(c, "messenger", &json!({"credential":"second"}))?;
            Ok(())
        })
        .unwrap();
    assert!(e
        .inner
        .db
        .query("SELECT * FROM hub_processed_replies", &[])
        .unwrap()
        .is_empty());
    assert!(e
        .inner
        .db
        .transaction(|c| service(c, "messenger"))
        .unwrap()
        .1["reply_floor"]
        .is_null());
}
#[test]
fn pruned_device_cannot_reattach_a_superseded_generation() {
    let dir = temp();
    let e = test_engine(&dir);
    e.set_plugin_roots(&[]).unwrap();
    let owner = json!({"principal_id":"none#alice","owner_user":"alice"});
    let mut old = json!({"channel_id":"old:human","host_id":"old","host_name":"Laptop","binding_generation":5,"owner_token":"old-owner"});
    let attached = e.rpc("human_device_attach", &old, Some(&owner)).unwrap();
    old["node_id"] = attached["record"]["listener_id"].clone();
    e.rpc("human_device_detach", &old, Some(&owner)).unwrap();
    let sibling = json!({"channel_id":"new:human","host_id":"new","host_name":"Laptop","binding_generation":1,"owner_token":"new-owner"});
    let fresh = e
        .rpc("human_device_attach", &sibling, Some(&owner))
        .unwrap();
    assert!(fresh["record"]["channels"].get("old:human").is_none());
    assert!(e
        .rpc("human_device_attach", &old, Some(&owner))
        .unwrap_err()
        .starts_with("STALE_GENERATION:"));
    old["binding_generation"] = json!(6);
    let replaced = e.rpc("human_device_attach", &old, Some(&owner)).unwrap();
    assert_eq!(
        replaced["record"]["channels"]["new:human"]["owner_token"],
        "new-owner"
    );
    assert_eq!(replaced["record"]["human_control_revision"], 1);
}
#[test]
fn human_device_fencing_inbox_cas_and_publish_pause_are_atomic() {
    let dir = temp();
    let e = test_engine(&dir);
    e.set_plugin_roots(&[]).unwrap();
    let owner = json!({"principal_id":"none#alice","owner_user":"alice"});
    let id = listener_id("none#alice", "human", "default");
    let device = |host: &str, generation: u64, token: &str| json!({"node_id":id,"channel_id":format!("{host}:human"),"host_id":host,"host_name":host,"binding_generation":generation,"owner_token":token});
    let a = device("laptop", 1, "a");
    let b = device("desktop", 1, "b");
    let attached = e.rpc("human_device_attach", &a, Some(&owner)).unwrap();
    assert_eq!(attached["record"]["about"], "Human");
    assert_eq!(attached["record"]["scope"], "workspace");
    assert_eq!(
        attached["record"]["channels"]["laptop:human"]["updated_at_ms"],
        attached["server_now_ms"]
    );
    e.rpc("human_device_attach", &b, Some(&owner)).unwrap();
    e.inner.db.transaction(|c| {
        let mut reg=parse(&kv_row(c,&id)?.unwrap(),"value");
        reg["channels"]["hub:messenger"]=json!({"channel_id":"hub:messenger","host_id":"hub","display_name":"Messenger","state":"ACTIVE","updated_at_ms":1});
        save_human(c,&id,&mut reg)?; Ok(())
    }).unwrap();
    let pause=e.rpc("human_inbox_update",&json!({"node_id":id,"expected_control_revision":1,"paused":true,"about":"Approvals","scope":"self"}),Some(&owner)).unwrap();
    let sibling = pause["record"]["channels"]["desktop:human"].clone();
    let detached = e.rpc("human_device_detach", &a, Some(&owner)).unwrap();
    assert_eq!(detached["record"]["human_paused"], true);
    assert_eq!(detached["record"]["human_control_revision"], 2);
    assert_eq!(detached["record"]["channels"]["desktop:human"], sibling);
    assert_eq!(
        detached["record"]["channels"]["hub:messenger"]["state"],
        "ACTIVE"
    );
    assert_eq!(detached["record"]["about"], "Approvals");
    assert!(e.rpc("js_publish",&json!({"subject":format!("cito.signal.{id}"),"data":{"signal_id":"paused","message":"blocked"}}),Some(&owner)).unwrap_err().starts_with("LISTENER_NOT_FOUND:"));
    let resume = e
        .rpc(
            "human_inbox_update",
            &json!({"node_id":id,"expected_control_revision":2,"paused":false}),
            Some(&owner),
        )
        .unwrap();
    assert_eq!(resume["record"]["about"], "Approvals");
    assert_eq!(resume["record"]["scope"], "self");
    assert!(e
        .rpc(
            "human_inbox_update",
            &json!({"node_id":id,"expected_control_revision":2,"paused":true}),
            Some(&owner)
        )
        .unwrap_err()
        .starts_with("CONTROL_REVISION_CONFLICT:"));
    let heartbeat = e.rpc("human_device_heartbeat", &b, Some(&owner)).unwrap();
    assert_eq!(heartbeat["record"]["human_control_revision"], 3);
    assert_eq!(heartbeat["record"]["human_paused"], false);
    let replacement = device("desktop", 2, "replacement");
    e.rpc("human_device_attach", &replacement, Some(&owner))
        .unwrap();
    for op in [
        "human_device_attach",
        "human_device_heartbeat",
        "human_device_detach",
    ] {
        assert!(e
            .rpc(op, &b, Some(&owner))
            .unwrap_err()
            .starts_with("STALE_GENERATION:"));
    }
    let state = e
        .rpc("human_device_heartbeat", &replacement, Some(&owner))
        .unwrap();
    assert_eq!(
        state["record"]["channels"]["laptop:human"]["state"],
        "UNLISTENED"
    );
    assert!(e
        .rpc(
            "kv_put",
            &json!({"key":id,"data":state["record"]}),
            Some(&owner)
        )
        .unwrap_err()
        .starts_with("FORBIDDEN:"));
    let stranger = json!({"principal_id":"none#bob","owner_user":"bob"});
    assert!(e
        .rpc("human_device_detach", &replacement, Some(&stranger))
        .unwrap_err()
        .starts_with("FORBIDDEN:"));
}

#[test]
fn fresh_device_adopts_retained_human_identity_and_verified_alias() {
    for alias in [false, true] {
        let dir = temp();
        let e = Arc::new(HubEngine::open(&dir.join("hub")).unwrap());
        let _cleanup = TestEngine((*e).clone());
        e.set_plugin_roots(&[]).unwrap();
        let mut client = super::super::transport::HubClient::new(
            "local".into(),
            json!({"url":"local://"}),
            "alice",
            Some(e.clone()),
        );
        let stored_pid = "none#alice";
        if alias {
            client.identity = json!({"principal_id":"oidc:https://id.example.com|alice","legacy_principal_id":stored_pid,"owner_user":"alice","oidc":true});
        }
        // A retained v1 key need not be derivable by this freshly enrolled Mac.
        let legacy_id = format!("c_{}", &hash("cito-v1|none#alice|human|default")[..16]);
        e.inner.db.transaction(|c| {
            let mut record = json!({"listener_id":legacy_id,"public_id":"alice-legacy","principal_id":stored_pid,"owner_user":"alice","harness":"human","session_id":"default","title":"Human","about":"Release approvals","scope":"self","human_paused":true,"human_control_revision":7,"binding_generation":1,"channels":{"sibling:human":{"host_id":"sibling","state":"ACTIVE","binding_generation":9,"owner_token":"sibling-owner","updated_at_ms":now_ms()}}});
            save_human(c, &legacy_id, &mut record)?; Ok(())
        }).unwrap();
        let before = client.kv_get(&legacy_id).unwrap().unwrap().0;
        let local = super::super::state::LocalState::open(&dir.join("state")).unwrap();
        for automatic in [true, false] {
            let args = json!({"about":"Must not overwrite","scope":"workspace"});
            let caller = json!({"harness":"human","session_id":"default"});
            let enrolled = if automatic {
                super::super::registration::register_automatic_human(
                    &local, &client, "fresh", &args, &caller,
                )
            } else {
                super::super::registration::register_endpoint(
                    &local, &client, "fresh", &args, &caller,
                )
            }
            .unwrap();
            assert_eq!(enrolled["canonical_listener_id"], legacy_id);
            assert!(local.listener("local", &legacy_id).unwrap().is_some());
            let record = client.kv_get(&legacy_id).unwrap().unwrap().0;
            for field in ["about", "scope", "human_paused", "human_control_revision"] {
                assert_eq!(record[field], before[field]);
            }
            assert_eq!(
                record["channels"]["sibling:human"],
                before["channels"]["sibling:human"]
            );
            assert_eq!(record["channels"]["fresh:human"]["state"], "ACTIVE");
        }
    }
}

#[test]
fn human_creation_scope_uses_hub_trust_on_both_entrypaths() {
    for (mode, expected) in [("none", "workspace"), ("token", "self"), ("oidc", "self")] {
        for device_first in [true, false] {
            let dir = temp();
            let e = TestEngine(HubEngine::open_with_auth_mode(&dir, mode).unwrap());
            e.set_plugin_roots(&[]).unwrap();
            let owner = json!({"principal_id":"none#alice","owner_user":"alice","auth_mode":mode});
            let device = json!({"host_id":"mac","channel_id":"mac:human","binding_generation":1,"owner_token":"owner"});
            if device_first {
                e.rpc("human_device_attach", &device, Some(&owner)).unwrap();
            }
            e.inner.db.transaction(|c| {
                attach(c, &json!({"principal_id":"none#alice","owner_user":"alice","plugin_id":"messenger"}), true, e.inner.default_human_scope, None)
            }).unwrap();
            if !device_first {
                e.rpc("human_device_attach", &device, Some(&owner)).unwrap();
            }
            let record = e
                .inner
                .db
                .transaction(|c| {
                    Ok(parse(
                        &kv_row(c, &listener_id("none#alice", "human", "default"))?.unwrap(),
                        "value",
                    ))
                })
                .unwrap();
            assert_eq!(
                record["scope"], expected,
                "{mode}, device_first={device_first}"
            );
            assert_eq!(record["title"], "Human");
        }
    }
}

#[test]
fn token_boundary_on_trusted_embedded_hub_preserves_creation_scope_through_pairing_restart() {
    use super::super::transport::HubClient;
    let dir = temp();
    let root = fixture(&dir);
    let e = Arc::new(test_engine(&dir));
    e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    let server = e
        .start_server(
            "127.0.0.1",
            0,
            HubAuth::new("token", Some("test-only".into()), None, None).unwrap(),
        )
        .unwrap();
    let client = |user: &str| {
        HubClient::new(
            "work".into(),
            json!({"url":format!("http://127.0.0.1:{}", server.port()),"authMode":"token","token":"test-only"}),
            user,
            None,
        )
    };
    let device = json!({"host_id":"mac","channel_id":"mac:human","binding_generation":1,"owner_token":"owner"});
    let trusted = json!({"principal_id":"none#local","owner_user":"local"});
    let local = e
        .rpc("human_device_attach", &device, Some(&trusted))
        .unwrap();
    assert_eq!(local["record"]["scope"], "workspace");
    let device_first = client("device");
    assert_eq!(
        device_first
            .human_request("human_device_attach", &device)
            .unwrap()["record"]["scope"],
        "self"
    );
    let manual_first = client("manual");
    manual_first.rpc("human_channels_configure", &json!({"plugin_id":"fixture","hub_config":{"credential":"test-only"},"user_config":{"address":"room"}})).unwrap();
    assert_eq!(
        manual_first
            .kv_get(&listener_id("none#manual", "human", "default"))
            .unwrap()
            .unwrap()
            .0["scope"],
        "self"
    );
    let pairing_first = client("paired");
    let pair = pairing_first
        .rpc("human_channel_pair_start", &json!({"plugin_id":"fixture"}))
        .unwrap();
    assert!(pairing_first
        .kv_get(&listener_id("none#paired", "human", "default"))
        .unwrap()
        .is_none());
    server.shutdown();
    e.shutdown();
    // Pending codes retain the requesting boundary even when restored by a trusted engine.
    let e = test_engine(&dir);
    e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    assert_eq!(e.redeem("fixture", &json!({"code":pair["code"],"is_private_chat":true,"external_chat_id":"paired-chat","external_user_id":"paired-user","user_config_patch":{"address":"paired-room"}})).unwrap()["ok"], true);
    let owner = json!({"principal_id":"none#paired","owner_user":"paired","auth_mode":"token"});
    let id = listener_id("none#paired", "human", "default");
    let record = e
        .inner
        .db
        .transaction(|c| Ok(parse(&kv_row(c, &id)?.unwrap(), "value")))
        .unwrap();
    assert_eq!(record["scope"], "self");
    e.rpc(
        "human_inbox_update",
        &json!({"node_id":id,"expected_control_revision":1,"scope":"workspace"}),
        Some(&owner),
    )
    .unwrap();
    // A second pairing must preserve explicitly changed policy.
    let pair = e
        .rpc(
            "human_channel_pair_start",
            &json!({"plugin_id":"fixture"}),
            Some(&owner),
        )
        .unwrap();
    e.redeem("fixture", &json!({"code":pair["code"],"is_private_chat":true,"external_chat_id":"paired-chat","external_user_id":"paired-user","user_config_patch":{"address":"paired-room"}})).unwrap();
    e.shutdown();
    let e = test_engine(&dir);
    e.set_plugin_roots(&[root]).unwrap();
    e.restore_attachments().unwrap();
    for (user, scope) in [
        ("device", "self"),
        ("manual", "self"),
        ("paired", "workspace"),
        ("local", "workspace"),
    ] {
        let record = e
            .inner
            .db
            .transaction(|c| {
                Ok(parse(
                    &kv_row(c, &listener_id(&format!("none#{user}"), "human", "default"))?.unwrap(),
                    "value",
                ))
            })
            .unwrap();
        assert_eq!(record["scope"], scope, "{user}");
    }
}

#[test]
fn idle_and_policy_only_hubs_prune_expired_audit_rows_without_channels() {
    let dir = temp();
    let e = test_engine(&dir);
    e.set_plugin_roots(&[]).unwrap();
    let owner = json!({"principal_id":"none#alice","owner_user":"alice"});
    let device = json!({"host_id":"mac","channel_id":"mac:human","binding_generation":1,"owner_token":"owner"});
    e.rpc("human_device_attach", &device, Some(&owner)).unwrap();
    e.rpc("human_device_detach", &device, Some(&owner)).unwrap();
    assert!(e
        .inner
        .db
        .query("SELECT * FROM hub_human_channels", &[])
        .unwrap()
        .is_empty());
    for (op, params) in [
        (
            "human_inbox_update",
            json!({"expected_control_revision":1,"paused":true}),
        ),
        (
            "human_inbox_update",
            json!({"expected_control_revision":2,"paused":false}),
        ),
        ("kv_snapshot", json!({})),
    ] {
        // Advance the maintenance deadline through persisted clock state, without sleeps.
        e.inner.db.transaction(|c| {
            c.execute("INSERT INTO hub_audit_log(ts_ms,principal_id,action,target_id) VALUES(?,'alice','expired','node'),(?,'alice','recent','node')", &[json!(now_ms()-604_801_000),json!(now_ms())])?;
            c.execute("UPDATE hub_meta SET int_val=0 WHERE key='last_maintenance_ms'", &[])?;
            Ok(())
        }).unwrap();
        e.rpc(op, &params, Some(&owner)).unwrap();
        assert!(
            e.inner
                .db
                .query("SELECT id FROM hub_audit_log WHERE action='expired'", &[])
                .unwrap()
                .is_empty(),
            "{op}"
        );
    }
    assert_eq!(
        e.inner
            .db
            .query("SELECT id FROM hub_audit_log WHERE action='recent'", &[])
            .unwrap()
            .len(),
        3
    );
    let record = e
        .inner
        .db
        .transaction(|c| {
            Ok(parse(
                &kv_row(c, &listener_id("none#alice", "human", "default"))?.unwrap(),
                "value",
            ))
        })
        .unwrap();
    assert_eq!(record["channels"]["mac:human"]["state"], "UNLISTENED");
}

#[test]
fn hub_stamps_changed_agent_leases_and_prunes_audit_history() {
    let dir = temp();
    let e = test_engine(&dir);
    e.set_plugin_roots(&[]).unwrap();
    for skew in [-120_000, 120_000] {
        let id = format!("agent_{}", if skew < 0 { "behind" } else { "ahead" });
        let client_time = now_ms() + skew;
        let mut record = json!({"listener_id":id,"harness":"fixture","session_id":"session","principal_id":"none#alice","scope":"workspace","state":"ACTIVE","binding_generation":1,"channels":{"mac:fixture":{"host_id":"mac","state":"ACTIVE","owner_token":"owner","updated_at_ms":client_time},"sibling:fixture":{"host_id":"sibling","state":"ACTIVE","updated_at_ms":client_time}}});
        e.rpc("kv_create", &json!({"key":id,"data":record}), None)
            .unwrap();
        let read = || {
            let wire = e.rpc("kv_get", &json!({"key":id}), None).unwrap();
            (
                serde_json::from_slice::<Value>(&B64.decode(s(&wire, "value_b64")).unwrap())
                    .unwrap(),
                wire["server_now_ms"].as_u64().unwrap(),
            )
        };
        let (registered, server_now) = read();
        assert!(
            registered["channels"]["mac:fixture"]["updated_at_ms"]
                .as_u64()
                .unwrap()
                .abs_diff(server_now)
                < 1000
        );
        record = registered;
        let sibling = record["channels"]["sibling:fixture"].clone();
        record["channels"]["mac:fixture"]["updated_at_ms"] = json!(client_time + 1);
        e.rpc("kv_put", &json!({"key":id,"data":record}), None)
            .unwrap();
        let (renewed, server_now) = read();
        assert_eq!(renewed["channels"]["sibling:fixture"], sibling);
        assert!(
            renewed["channels"]["mac:fixture"]["updated_at_ms"]
                .as_u64()
                .unwrap()
                .abs_diff(server_now)
                < 1000
        );
        assert_eq!(evaluate_node_lease(&renewed, server_now), "ACTIVE");
        let snapshot = e.rpc("kv_snapshot", &json!({}), None).unwrap();
        assert!(snapshot["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["key"] == id));
        send(&e, &id, &format!("signal_{id}"));
    }
    e.inner.db.transaction(|c| {
        c.execute("INSERT INTO hub_audit_log(ts_ms,principal_id,action,target_id) VALUES(?,'alice','old','node'),(?,'alice','recent','node')", &[json!(now_ms()-604_800_001),json!(now_ms())])?;
        prune(c)?;
        assert!(c.query("SELECT id FROM hub_audit_log WHERE action='old'", &[])?.is_empty());
        assert_eq!(c.query("SELECT id FROM hub_audit_log WHERE action='recent'", &[])?.len(), 1);
        Ok(())
    }).unwrap();
}

fn put(e: &HubEngine, id: &str, pid: &str, scope: &str) {
    e.rpc("kv_put",&json!({"key":id,"data_b64":encoded(&json!({"listener_id":id,"principal_id":pid,"owner_user":"alice","scope":scope,"harness":"fixture","session_id":"s1","binding_generation":1,"state":"ACTIVE","updated_at_ms":now_ms()}))}),None).unwrap();
}
fn send(e: &HubEngine, id: &str, sig: &str) {
    send_from(e, id, sig, "oidc:https://id.example.com|sender", "sender");
}
fn send_from(e: &HubEngine, id: &str, sig: &str, pid: &str, from: &str) {
    e.rpc("js_publish",&json!({"subject":format!("cito.signal.{id}"),"data_b64":encoded(&json!({"signal_id":sig,"from":{"principal_id":pid,"listener_id":from,"user":"bob"},"message":"hello"})),"opts":{"msgID":sig}}),None).unwrap();
}
#[test]
fn destination_replacement_isolates_prefixes_fences_late_results_and_preserves_unknown_effects() {
    let dir = temp();
    let e = test_engine(&dir);
    e.set_plugin_roots(&[]).unwrap();
    let plugin = hub_plugins::test_descriptor(&dir, "fixture", vec![], json!({}));
    let old = json!({"_external_chat_id":"41","_external_user_id":"41"});
    let next = json!({"_external_chat_id":"42","_external_user_id":"42"});
    e.inner.db.transaction(|c| {
        c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,enabled,user_config_json,last_delivered_seq) VALUES('none#alice','fixture','alice',0,?,7)",&[json!(old.to_string())])?;
        for (sig,status,token) in [("partial","failed","partial-token"),("late","in_flight","late-token"),("unknown","effect_unknown","unknown-token")] {
            c.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,reservation_id,from_listener_id,external_msg_id,delivered_ms) VALUES('fixture','none#alice',?,8,?,?,'sender','1',0)",&[json!(sig),json!(status),json!(token)])?;
            c.execute("INSERT INTO hub_channel_delivery_msgs VALUES('fixture','none#alice',?,?,'sender')",&[json!(sig),json!(sig)])?;
        }
        assert!(replace_destination(c,&plugin,"none#bob",None,&old).is_err(),"paused bindings still own their identities");
        let mut same=old.clone(); same["preference"]=json!(true);
        replace_destination(c,&plugin,"none#alice",Some(&old),&same)?;
        assert_eq!(c.query("SELECT * FROM hub_channel_delivery_msgs",&[])?.len(),3,"non-destination preferences preserve delivery mappings");
        replace_destination(c,&plugin,"none#alice",Some(&old),&next)?;
        assert!(c.query("SELECT * FROM hub_channel_delivery_msgs",&[])?.is_empty());
        assert_eq!(c.query("SELECT last_delivered_seq FROM hub_human_channels",&[])?[0]["last_delivered_seq"],7);
        Ok(())
    }).unwrap();
    e.delivery_result(
        "fixture",
        &json!({"delivery_id":"late-token","ok":true,"external_msg_ids":["99"]}),
    )
    .unwrap();
    assert!(e
        .inner
        .db
        .query("SELECT * FROM hub_channel_delivery_msgs", &[])
        .unwrap()
        .is_empty());
    let rows=e.inner.db.query("SELECT signal_id,status,reservation_id,from_listener_id FROM hub_channel_deliveries ORDER BY signal_id",&[]).unwrap();
    assert_eq!(
        rows.iter().map(|r| s(r, "status")).collect::<Vec<_>>(),
        vec!["effect_unknown", "failed", "effect_unknown"]
    );
    assert!(rows
        .iter()
        .all(|r| r["reservation_id"].is_null() && r["from_listener_id"].is_null()));
}
#[test]
fn channel_worker_quiesces_and_resumes_without_reconfiguring() {
    let dir = temp();
    let root = fixture(&dir);
    let hub = test_engine(&dir);
    hub.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    let owner = principal("alice");
    let test = || {
        hub.rpc(
            "human_channels_test",
            &json!({"plugin_id":"fixture"}),
            Some(&owner),
        )
    };
    let count = |op: &str| {
        std::fs::read_to_string(root.join("fixture/frames.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|frame| frame["op"] == op)
            .count()
    };
    hub.rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","hub_config":{"credential":"saved"}}),
        Some(&owner),
    )
    .unwrap();
    assert_eq!(test().unwrap()["ok"], true);
    assert_eq!(count("configure"), 1);
    hub.set_channels_active(false);
    wait_for(|| count("shutdown") == 1);
    assert_eq!(test().unwrap_err(), "HUB_DISABLED: Local Hub is disabled");
    assert_eq!(count("configure"), 1);
    hub.set_channels_active(true);
    wait_for(|| count("configure") == 2);
    assert_eq!(test().unwrap()["ok"], true);
}

#[test]
fn outbound_only_keeps_delivery_and_identity_while_fencing_pairing() {
    let dir = temp();
    let root = fixture(&dir);
    let path = root.join("fixture/plugin.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["supportsOutboundOnly"] = json!(true);
    manifest["serviceIdentity"] = json!({"field":"credential","separator":":"});
    std::fs::write(path, manifest.to_string()).unwrap();
    let hub = test_engine(&dir);
    hub.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    let owner = principal("alice");
    let rpc = |op, mut params: Value| {
        params["plugin_id"] = json!("fixture");
        hub.rpc(op, &params, Some(&owner))
    };
    let list = || rpc("human_channels_list", json!({})).unwrap()["channels"][0].clone();
    let configure = |params| rpc("human_channels_configure", params);
    let pair = || rpc("human_channel_pair_start", json!({}));
    configure(json!({"hub_config":{"credential":"123:private"},"user_config":{"address":"room"},"display_label":"  Test Room  "})).unwrap();
    assert_eq!(list()["display_label"], "Test Room");
    for invalid in [json!(true), json!("  "), json!("é".repeat(65))] {
        assert!(configure(json!({"display_label":invalid}))
            .unwrap_err()
            .starts_with("INVALID_INPUT:"));
    }
    assert!(configure(json!({"inbound_mode":null}))
        .unwrap_err()
        .starts_with("INVALID_INPUT:"));
    let remote = json!({"principal_id":"oidc:https://id.example.com|guest","owner_user":"guest","oidc":true});
    assert!(hub
        .rpc(
            "human_channels_configure",
            &json!({"plugin_id":"fixture","inbound_mode":"outbound_only"}),
            Some(&remote)
        )
        .unwrap_err()
        .starts_with("FORBIDDEN:"));
    assert!(pair().unwrap()["code"].is_string());
    assert_eq!(list()["service_label"], "fixture-account");
    assert_eq!(list()["pairing_pending"], true);
    hub.plugin_frame(
        "fixture",
        &json!({"op":"health","error":"Inbound polling conflict"}),
    );
    assert_eq!(list()["warning"], true);

    configure(json!({"inbound_mode":"outbound_only"})).unwrap();
    let channel = list();
    assert_eq!(channel["supports_outbound_only"], true);
    assert_eq!(channel["inbound_mode"], "outbound_only");
    assert_eq!(channel["service_identity"], "123");
    assert_eq!(channel["service_label"], "fixture-account");
    assert_eq!(channel["pairing_pending"], false);
    assert_eq!(channel["can_pair"], false);
    assert_eq!(channel["warning"], false);
    assert!(!channel.to_string().contains("123:private"));
    assert!(pair().unwrap_err().contains("requires Replies enabled"));
    assert_eq!(
        rpc("human_channels_test", json!({})).unwrap()["service_label"],
        "@123"
    );
    assert_eq!(list()["service_label"], "@123");
    rpc(
        "human_channels_test",
        json!({"hub_config":{"credential":"999:preview"}}),
    )
    .unwrap();
    assert_eq!(list()["service_label"], "@123");
    send(
        &hub,
        &listener_id("none#alice", "human", "default"),
        "outbound-notice",
    );
    wait_for(|| {
        !hub.inner.db.query("SELECT 1 FROM hub_channel_deliveries WHERE signal_id='outbound-notice' AND status='delivered'", &[]).unwrap().is_empty()
    });
    let frames = std::fs::read_to_string(root.join("fixture/frames.jsonl")).unwrap();
    assert!(frames
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .any(|frame| frame["op"] == "configure" && frame["inbound_mode"] == "outbound_only"));

    configure(json!({"inbound_mode":"primary"})).unwrap();
    assert_eq!(list()["can_pair"], true);
    assert_eq!(list()["pairing_pending"], false);
    let human = listener_id("none#alice", "human", "default");
    let registration = || hub.inner.db.read(|c| kv_row(c, &human)).unwrap().unwrap();
    let before = registration();
    assert_eq!(
        parse(&before, "value")["channels"]["hub:fixture"]["state"],
        "ACTIVE"
    );
    configure(json!({"reset_hub":true})).unwrap();
    let after = registration();
    assert_eq!(n(&after, "revision"), n(&before, "revision") + 1);
    assert_eq!(
        parse(&after, "value")["channels"]["hub:fixture"]["state"],
        "UNLISTENED"
    );
    assert!(list()["service_label"].is_null());
    assert!(list()["service_identity"].is_null());
}

#[test]
fn configure_destination_resume_skips_disabled_backlog_and_preserves_paired_labels() {
    let dir = temp();
    let root = fixture(&dir);
    let path = root.join("fixture/plugin.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["pairing"]["privateIdentityField"] = json!("address");
    std::fs::write(path, manifest.to_string()).unwrap();
    let e = test_engine(&dir);
    e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    let owner = json!({"principal_id":"none#alice","owner_user":"alice"});
    let configure = |patch: Value| {
        let mut patch = patch;
        patch["plugin_id"] = json!("fixture");
        e.rpc("human_channels_configure", &patch, Some(&owner))
            .unwrap()
    };
    let binding = || {
        e.inner
            .db
            .query(
                "SELECT * FROM hub_human_channels WHERE plugin_id='fixture'",
                &[],
            )
            .unwrap()
            .remove(0)
    };
    configure(
        json!({"hub_config":{"credential":"saved"},"user_config":{"address":"41"},"enabled":false}),
    );
    let cursor = binding()["last_delivered_seq"].clone();
    let node = listener_id("none#alice", "human", "default");
    send(&e, &node, "pending-destination");
    configure(json!({"user_config":{"address":"42"},"enabled":true}));
    assert!(binding()["last_delivered_seq"].as_i64().unwrap() > cursor.as_i64().unwrap());
    assert!(e
        .reserve_deliveries(&e.plugin("fixture").unwrap())
        .unwrap()
        .is_empty());
    send(&e, &node, "new-destination");
    wait_for(|| {
        !e.inner.db.query("SELECT 1 FROM hub_channel_deliveries WHERE signal_id='new-destination' AND status='delivered'", &[]).unwrap().is_empty()
    });
    let frames = std::fs::read_to_string(root.join("fixture/frames.jsonl")).unwrap();
    let delivery = frames
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|frame| frame["op"] == "deliver" && frame["signal"]["signal_id"] == "new-destination")
        .unwrap();
    assert_eq!(delivery["config"]["address"], "42");
    e.inner
        .db
        .execute(
            "UPDATE hub_human_channels SET display_label='@alice_tg'",
            &[],
        )
        .unwrap();
    let unchanged = configure(json!({"user_config":{"address":"42"},"enabled":true}));
    assert_eq!(unchanged["channels"][0]["display_label"], "@alice_tg");
    e.plugin_frame(
        "fixture",
        &json!({"op":"health","error":"HTTP 409: another getUpdates request"}),
    );
    let warned = e
        .rpc("human_channels_list", &json!({}), Some(&owner))
        .unwrap();
    assert_eq!(warned["channels"][0]["warning"], true);
    assert_eq!(warned["channels"][0]["status"], "ready");
    assert!(s(&warned["channels"][0], "detail").contains("409"));
    e.plugin_frame("fixture", &json!({"op":"health","error":null}));
    e.inner.db.execute("UPDATE hub_channel_deliveries SET status='effect_unknown',last_error='Response lost' WHERE signal_id='new-destination'", &[]).unwrap();
    let unknown = e
        .rpc("human_channels_list", &json!({}), Some(&owner))
        .unwrap();
    assert_eq!(unknown["channels"][0]["warning"], true);
    assert!(s(&unknown["channels"][0], "detail").contains("outcome unknown"));
    e.inner.db.execute("UPDATE hub_channel_deliveries SET status='delivered',last_error=NULL WHERE signal_id='new-destination'", &[]).unwrap();
    assert_eq!(
        e.rpc("human_channels_list", &json!({}), Some(&owner))
            .unwrap()["channels"][0]["warning"],
        false
    );
    for empty in [json!(""), Value::Null] {
        configure(json!({"user_config":{"address":"42"}}));
        let cleared = configure(json!({"user_config":{"address":empty}}));
        assert!(cleared["channels"][0]["display_label"].is_null());
        let config = parse(&binding(), "user_config_json");
        assert!(config.get("_external_chat_id").is_none());
        assert!(config.get("_external_user_id").is_none());
    }
}

#[test]
fn manual_private_identity_and_pair_redemption_share_ownership_policy() {
    let dir = temp();
    let e = test_engine(&dir);
    e.set_plugin_roots(&[]).unwrap();
    e.inner.plugins.lock().unwrap().push(hub_plugins::test_descriptor(&dir, "fixture", vec![], json!({"displayName":"Fixture Channel","fields":[{"key":"destination","scope":"user","type":"string","required":true}],"pairing":{"supported":true,"privateIdentityField":"destination"}})));
    e.pause_worker();
    let alice = json!({"principal_id":"oidc:https://id.example.com|alice","owner_user":"alice"});
    let bob = json!({"principal_id":"none#bob","owner_user":"bob"});
    e.human_rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","user_config":{"destination":" 42 "},"enabled":false}),
        Some(&alice),
    )
    .unwrap();
    let binding = e
        .inner
        .db
        .query("SELECT * FROM hub_human_channels", &[])
        .unwrap()
        .remove(0);
    assert_eq!(
        parse(&binding, "user_config_json")["_external_user_id"],
        "42"
    );
    assert_eq!(parse(&binding, "user_config_json")["destination"], "42");
    for destination in ["42", " 42 "] {
        assert!(e
            .human_rpc(
                "human_channels_configure",
                &json!({"plugin_id":"fixture","user_config":{"destination":destination}}),
                Some(&bob)
            )
            .unwrap_err()
            .contains("DESTINATION_IN_USE"));
    }
    e.inner.db.execute("INSERT INTO hub_pairing_codes(code,plugin_id,principal_id,owner_user,created_ms,expires_ms) VALUES('cito-owner','fixture','none#bob','bob',?,?)",&[json!(now_ms()),json!(now_ms()+60_000)]).unwrap();
    assert!(e.redeem("fixture",&json!({"code":"cito-owner","is_private_chat":true,"external_chat_id":"42","external_user_id":"42","user_config_patch":{"destination":"42"}})).unwrap_err().contains("DESTINATION_IN_USE"));
    let plugin = e.plugin("fixture").unwrap();
    e.inner
        .db
        .transaction(|c| {
            replace_destination(
                c,
                &plugin,
                "https://id.example.com#alice",
                None,
                &json!({"_external_chat_id":"42","_external_user_id":"42"}),
            )
        })
        .unwrap();
    e.inner.db.execute("UPDATE hub_pairing_codes SET instruction='Pair',pairing_command='/start cito-owner',pairing_url='https://example.com/pair'",&[]).unwrap();
    e.inner
        .db
        .execute(
            "UPDATE hub_pairing_codes SET principal_id=?,owner_user='alice'",
            &[alice["principal_id"].clone()],
        )
        .unwrap();
    let list = e.channel_list(&alice).unwrap();
    assert_eq!(list["channels"][0]["configured"], true);
    assert_eq!(list["channels"][0]["fields"][0]["value"], "42");
    assert_eq!(list["channels"][0]["pairing_pending"], true);
    assert_eq!(
        list["channels"][0]["pairing_url"],
        "https://example.com/pair"
    );
}
#[test]
fn unthreaded_reply_requires_one_recent_sender_and_threaded_reply_disambiguates_legacy_bindings() {
    let dir = temp();
    let e = test_engine(&dir);
    e.set_plugin_roots(&[]).unwrap();
    put(&e, "sender-a", "none#alice", "workspace");
    put(&e, "sender-b", "none#alice", "workspace");
    let config = json!({"_external_chat_id":"42","_external_user_id":"42"}).to_string();
    e.inner.db.transaction(|c|{
        c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,user_config_json) VALUES('none#alice','fixture','alice',?)",&[json!(config)])?;
        for (signal,sender) in [("a1","sender-a"),("a2","sender-a")] {
            c.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,delivered_ms,from_listener_id) VALUES('fixture','none#alice',?,1,'delivered',?,?)",&[json!(signal),json!(now_ms()),json!(sender)])?;
            c.execute("INSERT INTO hub_channel_delivery_msgs VALUES('fixture','none#alice',?,?,?)",&[json!(signal),json!(signal),json!(sender)])?;
        }
        Ok(())
    }).unwrap();
    let mut reply = json!({"update_id":"one-sender","external_chat_id":"42","external_user_id":"42","text":"Continue"});
    assert_eq!(e.inbound_reply("fixture", &reply).unwrap()["ok"], true);
    e.inner.db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,delivered_ms,from_listener_id) VALUES('fixture','none#alice','b1',2,'delivered',?,'sender-b')",&[json!(now_ms())]).unwrap();
    reply["update_id"] = json!("two-senders");
    assert!(e
        .inbound_reply("fixture", &reply)
        .unwrap_err()
        .contains("AMBIGUOUS_REPLY"));
    reply["reply_to_external_msg_id"] = json!("a1");
    assert_eq!(e.inbound_reply("fixture", &reply).unwrap()["ok"], true);
    e.inner
        .db
        .execute(
            "UPDATE hub_channel_deliveries SET delivered_ms=? WHERE signal_id='b1'",
            &[json!(now_ms() - 8 * 86_400_000)],
        )
        .unwrap();
    reply
        .as_object_mut()
        .unwrap()
        .remove("reply_to_external_msg_id");
    reply["update_id"] = json!("one-recent");
    assert_eq!(e.inbound_reply("fixture", &reply).unwrap()["ok"], true);
    e.inner.db.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,user_config_json,enabled) VALUES('none#bob','fixture','bob',?,0)",&[json!(config)]).unwrap();
    reply["update_id"] = json!("duplicate-identity");
    assert!(e
        .inbound_reply("fixture", &reply)
        .unwrap_err()
        .contains("AMBIGUOUS_REPLY"));
    reply["reply_to_external_msg_id"] = json!("a1");
    assert_eq!(e.inbound_reply("fixture", &reply).unwrap()["ok"], true);
    let published = e
        .inner
        .db
        .query(
            "SELECT json_extract(CAST(data AS TEXT),'$.from.principal_id') AS pid FROM hub_signals",
            &[],
        )
        .unwrap();
    assert_eq!(published.len(), 4);
    assert!(published.iter().all(|row| row["pid"] == "none#alice"));
}
#[test]
fn stopped_channel_process_waits_for_its_reader_callback() {
    let dir = temp();
    let root = fixture(&dir);
    let mut plugin = hub_plugins::discover(&[(root, false)], None)
        .unwrap()
        .remove(0);
    plugin.command = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s\\n' '{\"op\":\"cursor_save\",\"cursor\":\"old\"}'; read line".into(),
    ];
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(release_rx);
    let process = Arc::new(
        ChannelProcess::spawn(
            &plugin,
            Arc::new(move |_| {
                started_tx.send(()).unwrap();
                release.lock().unwrap().recv().unwrap();
                None
            }),
        )
        .unwrap(),
    );
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let stopping = process.clone();
    let worker = thread::spawn(move || {
        stopping.shutdown();
        done_tx.send(()).unwrap();
    });
    assert!(
        done_rx.recv_timeout(Duration::from_millis(30)).is_err(),
        "shutdown returned before the callback quiesced"
    );
    release_tx.send(()).unwrap();
    done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.join().unwrap();
    drop(process);
}
#[test]
fn pairing_preparation_quiesces_old_credential_frames_before_commit() {
    let dir = temp();
    let root = fixture(&dir);
    let e = test_engine(&dir);
    e.pause_worker();
    let script = r#"
import json, os, pathlib, sys, time
credential = None
for line in sys.stdin:
    request = json.loads(line)
    op = request['op']
    if op == 'configure':
        credential = request['hub_config'].get('credential')
    elif op == 'format_pair':
        print(json.dumps({'op':'rpc_result', 'req_id':request['req_id'],
                          'ok':True, 'instruction':'Pair with fixture'}), flush=True)
        following = json.loads(sys.stdin.readline())
        pathlib.Path('gate.tmp').write_text(json.dumps({'credential':credential, 'following':following}))
        pathlib.Path('gate.tmp').replace('gate.json')
        deadline = time.monotonic() + 3
        while not pathlib.Path('release').exists() and time.monotonic() < deadline:
            time.sleep(0.005)
        frames = [{'op':'cursor_save', 'cursor':'old-credential-cursor'},
                  {'op':'health', 'error':'Old credential poll failure'}]
        try:
            os.write(sys.stdout.fileno(), ''.join(json.dumps(f) + '\n' for f in frames).encode())
        except BrokenPipeError:
            pass  # Quiescing preparation can already have closed its reader.
        if following['op'] == 'shutdown':
            break
    elif op == 'test':
        print(json.dumps({'op':'rpc_result', 'req_id':request['req_id'], 'ok':True}), flush=True)
    elif op == 'shutdown':
        break
"#;
    let mut plugin = hub_plugins::discover(&[(root, false)], None)
        .unwrap()
        .remove(0);
    plugin.command = vec!["python3".into(), "-c".into(), script.into()];
    let plugin_dir = plugin.plugin_dir.clone();
    *e.inner.plugins.lock().unwrap() = vec![plugin];
    e.inner
        .db
        .transaction(|c| {
            save_service(c, "fixture", &json!({"credential":"A"}))?;
            Ok(())
        })
        .unwrap();
    let caller = json!({"principal_id":"none#alice","owner_user":"alice"});
    let pairing = {
        let e = e.clone();
        let caller = caller.clone();
        thread::spawn(move || {
            e.rpc(
                "human_channel_pair_start",
                &json!({"plugin_id":"fixture","hub_config":{"credential":"B"}}),
                Some(&caller),
            )
        })
    };
    let gate_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_for(|| plugin_dir.join("gate.json").exists());
        let gate: Value =
            serde_json::from_str(&std::fs::read_to_string(plugin_dir.join("gate.json")).unwrap())
                .unwrap();
        // Release the queued A frames at the boundary where the old path would
        // already have committed B and reconfigured this same polling reader.
        std::fs::write(plugin_dir.join("release"), "").unwrap();
        gate
    }));
    // Always release the child before joining, including a failed gate assertion.
    std::fs::write(plugin_dir.join("release"), "").unwrap();
    let paired = pairing.join().unwrap();
    let gate = gate_result.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    assert_eq!(gate["credential"], "A");
    assert_eq!(gate["following"]["op"], "shutdown");
    let paired = paired.unwrap();
    assert_eq!(paired["committed"], true);
    assert!(paired["code"].is_string());
    // Synchronize with the restarted process after it has received B.
    assert_eq!(
        e.rpc(
            "human_channels_test",
            &json!({"plugin_id":"fixture"}),
            Some(&caller)
        )
        .unwrap()["ok"],
        true
    );
    e.inner
        .db
        .transaction(|c| {
            let (cfg, state) = service(c, "fixture")?;
            assert_eq!(cfg["credential"], "B");
            assert!(state.get("cursor").is_none(), "{state}");
            assert!(state.get("reply_floor").is_none(), "{state}");
            assert!(state.get("poll_error").is_none(), "{state}");
            assert_eq!(
                c.query(
                    "SELECT code FROM hub_pairing_codes WHERE consumed_ms IS NULL",
                    &[]
                )?
                .len(),
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn oidc_admin_can_edit_redacted_service_fields() {
    if std::env::var_os("CITO_TEST_ADMIN_WORKER").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::hub::tests::oidc_admin_can_edit_redacted_service_fields",
                "--nocapture",
            ])
            .env("CITO_TEST_ADMIN_WORKER", "1")
            .env("CITO_OIDC_ADMIN_USERS", "lifecycle-admin")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let dir = temp();
    let root = fixture(&dir);
    let e = test_engine(&dir);
    e.set_plugin_roots(&[root]).unwrap();
    let admin = json!({"principal_id":"oidc:https://id.example.com|lifecycle-admin","owner_user":"alice","sub":"lifecycle-admin","oidc":true});
    let collision = json!({"principal_id":"oidc:https://id.example.com|guest","owner_user":"lifecycle-admin","sub":"guest","oidc":true});
    assert!(e
        .rpc(
            "human_channels_configure",
            &json!({"plugin_id":"fixture","hub_config":{"credential":"secret"}}),
            Some(&collision)
        )
        .unwrap_err()
        .starts_with("FORBIDDEN"));
    e.rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","hub_config":{"credential":"secret"}}),
        Some(&admin),
    )
    .unwrap();
    let list = e
        .rpc("human_channels_list", &json!({}), Some(&admin))
        .unwrap();
    let f = &list["channels"][0]["fields"][0];
    assert_eq!(f["hidden"], false);
    assert_eq!(f["redacted"], true);
    assert_eq!(f["value"], "");
    assert_eq!(f["configured"], true);
}
#[test]
fn channel_lifecycle_secret_patches_pair_superseding_and_readiness() {
    let dir = temp();
    let root = fixture(&dir);
    let manifest_path = root.join("fixture/plugin.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({"key":"user_credential","scope":"user","type":"secret","required":false}));
    std::fs::write(manifest_path, manifest.to_string()).unwrap();
    let e = test_engine(&dir);
    e.set_plugin_roots(&[root]).unwrap();
    let owner = json!({"principal_id":"none#alice","owner_user":"alice"});
    let list = || {
        e.rpc("human_channels_list", &json!({}), Some(&owner))
            .unwrap()["channels"][0]
            .clone()
    };
    let configure = |mut patch: Value| {
        patch["plugin_id"] = json!("fixture");
        e.rpc("human_channels_configure", &patch, Some(&owner))
            .unwrap()
    };
    let reg = || {
        e.inner
            .db
            .transaction(|c| {
                let b = one(
                    c,
                    "SELECT * FROM hub_human_channels WHERE plugin_id='fixture'",
                    &[],
                )?
                .ok_or("missing binding")?;
                find_human(c, &b)?.1.ok_or_else(|| "missing node".into())
            })
            .unwrap()
    };
    assert_eq!(list()["status"], "not_configured");
    assert_eq!(list()["enabled"], false);
    configure(json!({"enabled":true,"user_config":{"address":"room"}}));
    assert_eq!(reg()["channels"]["hub:fixture"]["state"], "UNLISTENED");
    e.inner.db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,attempts,next_retry_ms,delivered_ms) VALUES('fixture','none#alice','failed',1,'failed_permanent',6,999999,1)",&[]).unwrap();
    let bob = principal("bob");
    let events = e.subscribe(None).unwrap();
    e.rpc(
        "human_channel_pair_start",
        &json!({"plugin_id":"fixture","hub_config":{"credential":"saved"}}),
        Some(&bob),
    )
    .unwrap();
    assert_eq!(events.try_recv().unwrap(), json!({"type":"kv"}));
    assert_eq!(reg()["channels"]["hub:fixture"]["state"], "ACTIVE");
    let retries = e.inner.db.query("SELECT status,attempts,next_retry_ms FROM hub_channel_deliveries WHERE signal_id='failed'",&[]).unwrap();
    assert_eq!(
        retries,
        [json!({"status":"failed","attempts":0,"next_retry_ms":0})]
    );
    assert_eq!(list()["status"], "ready");
    let field = list()["fields"][0].clone();
    assert_eq!(field["hidden"], false);
    assert_eq!(field["redacted"], true);
    assert_eq!(field["configured"], true);
    assert_eq!(field["value"], "");
    let non_admin = json!({"principal_id":"oidc:https://id.example.com|ordinary","owner_user":"ordinary","sub":"ordinary","oidc":true});
    let hidden = e
        .rpc("human_channels_list", &json!({}), Some(&non_admin))
        .unwrap();
    assert_eq!(hidden["channels"][0]["fields"][0]["hidden"], true);
    for value in [json!(""), json!("   "), Value::Null, json!(REDACTED_SECRET)] {
        configure(json!({"hub_config":{"credential":value}}));
        assert_eq!(
            e.inner.db.transaction(|c| service(c, "fixture")).unwrap().0["credential"],
            "saved"
        );
    }
    configure(json!({"enabled":false}));
    assert_eq!(list()["status"], "paused");
    assert_eq!(list()["fields"][1]["value"], "room");
    assert_eq!(reg()["channels"]["hub:fixture"]["state"], "UNLISTENED");
    send(&e, "other", "boundary");
    configure(json!({"enabled":true}));
    assert_eq!(list()["status"], "ready");
    assert_eq!(
        e.inner
            .db
            .query("SELECT last_delivered_seq FROM hub_human_channels", &[])
            .unwrap()[0]["last_delivered_seq"],
        1
    );
    configure(json!({"reset_user":true}));
    assert_eq!(list()["status"], "hub_ready");
    assert_eq!(list()["enabled"], false);
    assert_eq!(list()["fields"][1]["value"], "");
    let pair = || {
        e.rpc(
            "human_channel_pair_start",
            &json!({"plugin_id":"fixture"}),
            Some(&owner),
        )
        .unwrap()
    };
    let first = pair();
    assert_eq!(list()["status"], "pairing_pending");
    let second = pair();
    assert_ne!(first["code"], second["code"]);
    let redeem = |code: &Value| {
        e.redeem("fixture",&json!({"code":code,"is_private_chat":true,"external_chat_id":"external","external_user_id":"user","display_label":"Alice","user_config_patch":{"address":"paired","user_credential":"user-saved"}}))
    };
    assert!(redeem(&first["code"])
        .unwrap_err()
        .starts_with("INVALID_INPUT"));
    configure(json!({"cancel_pair":true}));
    assert!(redeem(&second["code"]).is_err());
    let redeem_without_address = |code: &Value| {
        e.redeem("fixture", &json!({"code":code,"is_private_chat":true,"external_chat_id":"external","external_user_id":"user","user_config_patch":{}}))
    };
    let incomplete = pair();
    assert_eq!(
        redeem_without_address(&incomplete["code"]).unwrap()["ok"],
        true
    );
    assert_eq!(reg()["channels"]["hub:fixture"]["state"], "UNLISTENED");
    configure(json!({"user_config":{"address":"saved-room"}}));
    assert_eq!(reg()["channels"]["hub:fixture"]["state"], "ACTIVE");
    let retained = pair();
    assert_eq!(
        redeem_without_address(&retained["code"]).unwrap()["ok"],
        true
    );
    assert_eq!(list()["fields"][1]["value"], "saved-room");
    assert_eq!(reg()["channels"]["hub:fixture"]["state"], "ACTIVE");
    configure(json!({"reset_user":true}));
    let third = pair();
    assert_eq!(redeem(&third["code"]).unwrap()["ok"], true);
    assert_eq!(list()["fields"][1]["value"], "paired");
    assert_eq!(reg()["channels"]["hub:fixture"]["state"], "ACTIVE");
    assert!(list()["detail"].as_str().unwrap().contains("Alice"));
    for value in [
        json!(""),
        json!("   "),
        Value::Null,
        json!(REDACTED_SECRET),
        json!("user-saved"),
    ] {
        configure(json!({"user_config":{"user_credential":value}}));
        let binding = e
            .inner
            .db
            .query("SELECT * FROM hub_human_channels", &[])
            .unwrap()[0]
            .clone();
        assert_eq!(
            parse(&binding, "user_config_json"),
            json!({"address":"paired","user_credential":"user-saved","_external_chat_id":"external","_external_user_id":"user"})
        );
        assert_eq!(binding["display_label"], "Alice");
        assert_eq!(list()["display_label"], "Alice");
    }
    configure(json!({"user_config":{"address":"manual"}}));
    let b = e
        .inner
        .db
        .query("SELECT * FROM hub_human_channels", &[])
        .unwrap()[0]
        .clone();
    assert_eq!(
        parse(&b, "user_config_json"),
        json!({"address":"manual","user_credential":"user-saved"})
    );
    assert!(b["display_label"].is_null());
    e.inner.db.execute("UPDATE hub_plugin_services SET service_state_json='{\"cursor\":\"77\"}' WHERE plugin_id='fixture'",&[]).unwrap();
    let old_process = e.inner.processes.lock().unwrap()["fixture"].clone();
    put(&e, "sender", "none#bob", "workspace");
    e.inner
        .db
        .execute(
            "INSERT INTO hub_processed_replies VALUES('fixture','u1',?)",
            &[json!(now_ms())],
        )
        .unwrap();
    configure(json!({"hub_config":{"credential":"rotated"}}));
    e.inner
        .db
        .execute(
            "UPDATE hub_human_channels SET user_config_json=? WHERE plugin_id='fixture'",
            &[json!(
                json!({"address":"manual","_external_chat_id":"room","_external_user_id":"user"})
                    .to_string()
            )],
        )
        .unwrap();
    e.inner.db.execute("INSERT INTO hub_channel_delivery_msgs VALUES('fixture',?,'rotated-message','original','sender')",
        &[owner["principal_id"].clone()]).unwrap();
    let reply = e.inbound_reply("fixture", &json!({"update_id":"u1","external_chat_id":"room","external_user_id":"user","reply_to_external_msg_id":"rotated-message","text":"new bot reply"})).unwrap();
    assert_eq!(reply["ok"], true);
    assert_ne!(reply["duplicate"], true);
    assert!(reply["signal_id"].is_string());
    assert!(!old_process.alive());
    assert_eq!(
        e.inner.db.transaction(|c| service(c, "fixture")).unwrap().1,
        json!({})
    );
    configure(json!({"reset_hub":true}));
    assert_eq!(list()["status"], "not_configured");
    assert_eq!(list()["enabled"], false);
    assert!(!e.inner.processes.lock().unwrap().contains_key("fixture"));
    assert_eq!(reg()["channels"]["hub:fixture"]["state"], "UNLISTENED");
    let missing_service = pair();
    assert_eq!(redeem(&missing_service["code"]).unwrap()["ok"], true);
    assert_eq!(list()["fields"][1]["value"], "paired");
    assert_eq!(reg()["channels"]["hub:fixture"]["state"], "UNLISTENED");
    configure(json!({"hub_config":{"credential":"restored"}}));
    assert_eq!(reg()["channels"]["hub:fixture"]["state"], "ACTIVE");
}
#[test]
fn bot_switch_marks_delivery_unverified_until_current_destination_settles() {
    let dir = temp();
    let root = fixture(&dir);
    let path = root.join("fixture/plugin.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["serviceIdentity"] = json!({"field":"credential","separator":":"});
    manifest["pairing"]["deliveryVerificationHelpText"] =
        json!("Send /start to the bot or re-pair your account to verify delivery.");
    std::fs::write(path, manifest.to_string()).unwrap();
    let e = test_engine(&dir);
    e.pause_worker();
    e.set_plugin_roots(&[root]).unwrap();
    let owner = principal("alice");
    let save = |token: &str| {
        e.rpc("human_channels_configure", &json!({"plugin_id":"fixture","hub_config":{"credential":token},"user_config":{"address":"room"}}), Some(&owner)).unwrap()
    };
    let inventory = || {
        e.rpc("human_channels_list", &json!({}), Some(&owner))
            .unwrap()["channels"][0]
            .clone()
    };
    save("1:first");
    assert_eq!(inventory()["delivery_unverified"], false);
    save("2:other");
    e.plugin_frame(
        "fixture",
        &json!({"op":"health","service_label":"@new_bot"}),
    );
    let channel = inventory();
    assert_eq!(channel["delivery_unverified"], true);
    let detail = s(&channel, "detail");
    assert!(detail.contains("@new_bot") && detail.contains("/start") && detail.contains("re-pair"));
    save("2:other");
    assert_eq!(inventory()["delivery_unverified"], true);
    let node = listener_id("none#alice", "human", "default");
    send(&e, &node, "verify-current-bot");
    e.rpc("human_channel_poll", &json!({"plugin_id":"fixture"}), None)
        .unwrap();
    wait_for(|| inventory()["delivery_unverified"] == false);
}

#[test]
fn send_test_wait_allows_hub_channel_poll_and_rejects_missing_status() {
    let dir = temp();
    let root = fixture(&dir);
    let plugin_dir = root.join("fixture");
    let manifest_path = plugin_dir.join("plugin.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let script = plugin_dir.join("test_pipe.py");
    std::fs::write(
        &script,
        r#"import json, pathlib, sys
for line in sys.stdin:
    frame = json.loads(line)
    if frame['op'] == 'test':
        test = frame['req_id']
        pathlib.Path('test-received').touch()
    if frame['op'] == 'deliver':
        print(json.dumps({'op':'rpc_result','req_id':test}), flush=True)
        print(json.dumps({'op':'delivered','delivery_id':frame['delivery_id']}), flush=True)
"#,
    )
    .unwrap();
    manifest["command"] = json!(["python3", script]);
    std::fs::write(manifest_path, manifest.to_string()).unwrap();
    let e = test_engine(&dir);
    e.pause_worker();
    e.set_plugin_roots(&[root]).unwrap();
    let owner = principal("alice");
    e.rpc("human_channels_configure", &json!({"plugin_id":"fixture","hub_config":{"credential":"test-only"},"user_config":{"address":"room"}}), Some(&owner)).unwrap();
    let tester = e.0.clone();
    let test = thread::spawn(move || {
        tester.rpc("human_channels_test", &json!({"plugin_id":"fixture"}), None)
    });
    wait_for(|| plugin_dir.join("test-received").exists());
    let node = listener_id("none#alice", "human", "default");
    send(&e, &node, "after-test-started");
    e.rpc("human_channel_poll", &json!({"plugin_id":"fixture"}), None)
        .unwrap();
    let test = test.join().unwrap().unwrap();
    assert_eq!(test["effect_unknown"], true);
    assert_eq!(test["ok"], false);
    wait_for(|| {
        e.inner
            .db
            .query("SELECT status FROM hub_channel_deliveries", &[])
            .unwrap()
            == [json!({"status":"effect_unknown"})]
    });
    assert!(e
        .inner
        .db
        .query(
            "SELECT signal_id FROM hub_signals WHERE acked_at_ms IS NOT NULL",
            &[]
        )
        .unwrap()
        .is_empty());

    // Simulate the listener joining an HTTP worker that is waiting on the child.
    std::fs::remove_file(plugin_dir.join("test-received")).unwrap();
    let tester = e.0.clone();
    let (done, result) = mpsc::channel();
    let test = thread::spawn(move || {
        let value = tester.rpc("human_channels_test", &json!({"plugin_id":"fixture"}), None);
        let _ = done.send(value);
    });
    wait_for(|| plugin_dir.join("test-received").exists());
    e.shutdown_before(|| {
        assert!(result
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .starts_with("UNAVAILABLE:"));
    });
    test.join().unwrap();
}

#[test]
fn channel_test_uses_candidate_config_without_persisting_or_restarting_live_service() {
    let dir = temp();
    let root = fixture(&dir);
    let e = test_engine(&dir);
    e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    let owner = json!({"principal_id":"none#alice","owner_user":"alice"});
    e.rpc("human_channels_configure",&json!({"plugin_id":"fixture","enabled":false,"hub_config":{"credential":"saved"},"user_config":{"address":"saved-room"}}),Some(&owner)).unwrap();
    let before = e
        .inner
        .db
        .query("SELECT * FROM hub_plugin_services", &[])
        .unwrap();
    let binding = e
        .inner
        .db
        .query("SELECT * FROM hub_human_channels", &[])
        .unwrap();
    let live = e.inner.processes.lock().unwrap()["fixture"].clone();
    assert_eq!(e.rpc("human_channels_test",&json!({"plugin_id":"fixture","hub_config":{"credential":"candidate"},"user_config":{"address":"candidate-room"}}),Some(&owner)).unwrap()["ok"],true);
    assert_eq!(
        e.inner
            .db
            .query("SELECT * FROM hub_plugin_services", &[])
            .unwrap(),
        before
    );
    assert_eq!(
        e.inner
            .db
            .query("SELECT * FROM hub_human_channels", &[])
            .unwrap(),
        binding
    );
    assert!(Arc::ptr_eq(
        &live,
        &e.inner.processes.lock().unwrap()["fixture"]
    ));
    let frames = std::fs::read_to_string(root.join("fixture/frames.jsonl")).unwrap();
    let request: Value = frames
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["op"] == "test")
        .unwrap();
    assert_eq!(request["hub_config"]["credential"], "candidate");
    assert_eq!(request["user_config"]["address"], "candidate-room");
}
#[test]
fn attach_rejects_non_object_registrations_without_overwriting_kv() -> Result<(), String> {
    let dir = temp();
    let e = TestEngine(HubEngine::open(&dir)?);
    let pid = "none#alice";
    let id = listener_id(pid, "human", "default");
    for value in [
        json!([]),
        json!(42),
        json!(true),
        json!("invalid"),
        Value::Null,
    ] {
        e.rpc(
            "kv_put",
            &json!({"key":id,"data_b64":encoded(&value)}),
            None,
        )?;
        let binding = json!({"principal_id":pid,"owner_user":"alice","plugin_id":"fixture"});
        assert_eq!(
            e.inner
                .db
                .transaction(|c| attach(c, &binding, true, "workspace", None)),
            Err("INVALID_INPUT: Listener must be an object".into()),
        );
        let row = e
            .inner
            .db
            .transaction(|c| kv_row(c, &id))?
            .ok_or("Missing registration")?;
        assert_eq!(parse(&row, "value"), value);
    }
    Ok(())
}

#[test]
fn occupied_human_keys_require_the_owner_and_the_complete_human_tuple() {
    let dir = temp();
    let hub = test_engine(&dir);
    let owner = principal("alice");
    let key = listener_id("none#alice", "human", "default");
    let device = json!({"channel_id":"mac:human","host_id":"mac","binding_generation":1,"owner_token":"token"});
    for (pid, harness, session) in [
        ("none#bob", "human", "default"),
        ("none#alice", "sample", "default"),
        ("none#alice", "human", "other"),
    ] {
        let stored = json!({"harness":harness,"session_id":session,"principal_id":pid});
        hub.inner.db.execute("INSERT OR REPLACE INTO hub_kv(key,value,principal_id,revision,created_ms) VALUES(?,CAST(? AS BLOB),?,1,1)",&[json!(key),json!(stored.to_string()),json!(pid)]).unwrap();
        for op in [
            "human_device_attach",
            "human_device_heartbeat",
            "human_device_detach",
            "human_inbox_update",
        ] {
            assert!(
                hub.rpc(op, &device, Some(&owner))
                    .unwrap_err()
                    .starts_with("FORBIDDEN:"),
                "{op}"
            );
        }
        assert!(hub
            .inner
            .db
            .transaction(|c| attach(c, &owner, true, "workspace", None))
            .unwrap_err()
            .starts_with("FORBIDDEN:"));
        assert_eq!(
            hub.inner
                .db
                .query(
                    "SELECT revision,CAST(value AS TEXT) AS data FROM hub_kv",
                    &[]
                )
                .unwrap(),
            vec![json!({"revision":1,"data":stored.to_string()})]
        );
    }
}

#[test]
fn stream_and_consumer_configs_fit_their_read_responses() {
    let dir = temp();
    let e = test_engine(&dir);
    let name = "é".repeat(128);
    let long = format!("{name}x");
    let subject = format!("cito.signal.{}", "a".repeat(256 - "cito.signal.".len()));
    let add = |config: Value| e.rpc("stream_add", &json!({"config":config}), None);
    let update = |stream: &str, patch: Value| {
        e.rpc(
            "stream_update",
            &json!({"stream":stream,"patch":patch}),
            None,
        )
    };
    let consumer = |config: Value| e.rpc("consumer_add", &json!({"config":config}), None);
    for result in [
        add(json!({"name":long})),
        update(&long, json!({})),
        add(json!({"name":name,"subjects":[subject,long]})),
        update(&name, json!({"subjects":[long]})),
        add(json!({"name":name,"extra":"x".repeat(64*1024)})),
        consumer(json!({"name":long})),
        consumer(json!({"durable_name":long})),
        consumer(json!({"filter_subject":format!("{subject}a")})),
    ] {
        assert!(result.unwrap_err().starts_with("INVALID_INPUT:"));
    }
    let cfg = add(json!({"name":name,"subjects":[subject],"pad":""})).unwrap()["config"].clone();
    let pad = "a".repeat(64 * 1024 - cfg.to_string().len());
    let stored = update(&name, json!({"pad":pad})).unwrap()["config"].clone();
    assert_eq!(stored.to_string().len(), 64 * 1024);
    assert!(update(&name, json!({"pad":format!("{pad}a")}))
        .unwrap_err()
        .starts_with("INVALID_INPUT:"));
    assert_eq!(
        e.rpc("stream_info", &json!({"stream":name}), None).unwrap()["config"],
        stored
    );
    let mut cfg = json!({"durable_name":name,"filter_subject":subject,"pad":""});
    cfg["pad"] = json!("x".repeat(64 * 1024 - cfg.to_string().len()));
    assert_eq!(consumer(cfg.clone()).unwrap()["config"], cfg);
    let info = |stream: &str| {
        e.rpc("stream_info", &json!({"stream":stream}), None)
            .unwrap()
    };
    assert_eq!(info(SIGNALS)["state"]["consumer_count"], 1);
    assert_eq!(info(KV_STREAM)["state"]["consumer_count"], 0);
    assert_eq!(
        info(&name)["state"],
        json!({"messages":0,"last_seq":0,"first_seq":0,"bytes":0,"consumer_count":0})
    );
    cfg["pad"] = json!(format!("{}x", s(&cfg, "pad")));
    assert!(consumer(cfg).unwrap_err().starts_with("INVALID_INPUT:"));
}

#[test]
fn consumer_operations_validate_requested_and_persisted_streams_and_limit_id_batches() {
    let dir = temp();
    let hub = test_engine(&dir);
    let config = json!({"durable_name":"sample"});
    let req = json!({"config":config,"name":"sample","stream":"unsupported","seq":1});
    assert!(hub
        .rpc("consumer_add", &req, None)
        .unwrap_err()
        .starts_with("INVALID_INPUT:"));
    hub.rpc("consumer_add", &json!({"config":config}), None)
        .unwrap();
    for legacy in [false, true] {
        let mut req = req.clone();
        if legacy {
            hub.inner
                .db
                .execute("UPDATE hub_consumers SET stream='unsupported'", &[])
                .unwrap();
            req["stream"] = json!(SIGNALS);
        }
        for op in [
            "consumer_backlog",
            "consumer_next",
            "consumer_ack",
            "consumer_delete",
            "consumer_add",
        ] {
            assert!(
                hub.rpc(op, &req, None)
                    .unwrap_err()
                    .starts_with("INVALID_INPUT:"),
                "{op}"
            );
        }
    }
    let mut raw = vec!["z"; 1023];
    raw.push("a");
    for op in ["signal_claim_dispatch", "signal_ack"] {
        let mut request = json!({"node_id":"target","channel_id":"local","signal_ids":raw});
        let accepted = hub.rpc(op, &request, None).unwrap();
        assert_eq!(
            accepted[if op == "signal_ack" {
                "signal_ids"
            } else {
                "acked_ids"
            }],
            json!(["z", "a"])
        );
        request["signal_ids"]
            .as_array_mut()
            .unwrap()
            .push(json!("z"));
        assert!(hub
            .rpc(op, &request, None)
            .unwrap_err()
            .starts_with("INVALID_INPUT:"));
    }
}

#[test]
fn consumer_add_defers_a_large_first_signal_when_config_uses_its_headroom() {
    let dir = temp();
    let e = test_engine(&dir);
    put(&e, "target", "p", "workspace");
    let data = encoded(&json!({"message":"x".repeat(6 * 1024 * 1024 - 8192)}));
    let seq = e
        .rpc(
            "js_publish",
            &json!({"subject":"cito.signal.target", "data_b64":data}),
            None,
        )
        .unwrap()["seq"]
        .clone();
    let config =
        json!({"name":"wide","filter_subject":"cito.signal.target","note":"x".repeat(60 * 1024)});
    let added = e
        .rpc("consumer_add", &json!({"config":config}), None)
        .unwrap();
    assert_eq!(added["config"], config);
    assert_eq!(added["backlog"], json!([]));
    let next = e
        .rpc("consumer_next", &json!({"name":"wide"}), None)
        .unwrap();
    assert_eq!(next["seq"], seq);
    assert_eq!(next["data_b64"], data);
    assert!(json!({"ok":true,"result":next}).to_string().len() < MAX_RPC_BYTES);
}

#[test]
fn consumer_next_crosses_byte_pages_without_transport_acknowledgments() {
    let dir = temp();
    let e = test_engine(&dir);
    put(&e, "target", "p", "workspace");
    let mut sequences = Vec::new();
    for i in 0..4 {
        let result = e
            .rpc(
                "js_publish",
                &json!({
                    "subject":"cito.signal.target",
                    "data_b64":encoded(&json!({
                        "signal_id":format!("page-{i}"),
                        "message":"x".repeat(1024 * 1024),
                    })),
                }),
                None,
            )
            .unwrap();
        sequences.push(result["seq"].clone());
    }
    let page = e
        .rpc(
            "consumer_add",
            &json!({"config":{"durable_name":"paged","filter_subject":"cito.signal.target"}}),
            None,
        )
        .unwrap();
    let first_page: Vec<_> = page["backlog"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["seq"].clone())
        .collect();
    assert_eq!(first_page, sequences[..2]);
    for (i, seq) in sequences.iter().enumerate() {
        let item = e
            .rpc("consumer_next", &json!({"name":"paged"}), None)
            .unwrap();
        assert_eq!(&item["seq"], seq, "cursor stopped at item {i}");
        let payload: Value =
            serde_json::from_slice(&B64.decode(item["data_b64"].as_str().unwrap()).unwrap())
                .unwrap();
        assert_eq!(payload["signal_id"], format!("page-{i}"));
        assert_eq!(payload["message"].as_str().unwrap().len(), 1024 * 1024);
    }
    assert_eq!(
        e.rpc("consumer_next", &json!({"name":"paged"}), None)
            .unwrap(),
        Value::Null,
    );
    let backlog = e
        .rpc("consumer_backlog", &json!({"name":"paged"}), None)
        .unwrap();
    let pending: Vec<_> = backlog["backlog"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["seq"].clone())
        .collect();
    // Sequential reads never acknowledge transport backlog.
    assert_eq!(pending, first_page);
    for seq in &sequences[..2] {
        e.rpc("consumer_ack", &json!({"name":"paged","seq":seq}), None)
            .unwrap();
    }
    let backlog = e
        .rpc("consumer_backlog", &json!({"name":"paged"}), None)
        .unwrap();
    let pending: Vec<_> = backlog["backlog"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["seq"].clone())
        .collect();
    assert_eq!(pending, sequences[2..]);
}

#[test]
fn atomic_batch_claims_and_business_ack_are_distinct() {
    let dir = temp();
    let e = test_engine(&dir);
    put(&e, "target", "p", "workspace");
    send(&e, "target", "s1");
    send(&e, "target", "s2");
    let claim = |ids: Value, channel: &str| {
        e.rpc(
            "signal_claim_dispatch",
            &json!({"node_id":"target","signal_ids":ids,"channel_id":channel}),
            None,
        )
        .unwrap()
    };
    assert_eq!(claim(json!(["s2"]), "b")["claimed"], true);
    assert_eq!(claim(json!(["s1", "s2"]), "a")["reason"], "already_claimed");
    assert_eq!(claim(json!(["s1"]), "c")["claimed"], true);
    assert_eq!(
        claim(json!(["s1", "missing"]), "c")["acked_ids"],
        json!(["missing"])
    );
    e.rpc(
        "consumer_add",
        &json!({"config":{"durable_name":"consumer","filter_subject":"cito.signal.target"}}),
        None,
    )
    .unwrap();
    e.rpc("consumer_ack", &json!({"name":"consumer","seq":1}), None)
        .unwrap();
    assert_eq!(
        e.rpc(
            "stream_get_message",
            &json!({"stream":SIGNALS,"seq":1}),
            None
        )
        .unwrap()["acked_at_ms"],
        Value::Null
    );
    e.rpc(
        "signal_ack",
        &json!({"node_id":"target","signal_ids":["s1"],"channel_id":"c"}),
        None,
    )
    .unwrap();
    assert_eq!(claim(json!(["s1", "s2"]), "b")["acked_ids"], json!(["s1"]));
    e.shutdown();
    let e = test_engine(&dir);
    assert!(e
        .rpc("kv_get", &json!({"key":"target"}), None)
        .unwrap()
        .is_object());
    assert_eq!(
        e.rpc("consumer_backlog", &json!({"name":"consumer"}), None)
            .unwrap()["backlog"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn oidc_read_and_ack_events_are_private() {
    let dir = temp();
    let e = test_engine(&dir);
    let owner = json!({"principal_id":"oidc:https://id.example.com|owner","owner_user":"alice","oidc":true});
    let stranger = json!({"principal_id":"oidc:https://id.example.com|stranger","owner_user":"bob","oidc":true});
    put(&e, "target", s(&owner, "principal_id"), "self");
    send(&e, "target", "s1");
    assert!(e
        .rpc(
            "stream_get_message",
            &json!({"stream":SIGNALS,"seq":1}),
            Some(&stranger)
        )
        .unwrap_err()
        .starts_with("FORBIDDEN"));
    assert!(e
        .rpc(
            "stream_get_message",
            &json!({"stream":SIGNALS,"seq":1}),
            Some(&owner)
        )
        .is_ok());
    let a = e.subscribe(Some(owner.clone())).unwrap();
    let b = e.subscribe(Some(stranger)).unwrap();
    e.rpc(
        "signal_ack",
        &json!({"node_id":"target","signal_ids":["s1"]}),
        Some(&owner),
    )
    .unwrap();
    assert_eq!(
        a.recv_timeout(Duration::from_millis(100)).unwrap()["type"],
        "nc"
    );
    assert!(b.recv_timeout(Duration::from_millis(30)).is_err());
}
#[test]
fn workspace_directory_does_not_grant_message_or_ack_content() {
    let dir = temp();
    let e = test_engine(&dir);
    let recipient = json!({"principal_id":"oidc:https://id.example.com|owner","owner_user":"alice","oidc":true});
    let sender =
        json!({"principal_id":"oidc:https://id.example.com|sender","owner_user":"bob","oidc":true});
    let third = json!({"principal_id":"oidc:https://id.example.com|third","owner_user":"carol","oidc":true});
    put(&e, "target", s(&recipient, "principal_id"), "workspace");
    assert!(e
        .rpc("kv_get", &json!({"key":"target"}), Some(&third))
        .unwrap()
        .get("value_b64")
        .is_some());
    let recipient_events = e.subscribe(Some(recipient.clone())).unwrap();
    let third_events = e.subscribe(Some(third.clone())).unwrap();
    send(&e, "target", "s1");
    for principal in [&recipient, &sender] {
        assert!(e
            .rpc(
                "stream_get_message",
                &json!({"stream":SIGNALS,"seq":1}),
                Some(principal)
            )
            .is_ok());
    }
    assert!(e
        .rpc(
            "stream_get_message",
            &json!({"stream":SIGNALS,"seq":1}),
            Some(&third)
        )
        .unwrap_err()
        .starts_with("FORBIDDEN"));
    assert_eq!(
        recipient_events
            .recv_timeout(Duration::from_millis(100))
            .unwrap()["type"],
        "signal"
    );
    assert!(third_events
        .recv_timeout(Duration::from_millis(30))
        .is_err());
    e.rpc(
        "signal_ack",
        &json!({"node_id":"target","signal_ids":["s1"]}),
        Some(&recipient),
    )
    .unwrap();
    assert_eq!(
        recipient_events
            .recv_timeout(Duration::from_millis(100))
            .unwrap()["type"],
        "nc"
    );
    assert!(third_events
        .recv_timeout(Duration::from_millis(30))
        .is_err());
    e.rpc(
        "nc_publish",
        &json!({"subject":"cito.ack.target","data_b64":encoded(&json!({"signal_ids":["s1"]}))}),
        Some(&recipient),
    )
    .unwrap();
    assert_eq!(
        recipient_events
            .recv_timeout(Duration::from_millis(100))
            .unwrap()["type"],
        "nc"
    );
    assert!(third_events
        .recv_timeout(Duration::from_millis(30))
        .is_err());
}
#[test]
fn pair_token_encodes_exactly_six_bytes_as_uppercase_base32() {
    assert_eq!(encode_pair_token(*b"foobar"), "MZXW6YTBOI");
    assert_eq!(encode_pair_token([0; 6]), "AAAAAAAAAA");
    assert_eq!(encode_pair_token([255; 6]), "7777777774");
    let code = pair_code().unwrap();
    let token = code.strip_prefix("cito-").unwrap();
    assert_eq!(token.len(), 10);
    assert!(token
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b"234567".contains(&b)));
}
#[test]
fn account_info_counts_only_persisted_stream_configuration() {
    let dir = temp();
    let e = test_engine(&dir);
    assert_eq!(
        e.rpc("account_info", &json!({}), None).unwrap()["streams"],
        0
    );
    for stream in [SIGNALS, KV_STREAM] {
        assert_eq!(
            e.rpc("stream_info", &json!({"stream":stream}), None)
                .unwrap()["config"]["name"],
            stream
        );
    }
    assert_eq!(
        e.rpc("account_info", &json!({}), None).unwrap()["streams"],
        0
    );
    e.rpc("stream_add", &json!({"config":{"name":SIGNALS}}), None)
        .unwrap();
    assert_eq!(
        e.rpc("account_info", &json!({}), None).unwrap()["streams"],
        1
    );
    e.rpc(
        "stream_update",
        &json!({"stream":KV_STREAM,"patch":{"max_age":0}}),
        None,
    )
    .unwrap();
    assert_eq!(
        e.rpc("account_info", &json!({}), None).unwrap()["streams"],
        2
    );
    e.shutdown();
    let e = test_engine(&dir);
    assert_eq!(
        e.rpc("account_info", &json!({}), None).unwrap()["streams"],
        2
    );
}
fn fixture(dir: &Path) -> PathBuf {
    let root = dir.join("fixture-plugins");
    let plugin = root.join("fixture");
    std::fs::create_dir_all(&plugin).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/hub/channel.py");
    let manifest = json!({"id":"fixture","kind":"human-channel","scope":"hub","displayName":"Fixture","command":["python3",script],"pairing":{"supported":true},"fields":[{"key":"credential","scope":"hub","type":"secret","required":true},{"key":"address","scope":"user","type":"string","required":true}]});
    std::fs::write(plugin.join("plugin.json"), manifest.to_string()).unwrap();
    root
}
#[test]
fn channel_config_limits_apply_to_fields_and_merged_serialized_values() {
    let dir = temp();
    let root = fixture(&dir);
    let path = root.join("fixture/plugin.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for scope in ["hub", "user"] {
        for suffix in ["first", "second"] {
            manifest["fields"]
                .as_array_mut()
                .unwrap()
                .push(json!({"key":format!("{scope}_{suffix}"),"scope":scope,"type":"string"}));
        }
    }
    std::fs::write(path, manifest.to_string()).unwrap();
    let e = test_engine(&dir);
    e.pause_worker();
    e.set_plugin_roots(&[root]).unwrap();
    for scope in ["hub", "user"] {
        let first = format!("{scope}_first");
        let second = format!("{scope}_second");
        let configure = |key: &str, value: &str| {
            e.rpc(
                "human_channels_configure",
                &json!({"plugin_id":"fixture",format!("{scope}_config"):{key:value}}),
                None,
            )
        };
        let field_max = "é".repeat(32 * 1024);
        configure(&first, &field_max).unwrap();
        assert!(configure(&first, &format!("{field_max}x"))
            .unwrap_err()
            .starts_with("INVALID_INPUT:"));
        let remaining = 256 * 1024 - json!({&first:field_max,&second:""}).to_string().len();
        let last = "\u{0001}".repeat(remaining / 6) + &"x".repeat(remaining % 6);
        configure(&second, &last).unwrap();
        assert!(configure(&second, &format!("{last}x"))
            .unwrap_err()
            .starts_with("INVALID_INPUT:"));
        if scope == "user" {
            let process = e.ensure_process(&e.plugin("fixture").unwrap()).unwrap();
            e.inner.db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,delivered_ms) VALUES('fixture','p','pending',1,'in_flight',?)", &[json!(now_ms())]).unwrap();
            let err = e.rpc("human_channels_configure", &json!({"plugin_id":"fixture","reset_hub":true,"reset_user":false,"user_config":{&second:format!("{last}x")}}), None).unwrap_err();
            assert!(err.contains("256 KiB"), "{err}");
            assert!(process.alive());
            assert_eq!(
                e.inner
                    .db
                    .query(
                        "SELECT status FROM hub_channel_deliveries WHERE signal_id='pending'",
                        &[]
                    )
                    .unwrap()[0]["status"],
                "in_flight"
            );
        }
        let list = e.rpc("human_channels_list", &json!({}), None).unwrap();
        let fields = list["channels"][0]["fields"].as_array().unwrap();
        for (key, val) in [(&first, &field_max), (&second, &last)] {
            assert_eq!(
                fields.iter().find(|f| s(f, "key") == key).unwrap()["value"].as_str(),
                Some(val.as_str())
            );
        }
    }
}

#[test]
fn channel_inventory_bounds_the_complete_serialized_response() {
    let dir = temp();
    let root = fixture(&dir);
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("fixture/plugin.json")).unwrap()).unwrap();
    let e = test_engine(&dir);
    e.pause_worker();
    let caller = principal("alice");
    let cfg = channel_config_json(&json!({"address":"\u{0001}".repeat(43000)})).unwrap();
    for i in 0..33 {
        let id = format!("channel-{i}");
        manifest["id"] = json!(id);
        let path = root.join(&id);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("plugin.json"), manifest.to_string()).unwrap();
        e.inner.db.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,user_config_json) VALUES(?,?,'alice',?)", &[caller["principal_id"].clone(), json!(id), json!(cfg)]).unwrap();
    }
    e.set_plugin_roots(&[root]).unwrap();
    let list = || e.rpc("human_channels_list", &json!({}), Some(&caller));
    assert!(list().unwrap_err().contains("response frame budget"));
    e.inner
        .db
        .execute(
            "DELETE FROM hub_human_channels WHERE plugin_id='channel-0'",
            &[],
        )
        .unwrap();
    let result = list().unwrap();
    assert_eq!(result["channels"].as_array().unwrap().len(), 34);
    assert!(json!({"ok":true,"result":result}).to_string().len() < MAX_RPC_BYTES);
}

fn wait_for(mut f: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    while !f() {
        assert!(
            std::time::Instant::now() < deadline,
            "condition did not become true"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn tick_services_healthy_sibling_after_plugin_spawn_failure() {
    let dir = temp();
    let root = fixture(&dir);
    let e = test_engine(&dir);
    // Drive exactly one tick; a background retry must not satisfy this test.
    e.pause_worker();

    let healthy = root.join("b");
    std::fs::rename(root.join("fixture"), &healthy).unwrap();
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(healthy.join("plugin.json")).unwrap())
            .unwrap();
    manifest["id"] = json!("b");
    std::fs::write(healthy.join("plugin.json"), manifest.to_string()).unwrap();
    let broken = root.join("a");
    std::fs::create_dir_all(&broken).unwrap();
    manifest["id"] = json!("a");
    manifest["command"] = json!([dir.join("missing-executable")]);
    std::fs::write(broken.join("plugin.json"), manifest.to_string()).unwrap();
    e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    assert_eq!(
        e.inner
            .plugins
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    let pid = "none#alice";
    e.inner.db.transaction(|c| {
            for id in ["a", "b"] {
                save_service(c, id, &json!({"credential":"test-only"}))?;
                c.execute(
                    "INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,user_config_json) VALUES(?,?,'alice',?)",
                    &[json!(pid), json!(id), json!(json!({"address":"room"}).to_string())],
                )?;
            }
            Ok(())
        }).unwrap();
    let human = listener_id(pid, "human", "default");
    send(&e, &human, "sibling-notification");
    assert!(e.inner.processes.lock().unwrap().is_empty());
    assert!(e.tick().unwrap_err().starts_with("PLUGIN_EXEC_FAILED: a:"));
    wait_for(|| {
        !e.inner.db.query(
                "SELECT 1 AS n FROM hub_channel_deliveries WHERE plugin_id='b' AND signal_id='sibling-notification' AND status='delivered'",
                &[],
            ).unwrap().is_empty()
    });
    let frames: Vec<Value> = std::fs::read_to_string(healthy.join("frames.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        frames.iter().map(|f| s(f, "op")).collect::<Vec<_>>(),
        vec!["configure", "deliver"]
    );
    assert_eq!(frames[0]["hub_config"]["credential"], "test-only");
    assert_eq!(
        frames[0]["bindings"],
        json!([{"principal_id":pid,"user":"alice","config":{"address":"room"}}])
    );
    assert_eq!(frames[1]["signal"]["signal_id"], "sibling-notification");
    assert_eq!(frames[1]["signal"]["to"]["listener_id"], human);
    assert_eq!(frames[1]["signal"]["message"], "hello");
    assert_eq!(frames[1]["config"]["address"], "room");
}

#[test]
fn pairing_instruction_persists_and_admin_guard_covers_all_paths() {
    let dir = temp();
    let root = fixture(&dir);
    let e = TestEngine(HubEngine::open_with_auth_mode(&dir, "token").unwrap());
    e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    let owner = json!({"principal_id":"none#alice.smith","owner_user":"alice.smith"});
    e.rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","hub_config":{"credential":"test-only"}}),
        Some(&owner),
    )
    .unwrap();
    let pair = e
        .rpc(
            "human_channel_pair_start",
            &json!({"plugin_id":"fixture"}),
            Some(&owner),
        )
        .unwrap();
    let code = pair["code"].as_str().unwrap();
    assert_eq!(code.len(), 15);
    assert!(code[5..]
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b"234567".contains(&b)));
    assert!(pair["instruction"]
        .as_str()
        .unwrap()
        .contains("fixture-account"));
    e.shutdown();
    let e = TestEngine(HubEngine::open_with_auth_mode(&dir, "token").unwrap());
    e.set_plugin_roots(&[root]).unwrap();
    let list = e
        .rpc("human_channels_list", &json!({}), Some(&owner))
        .unwrap();
    assert_eq!(
        list["channels"][0]["pairing_instruction"],
        pair["instruction"]
    );
    assert_eq!(list["channels"][0]["fields"][0]["value"], "");
    assert!(pair["instruction"].as_str().unwrap().contains(code));
    let frame = json!({"code":code,"is_private_chat":true,"external_chat_id":"paired-chat","external_user_id":"paired-user","user_config_patch":{"address":"paired-room"}});
    let redeemed = e.redeem("fixture", &frame).unwrap();
    assert_eq!(redeemed["ok"], true);
    e.shutdown();
    let e = TestEngine(HubEngine::open_with_auth_mode(&dir, "token").unwrap());
    e.set_plugin_roots(&[fixture(&dir)]).unwrap();
    let mut replay = frame;
    replay["user_config_patch"] = json!({"address":"must-not-be-applied-twice"});
    assert_eq!(e.redeem("fixture", &replay).unwrap()["duplicate"], true);
    replay["external_user_id"] = json!("different-account");
    assert!(e.redeem("fixture", &replay).is_err());
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT code FROM hub_pairing_codes WHERE consumed_ms IS NOT NULL",
                &[]
            )
            .unwrap()[0]["code"],
        code
    );
    let user = json!({"principal_id":"oidc:https://id.example.com|ordinary-test-user","owner_user":"ordinary-test-user","sub":"ordinary-test-user","oidc":true});
    for op in [
        "human_channels_configure",
        "human_channels_test",
        "human_channel_pair_start",
    ] {
        let err = e
            .rpc(
                op,
                &json!({"plugin_id":"fixture","hub_config":{"credential":"other"}}),
                Some(&user),
            )
            .unwrap_err();
        assert!(err.starts_with("FORBIDDEN"), "{op}: {err}");
    }
    e.rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","user_config":{"address":"room"},"enabled":true}),
        Some(&user),
    )
    .unwrap();
    let human = e.inner.db.transaction(|c| find_human(c, &user)).unwrap().0;
    let node = e.rpc("kv_get", &json!({"key":human}), Some(&user)).unwrap();
    let reg: Value = serde_json::from_slice(&B64.decode(s(&node, "value_b64")).unwrap()).unwrap();
    assert_eq!(reg["scope"], "self");
    assert_eq!(
        evaluate_node_lease(&reg, (now_ms() + 10_000_000) as u64),
        "ACTIVE"
    );
}
#[test]
fn human_public_id_uses_canonical_slug_normalization() {
    let dir = temp();
    let root = fixture(&dir);
    let e = test_engine(&dir);
    e.set_plugin_roots(&[root]).unwrap();
    let caller = json!({"principal_id":"none#slug-fixture","owner_user":"İ"});
    e.rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","enabled":true,"user_config":{"address":"room"}}),
        Some(&caller),
    )
    .unwrap();
    let id = listener_id(s(&caller, "principal_id"), "human", "default");
    let row = e.inner.db.transaction(|c| kv_row(c, &id)).unwrap().unwrap();
    assert_eq!(
        parse(&row, "value")["public_id"],
        format!("user-{}", id.trim_start_matches("c_"))
    );
}
#[test]
fn cli_option_diagnostics_name_configuration_knobs() {
    for (flag, setting) in [
        ("--auth", "CITO_AUTH_MODE"),
        ("--port", "CITO_HUB_PORT"),
        ("--token", "CITO_HUB_TOKEN"),
        ("--issuer", "CITO_OIDC_ISSUER"),
        ("--client-id", "CITO_OIDC_CLIENT_ID"),
    ] {
        for args in [vec![flag.to_owned()], vec![flag.to_owned(), String::new()]] {
            assert!(cli_options(&args).unwrap_err().contains(setting));
        }
    }
    assert_eq!(
        cli_options(&["positional".into()]).unwrap_err(),
        "Unexpected argument: positional"
    );
}
#[test]
fn incomplete_bindings_skip_delivery_and_removed_plugins_restore_inactive() {
    let dir = temp();
    let root = fixture(&dir);
    let e = test_engine(&dir);
    e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    let owner = json!({"principal_id":"none#alice","owner_user":"alice"});
    e.rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","hub_config":{"credential":"saved"},"enabled":true}),
        Some(&owner),
    )
    .unwrap();
    let node = listener_id("none#alice", "human", "default");
    send(&e, &node, "unready");
    let plugin = e.plugin("fixture").unwrap();
    let process = e.ensure_process(&plugin).unwrap();
    e.replay(&plugin, &process).unwrap();
    assert!(e
        .inner
        .db
        .query("SELECT * FROM hub_channel_deliveries", &[])
        .unwrap()
        .is_empty());
    e.inner
        .db
        .execute(
            "UPDATE hub_human_channels SET user_config_json='{\"address\":\"room\"}'",
            &[],
        )
        .unwrap();
    e.restore_attachments().unwrap();
    e.replay(&plugin, &process).unwrap();
    assert_eq!(
        e.inner
            .db
            .query("SELECT * FROM hub_channel_deliveries", &[])
            .unwrap()
            .len(),
        1
    );
    e.inner.plugins.lock().unwrap().clear();
    e.restore_attachments().unwrap();
    let registration = e.rpc("kv_get", &json!({"key":node}), None).unwrap();
    let data: Value = serde_json::from_slice(
        &B64.decode(registration["value_b64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(data["channels"]["hub:fixture"]["state"], "UNLISTENED");
}
#[test]
fn subscriptions_unregister_on_drop_without_waiting_for_an_event() {
    let dir = temp();
    let e = test_engine(&dir);
    for _ in 0..20 {
        let subscription = e.subscribe(None).unwrap();
        assert_eq!(e.inner.subscribers.lock().unwrap().len(), 1);
        drop(subscription);
        assert!(e.inner.subscribers.lock().unwrap().is_empty());
    }
}
#[test]
fn channel_shutdown_kills_descendants_holding_stdout_open() {
    let dir = temp();
    let root = fixture(&dir);
    let mut plugin = hub_plugins::discover(&[(root, false)], None)
        .unwrap()
        .remove(0);
    plugin.command = vec![
        "/bin/sh".into(),
        "-c".into(),
        "sleep 30 & printf '%s\\n' '{\"op\":\"started\"}'; wait".into(),
    ];
    let (tx, rx) = mpsc::sync_channel(1);
    let process = ChannelProcess::spawn(
        &plugin,
        Arc::new(move |_| {
            let _ = tx.try_send(());
            None
        }),
    )
    .unwrap();
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let started = std::time::Instant::now();
    process.shutdown();
    assert!(started.elapsed() < Duration::from_secs(2));
}
#[test]
fn reservations_retry_holes_and_record_all_chunks() {
    let dir = temp();
    let root = fixture(&dir);
    let e = test_engine(&dir);
    e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    let owner = json!({"principal_id":"none#alice","owner_user":"alice"});
    let node = listener_id("none#alice", "human", "default");
    send(&e, &node, "historical");
    e.rpc("human_channels_configure",&json!({"plugin_id":"fixture","enabled":true,"hub_config":{"credential":"test-only"},"user_config":{"address":"room"}}),Some(&owner)).unwrap();
    send(&e, &node, "fail-once");
    send(&e, &node, "later");
    wait_for(|| {
        e.inner
            .db
            .query(
                "SELECT signal_id FROM hub_channel_deliveries WHERE status='delivered'",
                &[],
            )
            .unwrap()
            .len()
            == 2
    });
    let rows = e
        .inner
        .db
        .query("SELECT * FROM hub_channel_delivery_msgs", &[])
        .unwrap();
    assert_eq!(rows.len(), 4);
    assert!(!rows.iter().any(|r| r["signal_id"] == "historical"));
    let boundary = e
        .inner
        .db
        .query("SELECT last_delivered_seq FROM hub_human_channels", &[])
        .unwrap()[0]["last_delivered_seq"]
        .clone();
    assert_eq!(boundary, 1);
    e.rpc(
        "signal_ack",
        &json!({"node_id":node,"signal_ids":["later"]}),
        None,
    )
    .unwrap();
    wait_for(|| {
        std::fs::read_to_string(root.join("fixture/frames.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .any(|f| {
                f["op"] == "on_acked"
                    && f["external_msg_ids"]
                        .as_array()
                        .is_some_and(|a| a.len() == 2)
            })
    });
    e.shutdown();
    let frames = std::fs::read_to_string(root.join("fixture/frames.jsonl")).unwrap();
    let sends: Vec<Value> = frames
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|f: &Value| f["op"] == "deliver")
        .collect();
    assert_eq!(
        sends
            .iter()
            .filter(|f| f["signal"]["signal_id"] == "later")
            .count(),
        1
    );
    assert_eq!(
        sends
            .iter()
            .filter(|f| f["signal"]["signal_id"] == "fail-once")
            .count(),
        2
    );
}
#[test]
fn principal_alias_reregistration_preserves_replay_and_ack_message_ids() {
    let dir = temp();
    let root = fixture(&dir);
    let e = test_engine(&dir);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
        e.rpc(
            "human_channels_configure",
            &json!({"plugin_id":"fixture","hub_config":{"credential":"test-only"}}),
            None,
        )
        .unwrap();
        for (binding_pid, node_pid, signal) in [
            (
                "https://id.example.com#alice",
                "oidc:https://id.example.com|alice",
                "legacy-binding",
            ),
            (
                "oidc:https://id.example.com|bob",
                "https://id.example.com#bob",
                "canonical-binding",
            ),
        ] {
            let owner = json!({"principal_id":binding_pid,"owner_user":"alice"});
            e.rpc(
                "human_channels_configure",
                &json!({"plugin_id":"fixture","enabled":true,"user_config":{"address":signal}}),
                Some(&owner),
            )
            .unwrap();
            let old_node = listener_id(binding_pid, "human", "default");
            let mut registration = e
                .inner
                .db
                .transaction(|c| {
                    Ok(parse(
                        &kv_row(c, &old_node)?.ok_or("Missing Human node")?,
                        "value",
                    ))
                })
                .unwrap();
            let node = listener_id(node_pid, "human", "default");
            registration["listener_id"] = json!(node);
            registration["principal_id"] = json!(node_pid);
            // Seed historical principal aliases directly; public whole-record Human writes are forbidden.
            e.inner
                .db
                .transaction(|c| {
                    c.execute("DELETE FROM hub_kv WHERE key=?", &[json!(old_node)])?;
                    save_human(c, &node, &mut registration)?;
                    let mut unrelated = registration.clone();
                    unrelated["principal_id"] = json!("oidc:https://id.example.com|unrelated");
                    unrelated["listener_id"] = json!("unrelated-human");
                    save_human(c, "unrelated-human", &mut unrelated)?;
                    Ok(())
                })
                .unwrap();
            send_from(&e, &node, signal, binding_pid, "sender");
            wait_for(|| {
                !e.inner.db.query(
                "SELECT 1 AS n FROM hub_channel_deliveries WHERE principal_id=? AND signal_id=? AND status='delivered'",
                &[json!(binding_pid),json!(signal)],
            ).unwrap().is_empty()
            });
            assert_eq!(
                e.rpc(
                    "signal_ack",
                    &json!({"node_id":node,"signal_ids":[signal]}),
                    Some(&owner)
                )
                .unwrap()["acked_count"],
                1
            );
            wait_for(|| {
                std::fs::read_to_string(root.join("fixture/frames.jsonl"))
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                    .any(|f| f["op"] == "on_acked" && f["signal_ids"] == json!([signal]))
            });
        }
    }));
    // Stop the persistent child even when a behavioral assertion fails.
    e.shutdown();
    let frames = std::fs::read_to_string(root.join("fixture/frames.jsonl")).unwrap_or_default();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    let acks: Vec<Value> = frames
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|f| f["op"] == "on_acked")
        .collect();
    assert_eq!(acks.len(), 2);
    for (ack, signal, pid) in [
        (&acks[0], "legacy-binding", "https://id.example.com#alice"),
        (
            &acks[1],
            "canonical-binding",
            "oidc:https://id.example.com|bob",
        ),
    ] {
        let mut external = ack["external_msg_ids"].as_array().unwrap().clone();
        external.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        assert_eq!(
            external,
            vec![json!(format!("{signal}-1")), json!(format!("{signal}-2"))]
        );
        assert_eq!(ack["principal_id"], pid);
        assert_eq!(ack["user_config"]["address"], signal);
    }
}
#[test]
fn human_reply_rpc_notifies_bound_channels_with_source_message_ids() {
    let dir = temp();
    let root = fixture(&dir);
    let e = test_engine(&dir);
    e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    e.rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","hub_config":{"credential":"test-only"}}),
        None,
    )
    .unwrap();
    let owner = principal("alice");
    e.rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","enabled":true,"user_config":{"address":"reply-test"}}),
        Some(&owner),
    )
    .unwrap();
    let human = listener_id("none#alice", "human", "default");
    put(&e, "target", "none#bob", "workspace");
    send_from(&e, &human, "source", "none#bob", "target");
    wait_for(|| {
        !e.inner.db.query("SELECT 1 AS n FROM hub_channel_deliveries WHERE signal_id='source' AND status='delivered'", &[]).unwrap().is_empty()
    });
    let reply = e
        .rpc(
            "send_reply_from_human",
            &json!({"to":"target","message":"Approved","signal_id":"work:source"}),
            Some(&owner),
        )
        .unwrap();
    assert_eq!(reply["ok"], true);
    wait_for(|| {
        std::fs::read_to_string(root.join("fixture/frames.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .any(|f| f["op"] == "on_acked" && f["signal_ids"] == json!(["source"]))
    });
    assert!(e
        .inner
        .db
        .query(
            "SELECT acked_at_ms FROM hub_signals WHERE signal_id='source'",
            &[]
        )
        .unwrap()[0]["acked_at_ms"]
        .is_number());
    let frames = std::fs::read_to_string(root.join("fixture/frames.jsonl")).unwrap();
    let ack = frames
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|f| f["op"] == "on_acked" && f["signal_ids"] == json!(["source"]))
        .unwrap();
    assert_eq!(ack["principal_id"], "none#alice");
    let mut external = ack["external_msg_ids"].as_array().unwrap().clone();
    external.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    assert_eq!(external, vec![json!("source-1"), json!("source-2")]);
    let stored = e
        .inner
        .db
        .query(
            "SELECT CAST(data AS TEXT) AS data FROM hub_signals WHERE signal_id=?",
            &[reply["signal_id"].clone()],
        )
        .unwrap();
    assert_eq!(parse(&stored[0], "data")["in_reply_to"], "source");
}
#[test]
fn human_reply_rpc_uses_fallback_node_and_rolls_back_publication_on_ack_failure() {
    let dir = temp();
    let e = test_engine(&dir);
    let sender = principal("alice");
    let human = listener_id("none#alice", "human", "default");
    put(&e, "target", "none#bob", "workspace");
    send_from(&e, &human, "original", "none#bob", "target");
    let events = e.subscribe(None).unwrap();
    let request = json!({"to":"work:target","message":" \u{0081}Approv\u{0081}ed\t ","in_reply_to":"work:original","msg_id":"desktop-reply","host_id":"desktop","host_name":"Desktop"});
    put(&e, "wrong-sender", "none#bob", "workspace");
    let mut wrong = request.clone();
    wrong["to"] = json!("wrong-sender");
    assert!(e
        .rpc("send_reply_from_human", &wrong, Some(&sender))
        .unwrap_err()
        .contains("source signal sender"));
    // Drain the registration event before checking that a failed reply emits nothing.
    assert_eq!(events.try_recv().unwrap()["type"], "kv");
    e.inner.db.execute("CREATE TRIGGER fail_reply_ack BEFORE UPDATE OF acked_at_ms ON hub_signals WHEN OLD.signal_id='original' BEGIN SELECT RAISE(ABORT,'injected ack failure'); END", &[]).unwrap();
    assert!(e
        .rpc("send_reply_from_human", &request, Some(&sender))
        .unwrap_err()
        .contains("injected ack failure"));
    assert_eq!(
        e.inner
            .db
            .query("SELECT signal_id,acked_at_ms FROM hub_signals", &[])
            .unwrap(),
        vec![json!({"signal_id":"original","acked_at_ms":null})]
    );
    assert!(matches!(events.try_recv(), Err(mpsc::TryRecvError::Empty)));
    e.inner
        .db
        .execute("DROP TRIGGER fail_reply_ack", &[])
        .unwrap();
    let reply = e
        .rpc("send_reply_from_human", &request, Some(&sender))
        .unwrap();
    let published = events.recv_timeout(Duration::from_millis(100)).unwrap();
    assert_eq!(published["type"], "signal");
    let payload: Value =
        serde_json::from_slice(&B64.decode(s(&published, "data_b64")).unwrap()).unwrap();
    assert_eq!(payload["signal_id"], reply["signal_id"]);
    assert_eq!(payload["to"], "target");
    assert_eq!(payload["message"], "Approved");
    assert_eq!(payload["in_reply_to"], "original");
    assert_eq!(payload["from"]["principal_id"], "none#alice");
    assert_eq!(payload["from"]["title"], "Human");
    assert_eq!(payload["from"]["host_id"], "desktop");
    assert_eq!(payload["from"]["host_name"], "Desktop");
    assert_eq!(published["seq"], reply["seq"]);
    assert_eq!(
        payload["from"]["listener_id"],
        format!("alice-{}", human.trim_start_matches("c_"))
    );
    let event = events.recv_timeout(Duration::from_millis(100)).unwrap();
    assert_eq!(event["subject"], format!("cito.ack.{human}"));
    let ack: Value = serde_json::from_slice(&B64.decode(s(&event, "data_b64")).unwrap()).unwrap();
    assert_eq!(ack["node_id"], human);
    assert_eq!(ack["signal_ids"], json!(["original"]));
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT acked_at_ms FROM hub_signals WHERE signal_id='original'",
                &[]
            )
            .unwrap()[0]["acked_at_ms"],
        reply["acked_at_ms"]
    );
    assert!(reply["acked_at_ms"].is_number());
    let duplicate = e
        .rpc("send_reply_from_human", &request, Some(&sender))
        .unwrap();
    assert_eq!(duplicate["signal_id"], reply["signal_id"]);
    assert_eq!(duplicate["seq"], reply["seq"]);
    assert_eq!(
        e.inner
            .db
            .query("SELECT signal_id FROM hub_signals ORDER BY seq", &[])
            .unwrap(),
        vec![
            json!({"signal_id":"original"}),
            json!({"signal_id":reply["signal_id"]})
        ]
    );
}
#[test]
fn reply_mappings_outlive_payloads_for_seven_days_and_ack_missing_sources() {
    let dir = temp();
    let e = test_engine(&dir);
    put(&e, "sender", "none#bob", "workspace");
    let pid = "none#alice";
    e.inner.db.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,user_config_json) VALUES(?,'fixture','alice',?)",&[json!(pid),json!(json!({"_external_chat_id":"room","_external_user_id":"user"}).to_string())]).unwrap();
    e.inner.db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,from_listener_id,delivered_ms) VALUES('fixture',?,'aged',1,'delivered','sender',?)",&[json!(pid),json!(now_ms()-86_400_000)]).unwrap();
    e.inner.db.execute("INSERT INTO hub_channel_delivery_msgs(plugin_id,principal_id,external_msg_id,signal_id,from_listener_id) VALUES('fixture',?,'chunk','aged','sender')",&[json!(pid)]).unwrap();
    e.inner.db.transaction(prune).unwrap();
    let mut frame = json!({"update_id":"quoted","external_chat_id":"room","external_user_id":"user","reply_to_external_msg_id":"chunk","text":"Reply after a day"});
    assert_eq!(e.inbound_reply("fixture", &frame).unwrap()["ok"], true);
    frame
        .as_object_mut()
        .unwrap()
        .remove("reply_to_external_msg_id");
    frame["update_id"] = json!("fallback");
    assert_eq!(e.inbound_reply("fixture", &frame).unwrap()["ok"], true);
    e.inner
        .db
        .execute(
            "UPDATE hub_channel_deliveries SET delivered_ms=?",
            &[json!(now_ms() - 604_800_001)],
        )
        .unwrap();
    e.inner.db.transaction(prune).unwrap();
    frame["update_id"] = json!("expired");
    assert!(e
        .inbound_reply("fixture", &frame)
        .unwrap_err()
        .starts_with("LISTENER_NOT_FOUND:"));
    assert!(e
        .inner
        .db
        .query("SELECT * FROM hub_channel_delivery_msgs", &[])
        .unwrap()
        .is_empty());
}

#[test]
fn reply_dedup_target_validation_and_ack_commit_together() {
    let dir = temp();
    let e = test_engine(&dir);
    put(&e, "sender", "none#bob", "workspace");
    let pid = "none#alice";
    let node = listener_id(pid, "human", "default");
    e.rpc(
        "human_inbox_update",
        &json!({"expected_control_revision":1,"scope":"self"}),
        Some(&principal("alice")),
    )
    .unwrap();
    send(&e, &node, "original");
    e.inner.db.transaction(|c|{c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,user_config_json) VALUES(?,'fixture','alice',?)",&[json!(pid),json!(json!({"_external_chat_id":"room","_external_user_id":"user"}).to_string())])?;c.execute("INSERT INTO hub_channel_delivery_msgs(plugin_id,principal_id,external_msg_id,signal_id,from_listener_id) VALUES('fixture',?,'chunk-1','original','work:sender')",&[json!(pid)])?;Ok(())}).unwrap();
    e.inner.db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,from_listener_id,delivered_ms) VALUES('fixture',?,'original',1,'delivered','work:sender',?)", &[json!(pid),json!(now_ms())]).unwrap();
    let frame = json!({"op":"inbound_reply","update_id":"u1","external_chat_id":"room","external_user_id":"user","reply_to_external_msg_id":"chunk-1","text":"Approved"});
    let events = e.subscribe(None).unwrap();
    e.inner.db.execute("CREATE TRIGGER fail_source_ack BEFORE UPDATE OF acked_at_ms ON hub_signals WHEN OLD.signal_id='original' BEGIN SELECT RAISE(ABORT,'source ack failure'); END",&[]).unwrap();
    assert!(e
        .inbound_reply("fixture", &frame)
        .unwrap_err()
        .contains("source ack failure"));
    assert_eq!(
        e.inner
            .db
            .query("SELECT signal_id,acked_at_ms FROM hub_signals", &[])
            .unwrap(),
        vec![json!({"signal_id":"original","acked_at_ms":null})]
    );
    assert!(e
        .inner
        .db
        .query(
            "SELECT update_id FROM hub_processed_replies WHERE update_id='u1'",
            &[]
        )
        .unwrap()
        .is_empty());
    assert!(matches!(events.try_recv(), Err(mpsc::TryRecvError::Empty)));
    e.inner
        .db
        .execute("DROP TRIGGER fail_source_ack", &[])
        .unwrap();
    assert_eq!(e.inbound_reply("fixture", &frame).unwrap()["ok"], true);
    assert_eq!(
        e.inbound_reply("fixture", &frame).unwrap()["duplicate"],
        true
    );
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT seq FROM hub_signals WHERE subject='cito.signal.sender'",
                &[]
            )
            .unwrap()
            .len(),
        1
    );
    assert!(!e
        .inner
        .db
        .query(
            "SELECT acked_at_ms FROM hub_signals WHERE signal_id='original'",
            &[]
        )
        .unwrap()[0]["acked_at_ms"]
        .is_null());
    let mut retained = frame.clone();
    retained["update_id"] = json!("retained");
    let update_target = |state: &str, age: i64| {
        e.rpc("kv_put",&json!({"key":"sender","data_b64":encoded(&json!({"principal_id":"none#bob","harness":"fixture","session_id":"s1","scope":"workspace","state":state,"updated_at_ms":now_ms()-age}))}),None).unwrap();
    };
    update_target("ACTIVE", 61_000);
    assert_eq!(e.inbound_reply("fixture", &retained).unwrap()["ok"], true);
    for (state, age, update) in [
        ("UNLISTENED", 0, "revoked"),
        ("ACTIVE", 1_800_001, "expired"),
    ] {
        update_target(state, age);
        let mut rejected = frame.clone();
        rejected["update_id"] = json!(update);
        assert!(e
            .inbound_reply("fixture", &rejected)
            .unwrap_err()
            .starts_with("LISTENER_NOT_FOUND"));
        assert!(e
            .inner
            .db
            .query(
                "SELECT 1 AS n FROM hub_processed_replies WHERE update_id=?",
                &[json!(update)]
            )
            .unwrap()
            .is_empty());
    }
    let mut invalid = frame.clone();
    invalid["update_id"] = json!("u2");
    e.inner
        .db
        .execute("DELETE FROM hub_kv WHERE key='sender'", &[])
        .unwrap();
    assert!(e
        .inbound_reply("fixture", &invalid)
        .unwrap_err()
        .starts_with("LISTENER_NOT_FOUND"));
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT * FROM hub_processed_replies WHERE update_id='u2'",
                &[]
            )
            .unwrap()
            .len(),
        0
    );
}
#[test]
fn non_destructive_upgrade_preserves_legacy_payloads() {
    let dir = temp();
    let db = Database::open(&dir.join("hub.sqlite")).unwrap();
    db.execute("CREATE TABLE hub_pairing_codes(code TEXT PRIMARY KEY,plugin_id TEXT NOT NULL,principal_id TEXT NOT NULL,owner_user TEXT NOT NULL,created_ms INTEGER NOT NULL,expires_ms INTEGER NOT NULL,consumed_ms INTEGER)",&[]).unwrap();
    db.execute(
        "INSERT INTO hub_pairing_codes VALUES('legacy','fixture','p','alice',1,9999999999999,NULL)",
        &[],
    )
    .unwrap();
    let raw = "{ \"signal_id\" : \"original\" }\n";
    db.execute("CREATE TABLE hub_signals(seq INTEGER PRIMARY KEY AUTOINCREMENT,subject TEXT NOT NULL,msg_id TEXT UNIQUE,created_ms INTEGER NOT NULL,data BLOB NOT NULL,acked INTEGER NOT NULL DEFAULT 0)",&[]).unwrap();
    db.execute("INSERT INTO hub_signals(subject,created_ms,data) VALUES('cito.signal.target',?,CAST(? AS BLOB))",&[json!(now_ms()),json!(raw)]).unwrap();
    db.execute("CREATE TABLE hub_channel_deliveries(plugin_id TEXT NOT NULL,principal_id TEXT NOT NULL,signal_id TEXT NOT NULL,seq INTEGER NOT NULL,status TEXT NOT NULL,external_msg_id TEXT,from_listener_id TEXT,from_user TEXT,from_title TEXT,delivered_ms INTEGER NOT NULL,PRIMARY KEY(plugin_id,principal_id,signal_id))",&[]).unwrap();
    db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,external_msg_id,from_listener_id,delivered_ms) VALUES('fixture','p','original',1,'delivered','42','sender',?)",&[json!(now_ms())]).unwrap();
    db.execute("CREATE INDEX idx_hub_channel_deliveries_ext_msg ON hub_channel_deliveries(plugin_id,principal_id,external_msg_id)", &[]).unwrap();
    drop(db);
    let e = test_engine(&dir);
    assert!(e
        .inner
        .db
        .query(
            "SELECT 1 FROM sqlite_master WHERE name='idx_hub_channel_deliveries_ext_msg'",
            &[]
        )
        .unwrap()
        .is_empty());
    e.inner.db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,from_listener_id,reservation_id,delivered_ms) VALUES('fixture','p','new',2,'in_flight','sender','token',?)", &[json!(now_ms())]).unwrap();
    e.delivery_result("fixture", &json!({"delivery_id":"token","ok":true,"external_msg_id":"primary","external_msg_ids":["chunk"]})).unwrap();
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT status,external_msg_id FROM hub_channel_deliveries WHERE signal_id='new'",
                &[]
            )
            .unwrap(),
        vec![json!({"status":"delivered","external_msg_id":null})]
    );
    let signals = e
        .inner
        .db
        .query(
            "SELECT signal_id,CAST(data AS TEXT) AS data FROM hub_signals",
            &[],
        )
        .unwrap();
    assert_eq!(signals[0]["signal_id"], "original");
    assert_eq!(signals[0]["data"], raw);
    let mappings = e
        .inner
        .db
        .query(
            "SELECT external_msg_id,signal_id FROM hub_channel_delivery_msgs ORDER BY external_msg_id",
            &[],
        )
        .unwrap();
    assert_eq!(
        mappings,
        vec![
            json!({"external_msg_id":"42","signal_id":"original"}),
            json!({"external_msg_id":"chunk","signal_id":"new"}),
            json!({"external_msg_id":"primary","signal_id":"new"})
        ]
    );
    assert_eq!(
        e.inner
            .db
            .query("SELECT code,instruction FROM hub_pairing_codes", &[])
            .unwrap()[0]["code"],
        "legacy"
    );
}
#[test]
fn accepted_http_connections_wait_for_fragmented_headers() {
    let dir = temp();
    let e = test_engine(&dir);
    let server = e
        .start_server(
            "127.0.0.1",
            0,
            HubAuth::new("none", None, None, None).unwrap(),
        )
        .unwrap();
    let mut client = TcpStream::connect(server.address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    thread::sleep(Duration::from_millis(40));
    client.write_all(b"GET /health HTTP/1.1\r\n").unwrap();
    thread::sleep(Duration::from_millis(40));
    write!(client, "Host: {}\r\n\r\n", server.address).unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    for (field, status) in [
        (format!("X: {}", "a".repeat(65_536)), 431),
        ("Content-Length: 8388609".to_owned(), 413),
    ] {
        let mut stream = TcpStream::connect(server.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(stream,"POST /v1/rpc HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n{field}\r\n\r\n",server.address).unwrap();
        let response = crate::plugins::read_http_response(&mut stream).unwrap();
        assert_eq!(response.status, status);
        assert_eq!(
            serde_json::from_str::<Value>(&response.body).unwrap()["error"]["code"],
            "INVALID_INPUT"
        );
    }
}
#[test]
fn http_browser_boundary_and_idle_sse_shutdown() {
    for bind in ["127.0.0.1", "0.0.0.0"] {
        let dir = temp();
        let engine = test_engine(&dir);
        engine.set_plugin_roots(&[]).unwrap();
        let server = engine
            .start_server(bind, 0, HubAuth::new("none", None, None, None).unwrap())
            .unwrap();
        let address = format!("127.0.0.1:{}", server.port());
        let base = format!("http://{address}");
        for (host, origin, media, status) in [
            (address.clone(), None, "application/json", 200),
            (
                address.clone(),
                Some(base.clone()),
                "application/json; charset=utf-8",
                200,
            ),
            (
                address.clone(),
                Some(format!("http://127.0.0.1:{}", server.port() + 1)),
                "application/json",
                403,
            ),
            (
                address.clone(),
                Some(format!("https://{address}")),
                "application/json",
                403,
            ),
            (
                format!("127.0.0.1:{}", server.port() + 1),
                None,
                "application/json",
                403,
            ),
            (address.clone(), None, "text/plain", 415),
            (
                format!("hub.example:{}", server.port()),
                None,
                "application/json",
                if bind == "0.0.0.0" { 200 } else { 403 },
            ),
        ] {
            let mut request = ureq::post(&format!("{base}/v1/rpc"))
                .set("Host", &host)
                .set("Content-Type", media);
            if let Some(origin) = origin {
                request = request.set("Origin", &origin);
            }
            let actual = match request.send_string(r#"{"op":"account_info"}"#) {
                Ok(response) => response.status(),
                Err(ureq::Error::Status(status, _)) => status,
                Err(error) => panic!("{error}"),
            };
            assert_eq!(actual, status, "bind={bind} host={host} media={media}");
        }
        // A bounded prefix of a rejected upload must not reset the response.
        let mut upload = TcpStream::connect(&address).unwrap();
        upload
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        upload
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let request = format!(
            "POST /v1/rpc HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            MAX_RPC_BYTES + 1,
            "x".repeat(64 * 1024),
        );
        upload.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        upload.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 413 "), "{response}");
        drop(upload);

        let mut clients = Vec::new();
        for _ in 0..3 {
            let mut client = TcpStream::connect(&address).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            write!(client, "GET /v1/events HTTP/1.1\r\nHost: {address}\r\n\r\n").unwrap();
            let mut reader = BufReader::new(client);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains("200"), "{line}");
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                assert!(!line.is_empty(), "Unexpected end of SSE response");
                if line == ":ok\n" {
                    break;
                }
            }
            clients.push(reader);
        }
        let start = std::time::Instant::now();
        server.shutdown();
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "{:?}",
            start.elapsed()
        );
        assert!(engine.rpc("account_info", &json!({}), None).is_ok());
        drop(clients);
    }
}
#[test]
fn http_wire_and_stoppable_shared_engine() {
    let dir = temp();
    let e = test_engine(&dir);
    let server = e
        .start_server(
            "127.0.0.1",
            0,
            HubAuth::new("token", Some("secret".into()), None, None).unwrap(),
        )
        .unwrap();
    let base = format!("http://{}", server.address);
    let health: Value = ureq::get(&format!("{base}/health"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(health["pid"], std::process::id());
    assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
    assert!(ureq::post(&format!("{base}/v1/rpc"))
        .send_json(json!({"op":"account_info"}))
        .is_err());
    for params in [
        json!({"op":"stream_info","stream":SIGNALS}),
        json!({"op":"stream_info","params":{"stream":SIGNALS}}),
    ] {
        let response: Value = ureq::post(&format!("{base}/v1/rpc"))
            .set("Authorization", "Bearer secret")
            .send_json(params)
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(response["result"]["config"]["name"], SIGNALS);
    }
    assert!(server.wait_for_stop(&AtomicBool::new(true)).is_ok());
    server.shutdown();
    assert_eq!(
        server.wait_for_stop(&AtomicBool::new(false)).unwrap_err(),
        "Hub listener stopped unexpectedly"
    );
    assert!(e.rpc("account_info", &json!({}), None).is_ok());
}
#[test]
fn verified_oidc_identity_keeps_legacy_private_data_and_bindings() {
    for issuer in ["https://id.example.com", "http://127.0.0.1:9876/auth"] {
        let dir = temp();
        let root = fixture(&dir);
        let e = TestEngine(HubEngine::open_with_auth_mode(&dir, "oidc").unwrap());
        e.set_plugin_roots(&[root]).unwrap();
        let legacy = format!("{issuer}#subject");
        let caller = json!({"principal_id":format!("oidc:{issuer}|subject"),"sub":"subject","owner_user":"alice","oidc":true});
        let seed_pair = |code: &str, plugin: &str, owner: &str| {
            e.inner.db.execute("INSERT INTO hub_pairing_codes(code,plugin_id,principal_id,owner_user,created_ms,expires_ms) VALUES(?,?,?,'alice',?,?)", &[json!(code),json!(plugin),json!(owner),json!(now_ms()),json!(now_ms()+60_000)]).unwrap();
        };
        let redeem = |code: &str| {
            e.redeem("fixture", &json!({"code":code,"is_private_chat":true,"external_chat_id":"room","external_user_id":"alice","user_config_patch":{"address":"paired-room"}}))
        };
        seed_pair("pending", "fixture", &legacy);
        let list = || {
            e.rpc("human_channels_list", &json!({}), Some(&caller))
                .unwrap()
        };
        assert_eq!(list()["channels"][0]["pairing_code"], "pending");
        let cancelled = e
            .rpc(
                "human_channel_pair_cancel",
                &json!({"plugin_id":"fixture"}),
                Some(&caller),
            )
            .unwrap();
        assert_eq!(cancelled["committed"], true);
        assert!(redeem("pending")
            .unwrap_err()
            .contains("consumed pairing code"));
        assert_eq!(list()["channels"][0]["pairing_pending"], false);
        let mut other = e.plugin("fixture").unwrap();
        other.id = "second".into();
        e.inner.plugins.lock().unwrap().push(other);
        e.inner.db.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,user_config_json) VALUES(?,'second','alice',?)", &[caller["principal_id"].clone(), json!(json!({"address":"second-room"}).to_string())]).unwrap();
        put(&e, "legacy-node", &legacy, "self");
        send(&e, "legacy-node", "s1");
        e.inner.db.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,enabled,user_config_json) VALUES(?,'fixture','alice',0,?)",&[json!(legacy),json!(json!({"address":"legacy-room"}).to_string())]).unwrap();
        assert!(e
            .rpc(
                "stream_get_message",
                &json!({"stream":SIGNALS,"seq":1}),
                Some(&caller)
            )
            .is_ok());
        assert_eq!(
            e.rpc(
                "signal_claim_dispatch",
                &json!({"node_id":"legacy-node","signal_ids":["s1"],"channel_id":"a"}),
                Some(&caller)
            )
            .unwrap()["claimed"],
            true
        );
        let inventory = list();
        assert_eq!(
            inventory["channels"][0]["fields"][1]["value"],
            "legacy-room"
        );
        assert_eq!(
            inventory["channels"][1]["fields"][1]["value"],
            "second-room"
        );
        e.rpc(
            "human_channels_configure",
            &json!({"plugin_id":"fixture","user_config":{"address":"updated-room"}}),
            Some(&caller),
        )
        .unwrap();
        assert_eq!(list()["channels"][0]["fields"][1]["value"], "updated-room");
        e.rpc(
            "human_channels_test",
            &json!({"plugin_id":"fixture"}),
            Some(&caller),
        )
        .unwrap();
        let frames =
            std::fs::read_to_string(e.plugin("fixture").unwrap().plugin_dir.join("frames.jsonl"))
                .unwrap();
        assert!(frames
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .any(|f| f["op"] == "test" && f["user_config"]["address"] == "updated-room"));
        // Replacing the legacy destination retires only its own reply mapping.
        for (plugin, pid) in [
            ("fixture", legacy.as_str()),
            ("second", s(&caller, "principal_id")),
        ] {
            e.inner.db.execute("INSERT INTO hub_channel_delivery_msgs(plugin_id,principal_id,external_msg_id,signal_id,from_listener_id) VALUES(?,?,'message','s1','sender')", &[json!(plugin),json!(pid)]).unwrap();
        }
        seed_pair("replacement", "fixture", s(&caller, "principal_id"));
        assert_eq!(redeem("replacement").unwrap()["ok"], true);
        assert_eq!(
            e.inner
                .db
                .query(
                    "SELECT plugin_id,principal_id FROM hub_channel_delivery_msgs",
                    &[]
                )
                .unwrap(),
            vec![json!({"plugin_id":"second","principal_id":caller["principal_id"]})]
        );
        let inventory = list();
        assert_eq!(
            inventory["channels"][0]["fields"][1]["value"],
            "paired-room"
        );
        assert_eq!(
            inventory["channels"][1]["fields"][1]["value"],
            "second-room"
        );
        assert_eq!(
            e.inner
                .db
                .query(
                    "SELECT plugin_id,principal_id FROM hub_human_channels ORDER BY plugin_id",
                    &[]
                )
                .unwrap(),
            vec![
                json!({"plugin_id":"fixture","principal_id":legacy}),
                json!({"plugin_id":"second","principal_id":caller["principal_id"]})
            ]
        );
    }
}
#[test]
fn kv_snapshot_reconciles_expired_humans_after_collecting_each_bounded_page() {
    for limit in [2, 1024] {
        let dir = temp();
        let e = test_engine(&dir);
        for index in 0..7 {
            let id = format!("human-{index}");
            let reg = json!({"listener_id":id,"harness":"human","principal_id":"none#alice","session_id":"default","scope":"workspace",
                "state":"ACTIVE","about":"x".repeat(600_000),
                "channels":{"device:human":{"host_id":"device","state":"ACTIVE",
                    "updated_at_ms":1,"binding_generation":3}}});
            // Seed expired, unreconciled storage directly so snapshot must write.
            e.inner.db.execute(
                "INSERT INTO hub_kv(key,revision,created_ms,value,principal_id,scope) VALUES(?,1,1,CAST(? AS BLOB),'none#alice','workspace')",
                &[json!(id), json!(reg.to_string())],
            ).unwrap();
        }
        let events = e.subscribe(None).unwrap();
        let mut cursor = Value::Null;
        let mut keys = Vec::new();
        let mut pages = 0;
        loop {
            let page = e
                .rpc(
                    "kv_snapshot",
                    &json!({"after_key":cursor,"limit":limit}),
                    None,
                )
                .unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= 2 * 1024 * 1024);
            let entries = page["entries"].as_array().unwrap();
            assert!(!entries.is_empty() && entries.len() <= limit as usize);
            for entry in entries {
                let reg: Value = serde_json::from_slice(
                    &B64.decode(entry["value_b64"].as_str().unwrap()).unwrap(),
                )
                .unwrap();
                assert_eq!(reg["state"], "UNLISTENED");
                assert_eq!(reg["channels"], json!({}));
                assert_eq!(reg["device_generation_fences"]["device:human"], 3);
                assert_eq!(reg["human_control_revision"], 1);
                let stored = e.rpc("kv_get", &json!({"key":entry["key"]}), None).unwrap();
                assert_eq!(stored["revision"], entry["revision"]);
                assert_eq!(stored["value_b64"], entry["value_b64"]);
                keys.push(entry["key"].clone());
            }
            pages += 1;
            cursor = page["next_cursor"].clone();
            if cursor.is_null() {
                break;
            }
            assert_eq!(Some(&cursor), keys.last());
            assert!(pages < 7);
        }
        assert!(pages > 1);
        let committed: Vec<_> = events
            .try_iter()
            .filter(|event| event["type"] == "kv")
            .map(|event| event["key"].clone())
            .collect();
        assert_eq!(committed, keys);
        assert_eq!(
            keys,
            (0..7)
                .map(|i| json!(format!("human-{i}")))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn snapshot_reserves_the_last_key_before_and_after_reconciliation() {
    const BUDGET: usize = 2 * 1024 * 1024;
    for shrinks in [true, false] {
        let dir = temp();
        let e = test_engine(&dir);
        let keys = ["a".repeat(1024), "b".repeat(1024), "z".into()];
        let mut first = json!({"harness":"human","principal_id":"none#alice","session_id":"default","about":"a".repeat(4096)});
        if shrinks {
            first["channels"] = json!({"old:human":{"state":"ACTIVE","updated_at_ms":1,"display_name":"x".repeat(2048)}});
        }
        let mut second =
            json!({"harness":"human","principal_id":"none#bob","session_id":"default","about":""});
        e.inner.db.transaction(|c| {
            for (key, value) in keys.iter().zip([&first, &second, &json!({"about":"z".repeat(4096)})]) {
                c.execute("INSERT INTO hub_kv(key,revision,created_ms,value,principal_id) VALUES(?,1,1,CAST(? AS BLOB),?)", &[json!(key),json!(value.to_string()),value["principal_id"].clone()])?;
            }
            let mut used = 128;
            for key in &keys[..2] {
                used += kv_wire(&kv_row(c,key)?.unwrap())?.to_string().len() + 1;
            }
            // Shrinking: including row two would fit only after the SELECT closes.
            // Growing: both fit the SELECT, then the two Human repairs expand them.
            let slack = if shrinks { 512 } else { keys[1].len()+10 };
            second["about"] = json!("b".repeat((BUDGET-used-slack)/4*3));
            c.execute("UPDATE hub_kv SET value=CAST(? AS BLOB) WHERE key=?", &[json!(second.to_string()),json!(keys[1])])?;
            Ok(())
        }).unwrap();
        let mut cursor = Value::Null;
        let mut seen = Vec::new();
        while seen.len() < keys.len() {
            let page = e
                .rpc("kv_snapshot", &json!({"after_key":cursor}), None)
                .unwrap();
            assert!(page.to_string().len() <= BUDGET);
            let entries = page["entries"].as_array().unwrap();
            assert!(!entries.is_empty());
            if seen.is_empty() {
                assert_eq!(page["next_cursor"], keys[0], "shrinks={shrinks}");
            } else if seen.len() == 1 {
                assert_eq!(entries[0]["key"], keys[1]);
                assert!(s(&entries[0], "value_b64").len() > BUDGET - 20000);
            }
            seen.extend(entries.iter().map(|entry| s(entry, "key").to_owned()));
            cursor = page["next_cursor"].clone();
            if cursor.is_null() {
                break;
            }
            assert_eq!(cursor.as_str(), seen.last().map(String::as_str));
        }
        assert_eq!(seen, keys);
        assert!(cursor.is_null());
    }
}

#[test]
fn binary_payloads_survive_kv_stream_consumers_and_restart() {
    let dir = temp();
    let e = test_engine(&dir);
    let payloads = [
        Vec::new(),
        b"message".to_vec(),
        b"\xff\x00\xfe'); DROP TABLE hub_kv; --".to_vec(),
        b"{ \"message\" : \"hello\" }\n".to_vec(),
    ];
    for (i, raw) in payloads.iter().enumerate() {
        let key = format!("blob-{i}");
        let b64 = B64.encode(raw);
        let created = e
            .rpc("kv_create", &json!({"key":key,"data_b64":b64}), None)
            .unwrap();
        e.rpc(
            "kv_update",
            &json!({"key":key,"revision":created["revision"],"data_b64":b64}),
            None,
        )
        .unwrap();
        assert_eq!(
            e.rpc("kv_get", &json!({"key":key}), None).unwrap()["value_b64"],
            b64
        );
        let args = json!({"subject":"cito.signal.binary","data_b64":b64,"opts":{"msgID":format!("binary-{i}")}});
        let published = e.rpc("stream_publish", &args, None).unwrap();
        let duplicate = e.rpc("stream_publish", &args, None).unwrap();
        assert_eq!(duplicate["seq"], published["seq"]);
        assert_eq!(duplicate["duplicate"], true);
        assert_eq!(
            e.rpc(
                "stream_get_message",
                &json!({"stream":SIGNALS,"seq":published["seq"]}),
                None
            )
            .unwrap()["data_b64"],
            b64
        );
    }
    let snapshot = e.rpc("kv_snapshot", &json!({}), None).unwrap();
    for (i, entry) in snapshot["entries"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            B64.decode(entry["value_b64"].as_str().unwrap()).unwrap(),
            payloads[i]
        );
    }
    let consumer=e.rpc("consumer_add",&json!({"config":{"durable_name":"binary-consumer","filter_subject":"cito.signal.binary"}}),None).unwrap();
    for (i, item) in consumer["backlog"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            B64.decode(item["data_b64"].as_str().unwrap()).unwrap(),
            payloads[i]
        );
    }
    assert_eq!(
        consumer["backlog"].as_array().unwrap().len(),
        payloads.len()
    );
    e.shutdown();
    let e = test_engine(&dir);
    for (i, raw) in payloads.iter().enumerate() {
        assert_eq!(
            e.rpc("kv_get", &json!({"key":format!("blob-{i}")}), None)
                .unwrap()["value_b64"],
            B64.encode(raw)
        );
        assert_eq!(
            e.rpc(
                "stream_get_message",
                &json!({"stream":SIGNALS,"seq":i+1}),
                None
            )
            .unwrap()["data_b64"],
            B64.encode(raw)
        );
    }
}
#[test]
fn raw_nc_variants_preserve_ack_bytes_and_publish_signals_durably() {
    let dir = temp();
    let e = test_engine(&dir);
    let events = e.subscribe(None).unwrap();
    for (args, expected) in [
        (json!({"data":"raw"}), b"raw".to_vec()),
        (json!({"data":[0,255,1]}), vec![0, 255, 1]),
        (
            json!({"data":{"type":"Buffer","data":[254,0]}}),
            vec![254, 0],
        ),
        (json!({"data_b64":""}), Vec::new()),
        (
            json!({"data":{"signal_ids":["s1"]}}),
            b"{\"signal_ids\":[\"s1\"]}".to_vec(),
        ),
    ] {
        let mut args = args;
        args["subject"] = json!("cito.ack.target");
        e.rpc("nc_publish", &args, None).unwrap();
        let event = events.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(event["type"], "nc");
        assert_eq!(
            B64.decode(event["data_b64"].as_str().unwrap()).unwrap(),
            expected
        );
    }
    e.rpc(
        "nc_publish",
        &json!({"subject":"cito.signal.target","data":[255,0,254]}),
        None,
    )
    .unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_millis(100)).unwrap()["type"],
        "signal"
    );
    assert_eq!(
        e.rpc(
            "stream_get_message",
            &json!({"stream":SIGNALS,"seq":1}),
            None
        )
        .unwrap()["data_b64"],
        B64.encode([255, 0, 254])
    );
}
#[test]
fn opaque_oidc_payloads_keep_ownership_and_validate_available_metadata() {
    let dir = temp();
    let e = test_engine(&dir);
    let a =
        json!({"principal_id":"oidc:https://id.example.com|a","owner_user":"alice","oidc":true});
    let b = json!({"principal_id":"oidc:https://id.example.com|b","owner_user":"bob","oidc":true});
    e.rpc(
        "kv_create",
        &json!({"key":"opaque","data_b64":B64.encode([255,0])}),
        Some(&a),
    )
    .unwrap();
    assert!(e
        .rpc("kv_put", &json!({"key":"opaque","data_b64":""}), Some(&b))
        .is_err());
    e.rpc(
        "kv_put",
        &json!({"key":"legacy-unowned","data_b64":""}),
        None,
    )
    .unwrap();
    e.rpc(
        "kv_put",
        &json!({"key":"legacy-unowned","data_b64":""}),
        Some(&a),
    )
    .unwrap();
    assert!(e
        .rpc(
            "kv_put",
            &json!({"key":"legacy-unowned","data_b64":""}),
            Some(&b)
        )
        .is_err());
    put(&e, "private", s(&a, "principal_id"), "self");
    put(&e, "workspace", s(&a, "principal_id"), "workspace");
    let raw = json!({"subject":"cito.signal.private","data_b64":B64.encode([255,0])});
    assert!(e.rpc("stream_publish", &raw, Some(&b)).is_err());
    assert!(e.rpc("stream_publish", &raw, Some(&a)).is_ok());
    assert!(e
        .rpc(
            "nc_publish",
            &json!({"subject":"cito.signal.workspace","data":"plain text"}),
            Some(&b)
        )
        .is_ok());
    for body in [
        encoded(&json!({"from":null})),
        encoded(&json!({"from":{"principal_id":s(&a,"principal_id")}})),
        B64.encode(
            b"{\"from\":{\"principal_id\":\"oidc:https://id.example.com|a\"},\"message\":\"\xff\"}",
        ),
    ] {
        assert!(e
            .rpc(
                "stream_publish",
                &json!({"subject":"cito.signal.workspace","data_b64":body}),
                Some(&b)
            )
            .unwrap_err()
            .starts_with("PERMISSIONS_VIOLATION"));
    }
}
#[test]
fn rpc_preserves_payload_bytes() {
    let dir = temp();
    let e = test_engine(&dir);
    let registration="{ \"principal_id\" : \"none#alice\", \"scope\" : \"workspace\", \"harness\":\"fixture\", \"session_id\":\"s1\" }\n";
    let b64 = B64.encode(registration);
    e.rpc("kv_put", &json!({"key":"target","data_b64":b64}), None)
        .unwrap();
    assert_eq!(
        e.rpc("kv_get", &json!({"key":"target"}), None).unwrap()["value_b64"],
        b64
    );
    let signal = "{ \"signal_id\": \"s1\", \"message\" : \"hello\" }\n";
    let b64 = B64.encode(signal);
    e.rpc(
        "js_publish",
        &json!({"subject":"cito.signal.target","data_b64":b64}),
        None,
    )
    .unwrap();
    assert_eq!(
        e.rpc(
            "stream_get_message",
            &json!({"stream":SIGNALS,"seq":1}),
            None
        )
        .unwrap()["data_b64"],
        b64
    );
}
#[test]
fn concurrent_claim_has_one_winner() {
    let dir = temp();
    let e = test_engine(&dir);
    send(&e, "target", "s1");
    send(&e, "target", "s2");
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut handles = Vec::new();
    for channel in ["a", "b"] {
        let e = e.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            e.rpc(
                "signal_claim_dispatch",
                &json!({"node_id":"target","signal_ids":["s1","s2"],"channel_id":channel}),
                None,
            )
            .unwrap()["claimed"]
                == true
        }));
    }
    barrier.wait();
    let wins = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|b| *b)
        .count();
    assert_eq!(wins, 1);
}
#[test]
fn pending_request_fails_on_child_exit() {
    let root = temp();
    let dir = root.join("fixture");
    std::fs::create_dir_all(&dir).unwrap();
    let plugin = hub_plugins::test_descriptor(
        &dir,
        "fixture",
        vec![
            "python3".into(),
            "-c".into(),
            "import sys; sys.stdin.readline(); sys.exit(0)".into(),
        ],
        json!({}),
    );
    let p = ChannelProcess::spawn(&plugin, Arc::new(|_| None)).unwrap();
    let started = std::time::Instant::now();
    let result = p.request(json!({"op":"test"}));
    assert!(started.elapsed() < Duration::from_secs(1));
    match result {
        Ok(v) => assert_eq!(v["error"], "Plugin subprocess exited"),
        Err(e) => assert!(e.contains("exited")),
    }
    p.shutdown();
}
#[test]
fn delivered_unread_history_does_not_starve_later_notifications() {
    let dir = temp();
    let root = fixture(&dir);
    let e = test_engine(&dir);
    e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    let pid = "none#alice";
    let node = listener_id(pid, "human", "default");
    e.rpc("human_channels_configure",&json!({"plugin_id":"fixture","enabled":true,"hub_config":{"credential":"test-only"},"user_config":{"address":"room"}}),Some(&json!({"principal_id":pid,"owner_user":"alice"}))).unwrap();
    e.inner.db.transaction(|c|{for i in 0..201{let id=format!("message-{i}");c.execute("INSERT INTO hub_signals(subject,signal_id,created_ms,data) VALUES(?,?,?,CAST(? AS BLOB))",&[json!(format!("cito.signal.{node}")),json!(id),json!(now_ms()),json!(json!({"signal_id":id,"message":"hello","from":{"listener_id":"sender"}}).to_string())])?;if i<200{c.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,delivered_ms) VALUES('fixture',?,?,?,'delivered',?)",&[json!(pid),json!(id),json!(i+1),json!(now_ms())])?;}}Ok(())}).unwrap();
    wait_for(|| {
        e.inner.db.query("SELECT 1 AS n FROM hub_channel_deliveries WHERE signal_id='message-200' AND status='delivered'",&[]).unwrap().len()==1
    });
    e.shutdown();
    let frames = std::fs::read_to_string(root.join("fixture/frames.jsonl")).unwrap();
    assert_eq!(
        frames
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|f| f["op"] == "deliver")
            .count(),
        1
    );
}
#[test]
fn reservations_expire_without_delivery_progress_and_quarantine_after_exit() {
    let dir = temp();
    let root = fixture(&dir);
    let e = test_engine(&dir);
    e.set_plugin_roots(std::slice::from_ref(&root)).unwrap();
    e.rpc("human_channels_configure",&json!({"plugin_id":"fixture","enabled":true,"hub_config":{"credential":"test-only"},"user_config":{"address":"room"}}),Some(&json!({"principal_id":"none#alice","owner_user":"alice"}))).unwrap();
    let node = listener_id("none#alice", "human", "default");
    send(&e, &node, "concurrent");
    let plugin = e.plugin("fixture").unwrap();
    let process = {
        let _lifecycle = e.inner.lifecycle.lock().unwrap();
        e.ensure_process(&plugin).unwrap()
    };
    let barrier = Arc::new(std::sync::Barrier::new(5));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let (e, p, process, b) = (e.clone(), plugin.clone(), process.clone(), barrier.clone());
        handles.push(thread::spawn(move || {
            b.wait();
            e.replay(&p, &process).unwrap();
        }));
    }
    barrier.wait();
    for h in handles {
        h.join().unwrap();
    }
    wait_for(|| {
        e.inner.db.query("SELECT * FROM hub_channel_deliveries WHERE signal_id='concurrent' AND status='delivered'",&[]).unwrap().len()==1
    });
    // Control reservation time explicitly; only genuine progress may renew it.
    e.pause_worker();
    let old = now_ms() - 180_001;
    e.inner.db.execute("UPDATE hub_channel_deliveries SET status='in_flight',delivered_ms=?,reservation_id='multipart' WHERE signal_id='concurrent'", &[json!(old)]).unwrap();
    e.delivery_result(
        "fixture",
        &json!({"delivery_id":"stale","progress":true,"external_msg_id":"ignored"}),
    )
    .unwrap();
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT delivered_ms FROM hub_channel_deliveries WHERE signal_id='concurrent'",
                &[]
            )
            .unwrap()[0]["delivered_ms"],
        old
    );
    for part in ["part-1", "part-2"] {
        e.delivery_result(
            "fixture",
            &json!({"delivery_id":"multipart","progress":true,"external_msg_id":part}),
        )
        .unwrap();
        e.replay(&plugin, &process).unwrap();
        let row = e
            .inner
            .db
            .query(
                "SELECT * FROM hub_channel_deliveries WHERE signal_id='concurrent'",
                &[],
            )
            .unwrap()
            .remove(0);
        assert_eq!(row["reservation_id"], "multipart");
        assert_eq!(row["status"], "in_flight");
        assert!(n(&row, "delivered_ms") > old);
    }
    assert_eq!(e.inner.db.query("SELECT * FROM hub_channel_delivery_msgs WHERE external_msg_id IN ('part-1','part-2')", &[]).unwrap().len(), 2);
    e.inner.db.execute("UPDATE hub_channel_deliveries SET status='in_flight',delivered_ms=?,reservation_id='abandoned' WHERE signal_id='concurrent'",&[json!(now_ms()-180_001)]).unwrap();
    e.replay(&plugin, &process).unwrap();
    let row = e
        .inner
        .db
        .query(
            "SELECT * FROM hub_channel_deliveries WHERE signal_id='concurrent'",
            &[],
        )
        .unwrap()
        .remove(0);
    assert_ne!(row["reservation_id"], "abandoned");
    assert!(n(&row, "delivered_ms") > now_ms() - 1000);
    wait_for(|| {
        e.inner
            .db
            .query(
                "SELECT status FROM hub_channel_deliveries WHERE signal_id=\'concurrent\'",
                &[],
            )
            .unwrap()[0]["status"]
            == "delivered"
    });
    e.inner.db.execute("UPDATE hub_channel_deliveries SET status=\'in_flight\',reservation_id=\'crashed\' WHERE signal_id=\'concurrent\'",&[]).unwrap();
    // Stop periodic supervision so Send Test is deterministically the first
    // replacement caller after the crash, rather than racing the supervisor.
    e.pause_worker();
    process.shutdown();
    for signal in ["unsent-first", "unsent-next"] {
        send(&e, &node, signal);
    }
    assert!(e.replay(&plugin, &process).is_err());
    let unsent=e.inner.db.query("SELECT status,attempts,reservation_id FROM hub_channel_deliveries WHERE signal_id LIKE 'unsent-%'",&[]).unwrap();
    assert_eq!(
        unsent,
        vec![json!({"status":"failed","attempts":0,"reservation_id":null}); 2]
    );
    assert_eq!(
        e.rpc(
            "human_channels_test",
            &json!({"plugin_id":"fixture"}),
            Some(&json!({"principal_id":"none#alice","owner_user":"alice"}))
        )
        .unwrap()["ok"],
        true
    );
    let replacement = e.inner.processes.lock().unwrap()["fixture"].clone();
    e.replay(&plugin, &replacement).unwrap();
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT status FROM hub_channel_deliveries WHERE signal_id='concurrent'",
                &[]
            )
            .unwrap()[0]["status"],
        "effect_unknown"
    );
    e.shutdown();
    let frames = std::fs::read_to_string(root.join("fixture/frames.jsonl")).unwrap();
    assert_eq!(
        frames
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|f| f["op"] == "deliver" && f["signal"]["signal_id"] == "concurrent")
            .count(),
        2
    );
}
#[test]
fn replay_quarantines_only_the_frame_partially_written_to_the_pipe() {
    let dir = temp();
    let e = test_engine(&dir);
    e.pause_worker();
    let plugin_dir = dir.join("fixture");
    std::fs::create_dir(&plugin_dir).unwrap();
    let script = plugin_dir.join("prefix.py");
    std::fs::write(
        &script,
        "import os, pathlib\npathlib.Path('received').write_bytes(os.read(0, 1))\n",
    )
    .unwrap();
    let plugin = hub_plugins::test_descriptor(
        &plugin_dir,
        "fixture",
        vec!["python3".into(), script.to_str().unwrap().into()],
        json!({}),
    );
    let process = ChannelProcess::spawn(&plugin, Arc::new(|_| None)).unwrap();
    let pid = "none#alice";
    let node = listener_id(pid, "human", "default");
    // The frame is larger than the pipe, so closing after one byte prevents a full write.
    e.inner.db.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,user_config_json) VALUES(?,'fixture',?)", &[json!(pid),json!(json!({"address":"x".repeat(1024*1024)}).to_string())]).unwrap();
    for signal in ["partial", "unsent"] {
        send(&e, &node, signal);
    }
    e.inner.db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,delivered_ms,attempts) VALUES('fixture',?,'unsent',2,'failed',0,2)", &[json!(pid)]).unwrap();
    assert!(e.replay(&plugin, &process).is_err());
    process.shutdown();
    assert_eq!(std::fs::read(plugin_dir.join("received")).unwrap(), b"{");
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT signal_id,status,attempts FROM hub_channel_deliveries ORDER BY seq",
                &[]
            )
            .unwrap(),
        vec![
            json!({"signal_id":"partial","status":"effect_unknown","attempts":1}),
            json!({"signal_id":"unsent","status":"failed","attempts":2})
        ]
    );
    e.inner
        .db
        .execute("UPDATE hub_channel_deliveries SET next_retry_ms=0", &[])
        .unwrap();
    let retry = e.reserve_deliveries(&plugin).unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0]["signal"]["signal_id"], "unsent");
}

#[test]
fn personal_pause_and_delivery_retry_ledger_are_durable_without_io() {
    let dir = temp();
    let e = test_engine(&dir);
    e.set_plugin_roots(&[]).unwrap();
    let plugin = hub_plugins::test_descriptor(
        &dir,
        "fixture",
        vec![],
        json!({"fields":[{"key":"credential","scope":"hub","type":"secret"}]}),
    );
    let binding = json!({"principal_id":"none#alice","owner_user":"alice","plugin_id":"fixture"});
    e.inner.db.transaction(|c| {
        c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user) VALUES('none#alice','fixture','alice')",&[])?;
        attach(c,&binding,true,"workspace",None)
    }).unwrap();
    let node = listener_id("none#alice", "human", "default");
    send(&e, &node, "retry");
    e.inner
        .db
        .transaction(|c| {
            let mut reg = parse(&kv_row(c, &node)?.unwrap(), "value");
            reg["human_paused"] = json!(true);
            c.execute(
                "UPDATE hub_kv SET value=CAST(? AS BLOB) WHERE key=?",
                &[json!(reg.to_string()), json!(node)],
            )?;
            attach(c, &binding, true, "workspace", None)?;
            assert_eq!(
                parse(&kv_row(c, &node)?.unwrap(), "value")["state"],
                "UNLISTENED"
            );
            Ok(())
        })
        .unwrap();
    assert!(e.reserve_deliveries(&plugin).unwrap().is_empty());
    assert!(e
        .inner
        .db
        .query("SELECT acked_at_ms FROM hub_signals", &[])
        .unwrap()[0]["acked_at_ms"]
        .is_null());
    e.inner
        .db
        .transaction(|c| {
            let mut reg = parse(&kv_row(c, &node)?.unwrap(), "value");
            reg["human_paused"] = json!(false);
            super::super::lease::reconcile_registration_state(&mut reg, now_ms() as u64);
            c.execute(
                "UPDATE hub_kv SET value=CAST(? AS BLOB) WHERE key=?",
                &[json!(reg.to_string()), json!(node)],
            )?;
            Ok(())
        })
        .unwrap();
    for attempts in 0..=5 {
        let sends = e.reserve_deliveries(&plugin).unwrap();
        assert_eq!(sends.len(), 1);
        if attempts > 0 {
            assert_eq!(sends[0]["already_sent_ids"], json!(["101"]));
        }
        let token = &sends[0]["delivery_id"];
        e.delivery_result(
            "fixture",
            &json!({"delivery_id":token,"progress":true,"external_msg_ids":["101"]}),
        )
        .unwrap();
        e.delivery_result("fixture",&json!({"delivery_id":token,"ok":false,"retryable":true,"external_msg_ids":["101"],"error":"busy"})).unwrap();
        let row = e
            .inner
            .db
            .query("SELECT * FROM hub_channel_deliveries", &[])
            .unwrap()
            .remove(0);
        assert_eq!(row["attempts"], attempts + 1);
        if attempts < 5 {
            assert_eq!(row["status"], "failed");
            assert_eq!(
                delivery_transition(&json!({"ok":false}), attempts, 1_000),
                ("failed", 1_000 + (2_000_i64 << attempts))
            );
            assert!(n(&row, "next_retry_ms") > 0);
        } else {
            assert_eq!(row["status"], "failed_permanent");
        }
        assert!(e.reserve_deliveries(&plugin).unwrap().is_empty());
        e.inner
            .db
            .execute("UPDATE hub_channel_deliveries SET next_retry_ms=0", &[])
            .unwrap();
    }
    assert_eq!(
        delivery_transition(&json!({"ok":false,"retryable":false}), 0, 1),
        ("failed_permanent", 0)
    );
    assert_eq!(
        delivery_transition(&json!({"effect_unknown":true}), 0, 1),
        ("effect_unknown", 0)
    );
    assert_eq!(
        delivery_transition(&json!({"ok":true}), 5, 1),
        ("delivered", 0)
    );
    e.inner
        .db
        .execute(
            "UPDATE hub_channel_deliveries SET status='in_flight',reservation_id='uncertain'",
            &[],
        )
        .unwrap();
    e.delivery_result("fixture",&json!({"delivery_id":"uncertain","effect_unknown":true,"external_msg_ids":["101","102"],"error":"Response lost"})).unwrap();
    assert!(e.reserve_deliveries(&plugin).unwrap().is_empty());
    assert_eq!(
        e.inner
            .db
            .query("SELECT * FROM hub_channel_delivery_msgs", &[])
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        e.inner
            .db
            .query("SELECT status,last_error FROM hub_channel_deliveries", &[])
            .unwrap()[0],
        json!({"status":"effect_unknown","last_error":"Response lost"})
    );
    // Keep transport unconfigured while exercising configuration-only ledger resets.
    // The public configure path also performs a synchronous channel tick.
    e.pause_worker();
    let mut plugin = plugin.clone();
    plugin
        .fields
        .push(json!({"key":"transport","scope":"hub","type":"secret","required":true}));
    send(&e, &node, "after-binding");
    e.inner.plugins.lock().unwrap().push(plugin);
    e.human_rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","hub_config":{"credential":"new"}}),
        Some(&json!({"principal_id":"none#alice","owner_user":"alice"})),
    )
    .unwrap();
    assert_eq!(
        e.inner
            .db
            .query("SELECT status FROM hub_channel_deliveries", &[])
            .unwrap()[0]["status"],
        "effect_unknown"
    );
    e.inner.db.execute("UPDATE hub_channel_deliveries SET status='failed_permanent',attempts=6,next_retry_ms=9000",&[]).unwrap();
    e.human_rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","user_config":{}}),
        Some(&json!({"principal_id":"none#alice","owner_user":"alice"})),
    )
    .unwrap();
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT status,attempts,next_retry_ms FROM hub_channel_deliveries",
                &[]
            )
            .unwrap()[0],
        json!({"status":"failed","attempts":0,"next_retry_ms":0})
    );
    assert_eq!(
        e.inner
            .db
            .query("SELECT * FROM hub_channel_delivery_msgs", &[])
            .unwrap()
            .len(),
        2
    );
    let old_boundary = e
        .inner
        .db
        .query("SELECT last_delivered_seq FROM hub_human_channels", &[])
        .unwrap()[0]["last_delivered_seq"]
        .clone();
    e.inner.db.execute("UPDATE hub_channel_deliveries SET status='failed_permanent',attempts=6,next_retry_ms=9000,last_error='rejected'",&[]).unwrap();
    e.inner.db.execute("INSERT INTO hub_pairing_codes(code,plugin_id,principal_id,owner_user,created_ms,expires_ms) VALUES('cito-repair','fixture','none#alice','alice',?,?)",&[json!(now_ms()),json!(now_ms()+60_000)]).unwrap();
    assert_eq!(e.redeem("fixture",&json!({"code":"cito-repair","is_private_chat":true,"external_chat_id":"room","external_user_id":"alice","user_config_patch":{}})).unwrap()["ok"],true);
    assert_eq!(
        e.inner
            .db
            .query("SELECT last_delivered_seq FROM hub_human_channels", &[])
            .unwrap()[0]["last_delivered_seq"],
        old_boundary
    );
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT status,attempts,next_retry_ms,last_error FROM hub_channel_deliveries",
                &[]
            )
            .unwrap()[0],
        json!({"status":"failed","attempts":0,"next_retry_ms":0,"last_error":null})
    );
    assert!(
        e.inner
            .db
            .query("SELECT * FROM hub_channel_delivery_msgs", &[])
            .unwrap()
            .is_empty(),
        "first identity assignment retires legacy destination mappings"
    );
    e.inner
        .db
        .execute("UPDATE hub_human_channels SET enabled=0", &[])
        .unwrap();
    send(&e, &node, "while-disabled");
    let boundary = e.inner.db.transaction(max_seq).unwrap();
    assert!(boundary > old_boundary.as_i64().unwrap());
    e.inner.db.execute("INSERT INTO hub_pairing_codes(code,plugin_id,principal_id,owner_user,created_ms,expires_ms) VALUES('cito-reenable','fixture','none#alice','alice',?,?)",&[json!(now_ms()),json!(now_ms()+60_000)]).unwrap();
    e.redeem("fixture",&json!({"code":"cito-reenable","is_private_chat":true,"external_chat_id":"room","external_user_id":"alice","user_config_patch":{}})).unwrap();
    assert_eq!(
        e.inner
            .db
            .query(
                "SELECT last_delivered_seq,enabled FROM hub_human_channels",
                &[]
            )
            .unwrap()[0],
        json!({"last_delivered_seq":boundary,"enabled":1})
    );
    assert!(e.inner.processes.lock().unwrap().is_empty());
    e.inner.plugins.lock().unwrap().clear();
}

#[test]
fn stream_ingress_rejects_coercion_and_audit_excludes_private_payloads() {
    let dir = temp();
    let hub = test_engine(&dir);
    hub.rpc("kv_put", &json!({"key":"target","data":{"principal_id":"none#alice","scope":"workspace","state":"ACTIVE","owner_token":"private-owner-token","channels":{"a":{"owner_token":"private-nested-token"}}}}), None).unwrap();
    let signal = hub.rpc("js_publish", &json!({"subject":"cito.signal.target","data":{"signal_id":"published","message":"private-message-body"}}), None).unwrap();
    hub.rpc(
        "consumer_add",
        &json!({"config":{"name":"receiver","filter_subject":"cito.signal.target"}}),
        None,
    )
    .unwrap();
    for (key, value) in [
        ("max_age", json!(true)),
        ("duplicate_window", json!(1.5)),
        ("max_age", json!("1")),
    ] {
        for (op, params) in [
            ("stream_add", json!({"config":{"name":"test",key:value}})),
            (
                "stream_update",
                json!({"stream":SIGNALS,"patch":{key:value}}),
            ),
        ] {
            assert!(hub.rpc(op, &params, None).unwrap_err().contains(key));
        }
    }
    for seq in [json!(true), json!(0), json!(-1), json!(1.0), json!("1")] {
        for (op, params) in [
            ("stream_get_message", json!({"stream":SIGNALS,"seq":seq})),
            ("stream_get_message", json!({"stream":SIGNALS,"seqs":[seq]})),
            ("consumer_ack", json!({"name":"receiver","seq":seq})),
        ] {
            assert!(hub
                .rpc(op, &params, None)
                .unwrap_err()
                .starts_with("INVALID_INPUT:"));
        }
    }
    assert_eq!(
        hub.rpc(
            "consumer_ack",
            &json!({"name":"receiver","seq":signal["seq"]}),
            None
        )
        .unwrap()["acked"],
        true
    );
    let detail = hub
        .inner
        .db
        .query("SELECT detail_json FROM hub_audit_log", &[])
        .unwrap();
    let serialized = json!(detail).to_string();
    for secret in [
        "private-owner-token",
        "private-nested-token",
        "private-message-body",
    ] {
        assert!(!serialized.contains(secret));
    }
}

#[test]
fn fresh_publish_and_wake_claims_check_registration_incarnation() {
    let dir = temp();
    let hub = test_engine(&dir);
    let owner = principal("alice");
    let stranger = principal("bob");
    let register = |generation, token: &str, scope: &str| {
        hub.rpc("kv_put", &json!({"key":"agent","data":{"harness":"agent","principal_id":"none#alice","scope":scope,"state":"ACTIVE","binding_generation":generation,"channels":{"host:agent":{"state":"ACTIVE","host_id":"host","owner_token":token}}}}), Some(&owner)).unwrap()
    };
    let publish = |generation, caller: &Value, id: &str| {
        hub.rpc("js_publish", &json!({"subject":"cito.signal.agent","data":{"signal_id":id,"target_generation":generation,"from":{"principal_id":caller["principal_id"],"listener_id":"sender"},"message":"Hello"}}), Some(caller))
    };
    register(1, "first", "self");
    assert!(publish(0, &stranger, "private")
        .unwrap_err()
        .starts_with("FORBIDDEN_TARGET_SCOPE:"));
    assert!(publish(2, &owner, "stale")
        .unwrap_err()
        .starts_with("STALE_GENERATION:"));
    publish(1, &owner, "admitted").unwrap();
    let claim = |token: &str| {
        hub.rpc("signal_claim_dispatch", &json!({"node_id":"agent","channel_id":"host:agent","signal_ids":["admitted"],"owner_token":token}), Some(&owner))
    };
    assert_eq!(claim("first").unwrap()["claimed"], true);
    register(2, "second", "self");
    assert!(claim("first").unwrap_err().starts_with("STALE_GENERATION:"));
    assert_eq!(claim("second").unwrap()["reason"], "already_claimed");
    hub.inner
        .db
        .execute("UPDATE hub_signals SET claim_until_ms=0", &[])
        .unwrap();
    assert_eq!(claim("second").unwrap()["claimed"], true);
    // Freeze the cached clock at an old valid lease. Only the Hub clock is authoritative.
    hub.inner
        .db
        .transaction(|c| {
            let mut reg = parse(&kv_row(c, "agent")?.unwrap(), "value");
            reg["channels"]["host:agent"]["updated_at_ms"] = json!(1);
            reg["server_now_ms"] = json!(2);
            c.execute(
                "UPDATE hub_kv SET value=CAST(? AS BLOB) WHERE key='agent'",
                &[json!(reg.to_string())],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(publish(2, &owner, "expired")
        .unwrap_err()
        .starts_with("LISTENER_NOT_FOUND:"));
}

#[test]
fn retention_and_dedup_expire() {
    let dir = temp();
    let e = test_engine(&dir);
    e.pause_worker();
    e.rpc(
        "stream_update",
        &json!({"stream":SIGNALS,"patch":{"max_age":50_000_000,"duplicate_window":50_000_000}}),
        None,
    )
    .unwrap();
    send(&e, "target", "s1");
    e.inner
        .db
        .execute(
            "UPDATE hub_signals SET created_ms=?",
            &[json!(now_ms() - 1000)],
        )
        .unwrap();
    e.inner
        .db
        .execute(
            "UPDATE hub_meta SET int_val=0 WHERE key='last_maintenance_ms'",
            &[],
        )
        .unwrap();
    send(&e, "target", "s1");
    let rows = e
        .inner
        .db
        .query("SELECT seq FROM hub_signals", &[])
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["seq"], 2);

    // Expire only the publication key, leaving the original logical signal readable.
    e.rpc("stream_update", &json!({"stream":SIGNALS,"patch":{"max_age":60_000_000_000i64,"duplicate_window":50_000_000}}),None).unwrap();
    e.inner
        .db
        .execute(
            "UPDATE hub_signals SET created_ms=? WHERE seq=2",
            &[json!(now_ms() - 1000)],
        )
        .unwrap();
    let reused = e
        .rpc(
            "js_publish",
            &json!({"subject":"cito.signal.target","msg_id":"s1","data":{"signal_id":"s2"}}),
            None,
        )
        .unwrap();
    assert_eq!(reused["duplicate"], false);
    assert_eq!(
        e.rpc(
            "stream_get_message",
            &json!({"stream":SIGNALS,"seqs":[2,reused["seq"]]}),
            None
        )
        .unwrap()["messages"],
        json!([{"seq":2,"signal_id":"s1","acked_at_ms":null},{"seq":3,"signal_id":"s2","acked_at_ms":null}])
    );
}
