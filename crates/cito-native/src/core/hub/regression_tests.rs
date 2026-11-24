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
        ChannelPlugin {
            id: "sample".into(),
            dir: self.dir.clone(),
            is_builtin: false,
            command: vec![],
            manifest: json!({"fields":[],"serviceIdentity":{"field":"credential","separator":":"}}),
        }
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
        c.execute("UPDATE hub_plugin_services SET service_state_json=?", &[json!(json!({"cursor":42,"reply_floor":40}).to_string())])?;
        c.execute("INSERT INTO hub_processed_replies VALUES('sample','41',1)", &[])?;
        c.execute("INSERT INTO hub_channel_delivery_msgs VALUES('sample','none#alice','1','signal','sender')", &[])?;
        c.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,external_msg_id,from_listener_id,delivered_ms,reservation_id) VALUES('sample','none#alice','signal',1,'in_flight','1','sender',1,'old')", &[])?;
        save_channel_service(c, &plugin, &json!({"credential":"123:rotated"}))?;
        assert_eq!(service(c,"sample")?.1["cursor"],42);
        assert_eq!(c.query("SELECT * FROM hub_processed_replies", &[])?.len(),1);
        assert_eq!(c.query("SELECT * FROM hub_channel_delivery_msgs", &[])?.len(),1);
        save_channel_service(c, &plugin, &json!({"credential":"456:new"}))?;
        assert_eq!(service(c,"sample")?.1,json!({}));
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
