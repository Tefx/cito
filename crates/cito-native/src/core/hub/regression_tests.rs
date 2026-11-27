use super::*;

struct Fixture {
    hub: HubEngine,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "cito-hub-regression-{}",
            super::super::plugins::random_uuid_v4()
        ));
        let hub = HubEngine::open(&dir).unwrap();
        hub.pause_worker();
        Self { hub, dir }
    }
    fn plugin(&self) -> ChannelPlugin {
        hub_plugins::test_descriptor(
            &self.dir,
            "sample",
            vec![],
            json!({"serviceIdentity":{"field":"credential","separator":":"}}),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.hub.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn bot_identity_rotation_preserves_same_account_and_fences_replacement() {
    let f = Fixture::new();
    let plugin = f.plugin();
    f.hub.inner.db.transaction(|c| {
        save_channel_service(c, &plugin, &json!({"credential":"123:first"}))?;
        c.execute("UPDATE hub_plugin_services SET service_state_json=?", &[json!(json!({"cursor":42,"reply_floor":40,"inbound_mode":"outbound_only","service_label":"@sample"}).to_string())])?;
        c.execute("INSERT INTO hub_processed_replies VALUES('sample','41',1)", &[])?;
        c.execute("INSERT INTO hub_channel_delivery_msgs VALUES('sample','none#alice','1','signal','sender')", &[])?;
        c.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,external_msg_id,from_listener_id,delivered_ms,reservation_id) VALUES('sample','none#alice','signal',1,'in_flight','1','sender',1,'old')", &[])?;
        save_channel_service(c, &plugin, &json!({"credential":"123:rotated"}))?;
        assert_eq!(service(c,"sample")?.1,json!({"cursor":42,"reply_floor":40,"inbound_mode":"outbound_only","service_label":"@sample"}));
        assert_eq!(c.query("SELECT * FROM hub_processed_replies", &[])?.len(),1);
        assert_eq!(c.query("SELECT * FROM hub_channel_delivery_msgs", &[])?.len(),1);
        save_channel_service(c, &plugin, &json!({"credential":"456:new"}))?;
        assert_eq!(service(c,"sample")?.1,json!({"inbound_mode":"outbound_only"}));
        assert!(c.query("SELECT * FROM hub_processed_replies", &[])?.is_empty());
        assert!(c.query("SELECT * FROM hub_channel_delivery_msgs", &[])?.is_empty());
        let row = c.query("SELECT * FROM hub_channel_deliveries", &[])?.remove(0);
        assert_eq!(row["status"],"effect_unknown");
        assert!(row["reservation_id"].is_null());
        assert!(row["external_msg_id"].is_null());
        Ok(())
    }).unwrap();
    f.hub
        .delivery_result(
            "sample",
            &json!({"delivery_id":"old","ok":true,"external_msg_id":"2"}),
        )
        .unwrap();
    assert!(f
        .hub
        .inner
        .db
        .query("SELECT * FROM hub_channel_delivery_msgs", &[])
        .unwrap()
        .is_empty());
}

#[test]
fn channel_save_compares_the_persisted_service_before_writing_the_binding() -> Result<(), String> {
    let f = Fixture::new();
    let root = f.dir.join("plugins");
    let dir = root.join("sample");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Commit a competing rotation on a second SQLite connection during reader shutdown,
    // after the save's preparation transaction and before its binding transaction.
    let reader = r#"
import json, sqlite3, sys
for line in sys.stdin:
    frame = json.loads(line)
    if frame['op'] == 'test':
        print(json.dumps(dict(op='rpc_result', req_id=frame['req_id'], ok=True)), flush=True)
    if frame['op'] == 'shutdown':
        with sqlite3.connect(sys.argv[1]) as db:
            db.execute("UPDATE hub_plugin_services SET config_json=? WHERE plugin_id='sample'",
                       (json.dumps(dict(credential='second:secret')),))
        break
"#;
    std::fs::write(
        dir.join("plugin.json"),
        json!({
            "id":"sample", "displayName":"Sample", "kind":"human-channel",
            "command":["python3","-c",reader,f.dir.join("hub.sqlite")],
            "fields":[{"key":"credential","scope":"hub","type":"secret"}],
            "serviceIdentity":{"field":"credential","separator":":"}
        })
        .to_string(),
    )
    .map_err(|e| e.to_string())?;
    f.hub.set_plugin_roots(&[root])?;
    let owner = principal("alice");
    let configure = |p| f.hub.rpc("human_channels_configure", &p, Some(&owner));
    let manual = configure(
        json!({"plugin_id":"sample","hub_config":{"credential":"first:secret"},"display_label":"Manual"}),
    )?;
    let observed = &manual["channels"][0]["service_identity"];
    assert_eq!(observed, "first");
    assert_eq!(
        f.hub.rpc(
            "human_channels_test",
            &json!({"plugin_id":"sample"}),
            Some(&owner)
        )?["ok"],
        true
    );

    // A same-account secret change requires reaping the reader, but not a new identity.
    let mut link = json!({"plugin_id":"sample","hub_config":{"credential":"first:renewed"},"display_label":"Linked","expected_service_identity":observed});
    assert_eq!(
        configure(link.clone()).unwrap_err(),
        "CONFLICT: Channel service identity changed before Link completed; refresh and try again."
    );
    for invalid in [Value::Null, json!(""), json!(" \t"), json!(17)] {
        link["expected_service_identity"] = invalid;
        assert_eq!(
            configure(link.clone()).unwrap_err(),
            "CONFLICT: expected_service_identity must be a non-empty string"
        );
    }
    let inventory = f.hub.rpc("human_channels_list", &json!({}), Some(&owner))?;
    let live = &inventory["channels"][0];
    assert_eq!(live["display_label"], "Manual");
    assert_eq!(live["service_identity"], "second");
    let matched = configure(
        json!({"plugin_id":"sample","expected_service_identity":live["service_identity"],"display_label":"Linked"}),
    )?;
    assert_eq!(matched["committed"], true);
    assert_eq!(matched["channels"][0]["display_label"], "Linked");
    Ok(())
}

#[test]
fn public_identity_does_not_fall_back_to_credentials() {
    let f = Fixture::new();
    let mut plugin = f.plugin();
    let project = |plugin: &ChannelPlugin, value| {
        public_service_identity(plugin, &json!({"credential":value}))
    };
    assert_eq!(project(&plugin, "123:secret"), "123");
    for malformed in ["secret", ":secret", "123:", ""] {
        assert!(project(&plugin, malformed).is_null());
    }
    plugin.channel.service_identity["separator"] = json!("");
    assert!(project(&plugin, "123:secret").is_null());
    plugin.channel.service_identity = Value::Null;
    assert!(project(&plugin, "123:secret").is_null());
}

#[test]
fn label_only_health_preserves_poll_error_and_notifies_on_changes() {
    let f = Fixture::new();
    let sub = f.hub.subscribe(None).unwrap();
    let health = |frame| {
        f.hub.plugin_frame("sample", &frame);
        f.hub
            .inner
            .db
            .transaction(|c| service(c, "sample"))
            .unwrap()
            .1
    };
    assert_eq!(
        health(json!({"op":"health","error":"Conflict"}))["poll_error"],
        "Conflict"
    );
    assert_eq!(sub.try_recv().unwrap(), json!({"type":"kv"}));
    let label = json!({"op":"health","service_label":"@sample"});
    assert_eq!(
        health(label.clone()),
        json!({"poll_error":"Conflict","service_label":"@sample"})
    );
    assert_eq!(sub.try_recv().unwrap(), json!({"type":"kv"}));
    health(label);
    assert!(sub.try_recv().is_err());
    assert_eq!(
        health(json!({"op":"health","error":null})),
        json!({"service_label":"@sample"})
    );
    assert_eq!(sub.try_recv().unwrap(), json!({"type":"kv"}));
}

#[test]
fn reservations_are_globally_bounded_and_local_backpressure_does_not_spend_attempts() {
    let f = Fixture::new();
    let plugin = f.plugin();
    f.hub.inner.db.transaction(|c| {
        for user in ["alice", "bob"] {
            let binding = json!({"principal_id":format!("none#{user}"),"owner_user":user,"plugin_id":"sample"});
            attach(c, &binding, true, "workspace", Some(&plugin))?;
            let (node, _) = find_human(c, &binding)?;
            let subject = json!(format!("cito.signal.{node}"));
            c.execute("INSERT INTO hub_human_channels(principal_id,plugin_id,owner_user) VALUES(?,'sample',?)", &[binding["principal_id"].clone(),json!(user)])?;
            for _ in 0..200 {
                c.execute("INSERT INTO hub_signals(subject,created_ms,data) VALUES(?,?,X'00')", &[subject.clone(),json!(now_ms())])?;
            }
            for index in 0..24 {
                let id = format!("{user}-{index}");
                let signal = json!({"signal_id":id,"from":{"listener_id":"sender"},"message":"Message"});
                c.execute("INSERT INTO hub_signals(subject,signal_id,created_ms,data) VALUES(?,?,?,CAST(? AS BLOB))", &[subject.clone(),json!(id),json!(now_ms()),json!(signal.to_string())])?;
            }
        }
        Ok(())
    }).unwrap();
    let reservations = f.hub.reserve_deliveries(&plugin).unwrap();
    assert_eq!(reservations.len(), 32);
    assert!(reservations.iter().any(|r| r["user"] == "alice"));
    assert!(reservations.iter().any(|r| r["user"] == "bob"));
    assert!(f.hub.reserve_deliveries(&plugin).unwrap().is_empty());
    for (reservation, error) in reservations.iter().zip([
        "Outbound queue is full",
        "Delivery reservation expired before send",
    ]) {
        f.hub
            .inner
            .db
            .execute(
                "UPDATE hub_channel_deliveries SET attempts=5 WHERE reservation_id=?",
                &[reservation["delivery_id"].clone()],
            )
            .unwrap();
        f.hub.delivery_result("sample", &json!({"delivery_id":reservation["delivery_id"],"ok":false,"retryable":true,"effect_unknown":false,"error":error})).unwrap();
    }
    let deferred = f
        .hub
        .inner
        .db
        .query(
            "SELECT * FROM hub_channel_deliveries WHERE status='failed'",
            &[],
        )
        .unwrap();
    assert_eq!(deferred.len(), 2);
    assert!(deferred.iter().all(|row| row["attempts"] == 5));
    let fresh = f.hub.reserve_deliveries(&plugin).unwrap();
    let second_user = s(&reservations[31], "user");
    assert_eq!(
        fresh
            .iter()
            .map(|r| s(&r["signal"], "signal_id"))
            .collect::<Vec<_>>(),
        [format!("{second_user}-8"), format!("{second_user}-9")]
    );
    f.hub
        .inner
        .db
        .execute(
            "UPDATE hub_channel_deliveries SET next_retry_ms=0 WHERE status='failed'",
            &[],
        )
        .unwrap();
    for reservation in fresh {
        f.hub
            .delivery_result(
                "sample",
                &json!({"delivery_id":reservation["delivery_id"],"ok":true}),
            )
            .unwrap();
    }
    let retried = f.hub.reserve_deliveries(&plugin).unwrap();
    assert_eq!(
        retried
            .iter()
            .map(|r| s(&r["signal"], "signal_id"))
            .collect::<Vec<_>>(),
        reservations[..2]
            .iter()
            .map(|r| s(&r["signal"], "signal_id"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn http_shutdown_joins_live_event_streams_without_closing_the_shared_engine() -> Result<(), String>
{
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;

    let f = Fixture::new();
    let server = f
        .hub
        .start_server("127.0.0.1", 0, HubAuth::new("none", None, None, None)?)?;
    let mut stream = TcpStream::connect(server.address).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    write!(
        stream,
        "GET /v1/events HTTP/1.1\r\nHost: {}\r\n\r\n",
        server.address
    )
    .map_err(|e| e.to_string())?;
    let mut events = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(events.read_line(&mut line).map_err(|e| e.to_string())?, 0);
        if line == ":ok\n" {
            break;
        }
    }
    let response: Value = ureq::post(&format!("http://{}/v1/rpc", server.address))
        .timeout(Duration::from_secs(2))
        .send_json(json!({"op":"stream_info","params":{"stream":SIGNALS}}))
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    assert_eq!(response["ok"], true);
    let start = std::time::Instant::now();
    server.shutdown();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(!server.is_running());
    assert!(f
        .hub
        .rpc("stream_info", &json!({"stream":SIGNALS}), None)
        .is_ok());
    Ok(())
}

#[test]
fn scope_revocation_sends_a_payload_free_hint_to_nonowners() {
    let f = Fixture::new();
    let alice = json!({"principal_id":"oidc:https://id.example.com|alice","owner_user":"alice","oidc":true});
    let bob =
        json!({"principal_id":"oidc:https://id.example.com|bob","owner_user":"bob","oidc":true});
    let subscription = f.hub.subscribe(Some(bob)).unwrap();
    let row = json!({"listener_id":"agent","principal_id":alice["principal_id"],"harness":"sample","session_id":"session","scope":"workspace"});
    f.hub
        .rpc(
            "kv_create",
            &json!({"key":"agent","data":row.to_string()}),
            Some(&alice),
        )
        .unwrap();
    assert!(subscription.try_recv().is_ok());
    let mut private = row;
    private["scope"] = json!("self");
    f.hub
        .rpc(
            "kv_put",
            &json!({"key":"agent","data":private.to_string()}),
            Some(&alice),
        )
        .unwrap();
    assert_eq!(subscription.try_recv().unwrap(), json!({"type":"kv"}));
    assert!(subscription.try_recv().is_err());
    f.hub
        .rpc(
            "human_inbox_update",
            &json!({"expected_control_revision":1,"scope":"workspace"}),
            Some(&alice),
        )
        .unwrap();
    assert!(subscription.try_recv().is_ok());
    f.hub
        .rpc(
            "human_inbox_update",
            &json!({"expected_control_revision":2,"scope":"self"}),
            Some(&alice),
        )
        .unwrap();
    assert_eq!(subscription.try_recv().unwrap(), json!({"type":"kv"}));
    assert!(subscription.try_recv().is_err());
}
