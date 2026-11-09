use super::*;

fn temp() -> PathBuf {
    let dir = std::env::temp_dir().join(random_id("cito-hub-test-").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn put(e: &HubEngine, id: &str, pid: &str, scope: &str) {
    e.rpc("kv_put",&json!({"key":id,"data_b64":encoded(&json!({"listener_id":id,"principal_id":pid,"owner_user":"alice","scope":scope,"harness":"fixture","session_id":"s1","binding_generation":1,"state":"ACTIVE","updated_at_ms":now_ms()}))}),None).unwrap();
}
fn send(e: &HubEngine, id: &str, sig: &str) {
    e.rpc("js_publish",&json!({"subject":format!("cito.signal.{id}"),"data_b64":encoded(&json!({"signal_id":sig,"from":{"principal_id":"oidc:https://id.example.com|sender","listener_id":"sender","user":"bob"},"message":"hello"})),"opts":{"msgID":sig}}),None).unwrap();
}
#[test]
fn attach_rejects_non_object_registrations_without_overwriting_kv() -> Result<(), String> {
    let dir = temp();
    let e = HubEngine::open(&dir)?;
    let result = (|| {
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
                e.inner.db.transaction(|c| attach(c, &binding, true, false)),
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
    })();
    e.shutdown();
    std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    result
}

#[test]
fn consumer_next_crosses_byte_pages_without_transport_acknowledgments() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn atomic_batch_claims_and_business_ack_are_distinct() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
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
    let e = HubEngine::open(&dir).unwrap();
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn oidc_read_and_ack_events_are_private() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn workspace_directory_does_not_grant_message_or_ack_content() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
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
    let e = HubEngine::open(&dir).unwrap();
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
    let e = HubEngine::open(&dir).unwrap();
    assert_eq!(
        e.rpc("account_info", &json!({}), None).unwrap()["streams"],
        2
    );
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
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
    let e = HubEngine::open(&dir).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Drive exactly one tick; a background retry must not satisfy this test.
        e.inner.closed.store(true, Ordering::SeqCst);
        let worker = e.inner.worker.lock().unwrap().take().unwrap();
        worker.join().unwrap();
        e.inner.closed.store(false, Ordering::SeqCst);

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
        e.set_plugin_roots(&[root.clone()]).unwrap();
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
    }));
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn pairing_instruction_persists_and_admin_guard_covers_all_paths() {
    let dir = temp();
    let root = fixture(&dir);
    let e = HubEngine::open_with_remote(&dir, true).unwrap();
    e.set_plugin_roots(&[root.clone()]).unwrap();
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
    let e = HubEngine::open_with_remote(&dir, true).unwrap();
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
    let redeemed=e.redeem("fixture",&json!({"code":code,"is_private_chat":true,"external_chat_id":"paired-chat","external_user_id":"paired-user","user_config_patch":{"address":"paired-room"}})).unwrap();
    assert_eq!(redeemed["ok"], true);
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
    let human = e
        .inner
        .db
        .transaction(|c| find_human(c, s(&user, "principal_id")))
        .unwrap()
        .unwrap();
    let node = e.rpc("kv_get", &json!({"key":human}), Some(&user)).unwrap();
    let reg: Value = serde_json::from_slice(&B64.decode(s(&node, "value_b64")).unwrap()).unwrap();
    assert_eq!(reg["scope"], "self");
    assert_eq!(
        evaluate_node_lease(&reg, (now_ms() + 10_000_000) as u64),
        "ACTIVE"
    );
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn human_public_id_uses_canonical_slug_normalization() {
    let dir = temp();
    let root = fixture(&dir);
    let e = HubEngine::open(&dir).unwrap();
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
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
fn reservations_retry_holes_and_record_all_chunks() {
    let dir = temp();
    let root = fixture(&dir);
    let e = HubEngine::open(&dir).unwrap();
    e.set_plugin_roots(&[root.clone()]).unwrap();
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
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn principal_alias_reregistration_preserves_replay_and_ack_message_ids() {
    let dir = temp();
    let root = fixture(&dir);
    let e = HubEngine::open(&dir).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        e.set_plugin_roots(&[root.clone()]).unwrap();
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
            e.rpc("kv_delete", &json!({"key":old_node}), None).unwrap();
            e.rpc(
                "kv_put",
                &json!({"key":node,"data_b64":encoded(&registration)}),
                None,
            )
            .unwrap();
            // A newer unrelated Human node must not capture this binding's replay.
            let mut unrelated = registration.clone();
            unrelated["principal_id"] = json!("oidc:https://id.example.com|unrelated");
            unrelated["listener_id"] = json!("unrelated-human");
            e.rpc(
                "kv_put",
                &json!({"key":"unrelated-human","data_b64":encoded(&unrelated)}),
                None,
            )
            .unwrap();
            send(&e, &node, signal);
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
    std::fs::remove_dir_all(dir).unwrap();
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
    let e = HubEngine::open(&dir).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        e.set_plugin_roots(&[root.clone()]).unwrap();
        e.rpc(
            "human_channels_configure",
            &json!({"plugin_id":"fixture","hub_config":{"credential":"test-only"}}),
            None,
        )
        .unwrap();
        let owner = local_principal("alice");
        e.rpc(
            "human_channels_configure",
            &json!({"plugin_id":"fixture","enabled":true,"user_config":{"address":"reply-test"}}),
            Some(&owner),
        )
        .unwrap();
        let human = listener_id("none#alice", "human", "default");
        put(&e, "target", "none#bob", "workspace");
        send(&e, &human, "source");
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
    }));
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
#[test]
fn human_reply_rpc_uses_fallback_node_and_rolls_back_publication_on_ack_failure() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
    let sender = local_principal("alice");
    let human = listener_id("none#alice", "human", "default");
    put(&e, "target", "none#bob", "workspace");
    send(&e, &human, "original");
    let events = e.subscribe(None).unwrap();
    let request = json!({"to":"work:target","message":" \u{0081}Approv\u{0081}ed\t ","in_reply_to":"work:original","msg_id":"desktop-reply","host_id":"desktop","host_name":"Desktop"});
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn reply_dedup_target_validation_and_ack_commit_together() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
    put(&e, "sender", "none#bob", "workspace");
    let pid = "none#alice";
    let node = listener_id(pid, "human", "default");
    put(&e, &node, pid, "self");
    send(&e, &node, "original");
    e.inner.db.transaction(|c|{c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user,user_config_json) VALUES(?,'fixture','alice',?)",&[json!(pid),json!(json!({"_external_chat_id":"room","_external_user_id":"user"}).to_string())])?;c.execute("INSERT INTO hub_channel_delivery_msgs(plugin_id,principal_id,external_msg_id,signal_id,from_listener_id) VALUES('fixture',?,'chunk-1','original','work:sender')",&[json!(pid)])?;Ok(())}).unwrap();
    let frame = json!({"op":"inbound_reply","update_id":"u1","external_chat_id":"room","external_user_id":"user","reply_to_external_msg_id":"chunk-1","text":"Approved"});
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
        e.rpc("kv_put",&json!({"key":"sender","data_b64":encoded(&json!({"principal_id":"none#bob","scope":"workspace","state":state,"updated_at_ms":now_ms()-age}))}),None).unwrap();
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
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
    drop(db);
    let e = HubEngine::open(&dir).unwrap();
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
            "SELECT external_msg_id,signal_id FROM hub_channel_delivery_msgs",
            &[],
        )
        .unwrap();
    assert_eq!(mappings[0]["external_msg_id"], "42");
    assert_eq!(mappings[0]["signal_id"], "original");
    assert_eq!(
        e.inner
            .db
            .query("SELECT code,instruction FROM hub_pairing_codes", &[])
            .unwrap()[0]["code"],
        "legacy"
    );
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn accepted_http_connections_wait_for_fragmented_headers() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
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
    client.write_all(b"Host: localhost\r\n\r\n").unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    server.shutdown();
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn http_wire_and_stoppable_shared_engine() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
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
    server.shutdown();
    assert!(e.rpc("account_info", &json!({}), None).is_ok());
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn verified_oidc_identity_keeps_legacy_private_data_and_bindings() {
    let dir = temp();
    let root = fixture(&dir);
    let e = HubEngine::open_with_remote(&dir, true).unwrap();
    e.set_plugin_roots(&[root]).unwrap();
    let legacy = "https://id.example.com#subject";
    let caller = json!({"principal_id":"oidc:https://id.example.com|subject","legacy_principal_id":legacy,"sub":"subject","owner_user":"alice","oidc":true});
    put(&e, "legacy-node", legacy, "self");
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
    let list = e
        .rpc("human_channels_list", &json!({}), Some(&caller))
        .unwrap();
    assert_eq!(list["channels"][0]["fields"][1]["value"], "legacy-room");
    e.rpc(
        "human_channels_configure",
        &json!({"plugin_id":"fixture","user_config":{"address":"updated-room"}}),
        Some(&caller),
    )
    .unwrap();
    assert_eq!(
        e.inner
            .db
            .query("SELECT principal_id FROM hub_human_channels", &[])
            .unwrap()[0]["principal_id"],
        legacy
    );
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn binary_payloads_survive_kv_stream_consumers_and_restart() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
    let payloads = vec![
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
    let e = HubEngine::open(&dir).unwrap();
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn raw_nc_variants_preserve_ack_bytes_and_publish_signals_durably() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn opaque_oidc_payloads_keep_ownership_and_validate_available_metadata() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn rpc_preserves_payload_bytes() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn concurrent_claim_has_one_winner() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
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
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn pending_request_fails_on_child_exit() {
    let dir = temp();
    let plugin = ChannelPlugin {
        id: "fixture".into(),
        dir: dir.clone(),
        manifest: json!({}),
        command: vec![
            "python3".into(),
            "-c".into(),
            "import sys; sys.stdin.readline(); sys.exit(0)".into(),
        ],
    };
    let p = ChannelProcess::spawn(&plugin, Arc::new(|_| None)).unwrap();
    let started = std::time::Instant::now();
    let result = p.request(json!({"op":"test"}));
    assert!(started.elapsed() < Duration::from_secs(1));
    match result {
        Ok(v) => assert_eq!(v["error"], "Plugin subprocess exited"),
        Err(e) => assert!(e.contains("exited")),
    }
    p.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn delivered_unread_history_does_not_starve_later_notifications() {
    let dir = temp();
    let root = fixture(&dir);
    let e = HubEngine::open(&dir).unwrap();
    e.set_plugin_roots(&[root.clone()]).unwrap();
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
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn reservations_renew_while_process_is_live_and_replay_after_exit() {
    let dir = temp();
    let root = fixture(&dir);
    let e = HubEngine::open(&dir).unwrap();
    e.set_plugin_roots(&[root.clone()]).unwrap();
    e.rpc("human_channels_configure",&json!({"plugin_id":"fixture","enabled":true,"hub_config":{"credential":"test-only"},"user_config":{"address":"room"}}),Some(&json!({"principal_id":"none#alice","owner_user":"alice"}))).unwrap();
    let node = listener_id("none#alice", "human", "default");
    send(&e, &node, "concurrent");
    let plugin = e.plugin("fixture").unwrap();
    let process = e.ensure_process(&plugin).unwrap();
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
    assert_eq!(row["reservation_id"], "abandoned");
    assert_eq!(row["status"], "in_flight");
    assert!(n(&row, "delivered_ms") > now_ms() - 1000);
    process.shutdown();
    let replacement = e.ensure_process(&plugin).unwrap();
    e.replay(&plugin, &replacement).unwrap();
    wait_for(|| {
        e.inner.db.query("SELECT * FROM hub_channel_deliveries WHERE signal_id='concurrent' AND status='delivered'",&[]).unwrap().len()==1
    });
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
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn retention_and_dedup_expire() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
    e.rpc(
        "stream_update",
        &json!({"stream":SIGNALS,"patch":{"max_age":50_000_000,"duplicate_window":50_000_000}}),
        None,
    )
    .unwrap();
    send(&e, "target", "s1");
    thread::sleep(Duration::from_millis(80));
    send(&e, "target", "s1");
    let rows = e
        .inner
        .db
        .query("SELECT seq FROM hub_signals", &[])
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["seq"], 2);
    e.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
