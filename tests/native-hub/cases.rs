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
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/native-hub/channel.py");
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
    assert!(node_active(&reg, 0, now_ms() + 10_000_000));
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
    let id = human_id(s(&caller, "principal_id"));
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
    let node = human_id("none#alice");
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
fn reply_dedup_target_validation_and_ack_commit_together() {
    let dir = temp();
    let e = HubEngine::open(&dir).unwrap();
    put(&e, "sender", "none#bob", "workspace");
    let pid = "none#alice";
    let node = human_id(pid);
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
    let node = human_id(pid);
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
fn reservations_are_exclusive_and_stale_inflight_is_retried() {
    let dir = temp();
    let root = fixture(&dir);
    let e = HubEngine::open(&dir).unwrap();
    e.set_plugin_roots(&[root.clone()]).unwrap();
    e.rpc("human_channels_configure",&json!({"plugin_id":"fixture","enabled":true,"hub_config":{"credential":"test-only"},"user_config":{"address":"room"}}),Some(&json!({"principal_id":"none#alice","owner_user":"alice"}))).unwrap();
    let node = human_id("none#alice");
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
    e.inner.db.execute("UPDATE hub_channel_deliveries SET status='in_flight',delivered_ms=?,reservation_id='abandoned' WHERE signal_id='concurrent'",&[json!(now_ms()-60_001)]).unwrap();
    e.replay(&plugin, &process).unwrap();
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
