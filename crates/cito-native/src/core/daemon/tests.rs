use super::{
    channels::{remote_channels_identity, ChannelInventory},
    host_hub::{advertised_hub_host, tailscale_host_from_status},
    mcp::default_session_title,
    projection::{address_peers, sender_display_address},
    ui_ipc::resolve_ui_binary,
    *,
};
use std::io::{BufRead, BufReader, Read};
use std::net::TcpStream;

fn settle_setup_inspections(service: &mut DaemonService) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while service
        .setup_inspections
        .values()
        .any(|entry| entry.worker.is_some())
    {
        assert!(Instant::now() < deadline, "Setup inspection did not settle");
        service.refresh_setup_inspections();
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn activity_auto_listen_pin_stop_idle_and_reopen_follow_orthogonal_intent() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("cito-lifecycle-{}", random_uuid_v4()));
    fs::create_dir_all(dir.join("home")).map_err(|e| e.to_string())?;
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    fs::write(
        &config.config_path,
        json!({"hubs":{"local":{"url":"local://","autoListen":true}}}).to_string(),
    )
    .map_err(|e| e.to_string())?;
    let script = dir.join("provider.py");
    fs::write(
        &script,
        r#"import json, pathlib, sys
root = pathlib.Path(__file__).parent
r = json.loads(sys.stdin.readline())
op = r['op']
v = dict(ok=True)
if op == 'setup_inspect': v.update(installed=True, configured=True, paths=[])
elif op == 'discover_sessions': v['sessions'] = json.loads((root/'sessions.json').read_text())
elif op == 'probe': v['status'] = (root/'probe.txt').read_text()
print(json.dumps(v))
"#,
    )
    .map_err(|e| e.to_string())?;
    let plugin = crate::plugins::PluginDescriptor {
        id: "fixture".into(),
        kind: "agent-harness".into(),
        display_name: "Fixture".into(),
        tier: "test".into(),
        order: 0,
        sf_symbol: "terminal".into(),
        tint: "blue".into(),
        cli_flag: false,
        pid_bound: false,
        auto_listen: crate::plugins::AutoListen {
            mode: crate::plugins::AutoListenMode::Activity,
            recent_window_ms: Some(900_000),
            idle_timeout_ms: Some(3_600_000),
        },
        plugin_dir: dir.clone(),
        is_builtin: false,
        command: vec![
            "/usr/bin/python3".into(),
            script.to_string_lossy().into_owned(),
        ],
        channel_env: None,
        fields: vec![],
        post_setup_hint: None,
        post_disconnect_hint: None,
    };
    let plugins = LoadedPlugins {
        plugins: vec![plugin.clone()],
        rejected_ids: HashSet::new(),
    };
    let mut candidates:Vec<Value>=(0..12).map(|i|json!({"harness":"fixture","session_id":format!("s{i}"),"title":"Work","cwd":"/workspace","activity":{"revision":10,"at_ms":now_ms(),"kind":"unknown"}})).collect();
    let save = |rows: &Vec<Value>| {
        fs::write(dir.join("sessions.json"), json!(rows).to_string()).map_err(|e| e.to_string())
    };
    save(&candidates)?;
    fs::write(dir.join("probe.txt"), "alive").map_err(|e| e.to_string())?;
    let mut service = DaemonService::open(&config, &plugins)?;
    service.local.set_plugin_roots(&[])?;
    let result = (|| {
        settle_setup_inspections(&mut service);
        service.refresh_available()?;
        let agents = || {
            service.state.active().map(|rows| {
                rows.into_iter()
                    .filter(|r| r["harness"] == "fixture")
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            agents()?.len(),
            12,
            "Activity candidates must not inherit the eight-card menu limit"
        );
        assert!(agents()?.iter().all(|r| r["listen_mode"] == "auto"));
        let id = listener_id("none#alice", "fixture", "s0");
        let row = || {
            service
                .state
                .listener("local", &id)
                .and_then(|r| r.ok_or("Missing row".into()))
        };
        assert_eq!(row()?["last_activity_revision"], "10");
        service.listen("local", &json!({"about":"Pinned"}), &candidates[0])?;
        service.set_auto_listen("local", false)?;
        assert_eq!(
            service
                .state
                .active()?
                .iter()
                .filter(|r| r["harness"] == "fixture")
                .count(),
            1
        );
        assert_eq!(
            service.state.listener("local", &id)?.ok_or("Missing pin")?["listen_mode"],
            "manual"
        );
        assert!(service
            .action(
                &json!({"op":"set_listen_mode","hub":"local","listener_id":id,"listen_mode":"auto"})
            )
            .unwrap_err()
            .starts_with("INVALID_INPUT:"));
        service.set_auto_listen("local", true)?;
        service.refresh_available()?;
        service.action(
            &json!({"op":"set_listen_mode","hub":"local","listener_id":id,"listen_mode":"auto"}),
        )?;
        service.action(
            &json!({"op":"unlisten_session","hub":"local","harness":"fixture","session_id":"s0"}),
        )?;
        assert_eq!(service.slot("local")?.config["autoListen"], true);
        candidates[0]["activity"]["revision"] = json!(11);
        save(&candidates)?;
        service.refresh_available()?;
        let reopened = LocalState::open(&config.state_dir)?;
        let stopped = reopened
            .listener("local", &id)?
            .ok_or("Missing stopped row")?;
        assert_eq!(stopped["user_stopped"], 1);
        assert_eq!(stopped["status"], "UNLISTENED");
        service.listen("local", &json!({"about":"Manual resume"}), &candidates[0])?;
        let manual = service
            .state
            .listener("local", &id)?
            .ok_or("Missing manual row")?;
        assert_eq!(manual["listen_mode"], "manual");
        assert_eq!(manual["user_stopped"], 0);
        service.state.db.execute(
            "UPDATE local_listeners SET last_activity_at_ms=? WHERE listener_id=?",
            &[json!(now_ms() - 3_600_001), json!(id)],
        )?;
        service.auto_listen()?;
        assert_eq!(
            service
                .state
                .listener("local", &id)?
                .ok_or("Missing pinned row")?["status"],
            "ACTIVE"
        );
        service.action(
            &json!({"op":"set_listen_mode","hub":"local","listener_id":id,"listen_mode":"auto"}),
        )?;
        service.state.db.execute(
            "UPDATE local_listeners SET last_activity_at_ms=?,lease_until_ms=? WHERE listener_id=?",
            &[
                json!(now_ms() - 3_600_001),
                json!(now_ms() + 30_000),
                json!(id),
            ],
        )?;
        service.auto_listen()?;
        assert_eq!(
            service
                .state
                .listener("local", &id)?
                .ok_or("Missing leased row")?["status"],
            "ACTIVE"
        );
        service.state.db.execute(
            "UPDATE local_listeners SET lease_until_ms=0 WHERE listener_id=?",
            &[json!(id)],
        )?;
        service.state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,delivered_at_ms) VALUES('local','grace',?,1,1,'bob','host','Sender','hello','',?,'DISPATCHED',?)",&[json!(id),json!(now_ms()+1000),json!(now_ms())])?;
        candidates[0]["activity"] = json!({"revision":12,"at_ms":now_ms(),"kind":"cito_effect"});
        save(&candidates)?;
        service.refresh_available()?;
        assert_eq!(
            service
                .state
                .listener("local", &id)?
                .ok_or("Missing grace row")?["status"],
            "ACTIVE"
        );
        service.state.db.execute(
            "UPDATE local_signals SET delivered_at_ms=? WHERE signal_id='grace'",
            &[json!(now_ms() - 300_001)],
        )?;
        service.auto_listen()?;
        let retired = service
            .state
            .listener("local", &id)?
            .ok_or("Missing idle row")?;
        assert_eq!(retired["status"], "UNLISTENED");
        assert_eq!(retired["retirement_reason"], "idle");
        assert_eq!(retired["retired_activity_revision"], "11");
        assert_eq!(retired["user_stopped"], 0);
        candidates[0]["activity"] = json!({"revision":11,"at_ms":now_ms(),"kind":"user_turn"});
        save(&candidates)?;
        service.refresh_available()?;
        assert_eq!(
            service
                .state
                .listener("local", &id)?
                .ok_or("Missing idle row")?["status"],
            "UNLISTENED"
        );
        candidates[0]["activity"]["revision"] = json!(13);
        save(&candidates)?;
        service.refresh_available()?;
        assert_eq!(
            service
                .state
                .listener("local", &id)?
                .ok_or("Missing reactivated row")?["status"],
            "ACTIVE"
        );
        // A long provider closure must not renew an expired automatic lease.
        service.retire_id("local", &id, "hard_exit")?;
        let old = now_ms() - 3_600_001;
        service.state.db.execute(
            "UPDATE local_listeners SET last_activity_at_ms=? WHERE listener_id=?",
            &[json!(old), json!(id)],
        )?;
        candidates[0]["activity"]["at_ms"] = json!(old);
        save(&candidates)?;
        service.refresh_available()?;
        assert_eq!(
            service
                .state
                .listener("local", &id)?
                .ok_or("Missing expired row")?["status"],
            "UNLISTENED"
        );
        candidates[0]["activity"] = json!({"revision":14,"at_ms":now_ms(),"kind":"user_turn"});
        save(&candidates)?;
        service.refresh_available()?;
        service.auto_listen()?;
        let renewed = service
            .state
            .listener("local", &id)?
            .ok_or("Missing renewed row")?;
        assert_eq!(renewed["status"], "ACTIVE");
        assert!(renewed["last_activity_at_ms"]
            .as_u64()
            .is_some_and(|at| at > old));
        service.listen("local", &json!({"about":"Pinned"}), &candidates[0])?;
        let captured = service.state.listener("local", &id)?.ok_or("Missing pin")?;
        fs::write(dir.join("probe.txt"), "closed_definitive").map_err(|e| e.to_string())?;
        super::super::receiver::probe_listener(
            &service.state,
            service.slot("local")?,
            &captured,
            &plugin,
            &config.state_dir,
        )?;
        let closed = service
            .state
            .listener("local", &id)?
            .ok_or("Missing closed row")?;
        assert_eq!(closed["user_stopped"], 0);
        assert_eq!(closed["status"], "UNLISTENED");
        fs::write(dir.join("probe.txt"), "alive").map_err(|e| e.to_string())?;
        service.refresh_available()?;
        let restored = service
            .state
            .listener("local", &id)?
            .ok_or("Missing restored row")?;
        assert_eq!(restored["listen_mode"], "manual");
        assert_eq!(
            restored["last_activity_at_ms"],
            closed["last_activity_at_ms"]
        );
        service.action(
            &json!({"op":"set_listen_mode","hub":"local","listener_id":id,"listen_mode":"auto"}),
        )?;
        // A still-pending wake protects the listener even without a dispatch lease.
        service.state.db.execute(
            "UPDATE local_listeners SET last_activity_at_ms=? WHERE listener_id=?",
            &[json!(now_ms() - 3_600_001), json!(id)],
        )?;
        service.state.db.execute("UPDATE local_signals SET state='DISPATCHING',held_reason=NULL,acknowledged_at_ms=NULL,expires_at_ms=? WHERE signal_id='grace'",&[json!(now_ms()+30_000)])?;
        service.auto_listen()?;
        assert_eq!(
            service
                .state
                .listener("local", &id)?
                .ok_or("Missing pending row")?["status"],
            "ACTIVE"
        );
        service.state.db.execute("UPDATE local_signals SET state='DISPATCHED',acknowledged_at_ms=? WHERE signal_id='grace'",&[json!(now_ms())])?;
        service
            .plugins
            .plugins
            .iter_mut()
            .find(|p| p.id == "fixture")
            .ok_or("Missing plugin")?
            .auto_listen
            .idle_timeout_ms = None;
        service.auto_listen()?;
        assert_eq!(
            service
                .state
                .listener("local", &id)?
                .ok_or("Missing timeless row")?["status"],
            "ACTIVE"
        );
        for action in [
            json!({"op":"unlisten_all"}),
            json!({"op":"unlisten_all","hub":null}),
            json!({"op":"unlisten_all","hub":"local"}),
        ] {
            service.set_auto_listen("local", true)?;
            for candidate in &candidates[..2] {
                service.listen("local", &json!({"about":"Stop witness"}), candidate)?;
            }
            let auto_id = listener_id("none#alice", "fixture", "s1");
            service.action(&json!({"op":"set_listen_mode","hub":"local","listener_id":auto_id,"listen_mode":"auto"}))?;
            let expected = service
                .state
                .active()?
                .iter()
                .filter(|r| r["harness"] == "fixture")
                .count();
            assert!(expected >= 2);
            assert_eq!(service.action(&action)?["removed"], expected, "{action}");
            assert_eq!(service.action(&action)?["removed"], 0, "{action}");
            assert_eq!(service.slot("local")?.config["autoListen"], false);
            assert!(service
                .state
                .active()?
                .iter()
                .all(|r| r["harness"] != "fixture"));
            for session in ["s0", "s1"] {
                let row = service
                    .state
                    .listener("local", &listener_id("none#alice", "fixture", session))?
                    .ok_or("Missing stopped row")?;
                assert_eq!(row["user_stopped"], 1, "{action}");
            }
        }
        Ok(())
    })();
    if let Some(mut activity) = service.session_activity.take() {
        activity.shutdown()?;
    }
    drop(service);
    fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    result
}

#[test]
fn oidc_pre_expiry_recovery_preserves_slot_and_inventory_until_verified_replacement(
) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("cito-renewal-{}", random_uuid_v4()));
    fs::create_dir_all(dir.join("home")).map_err(|e| e.to_string())?;
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    fs::write(&config.config_path, "{}").map_err(|e| e.to_string())?;
    let mut service = DaemonService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        },
    )?;
    let raw = json!({"hubs":{"local":{"url":"local://"},"work":{"url":"http://127.0.0.1:1","authMode":"oidc","issuer":"https://id.example.com","clientId":"client","token":"old"}}});
    fs::write(&config.config_path, raw.to_string()).map_err(|e| e.to_string())?;
    let (_, hubs, _) = crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))?;
    let hub = hubs["work"].clone();
    let mut old = HubClient::new(
        "work".into(),
        hub.clone(),
        "alice",
        Some(service.local.clone()),
    );
    old.directory()?;
    old.local = None;
    old.expires_at = now_ms() / 1000 + 20;
    let identity = old.identity.clone();
    let closed = old.closed.clone();
    let rows = old.cached_directory();
    let inventory = Arc::new(ChannelInventory::new(remote_channels_identity(&old)));
    inventory.cached.lock().map_err(|e| e.to_string())?.value =
        json!({"channels":[{"id":"messenger","status":"ready"}]});
    service
        .channel_inventories
        .insert("work".into(), inventory.clone());
    service.slots.insert("work".into(), old);
    service
        .receivers
        .insert("auto:work".into(), thread::spawn(|| {}));
    let (tx, rx) = std::sync::mpsc::channel();
    service.oidc_recoveries.insert(
        "work".into(),
        OidcRecovery {
            config: hub.clone(),
            rejected: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            worker: thread::spawn(move || rx.recv().unwrap()),
        },
    );
    service.reload_slots()?;
    assert!(!closed.load(Ordering::SeqCst));
    assert_eq!(service.slots["work"].bearer.as_deref(), Some("old"));
    assert!(Arc::ptr_eq(
        &inventory,
        &service.channel_inventories["work"]
    ));
    assert_eq!(service.slots["work"].cached_directory().len(), rows.len());
    let token = format!(
        "header.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({"exp":now_ms()/1000+300}).to_string())
    );
    tx.send(Ok((token.clone(), identity)))
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(1);
    while !service.oidc_recoveries["work"].worker.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    service.reload_slots()?;
    assert_eq!(
        service.slots["work"].bearer.as_deref(),
        Some(token.as_str())
    );
    assert!(!closed.load(Ordering::SeqCst));
    assert!(Arc::ptr_eq(&closed, &service.slots["work"].closed));
    assert!(Arc::ptr_eq(
        &inventory,
        &service.channel_inventories["work"]
    ));
    assert_eq!(service.slots["work"].cached_directory().len(), rows.len());
    let expired = service
        .slots
        .get_mut("work")
        .ok_or("Missing renewed slot")?;
    expired.expires_at = now_ms() / 1000 - 1;
    expired.auth_retry_at = now_ms() + 5000;
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel = cancelled.clone();
    service.oidc_recoveries.insert(
        "work".into(),
        OidcRecovery {
            config: hub,
            rejected: None,
            cancelled,
            worker: thread::spawn(move || {
                while !cancel.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(1));
                }
                Err("Recovery cancelled".into())
            }),
        },
    );
    service.reload_slots()?;
    assert!(
        closed.load(Ordering::SeqCst),
        "Expiry must retire the stream even while renewal is pending"
    );
    assert!(service.slots["work"].cached_directory().is_empty());
    assert!(!service.channel_inventories.contains_key("work"));
    service.shutdown();
    fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    Ok(())
}

#[test]
fn channel_inventory_preserves_channels_on_transport_failure() -> Result<(), String> {
    let slot = HubClient::new(
        "work".into(),
        json!({"url":"http://127.0.0.1:0"}),
        "alice",
        None,
    );
    let inventory = ChannelInventory::new(remote_channels_identity(&slot));
    let channels = json!([{"id":"fixture","status":"ready","fields":[]}]);
    inventory.cached.lock().map_err(|e| e.to_string())?.value = json!({"channels":channels});
    for generation in 0..2 {
        assert!(inventory.refresh(&slot, generation).is_err());
        let cached = inventory.cached.lock().map_err(|e| e.to_string())?;
        assert_eq!(cached.value["channels"], channels);
        assert_eq!(cached.value["status"], "error");
        assert!(cached.value["detail"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
        assert_eq!(cached.completed_gen, generation + 1);
    }
    let fresh = ChannelInventory::new(remote_channels_identity(&slot));
    assert!(fresh.refresh(&slot, 0).is_err());
    let cached = fresh.cached.lock().map_err(|e| e.to_string())?;
    assert!(cached.value["channels"].is_null());
    assert_eq!(cached.value["status"], "error");
    Ok(())
}

#[test]
fn agent_stop_all_and_url_scoped_send_reservations_preserve_personal_state() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("cito-daemon-unit-{}", random_uuid_v4()));
    fs::create_dir_all(dir.join("home")).map_err(|e| e.to_string())?;
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    fs::write(&config.config_path, json!({"user":"alice","hubs":{"local":{"url":"local://"},"archive":{"url":"https://hub.example.com","enabled":false,"autoListen":true}}}).to_string()).map_err(|e| e.to_string())?;
    let plugins = LoadedPlugins {
        plugins: vec![],
        rejected_ids: HashSet::new(),
    };
    let mut service = DaemonService::open(&config, &plugins)?;
    service.local.set_plugin_roots(&[])?;
    let result = (|| {
        // Pause can precede local enrollment, even when the Hub already knows this Node.
        service
            .state
            .db
            .execute("DELETE FROM local_listeners WHERE harness=\'human\'", &[])?;
        let human_id = listener_id(
            string(&service.slot("local")?.identity, "principal_id"),
            "human",
            "default",
        );
        service.unlisten_id("local", &human_id)?;
        assert_eq!(
            service
                .state
                .listener("local", &human_id)?
                .ok_or("Missing pause placeholder")?["about"],
            "Human"
        );
        let updated=service.action(&json!({"op":"human_inbox_update","hub":"local","paused":true,"expected_control_revision":1,"about":"Release approvals","scope":"self"}))?;
        assert_eq!(updated["record"]["human_paused"], true);
        assert_eq!(
            service.snapshot()?["hubs"][0]["human_about"],
            "Release approvals"
        );
        assert_eq!(
            service.snapshot()?["hubs"][0]["human_state_status"],
            "current"
        );
        service.action(&json!({"op":"human_inbox_update","hub":"local","paused":false,"expected_control_revision":2}))?;
        assert_eq!(
            service.snapshot()?["hubs"][0]["human_about"],
            "Release approvals"
        );
        for name in ["cito_listen", "cito_unlisten"] {
            assert!(service
                .tool(
                    name,
                    &json!({"about":"Human"}),
                    &json!({"expected_harness":"human"})
                )
                .unwrap_err()
                .starts_with("FORBIDDEN:"));
        }
        for harness in ["human", "fixture"] {
            super::super::registration::register_listener(
                &service.state,
                service.slot("local")?,
                &service.host_id,
                &json!({"about":"Receiver","scope":"workspace"}),
                &json!({"harness":harness,"session_id":"default","title":"Receiver"}),
            )?;
        }
        let target = service
            .state
            .active()?
            .into_iter()
            .find(|row| row["harness"] == "fixture")
            .ok_or("Missing agent")?;
        let args =
            json!({"to":target["listener_id"],"message":"Review ready","idempotency_key":"same"});
        let first = service.send("local", &args, &json!({}))?;
        assert_eq!(
            service.send("local", &args, &json!({}))?["signal_id"],
            first["signal_id"]
        );
        // Keep the embedded transport hermetic while changing the target URL input
        // used by the real send reservation path under the same Hub name.
        service
            .slots
            .get_mut("local")
            .ok_or("Missing local slot")?
            .config["url"] = json!("https://hub.example.com");
        let second = service.send("local", &args, &json!({}))?;
        assert_ne!(second["signal_id"], first["signal_id"]);
        assert_eq!(
            service
                .state
                .db
                .query("SELECT * FROM local_sent_idempotency", &[])?
                .len(),
            2
        );
        service
            .slots
            .get_mut("local")
            .ok_or("Missing local slot")?
            .config["url"] = json!("local://");
        service.action(&json!({"op":"unlisten_all"}))?;
        let remaining = service.state.active()?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0]["harness"], "human");
        assert_eq!(
            service
                .slot("local")?
                .kv_get(string(&remaining[0], "listener_id"))?
                .ok_or("Missing personal registration")?
                .0["human_paused"],
            false
        );
        let settings = service.settings_for_hub("local");
        let disabled = settings["hubs"]
            .as_array()
            .ok_or("Missing hubs")?
            .iter()
            .find(|hub| hub["name"] == "archive")
            .ok_or("Missing disabled Hub")?;
        assert_eq!(disabled["user"], service.config.effective_user("archive")?);
        assert_eq!(
            disabled["auto_listen"], false,
            "Bulk stop disables even an empty disabled Hub"
        );
        let personal = remaining[0].clone();
        let personal_signal = service.send(
            "local",
            &json!({"to":personal["listener_id"],"message":"Unread personal notification"}),
            &json!({}),
        )?;
        service.listen(
            "local",
            &json!({"about":"Receiver"}),
            &json!({"harness":"fixture","session_id":"default","title":"Receiver"}),
        )?;
        let client = service.slot("local")?.clone();
        client.closed.store(true, Ordering::SeqCst);
        assert!(service
            .unlisten_id("local", string(&personal, "listener_id"))
            .is_err());
        assert!(service
            .unlisten_id("local", string(&target, "listener_id"))
            .is_err());
        assert_eq!(
            LocalState::open(&config.state_dir)?
                .db
                .query("SELECT * FROM pending_hub_detaches", &[])?
                .len(),
            2
        );
        assert_eq!(
            service
                .state
                .listener("local", string(&personal, "listener_id"))?
                .ok_or("Missing paused listener")?["status"],
            "UNLISTENED"
        );
        service.ensure_human_nodes()?; // Failed reconciliation retains local intent.
        client.closed.store(false, Ordering::SeqCst);
        assert_eq!(
            client
                .kv_get(string(&personal, "listener_id"))?
                .ok_or("Missing remote listener")?
                .0["human_paused"],
            false
        );
        service.ensure_human_nodes()?;
        assert_eq!(
            client
                .kv_get(string(&personal, "listener_id"))?
                .ok_or("Missing paused registration")?
                .0["human_paused"],
            false
        );
        assert!(service
            .state
            .db
            .query("SELECT * FROM pending_hub_detaches", &[])?
            .is_empty());
        assert_eq!(
            client
                .kv_get(string(&target, "listener_id"))?
                .ok_or("Missing agent registration")?
                .0["state"],
            "UNLISTENED"
        );
        assert!(service.local.rpc(
            "stream_get_message",
            &json!({"stream":"CITO_SIGNALS","seq":personal_signal["seq"]}),
            None
        )?["acked_at_ms"]
            .is_null());
        let other = LocalState::open(&dir.join("other-mac"))?;
        super::super::registration::register_listener(
            &other,
            &client,
            "other-mac",
            &json!({"about":"Receiver"}),
            &json!({"harness":"human","session_id":"default","title":"Receiver"}),
        )?;
        service.ensure_human_nodes()?;
        assert_eq!(
            client
                .kv_get(string(&personal, "listener_id"))?
                .ok_or("Missing personal registration")?
                .0["human_paused"],
            false,
            "Device detach and sibling attach must preserve shared inbox policy"
        );
        client.closed.store(true, Ordering::SeqCst);
        assert!(service
            .unlisten_id("local", string(&personal, "listener_id"))
            .is_err());
        client.closed.store(false, Ordering::SeqCst);
        assert!(service
            .state
            .db
            .query(
                "SELECT * FROM local_signals WHERE acknowledged_at_ms IS NOT NULL",
                &[]
            )?
            .is_empty());
        super::super::registration::register_listener(
            &service.state,
            &client,
            &service.host_id,
            &json!({"about":"Receiver"}),
            &json!({"harness":"human","session_id":"default","title":"Receiver"}),
        )?;
        service.ensure_human_nodes()?;
        assert_eq!(
            client
                .kv_get(string(&personal, "listener_id"))?
                .ok_or("Missing resumed registration")?
                .0["human_paused"],
            false
        );
        assert!(service
            .state
            .db
            .query("SELECT * FROM pending_hub_detaches", &[])?
            .is_empty());
        assert!(service.local.rpc(
            "stream_get_message",
            &json!({"stream":"CITO_SIGNALS","seq":personal_signal["seq"]}),
            None
        )?["acked_at_ms"]
            .is_null());
        let client = HubClient::new(
            "local".into(),
            service.slot("local")?.config.clone(),
            string(&personal, "owner_user"),
            Some(service.local.clone()),
        );
        let mut raw = service.raw_config()?;
        raw["hubs"]["local"]["enabled"] = json!(false);
        fs::write(&config.config_path, raw.to_string()).map_err(|e| e.to_string())?;
        service.reload_slots()?;
        assert!(!service.slots.contains_key("local"));
        assert!(service
            .unlisten_id("local", string(&personal, "listener_id"))
            .unwrap_err()
            .starts_with("DETACH_PENDING:"));
        assert_eq!(
            service
                .state
                .db
                .query("SELECT hub_url FROM pending_hub_detaches", &[])?[0]["hub_url"],
            "local://"
        );

        raw["hubs"]["local"]["url"] = json!("https://other.example.com");
        fs::write(&config.config_path, raw.to_string()).map_err(|e| e.to_string())?;
        let mut edited = client.clone();
        // Preserve the embedded transport as a sentinel: URL fencing must
        // reject the detach before it can mutate even an available Hub.
        edited.config["url"] = raw["hubs"]["local"]["url"].clone();
        DaemonService::reconcile_unlistened(&service.state, &edited, &service.host_id)?;
        assert!(service
            .state
            .db
            .query("SELECT * FROM pending_hub_detaches", &[])?
            .is_empty());
        assert_eq!(
            client
                .kv_get(string(&personal, "listener_id"))?
                .ok_or("Missing personal registration")?
                .0["human_paused"],
            false
        );

        // Historical unknown targets must never act as wildcard URLs.
        service.state.db.execute(
            "INSERT INTO pending_hub_detaches(hub,listener_id,hub_url) VALUES('local',?,NULL)",
            &[personal["listener_id"].clone()],
        )?;
        DaemonService::reconcile_unlistened(&service.state, &client, &service.host_id)?;
        assert!(service
            .state
            .db
            .query("SELECT * FROM pending_hub_detaches", &[])?
            .is_empty());
        assert_eq!(
            client
                .kv_get(string(&personal, "listener_id"))?
                .ok_or("Missing personal registration")?
                .0["human_paused"],
            false
        );

        // Rejected work was discarded. Restoring the original URL permits
        // a fresh explicit pause to reconcile against its captured target.
        raw["hubs"]["local"]["url"] = json!("local://");
        fs::write(&config.config_path, raw.to_string()).map_err(|e| e.to_string())?;
        assert!(service
            .unlisten_id("local", string(&personal, "listener_id"))
            .unwrap_err()
            .starts_with("DETACH_PENDING:"));
        assert_eq!(
            service
                .state
                .db
                .query("SELECT hub_url FROM pending_hub_detaches", &[])?[0]["hub_url"],
            "local://"
        );
        raw["hubs"]["local"]["enabled"] = json!(true);
        fs::write(&config.config_path, raw.to_string()).map_err(|e| e.to_string())?;
        service.reload_slots()?;
        assert_eq!(
            client
                .kv_get(string(&personal, "listener_id"))?
                .ok_or("Missing paused registration")?
                .0["human_paused"],
            false
        );
        assert!(service
            .state
            .db
            .query("SELECT * FROM pending_hub_detaches", &[])?
            .is_empty());
        assert!(service.local.rpc(
            "stream_get_message",
            &json!({"stream":"CITO_SIGNALS","seq":personal_signal["seq"]}),
            None
        )?["acked_at_ms"]
            .is_null());
        Ok(())
    })();
    service.shutdown();
    fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    result
}

#[test]
fn local_enrollment_widens_snapshot_addresses_without_a_directory_query() -> Result<(), String> {
    let caller = json!({"harness":"fixture","cwd":"/home/alice","title":"alice"});
    let home = std::path::Path::new("/home/alice");
    assert_eq!(
        default_session_title(&caller, home, "Fixture", "alice", ""),
        "Fixture"
    );
    assert_eq!(
        default_session_title(&caller, home, "Fixture", "alice", "Session"),
        "Fixture"
    );
    assert_eq!(
        default_session_title(&caller, home, "Fixture", "alice", "Release audit"),
        "Release audit"
    );
    let mut promoted = caller.clone();
    promoted["title"] = json!("Auth refactor");
    for cwd in ["/home/alice", "~", "/tmp"] {
        promoted["cwd"] = json!(cwd);
        for about in ["Auth refactor", "", "CLI session"] {
            assert_eq!(
                default_session_title(&promoted, home, "Fixture", "alice", about),
                "Auth refactor"
            );
        }
    }
    let other_home = std::path::Path::new("/home/account");
    for raw in [
        "",
        "alice",
        "@alice",
        "account",
        "@account",
        "Agent",
        "Agent session",
        "Session",
        "CLI session",
        "Human",
        "Human node",
        "Human channel",
    ] {
        let generic = json!({"harness":"fixture","cwd":"~","title":raw});
        for (about, expected) in [
            ("Auth refactor", "Auth refactor"),
            ("CLI session", "Fixture"),
            ("alice", "Fixture"),
            ("@alice", "Fixture"),
            ("account", "Fixture"),
            ("@account", "Fixture"),
        ] {
            let title = default_session_title(&generic, other_home, "Fixture", "alice", about);
            assert_eq!(title, expected, "raw={raw}, about={about}");
            let mut second_pass = generic.clone();
            second_pass["title"] = json!(title);
            assert_eq!(
                default_session_title(&second_pass, other_home, "Fixture", "alice", about),
                expected,
                "second pass: raw={raw}, about={about}"
            );
        }
    }
    let dir = std::env::temp_dir().join(format!("cito-addresses-{}", random_uuid_v4()));
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    let plugins = LoadedPlugins {
        plugins: vec![],
        rejected_ids: Default::default(),
    };
    let mut service = DaemonService::open(&config, &plugins)?;
    let result = (|| {
        // Model a post-RPC completion delayed across a real reconnect and a
        // replacement owner's failed revocation, without production hooks.
        {
            let caller =
                json!({"harness":"fixture","session_id":"late-cleanup","title":"Cleanup","cwd":""});
            let enrolled = service.listen("local", &json!({"about":"Cleanup"}), &caller)?;
            let id = string(&enrolled, "canonical_listener_id");
            let old = service.slot("local")?.clone();
            let prior = service
                .state
                .listener("local", id)?
                .ok_or("Missing old binding")?;
            let (mut remote, revision) = old.kv_get(id)?.ok_or("Missing old remote binding")?;
            remote["owner_token"] = json!("sibling-owner");
            remote["channels"]["sibling:fixture"] = json!({"channel_id":"sibling:fixture","kind":"fixture","host_id":"sibling","state":"ACTIVE","owner_token":"sibling-owner","priority":100,"updated_at_ms":now_ms()});
            old.kv_save(id, &remote, Some(revision))?;
            let remote = old.kv_get(id)?.ok_or("Missing Hub-stamped sibling")?.0;
            assert!(service.unlisten_id("local", id)?);
            let authority = service.state.registration_lock("local")?;
            assert!(Arc::ptr_eq(&old.registration, &authority));
            assert!(Arc::ptr_eq(
                &service.state.clone().registration_lock("local")?,
                &authority
            ));
            assert!(!Arc::ptr_eq(
                &service.state.registration_lock("other")?,
                &authority
            ));
            service.action(&json!({"op":"hub_action","hub":"local","action":"reconnect"}))?;
            let current = service.slot("local")?.clone();
            assert!(old.closed.load(Ordering::SeqCst));
            assert!(
                !Arc::ptr_eq(&old.closed, &current.closed),
                "Reconnect must actually replace the client"
            );
            assert!(
                Arc::ptr_eq(&current.registration, &authority),
                "Reconnect must preserve registration authority"
            );
            let guard = authority
                .lock()
                .map_err(|_| "Test registration lock poisoned")?;
            assert!(matches!(
                current.registration.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            drop(guard);
            service.listen("local", &json!({"about":"Replacement"}), &caller)?;
            let replacement = service
                .state
                .listener("local", id)?
                .ok_or("Missing replacement")?;
            assert_eq!(
                replacement["binding_generation"], prior["binding_generation"],
                "Live sibling retains the shared generation"
            );
            assert_ne!(replacement["owner_token"], prior["owner_token"]);
            // Fail the real reconciler's remote IO, retaining the newer intent.
            let backend =
                super::super::database::Database::open(&config.state_dir.join("hub.sqlite"))?;
            backend.execute("ALTER TABLE hub_kv RENAME TO unavailable_hub_kv", &[])?;
            let revoked = service.unlisten_id("local", id);
            backend.execute("ALTER TABLE unavailable_hub_kv RENAME TO hub_kv", &[])?;
            assert!(
                revoked.is_err(),
                "Explicit detach must report transport failure"
            );
            let newer = service
                .state
                .listener("local", id)?
                .ok_or("Missing newer revocation")?;
            assert_eq!(newer["status"], "UNLISTENED");
            assert_eq!(newer["owner_token"], replacement["owner_token"]);
            let generation = newer["binding_generation"]
                .as_u64()
                .ok_or("Missing generation")?;
            let pending = service.state.db.query(
                "SELECT * FROM pending_hub_detaches WHERE hub='local' AND listener_id=?",
                &[json!(id)],
            )?;
            assert_eq!(pending.len(), 1);
            service.state.clear_pending_detach_if_current(
                "local",
                id,
                prior["binding_generation"]
                    .as_u64()
                    .ok_or("Missing old generation")?,
                string(&prior, "owner_token"),
            )?;
            service.state.clear_pending_detach_if_current(
                "local",
                id,
                generation + 1,
                string(&newer, "owner_token"),
            )?;
            assert_eq!(
                service.state.db.query(
                    "SELECT * FROM pending_hub_detaches WHERE hub='local' AND listener_id=?",
                    &[json!(id)]
                )?,
                pending,
                "Old completion or wrong generation cannot erase newer cleanup"
            );
            let channel = format!("{}:fixture", service.host_id);
            assert_eq!(
                current.kv_get(id)?.ok_or("Missing orphan")?.0["channels"][&channel]["state"],
                "ACTIVE"
            );
            DaemonService::reconcile_unlistened(&service.state, &current, &service.host_id)?;
            assert!(service
                .state
                .db
                .query(
                    "SELECT 1 FROM pending_hub_detaches WHERE hub='local' AND listener_id=?",
                    &[json!(id)]
                )?
                .is_empty());
            let after = current.kv_get(id)?.ok_or("Missing cleaned Node")?.0;
            assert_eq!(after["channels"][&channel]["state"], "UNLISTENED");
            assert_eq!(
                after["channels"]["sibling:fixture"],
                remote["channels"]["sibling:fixture"]
            );
            assert_eq!(after["owner_token"], "sibling-owner");
        }
        for session in ["session-26", "session-324"] {
            service.listen(
                "local",
                &json!({"about":"Session"}),
                &json!({"harness":"fixture","session_id":session,"title":"Review","cwd":""}),
            )?;
        }
        let snapshot = service.snapshot()?;
        for (session, address) in [
            ("session-26", "alice/fixture#a5705540"),
            ("session-324", "alice/fixture#a5706e4d"),
        ] {
            let row = snapshot["listeners"]
                .as_array()
                .ok_or("Missing listeners")?
                .iter()
                .find(|row| row["session_id"] == session)
                .ok_or("Missing enrolled session")?;
            assert_eq!(row["display_address"], address);
            assert!(service
                .slot("local")?
                .cached_directory()
                .iter()
                .any(|peer| peer["listener_id"] == row["listener_id"]));
        }
        let id = listener_id("none#alice", "fixture", "session-324");
        for state in ["UNLISTENED", "ACTIVE"] {
            let slot = service.slot("local")?;
            let (mut stale, revision) = slot.kv_get(&id)?.ok_or("Missing collision peer")?;
            stale["state"] = json!(state);
            stale["updated_at_ms"] = json!(1);
            for channel in stale["channels"]
                .as_object_mut()
                .ok_or("Missing channels")?
                .values_mut()
            {
                channel["state"] = json!("UNLISTENED");
                channel["updated_at_ms"] = json!(1);
            }
            slot.kv_save(&id, &stale, Some(revision))?;
            slot.directory()?;
            let merged = address_peers(&service.state, slot)
                .into_iter()
                .find(|peer| peer["listener_id"] == id)
                .ok_or("Missing merged witness")?;
            assert_eq!(merged["channels"], stale["channels"]);
            assert!(super::super::identity::is_live_peer(&merged, now_ms()));
            let snapshot = service.snapshot()?;
            for (session, address) in [
                ("session-26", "alice/fixture#a5705540"),
                ("session-324", "alice/fixture#a5706e4d"),
            ] {
                assert_eq!(
                    snapshot["listeners"]
                        .as_array()
                        .ok_or("Missing listeners")?
                        .iter()
                        .find(|row| row["session_id"] == session)
                        .ok_or("Missing session")?["display_address"],
                    address
                );
            }
        }
        // A registration absent from the Hub cache still participates locally.
        service.local.rpc(
            "kv_delete",
            &json!({"key":id}),
            Some(&super::super::identity::principal("alice")),
        )?;
        service.slot("local")?.directory()?;
        let snapshot = service.snapshot()?;
        let first = snapshot["listeners"]
            .as_array()
            .ok_or("Missing listeners")?
            .iter()
            .find(|row| row["session_id"] == "session-26")
            .ok_or("Missing first session")?;
        assert_eq!(first["display_address"], "alice/fixture#a5705540");
        let activity = service.activity_dto(vec![json!({"hub":"local","listener_id":"missing","from_listener_id":"alice-abcd123456789012","from_user":"alice","from_harness":"fixture","message":"hello"})]);
        assert_eq!(activity[0]["from_display_address"], "alice/fixture#abcd");
        assert!(activity[0]["target_display_address"].is_null());
        // Exercise late revocation in both destination iteration orders without
        // a network server or sleeps: setup inspection performs the local transition.
        let other = Arc::new(HubEngine::open(&dir.join("other-hub"))?);
        let fanout_result = (|| -> Result<(), String> {
            service.slots.insert(
                "work".into(),
                HubClient::new(
                    "work".into(),
                    json!({"url":"local://"}),
                    "alice",
                    Some(other.clone()),
                ),
            );
            service
                .slots
                .get_mut("work")
                .ok_or("Missing test Hub")?
                .registration = service.state.registration_lock("work")?;
            let script = dir.join("fixture.py");
            fs::write(&script,r#"# purpose: drive discovery and late setup-time revocation in a unit sandbox.
# usage: python3 fixture.py with one NDJSON plugin request.
# effects: reads candidate/revocation files and updates only the sandbox state database.
# requires: Python 3, sqlite3, and a pre-created local state.
import json, pathlib, sqlite3, sys, time
root = pathlib.Path(__file__).parent
request = json.loads(sys.stdin.readline())
result = dict(ok=True)
if request['op'] == 'discover_sessions':
    candidates = json.loads((root / 'candidate.json').read_text())
    result['sessions'] = candidates if isinstance(candidates, list) else [candidates]
elif request['op'] == 'setup_inspect':
    marker = root / 'revoke.json'
    if marker.exists():
        revoke = json.loads(marker.read_text())
        with sqlite3.connect(root / 'state' / 'state.sqlite') as db:
            if 'endpoint_pid' in revoke:
                db.execute("UPDATE local_listeners SET endpoint_pid=?,updated_at=? WHERE hub=? AND harness='fixture' AND session_id=? AND status='ACTIVE'", (revoke['endpoint_pid'], time.time_ns() // 1000000, revoke['hub'], revoke['session_id']))
            else:
                db.execute("UPDATE local_listeners SET status='UNLISTENED',updated_at=? WHERE hub=? AND harness='fixture' AND session_id=? AND status='ACTIVE'", (time.time_ns() // 1000000, revoke['hub'], revoke['session_id']))
    result.update(installed=True, configured=True, paths=[])
print(json.dumps(result), flush=True)
"#).map_err(|e|e.to_string())?;
            service
                .plugins
                .plugins
                .push(crate::plugins::PluginDescriptor {
                    id: "fixture".into(),
                    kind: "agent-harness".into(),
                    display_name: "Fixture".into(),
                    tier: "test".into(),
                    order: 0,
                    sf_symbol: String::new(),
                    tint: String::new(),
                    cli_flag: false,
                    pid_bound: true,
                    auto_listen: Default::default(),
                    plugin_dir: dir.clone(),
                    is_builtin: false,
                    command: vec![
                        "/usr/bin/python3".into(),
                        script.to_string_lossy().into_owned(),
                    ],
                    channel_env: None,
                    fields: vec![],
                    post_setup_hint: None,
                    post_disconnect_hint: None,
                });
            service.invalidate_setup("fixture");
            settle_setup_inspections(&mut service);
            service.session_activity = Some(crate::core::session_activity::SessionActivity::new(
                &config,
                &service.plugins,
            ));
            let order: Vec<_> = service.slots.keys().cloned().collect();
            for revoked_hub in order {
                for slot in service.slots.values_mut() {
                    slot.config["autoListen"] = json!(false);
                }
                let session = format!("fanout-{revoked_hub}");
                let candidate = json!({"harness":"fixture","session_id":session,"title":"Review","cwd":"","endpoint_pid":std::process::id()});
                fs::write(dir.join("candidate.json"), candidate.to_string())
                    .map_err(|e| e.to_string())?;
                service.listen(&revoked_hub, &json!({"about":"Review"}), &candidate)?;
                service.refresh_available()?;
                if !service.session_activity.as_ref().is_some_and(|activity| {
                    activity
                        .candidates()
                        .any(|row| row["session_id"] == session)
                }) {
                    return Err("Expected a confirmed cached candidate".into());
                }
                fs::write(
                    dir.join("revoke.json"),
                    json!({"hub":revoked_hub,"session_id":session}).to_string(),
                )
                .map_err(|e| e.to_string())?;
                for slot in service.slots.values_mut() {
                    slot.config["autoListen"] = json!(true);
                }
                service.invalidate_setup("fixture");
                settle_setup_inspections(&mut service);
                service.auto_listen()?;
                let rows=service.state.db.query("SELECT hub,status FROM local_listeners WHERE harness='fixture' AND session_id=?",&[json!(session)])?;
                if rows.len() != 1
                    || rows[0]["status"] != "UNLISTENED"
                    || rows[0]["hub"] != revoked_hub
                {
                    return Err(format!(
                        "Late revocation must veto the entire candidate fan-out: {rows:?}"
                    ));
                }
                fs::remove_file(dir.join("revoke.json")).map_err(|e| e.to_string())?;
                service.auto_listen()?;
                if !service.state.db.query("SELECT 1 FROM local_listeners WHERE harness='fixture' AND session_id=? AND status='ACTIVE'",&[json!(session)])?.is_empty() { return Err("Stale presentation cannot regain enrollment authority".into()); }
                service.refresh_available()?;
                if service.state.db.query("SELECT 1 FROM local_listeners WHERE harness='fixture' AND session_id=? AND status='ACTIVE'",&[json!(session)])?.len()!=2 { return Err("New discovery must restore enrollment on both Hubs".into()); }
            }
            let p2 = crate::support::process_parent(std::process::id())
                .filter(|pid| *pid != std::process::id() && crate::support::alive(*pid))
                .ok_or("Missing second live PID")?;
            for (after_save, fail_detach, rebind) in [
                (false, false, false),
                (true, false, false),
                (true, true, false),
                (false, false, true),
                (true, false, true),
            ] {
                for slot in service.slots.values_mut() {
                    slot.config["autoListen"] = json!(false);
                }
                let session = if rebind {
                    if after_save {
                        "rebind-save"
                    } else {
                        "rebind-read"
                    }
                } else if fail_detach {
                    "inflight-retry"
                } else if after_save {
                    "inflight-save"
                } else {
                    "inflight-read"
                };
                let candidate = json!({"harness":"fixture","session_id":session,"title":"Review","cwd":"","endpoint_pid":std::process::id()});
                fs::write(dir.join("candidate.json"), candidate.to_string())
                    .map_err(|e| e.to_string())?;
                let initial = service.listen("local", &json!({"about":"Review"}), &candidate)?;
                service.refresh_available()?;
                let cached = service
                    .session_activity
                    .as_ref()
                    .and_then(|activity| {
                        activity
                            .candidates()
                            .find(|row| row["session_id"] == session)
                    })
                    .ok_or("Missing in-flight candidate")?;
                let discovered_at = cached["discovered_at"]
                    .as_u64()
                    .filter(|stamp| *stamp > 0)
                    .ok_or("Missing discovery timestamp")?;
                let a = service.slot("local")?.clone();
                let b = service.slot("work")?.clone();
                assert!(!Arc::ptr_eq(&a.registration, &b.registration));
                let state = service.state.clone();
                let id = string(&initial, "canonical_listener_id");
                let captured = state
                    .listener("local", id)?
                    .ok_or("Missing revocation source")?;
                let channel = format!("{}:fixture", service.host_id);
                if after_save {
                    // A sibling keeps the shared Node active: cleanup must fence the
                    // new channel token, not the Node's retained sibling token.
                    let mut sibling = a.kv_get(id)?.ok_or("Missing source Node")?.0;
                    sibling["owner_token"] = json!("sibling-owner");
                    sibling["channels"] = json!({"sibling:fixture":{"channel_id":"sibling:fixture","kind":"fixture","host_id":"sibling","state":"ACTIVE","owner_token":"sibling-owner","updated_at_ms":now_ms()}});
                    b.kv_save(id, &sibling, None)?;
                }
                let mut probe = service
                    .plugins
                    .find_by_id("fixture")
                    .ok_or("Missing probe fixture")?
                    .clone();
                probe.command = vec![
                    "/usr/bin/python3".into(),
                    "-c".into(),
                    "print('{\"ok\":true,\"status\":\"closed_definitive\"}')".into(),
                ];
                let writer = super::super::database::Database::open(&if after_save {
                    config.state_dir.join("state.sqlite")
                } else {
                    dir.join("other-hub/hub.sqlite")
                })?;
                // Only B is a destination, so stale-error handling cannot borrow
                // a later Hub's preflight veto to invalidate cached authority.
                for (hub, slot) in &mut service.slots {
                    slot.config["autoListen"] = json!(hub == "work");
                }
                let remote_db =
                    super::super::database::Database::open(&dir.join("other-hub/hub.sqlite"))?;
                let mut backend_unavailable = false;
                let daemon = &mut service;
                let outcome = std::thread::scope(|scope| -> Result<(), String> {
                    let pending=writer.transaction(|connection| {
                            let pending=scope.spawn(move ||daemon.auto_listen());
                            let deadline=Instant::now()+Duration::from_secs(3);
                            loop {
                                let entered=if after_save {
                                    b.kv_get(id)?.is_some_and(|(remote,_)|remote["channels"][&channel]["state"]=="ACTIVE")
                                } else { matches!(b.registration.try_lock(),Err(std::sync::TryLockError::WouldBlock)) };
                                if entered { break; }
                                if Instant::now()>=deadline { return Err("Registration did not reach the controlled RPC interval".into()); }
                                thread::sleep(Duration::from_millis(5));
                            }
                            if after_save {
                                // Commit after the Hub save while registration waits for
                                // its local write lock; model the inclusive same-ms cutoff.
                                let changed=connection.execute("UPDATE local_listeners SET status=?,endpoint_pid=?,updated_at=? WHERE hub='local' AND listener_id=? AND status='ACTIVE' AND binding_generation=? AND owner_token=?",&[json!(if rebind { "ACTIVE" } else { "UNLISTENED" }),json!(if rebind {p2} else {std::process::id()}),json!(discovered_at),json!(id),captured["binding_generation"].clone(),captured["owner_token"].clone()])?;
                                if changed!=1 { return Err("Expected a fenced cross-Hub revocation".into()); }
                                if fail_detach {
                                    // Preserve saved data while failing the actual cleanup RPC below
                                    // HubClient; no production hooks or network server are needed.
                                    remote_db.execute("ALTER TABLE hub_kv RENAME TO unavailable_hub_kv", &[])?;
                                    backend_unavailable=true;
                                }
                            } else if rebind {
                                state.pause_listener_for(&captured)?;
                                let mut rebound=candidate.clone();
                                rebound["endpoint_pid"]=json!(p2);
                                super::super::registration::register_listener(&state,&a,string(&captured,"host_id"),&json!({"about":"Rebound"}),&rebound)?;
                            } else {
                                super::super::receiver::probe_listener(&state,&a,&captured,&probe,&config.state_dir)?;
                            }
                            Ok(pending)
                        })?;
                    pending
                        .join()
                        .map_err(|_| "Auto-Listen worker panicked")??;
                    Ok(())
                });
                let toggled = if backend_unavailable {
                    let toggled = (|| -> Result<(), String> {
                        if b.kv_get(id).is_ok() {
                            return Err(
                                "Expected the Hub to remain unavailable during enable".into()
                            );
                        }
                        service.mutate_config(|raw| {
                            if raw["hubs"].is_null() {
                                raw["hubs"] = json!({"local":{"url":"local://"}});
                            }
                            raw["hubs"]["work"] = b.config.clone();
                            Ok(())
                        })?;
                        fs::write(dir.join("candidate.json"), "[]").map_err(|e| e.to_string())?;
                        assert!(
                            service.set_auto_listen("work", false).is_err(),
                            "Unavailable detach must remain visible"
                        );
                        assert_eq!(service.slot("work")?.config["autoListen"], false);
                        service.set_auto_listen("work", true)?;
                        service.refresh_available()?;
                        if state.db.query("SELECT 1 FROM pending_hub_detaches WHERE hub='work' AND listener_id=?",&[json!(id)])?.len()!=1 {
                                return Err("Enabling Auto-Listen lost cleanup for an undiscovered session".into());
                            }
                        if service.session_activity.as_ref().is_some_and(|activity| {
                            activity
                                .candidates()
                                .any(|row| row["session_id"] == session)
                        }) {
                            return Err("Empty discovery retained the revoked session".into());
                        }
                        Ok(())
                    })();
                    // Restore the backend even if the enable/discovery assertions fail.
                    remote_db.execute("ALTER TABLE unavailable_hub_kv RENAME TO hub_kv", &[])?;
                    toggled
                } else {
                    Ok(())
                };
                outcome.map_err(|e| format!("{session}: controlled auto-listen: {e}"))?;
                toggled.map_err(|e| format!("{session}: recovery toggles: {e}"))?;
                let revoked = state
                    .listener("local", id)?
                    .ok_or("Missing revoked source")?;
                assert_eq!(
                    revoked["status"],
                    if rebind { "ACTIVE" } else { "UNLISTENED" }
                );
                if rebind {
                    assert_eq!(revoked["endpoint_pid"], p2);
                }
                assert!(revoked["updated_at"]
                    .as_u64()
                    .is_some_and(|stamp| stamp >= discovered_at));
                if state
                    .listener("work", id)?
                    .is_some_and(|row| row["status"] == "ACTIVE")
                {
                    return Err("Stale discovery left an ACTIVE destination binding".into());
                }
                if fail_detach {
                    let retained = state.listener("work", id)?.ok_or("Missing retry owner")?;
                    let remote = b.kv_get(id)?.ok_or("Missing orphaned channel")?.0;
                    assert_eq!(retained["status"], "UNLISTENED");
                    assert_eq!(retained["binding_generation"], remote["binding_generation"]);
                    assert_eq!(
                        retained["owner_token"],
                        remote["channels"][&channel]["owner_token"]
                    );
                    assert_eq!(remote["channels"][&channel]["state"], "ACTIVE");
                    if state
                        .db
                        .query(
                            "SELECT 1 FROM pending_hub_detaches WHERE hub='work' AND listener_id=?",
                            &[json!(id)],
                        )?
                        .is_empty()
                    {
                        return Err("Failed compensation lost its durable retry intent".into());
                    }
                    DaemonService::reconcile_unlistened(&state, &b, &service.host_id)?;
                }
                if after_save {
                    assert!(state
                        .db
                        .query(
                            "SELECT 1 FROM pending_hub_detaches WHERE hub='work' AND listener_id=?",
                            &[json!(id)]
                        )?
                        .is_empty());
                    let remote = b.kv_get(id)?.ok_or("Missing compensated Node")?.0;
                    if remote["channels"][&channel]["state"] != "UNLISTENED"
                        || remote["channels"]["sibling:fixture"]["state"] != "ACTIVE"
                        || remote["channels"]["sibling:fixture"]["owner_token"] != "sibling-owner"
                    {
                        return Err("Compensation must detach only the stale channel and retain its sibling".into());
                    }
                    if !rebind {
                        super::super::receiver::detach_channel_expected(
                            &a, id, &channel, &captured,
                        )?;
                    }
                } else if b.kv_get(id)?.is_some() {
                    return Err("A pre-save veto must not create a remote Node".into());
                }
                if service.session_activity.as_ref().is_some_and(|activity| {
                    activity
                        .candidates()
                        .any(|row| row["session_id"] == session)
                }) {
                    return Err(
                        "STALE_DISCOVERY must invalidate cached enrollment authority".into(),
                    );
                }
                if !after_save && !rebind {
                    // Explicit Listen omits the private fence even when caller metadata
                    // came from discovery; a retained revocation must not veto intent.
                    service.listen("work", &json!({"about":"Manual intent"}), &cached)?;
                    assert_eq!(
                        state
                            .listener("work", id)?
                            .ok_or("Missing manual binding")?["status"],
                        "ACTIVE"
                    );
                    service.unlisten_id("work", id)?;
                }
                for slot in service.slots.values_mut() {
                    slot.config["autoListen"] = json!(false);
                }
                if fail_detach {
                    fs::write(dir.join("candidate.json"), candidate.to_string())
                        .map_err(|e| e.to_string())?;
                }
                if rebind {
                    let mut next = candidate.clone();
                    next["endpoint_pid"] = json!(p2);
                    fs::write(dir.join("candidate.json"), next.to_string())
                        .map_err(|e| e.to_string())?;
                }
                // Discovery start must be strictly later even at millisecond resolution.
                let cutoff = state.db.query("SELECT MAX(updated_at) AS cutoff FROM local_listeners WHERE harness='fixture' AND session_id=?", &[json!(session)])?.first().and_then(|row|row["cutoff"].as_u64()).unwrap_or(0);
                let deadline = Instant::now() + Duration::from_secs(1);
                while now_ms() <= cutoff {
                    if Instant::now() >= deadline {
                        return Err("Clock did not advance past enrollment cutoff".into());
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                service.refresh_available()?;
                let fresh = service
                    .session_activity
                    .as_ref()
                    .and_then(|activity| {
                        activity
                            .candidates()
                            .find(|row| row["session_id"] == session)
                    })
                    .ok_or("Missing fresh recovery candidate")?;
                assert!(fresh["discovered_at"].as_u64() > revoked["updated_at"].as_u64());
                // Re-enabling explicitly clears per-session Stop intent; a fresh
                // discovery alone no longer has that authority.
                let hubs: Vec<_> = service.slots.keys().cloned().collect();
                for hub in hubs {
                    service.set_auto_listen(&hub, true)?;
                }
                service.refresh_available()?;
                if rebind {
                    assert_eq!(
                        state
                            .listener("work", id)?
                            .ok_or("Missing fresh P2 binding")?["endpoint_pid"],
                        p2
                    );
                }
                for hub in ["local", "work"] {
                    assert_eq!(
                        state
                            .listener(hub, id)?
                            .ok_or("Missing recovered binding")?["status"],
                        "ACTIVE"
                    );
                }
                assert_eq!(
                    b.kv_get(id)?.ok_or("Missing recovered Node")?.0["channels"][&channel]["state"],
                    "ACTIVE"
                );
            }
            for mode in ["manual", "snapshot", "snapshot-direct", "preflight"] {
                for slot in service.slots.values_mut() {
                    slot.config["autoListen"] = json!(false);
                }
                let session = format!("active-{mode}");
                let candidate = json!({"harness":"fixture","session_id":session,"title":"Review","cwd":"","endpoint_pid":std::process::id()});
                fs::write(dir.join("candidate.json"), candidate.to_string())
                    .map_err(|e| e.to_string())?;
                let initial = service.listen("local", &json!({"about":"Review"}), &candidate)?;
                service.refresh_available()?;
                let cached = service
                    .session_activity
                    .as_ref()
                    .and_then(|activity| {
                        activity
                            .candidates()
                            .find(|row| row["session_id"] == session)
                    })
                    .ok_or("Missing P1 snapshot")?;
                let id = string(&initial, "canonical_listener_id");
                let captured = service
                    .state
                    .listener("local", id)?
                    .ok_or("Missing P1 binding")?;
                let mut rebound = candidate.clone();
                rebound["endpoint_pid"] = json!(p2);
                if mode == "manual" {
                    service.state.pause_listener_for(&captured)?;
                    assert!(
                        !service
                            .session_activity
                            .as_ref()
                            .is_some_and(|activity| activity
                                .candidates()
                                .any(|row| row["session_id"] == session)),
                        "Revocation must invalidate cache before the next tick"
                    );
                    // A successful explicit Listen must invalidate even a currently
                    // confirmed inventory, independently of the prior pause event.
                    service.refresh_available()?;
                    assert!(service
                        .session_activity
                        .as_ref()
                        .is_some_and(|activity| activity
                            .candidates()
                            .any(|row| row["session_id"] == session)));
                    service.listen("local", &json!({"about":"Rebound"}), &rebound)?;
                } else if mode.starts_with("snapshot") {
                    // A separate state handle exercises the durable SQL fence without
                    // relying on the daemon's in-memory invalidation notification.
                    let independent = LocalState::open(&config.state_dir)?;
                    if mode == "snapshot" {
                        independent.pause_listener_for(&captured)?;
                    }
                    super::super::registration::register_listener(
                        &independent,
                        service.slot("local")?,
                        &service.host_id,
                        &json!({"about":"Rebound"}),
                        &rebound,
                    )?;
                    assert!(service
                        .session_activity
                        .as_ref()
                        .is_some_and(|activity| activity
                            .candidates()
                            .any(|row| row["session_id"] == session)));
                } else {
                    // Setup runs after the initial snapshot and before live preflight.
                    fs::write(
                        dir.join("revoke.json"),
                        json!({"hub":"local","session_id":session,"endpoint_pid":p2}).to_string(),
                    )
                    .map_err(|e| e.to_string())?;
                }
                for (hub, slot) in &mut service.slots {
                    slot.config["autoListen"] = json!(hub == "work");
                }
                if mode == "preflight" {
                    service.invalidate_setup("fixture");
                    settle_setup_inspections(&mut service);
                }
                service.auto_listen()?;
                if mode == "preflight" {
                    fs::remove_file(dir.join("revoke.json")).map_err(|e| e.to_string())?;
                }
                let active = service
                    .state
                    .listener("local", id)?
                    .ok_or("Missing P2 binding")?;
                assert_eq!(active["status"], "ACTIVE");
                assert_eq!(active["endpoint_pid"], p2);
                assert!(active["updated_at"].as_u64() >= cached["discovered_at"].as_u64());
                if service.state.listener("work", id)?.is_some() {
                    return Err("Cached P1 crossed an ACTIVE P2 rebind".into());
                }
                if service.session_activity.as_ref().is_some_and(|activity| {
                    activity
                        .candidates()
                        .any(|row| row["session_id"] == session)
                }) {
                    return Err("Rebind did not invalidate P1 discovery".into());
                }
                for slot in service.slots.values_mut() {
                    slot.config["autoListen"] = json!(false);
                }
                fs::write(dir.join("candidate.json"), rebound.to_string())
                    .map_err(|e| e.to_string())?;
                let deadline = Instant::now() + Duration::from_secs(1);
                while now_ms() <= active["updated_at"].as_u64().unwrap_or(0) {
                    if Instant::now() >= deadline {
                        return Err("Clock did not advance past rebind cutoff".into());
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                service.refresh_available()?;
                service
                    .slots
                    .get_mut("work")
                    .ok_or("Missing destination")?
                    .config["autoListen"] = json!(true);
                service.auto_listen()?;
                assert_eq!(
                    service
                        .state
                        .listener("work", id)?
                        .ok_or("Missing P2 enrollment")?["endpoint_pid"],
                    p2
                );
                assert_eq!(
                    service
                        .state
                        .listener("work", id)?
                        .ok_or("Missing P2 enrollment")?["status"],
                    "ACTIVE"
                );
            }
            Ok(())
        })();
        other.shutdown();
        fanout_result?;
        Ok(())
    })();
    service.shutdown();
    fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    result
}
#[test]
fn sender_addresses_use_readable_metadata_before_directory_arrival() {
    for (hub, expected) in [
        ("local", "alice/fixture#abcd"),
        ("work", "work:alice/fixture#abcd"),
    ] {
        assert_eq!(
            sender_display_address(
                hub,
                "local",
                Some("alice-abcd123456789012"),
                "alice",
                "fixture",
                &[]
            )
            .as_deref(),
            Some(expected)
        );
    }
    assert_eq!(
        sender_display_address("local", "local", None, "alice", "fixture", &[]),
        None
    );
    assert_eq!(
        sender_display_address("local", "local", None, "alice", "human", &[]).as_deref(),
        Some("@alice")
    );
}
#[test]
fn default_hub_repair_handles_missing_defaults_and_all_disabled_hubs() {
    for (mut config, expected) in [
        (
            json!({"hubs":{"alpha":{"enabled":true},"local":{"enabled":true}}}),
            "local",
        ),
        (
            json!({"defaultHub":"zeta","hubs":{"alpha":{},"local":{},"zeta":{}}}),
            "zeta",
        ),
        (
            json!({"default_hub":"missing","hubs":{"alpha":{"enabled":false},"local":{"disabled":true}}}),
            "local",
        ),
        (
            json!({"defaultHub":"missing","hubs":{"zeta":{"enabled":false},"alpha":{"disabled":true}}}),
            "alpha",
        ),
        (json!({"hubs":{}}), "local"),
    ] {
        repair_default_hub(&mut config);
        assert_eq!(config["default_hub"], expected);
        assert!(config.get("defaultHub").is_none());
    }
}
#[test]
fn config_mutation_rejects_non_objects_under_lock() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("cito-config-shape-{}", random_uuid_v4()));
    let result = (|| {
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let config = NativeConfig {
            state_dir: dir.join("state"),
            config_path: dir.join("config.json"),
            home_dir: dir.join("home"),
            daemon_port: 0,
            user: "alice".into(),
            default_hub: "local".into(),
        };
        let plugins = LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        };
        let mut service = DaemonService::open(&config, &plugins)?;
        let result = (|| {
            for raw in ["[]", "42", "true", "\"config\"", "null"] {
                fs::write(&config.config_path, raw).map_err(|e| e.to_string())?;
                assert_eq!(
                    service.mutate_config(|v| {
                        v["user"] = json!("bob");
                        Ok(())
                    }),
                    Err("INVALID_INPUT: Config must be an object".into())
                );
                assert_eq!(
                    fs::read_to_string(&config.config_path).map_err(|e| e.to_string())?,
                    raw
                );
            }
            let before = r#"{"other":7}"#;
            fs::write(&config.config_path, before).map_err(|e| e.to_string())?;
            for candidate in [
                json!({"hubs":[]}),
                json!({"hubs":{"work":[]}}),
                json!({"hubs":{"work":{"authMode":"invalid"}}}),
                json!({"hubs":{"work":{"enabled":false,"user":"alice smith"}}}),
                json!({"default_hub":"missing"}),
                json!({"defaultHub":"missing"}),
                json!({"defaultHub":"local","default_hub":"missing"}),
                json!({"default_hub":"local","defaultHub":"missing"}),
                json!({"default_hub":42}),
                json!({"defaultHub":null}),
                json!({"user":"alice smith"}),
                json!({"user":42}),
            ] {
                let error = service
                    .mutate_config(|v| {
                        *v = candidate;
                        Ok(())
                    })
                    .unwrap_err();
                assert!(error.starts_with("INVALID_INPUT:"), "{error}");
                assert_eq!(
                    fs::read_to_string(&config.config_path).map_err(|e| e.to_string())?,
                    before
                );
            }
            service.mutate_config(|v| {
                v["user"] = json!("bob");
                Ok(())
            })?;
            assert_eq!(service.raw_config()?, json!({"other":7,"user":"bob"}));
            for key in ["defaultHub", "default_hub"] {
                let mut candidate = json!({"hubs":{"work":{"url":"local://"}}});
                candidate[key] = json!("work");
                service.mutate_config(|v| {
                    *v = candidate.clone();
                    Ok(())
                })?;
                assert_eq!(service.raw_config()?, candidate);
            }
            for (requested, url, expected) in [
                ("Office Mac", "https://office.example.com", "office-mac"),
                ("Work", "local://", "work"),
                ("", "https://node.example.com", "node"),
                ("", "https://node.other.example.com", "node-2"),
                ("", "https://local.example.com", "local-2"),
                ("", "http://192.0.2.8:4291", "remote"),
                ("", "http://[::1]:4291", "remote-2"),
                ("", "http://192.168.1.2:4291", "lan"),
                ("", "http://10.1.2.3:4291", "lan-2"),
                ("", "http://172.16.0.2:4291", "lan-3"),
                ("", "http://100.64.0.2:4291", "tailscale"),
            ] {
                service.action(
                    &json!({"op":"setup_save_hub","name":requested,"url":url,"enabled":false}),
                )?;
                assert_eq!(service.raw_config()?["hubs"][expected]["url"], url);
            }
            service.mutate_config(|v| {
                v["host_hub"] = json!({"bind":"::1","enabled":false});
                Ok(())
            })?;
            let settings = service.settings();
            assert_eq!(settings["host_hub"]["endpoint_url"], "http://[::1]:4291");
            assert_eq!(settings["host_hub"]["lan_url"], "http://[::1]:4291");
            assert!(settings["host_hub"]["lan_invite_command"]
                .as_str()
                .unwrap()
                .contains("--url http://[::1]:4291"));
            service.action(
                &json!({"op":"setup_save_hub","name":"user","url":"local://","enabled":false}),
            )?;
            assert!(service.action(&json!({"op":"setup_save_hub","edit":true,"original_name":"user","name":"user","url":"http://"})).is_err());
            let user_error = service.last_hub_errors["user"].clone();
            for requested in ["!!!", "local", "Office Mac"] {
                let before = fs::read(&config.config_path).map_err(|e| e.to_string())?;
                assert!(service.action(&json!({"op":"setup_save_hub","name":requested,"url":"https://different.example.com","enabled":false})).is_err());
                assert_eq!(
                    fs::read(&config.config_path).map_err(|e| e.to_string())?,
                    before
                );
            }
            assert_eq!(service.last_hub_errors["user"], user_error);
            assert!(service.action(&json!({"op":"setup_save_hub","edit":true,"original_name":"work","name":"work","url":"http://"})).is_err());
            let work_error = service.last_hub_errors["work"].clone();
            for (op, requested, canonical, url) in [
                (
                    "setup_save_hub",
                    Some("Office Mac"),
                    "office-mac",
                    "https://office.example.com",
                ),
                ("save_hub", None, "node-2", "https://node.other.example.com"),
            ] {
                assert!(service.action(&json!({"op":op,"edit":true,"original_name":canonical,"name":canonical,"url":"http://"})).is_err());
                let failed = service.settings_for_hub(canonical);
                assert!(failed["hubs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|hub| hub["name"] == canonical)
                    .unwrap()["error"]
                    .is_string());
                let mut retry = json!({"op":op,"url":url,"enabled":false});
                if let Some(requested) = requested {
                    retry["name"] = json!(requested);
                }
                assert_eq!(service.action(&retry)?, json!({"ok":true}));
                let recovered = service.settings_for_hub(canonical);
                assert!(recovered["hubs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|hub| hub["name"] == canonical)
                    .unwrap()["error"]
                    .is_null());
                assert_eq!(service.last_hub_errors["work"], work_error);
            }
            service.action(&json!({"op":"setup_save_hub","edit":true,"original_name":"office-mac","name":"Office Mac","url":"https://updated.example.com","enabled":false}))?;
            assert_eq!(
                service.raw_config()?["hubs"]["office-mac"]["url"],
                "https://updated.example.com"
            );
            Ok(())
        })();
        service.shutdown();
        result
    })();
    fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    result
}
#[test]
fn ui_acknowledgements_preserve_other_hubs_with_colliding_signal_ids() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("cito-ack-scope-{}", random_uuid_v4()));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    fs::write(
        &config.config_path,
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":"local://","enabled":false}}})
            .to_string(),
    )
    .map_err(|e| e.to_string())?;
    let plugins = LoadedPlugins {
        plugins: vec![],
        rejected_ids: Default::default(),
    };
    let mut service = DaemonService::open(&config, &plugins)?;
    let result = (|| {
        for hub in ["local", "work"] {
            service.state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) VALUES(?,'shared','node',1,1,'alice','host','','message','now',?,'RECEIVED')", &[json!(hub),json!(now_ms()+60_000)])?;
        }
        service.action(&json!({"op":"ack_signals","hub":"local","ack_all":true}))?;
        let state = service.state.clone();
        let unread = || {
            state.db.query(
                "SELECT hub FROM local_signals WHERE acknowledged_at_ms IS NULL",
                &[],
            )
        };
        assert_eq!(unread()?, vec![json!({"hub":"work"})]);
        service.action(&json!({"op":"ack_signals","signal_ids":["work:shared"]}))?;
        assert!(unread()?.is_empty());
        for (id, message) in [("matching", "review ready"), ("unmatched", "build passed")] {
            state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) VALUES('local',?,'node',2,1,'alice','host','',?,'now',?,'RECEIVED')", &[json!(id),json!(message),json!(now_ms()+60_000)])?;
        }
        let (_, matched, _) = state.query_activity("all", " review ")?;
        assert_eq!(matched, 1);
        service.action(&json!({"op":"ack_signals","ack_all":true,"query":" review "}))?;
        let remaining = state.db.query(
            "SELECT signal_id FROM local_signals WHERE acknowledged_at_ms IS NULL",
            &[],
        )?;
        assert_eq!(remaining, vec![json!({"signal_id":"unmatched"})]);
        service.action(&json!({"op":"ack_signals","signal_ids":["unmatched"],"query":"review"}))?;
        assert!(unread()?.is_empty());

        let client = service.slot("local")?.clone();
        let human = super::super::registration::local_node(&state, &client, "human", "default")?
            .ok_or("Missing Human")?;
        let id = string(&human, "listener_id");
        let channel = format!("{}:human", service.host_id);
        let hub_db = super::super::database::Database::open(&config.state_dir.join("hub.sqlite"))?;
        for (age, expected_heartbeats) in [(0, 0), (21_000, 1), (0, 0)] {
            let mut remote = client.kv_get(id)?.ok_or("Missing remote Human")?.0;
            remote["channels"][&channel]["updated_at_ms"] = json!(now_ms() - age);
            hub_db.execute(
                "UPDATE hub_kv SET value=CAST(? AS BLOB) WHERE key=?",
                &[json!(remote.to_string()), json!(id)],
            )?;
            client.directory()?;
            let count = || -> Result<u64, String> {
                Ok(hub_db.query(
                    "SELECT COUNT(*) AS n FROM hub_audit_log WHERE action='human_device_heartbeat'",
                    &[],
                )?[0]["n"]
                    .as_u64()
                    .unwrap_or(0))
            };
            let before = count()?;
            service.poll()?;
            for (_, task) in service.receivers.drain() {
                task.join().map_err(|_| "Receiver panicked")?;
            }
            assert_eq!(
                count()? - before,
                expected_heartbeats,
                "Heartbeat age {age}"
            );
        }

        // Settings can arrive before Directory, and must retain the inventory's policy.
        let remote = HubClient::new(
            "local".into(),
            json!({"url":"https://hub.example.com","authMode":"token"}),
            "alice",
            None,
        );
        let inventory = Arc::new(ChannelInventory::new(remote_channels_identity(&remote)));
        inventory.cached.lock().map_err(|e| e.to_string())?.value = json!({"channels":[{"id":"messenger","enabled":true,"configured":true,"inbox_paused":true,"delivery_enabled":false}]});
        service
            .channel_inventories
            .insert("local".into(), inventory);
        if let Some(old) = service.slots.insert("local".into(), remote) {
            old.close();
        }
        assert_eq!(
            service.human_projection("local")["human_state_status"],
            "unknown"
        );
        let settings = service.settings_for_hub("local");
        assert_eq!(settings["channels"][0]["inbox_paused"], true);
        assert_eq!(settings["channels"][0]["delivery_enabled"], false);
        Ok(())
    })();
    service.shutdown();
    fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    result
}
#[test]
fn invalid_host_hub_reload_preserves_running_server() -> Result<(), String> {
    for (status, expected) in [
        (
            json!({"BackendState":"Running","Self":{"DNSName":"mac-studio.tailnet.ts.net.","TailscaleIPs":["100.64.0.2"]}}),
            Some("mac-studio.tailnet.ts.net"),
        ),
        (
            json!({"BackendState":"Running","Self":{"DNSName":"","TailscaleIPs":["fd7a:115c:a1e0::2","100.64.0.2"]}}),
            Some("100.64.0.2"),
        ),
        (
            json!({"BackendState":"Stopped","Self":{"DNSName":"offline.tailnet.ts.net."}}),
            None,
        ),
        (
            json!({"BackendState":"Running","Self":{"TailscaleIPs":["fd7a:115c:a1e0::2"]}}),
            None,
        ),
    ] {
        assert_eq!(tailscale_host_from_status(&status).as_deref(), expected);
        if status["BackendState"] == "Running" {
            let mut offline = status.clone();
            offline["Self"]["Online"] = json!(false);
            assert_eq!(tailscale_host_from_status(&offline), None);
            for magic in [false, true] {
                let mut candidate = status.clone();
                candidate["CurrentTailnet"] = json!({"MagicDNSEnabled":magic});
                candidate["Self"]["DNSName"] = json!("studio.local.");
                candidate["Self"]["TailscaleIPs"] = json!(["100.64.0.9"]);
                assert_eq!(
                    tailscale_host_from_status(&candidate).as_deref(),
                    Some("100.64.0.9")
                );
                candidate["Self"]["DNSName"] = json!("studio.tailnet.ts.net.");
                assert_eq!(
                    tailscale_host_from_status(&candidate).as_deref(),
                    Some(if magic {
                        "studio.tailnet.ts.net"
                    } else {
                        "100.64.0.9"
                    })
                );
            }
        }
    }
    assert_eq!(advertised_hub_host("127.0.0.1"), "127.0.0.1");
    let dir = std::env::temp_dir().join(format!("cito-host-validation-{}", random_uuid_v4()));
    let result = (|| {
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let config = NativeConfig {
            state_dir: dir.join("state"),
            config_path: dir.join("config.json"),
            home_dir: dir.join("home"),
            daemon_port: 0,
            user: "alice".into(),
            default_hub: "local".into(),
        };
        let plugins = LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        };
        let mut service = DaemonService::open(&config, &plugins)?;
        let result = (|| {
            let host = json!({"enabled":true,"bind":"127.0.0.1","port":0,"auth_mode":"token","token":"previous-token"});
            let mut committed = json!({"host_hub":host,"other":7}).to_string();
            fs::write(&config.config_path, &committed).map_err(|e| e.to_string())?;
            service.reload_slots()?;
            let address = service.host_server.as_ref().unwrap().address;
            service.reload_slots()?;
            assert_eq!(service.host_server.as_ref().unwrap().address, address);
            let rpc = |address, token| -> Result<(), String> {
                let response: Value = ureq::post(&format!("http://{address}/v1/rpc"))
                    .set("Authorization", &format!("Bearer {token}"))
                    .timeout(Duration::from_secs(2))
                    .send_json(json!({"op":"kv_snapshot"}))
                    .map_err(|e| e.to_string())?
                    .into_json()
                    .map_err(|e| e.to_string())?;
                assert_eq!(response["ok"], true);
                assert!(response["result"]["entries"].is_array());
                Ok(())
            };
            let occupied = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
            let occupied_port = occupied.local_addr().map_err(|e| e.to_string())?.port();
            for (key, value) in [
                ("auth_mode", json!("invalid")),
                ("token", json!(" ")),
                ("port", json!(70000)),
                ("bind", json!("")),
                ("bind", json!("127.0.0.1\u{0000}")),
                ("bind", json!("invalid-bind-host")),
                ("bind", json!("not:an:ip:address")),
                ("port", json!(occupied_port)),
            ] {
                let mut invalid = host.clone();
                invalid[key] = value.clone();
                let reload = json!({"host_hub":invalid}).to_string();
                fs::write(&config.config_path, &reload).map_err(|e| e.to_string())?;
                service.reload_slots()?;
                let settings = service.settings();
                let error = settings["host_hub"]["error"].as_str().unwrap();
                assert!(error.starts_with("INVALID_INPUT:"), "{error}");
                assert_eq!(settings["host_hub"]["running"], true);
                assert_eq!(settings["host_hub"]["port"], address.port());
                assert_eq!(service.host_server.as_ref().unwrap().address, address);
                assert_eq!(
                    fs::read(&config.config_path).map_err(|e| e.to_string())?,
                    reload.as_bytes()
                );
                rpc(address, "previous-token")?;

                // Restoring the exact applied config clears reload feedback without rebinding.
                fs::write(&config.config_path, &committed).map_err(|e| e.to_string())?;
                service.reload_slots()?;
                assert_eq!(service.settings()["host_hub"]["error"], Value::Null);
                assert_eq!(service.host_server.as_ref().unwrap().address, address);
                rpc(address, "previous-token")?;

                let mut action = json!({"op":"set_host_hub","enabled":true});
                action[key] = value;
                let error = service.action(&action).unwrap_err();
                assert!(error.starts_with("INVALID_INPUT:"), "{action}: {error}");
                assert_eq!(service.settings()["host_hub"]["error"], error);
                // An unchanged config cannot resolve a rejected UI action.
                service.reload_slots()?;
                assert_eq!(service.settings()["host_hub"]["error"], error);
                assert_eq!(
                    fs::read(&config.config_path).map_err(|e| e.to_string())?,
                    committed.as_bytes()
                );
                assert_eq!(service.host_server.as_ref().unwrap().address, address);
                rpc(address, "previous-token")?;
            }

            // A valid external edit supersedes the reload/action error without
            // abandoning the current socket or its working credentials.
            let mut valid = host.clone();
            valid["port"] = json!(address.port());
            committed = json!({"host_hub":valid,"other":7}).to_string();
            fs::write(&config.config_path, &committed).map_err(|e| e.to_string())?;
            service.reload_slots()?;
            assert_eq!(service.settings()["host_hub"]["error"], Value::Null);
            assert_eq!(service.host_server.as_ref().unwrap().address, address);
            rpc(address, "previous-token")?;

            // A write failure after binding must restore the old socket and credentials.
            let tmp = config
                .config_path
                .with_extension(format!("json.{}.tmp", std::process::id()));
            fs::create_dir(&tmp).map_err(|e| e.to_string())?;
            let result =
                service.action(&json!({"op":"set_host_hub","port":0,"token":"uncommitted-token"}));
            fs::remove_dir(&tmp).map_err(|e| e.to_string())?;
            assert!(result.is_err());
            assert_eq!(
                fs::read(&config.config_path).map_err(|e| e.to_string())?,
                committed.as_bytes()
            );
            assert_eq!(service.host_server.as_ref().unwrap().address, address);
            rpc(address, "previous-token")?;

            // Updating credentials on the same socket must release and rebind that port.
            service.action(
                &json!({"op":"set_host_hub","port":address.port(),"token":"replacement-token"}),
            )?;
            assert_eq!(service.host_server.as_ref().unwrap().address, address);
            rpc(address, "replacement-token")?;
            let rejected = ureq::post(&format!("http://{address}/v1/rpc"))
                .set("Authorization", "Bearer previous-token")
                .timeout(Duration::from_secs(2))
                .send_json(json!({"op":"kv_snapshot"}));
            assert!(matches!(rejected, Err(ureq::Error::Status(401, _))));

            // Saving the current port still sends an explicit settings acknowledgement.
            let settings = service.settings();
            let ui = Arc::new(UiMailbox::default());
            service.ui = Some(ui.clone());
            service.action(&json!({"op":"set_host_hub","port":address.port()}))?;
            {
                let frames = ui.pending.lock().map_err(|e| e.to_string())?;
                assert!(frames.iter().any(|(_, bytes)| {
                    let frame: Value = serde_json::from_slice(bytes).unwrap();
                    frame["op"] == "settings" && frame["settings"] == settings
                }));
            }

            // Port zero requests a distinct listener while the old socket is still held.
            service.action(&json!({"op":"set_host_hub","port":0}))?;
            let replacement = service.host_server.as_ref().unwrap().address;
            assert_ne!(replacement, address);
            rpc(replacement, "replacement-token")?;
            fs::write(
                &config.config_path,
                json!({"host_hub":{"enabled":false}}).to_string(),
            )
            .map_err(|e| e.to_string())?;
            service.reload_slots()?;
            assert!(service.host_server.is_none());
            assert!(TcpStream::connect(replacement).is_err());
            Ok(())
        })();
        service.shutdown();
        result
    })();
    fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    result
}
#[test]
fn packaged_app_resolves_ui_and_honors_overrides() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("cito-ui-layout-{}", random_uuid_v4()));
    let root = dir.join("Cito.app/Contents");
    let result = (|| {
        fs::create_dir_all(root.join("Helpers")).map_err(|e| e.to_string())?;
        let bin = root.join("Helpers/cito-ui");
        fs::write(&bin, "").map_err(|e| e.to_string())?;
        assert_eq!(resolve_ui_binary(&root, None), Some(bin));
        let override_bin = dir.join("custom-ui");
        fs::write(&override_bin, "").map_err(|e| e.to_string())?;
        assert_eq!(
            resolve_ui_binary(&root, Some(override_bin.clone())),
            Some(override_bin)
        );
        Ok(())
    })();
    let _ = fs::remove_dir_all(dir);
    result
}
#[test]
fn remediation_http_config_dtos_and_committed_receipts() -> Result<(), String> {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    let dir = std::env::temp_dir().join(format!("cito-boundary-{}", random_uuid_v4()));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join("managed.json");
    fs::write(&target, "{}").map_err(|e| e.to_string())?;
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    std::os::unix::fs::symlink(&target, &config.config_path).map_err(|e| e.to_string())?;
    let plugins = LoadedPlugins {
        plugins: vec![],
        rejected_ids: Default::default(),
    };
    let mut service = DaemonService::open(&config, &plugins)?;
    let result = (|| -> Result<(), String> {
        // Publication must follow a dangling symlink instead of replacing it.
        fs::remove_file(&target).map_err(|e| e.to_string())?;
        service.mutate_config(|value| {
            value["user"] = json!("bob");
            Ok(())
        })?;
        assert!(fs::symlink_metadata(&config.config_path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink());
        let stored: Value =
            serde_json::from_str(&fs::read_to_string(&target).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        assert_eq!(stored["user"], "bob");
        // Resolve directory symlinks before interpreting a relative parent.
        fs::remove_file(&config.config_path).map_err(|e| e.to_string())?;
        fs::create_dir_all(dir.join("real/sub")).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink("real/sub", dir.join("linkdir")).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink("linkdir/../settings.json", &config.config_path)
            .map_err(|e| e.to_string())?;
        assert!(!dir.join("real/settings.json").exists());
        service.mutate_config(|value| {
            value["user"] = json!("charlie");
            Ok(())
        })?;
        let stored: Value = serde_json::from_str(
            &fs::read_to_string(&config.config_path).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        assert_eq!(stored["user"], "charlie");
        assert!(dir.join("real/settings.json").is_file());
        assert!(!dir.join("settings.json").exists());
        assert!(fs::symlink_metadata(&config.config_path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink());
        // Local state contains ownership secrets; every exported session/activity
        // row must cross the DTO projection even in lightweight status updates.
        let active = service.state.active()?;
        assert!(!active.is_empty());
        let snapshot = service.snapshot()?;
        for key in ["listeners", "sessions"] {
            assert!(!snapshot[key].as_array().unwrap().is_empty());
            for row in snapshot[key].as_array().unwrap() {
                assert!(row.get("owner_token").is_none());
                assert!(row.get("dispatcher_token").is_none());
                assert!(row.get("hub_url").is_none());
                assert_eq!(row["is_self_principal"], true);
                assert_eq!(row["can_set_scope"], false);
                for control in ["can_pin", "can_rename", "can_stop"] {
                    assert_eq!(row[control], row["harness"] != "human", "{row}");
                }
            }
        }
        for hubs in [&snapshot["hubs"], &snapshot["settings"]["hubs"]] {
            let hub = hubs
                .as_array()
                .unwrap()
                .iter()
                .find(|hub| hub["name"] == "local")
                .unwrap();
            assert_eq!(hub["is_builtin"], true);
            assert_eq!(hub["supports_workspace_scope"], false);
            assert_eq!(hub["can_login"], false);
        }
        let row = &active[0];
        let signal = json!({"signal_id":"dto-signal","target_generation":row["binding_generation"],"from":{"principal_id":row["principal_id"],"user":"alice","host_name":"desktop"},"message":"hello"});
        service.state.admit(
            "local",
            row,
            &signal,
            1,
            "2026-01-01T00:00:00Z",
            now_ms() + 60_000,
            &Value::Null,
        )?;
        service.state.db.execute(
            "UPDATE local_signals SET dispatcher_token='secret-dispatcher'",
            &[],
        )?;
        let activity = service.snapshot()?["activity"].clone();
        assert!(!activity.as_array().unwrap().is_empty());
        assert!(!activity.to_string().contains("secret-dispatcher"));
        assert!(activity[0].get("dispatcher_token").is_none());
        assert_eq!(activity[0]["message"], "hello");
        assert!(
            activity[0]["can_reply"].is_boolean(),
            "Activity DTOs retain reply authority"
        );
        service.state.db.execute("DROP TABLE wake_retries", &[])?;
        let sent = service.tool("cito_send", &json!({"to":"@self","message":"committed despite maintenance failure","idempotency_key":"maintenance"}), &json!({}))?;
        assert_eq!(sent["ok"], true);
        assert!(sent["signal_id"].as_str().unwrap().contains("sig_"));
        assert!(sent["maintenance_warning"].is_string(), "{sent}");
        let reservation = service
            .state
            .db
            .query("SELECT * FROM local_sent_idempotency", &[])?;
        assert_eq!(reservation.len(), 1);
        assert_eq!(reservation[0]["status"], "wake_accepted");
        Ok(())
    })();
    let shared = Arc::new(Mutex::new(service));
    let http_result = (|| -> Result<(), String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        for (method, headers, status) in [
            ("GET /health", format!("Host: {address}\r\n"), 200),
            (
                "GET /health",
                format!("Host: {address}\r\nOrigin: http://{address}\r\n"),
                200,
            ),
            (
                "GET /health",
                format!(
                    "Host: {address}\r\nOrigin: http://127.0.0.1:{}\r\n",
                    address.port() + 1
                ),
                403,
            ),
            (
                "GET /health",
                format!("Host: attacker.example:{}\r\n", address.port()),
                403,
            ),
            (
                "POST /mcp",
                format!("Host: {address}\r\nContent-Type: text/plain\r\n"),
                415,
            ),
            ("POST /mcp", format!("Host: {address}\r\nContent-Type: application/json\r\n"), 200),
            ("POST /mcp", format!("Host: {address}\r\nContent-Type: application/json\r\nOrigin: http://127.0.0.1:{}\r\n", address.port() + 1), 403),
            ("POST /mcp", format!("Host: attacker.example:{}\r\nContent-Type: application/json\r\n", address.port()), 403),
        ] {
            let service = shared.clone();
            let mut client = TcpStream::connect(address).map_err(|e| e.to_string())?;
            let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
            let worker = thread::spawn(move || {
                super::http_actions::handle_http(stream, service, Arc::new(json!({})))
            });
            let body = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}).to_string();
            write!(
                client,
                "{method} HTTP/1.1\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len()
            )
            .map_err(|e| e.to_string())?;
            let mut response = String::new();
            client
                .read_to_string(&mut response)
                .map_err(|e| e.to_string())?;
            worker.join().map_err(|_| "HTTP worker panicked")?;
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status}")),
                "{response}"
            );
        }
        let mut committed =
            json!({"ok":true,"signal_id":"missing:sig_committed","seq":1,"status":"wake_accepted"});
        super::mcp::wait_send_receipt(&shared, &mut committed);
        assert_eq!(committed["signal_id"], "missing:sig_committed");
        assert_eq!(committed["ok"], true);
        assert_eq!(committed["receipt_status"], "unknown");
        Ok(())
    })();
    shared.lock().map_err(|e| e.to_string())?.shutdown();
    fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    result.and(http_result)
}

#[test]
fn coalesced_snapshots_retain_command_results() -> Result<(), String> {
    let mailbox = UiMailbox::default();
    for i in 0..40 {
        mailbox.emit(json!({"op":"settings","hub":"local","revision":i}));
    }
    let results = [
        json!({"op":"settings","hub":"local","request_id":"gateway-save"}),
        json!({"op":"settings","hub":"local","request_id":"personal-save"}),
        json!({"op":"settings","hub":"local","ok":true,"source_op":"setup_save_hub"}),
        json!({"op":"state","ok":true,"source_op":"clear_activity_history","request_id":"delete-a"}),
        json!({"op":"state","ok":true,"source_op":"ack_signals","request_id":"read-b"}),
        json!({"op":"composer_result","ok":true}),
        json!({"op":"pair_code","code":"123456"}),
    ];
    for result in &results {
        mailbox.emit(result.clone());
    }
    let snapshot = json!({"op":"settings","hub":"local","revision":40});
    mailbox.emit(snapshot.clone());
    assert_eq!(
        mailbox.pending.lock().map_err(|e| e.to_string())?.len(),
        1 + results.len()
    );
    for expected in results.into_iter().chain(std::iter::once(snapshot)) {
        let bytes = mailbox.next().ok_or("Missing UI frame")?;
        let actual: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        assert_eq!(actual, expected);
    }
    mailbox.close();
    assert_eq!(mailbox.next(), None);
    Ok(())
}
#[test]
fn snapshot_and_remote_human_projection_share_the_hub_clock_and_freshness() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!(
        "cito-projection-{}",
        crate::plugins::random_uuid_v4()
    ));
    fs::create_dir_all(dir.join("home")).map_err(|e| e.to_string())?;
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    fs::write(&config.config_path, "{}").map_err(|e| e.to_string())?;
    let mut service = DaemonService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        },
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let url = format!(
        "http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        let mut length = 0;
        reader.read_line(&mut line).unwrap();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
        }
        reader.read_exact(&mut vec![0; length]).unwrap();
        respond(
            &mut stream,
            200,
            &json!({"ok":true,"result":{"server_now_ms":1000,"record":{"listener_id":"c_fixture","public_id":"alice-fixture","principal_id":"none#alice","owner_user":"alice","harness":"human","session_id":"default","state":"UNLISTENED","scope":"workspace","about":"Keep","human_control_revision":3,"updated_at_ms":1000,"channels":{"remote:human":{"host_id":"remote","host_name":"Laptop","state":"ACTIVE","updated_at_ms":1000}}}}}),
        );
    });
    let client = HubClient::new(
        "work".into(),
        json!({"url":url,"authMode":"none"}),
        "alice",
        None,
    );
    client.human_request("human_device_attach", &json!({}))?;
    server.join().map_err(|_| "Fixture server panicked")?;
    fs::write(
        &config.config_path,
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":client.config["url"]}}})
            .to_string(),
    )
    .map_err(|e| e.to_string())?;
    service.slots.insert("work".into(), client.clone());
    let snapshot = service.snapshot()?;
    let hub = snapshot["hubs"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["name"] == "work"))
        .ok_or("Missing Hub projection")?;
    let peer = snapshot["my_remote_listeners"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["hub"] == "work"))
        .ok_or("Missing remote Human")?;
    assert_eq!(hub["human_availability"], "ACTIVE");
    assert_eq!(hub["human_state_status"], "current");
    assert_eq!(hub["human_can_set_scope"], true);
    assert!(snapshot["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["hub"] != "work"));
    assert_eq!(peer["human_availability"], hub["human_availability"]);
    assert_eq!(peer["human_state_status"], hub["human_state_status"]);
    assert_eq!(peer["human_can_set_scope"], hub["human_can_set_scope"]);
    assert_eq!(peer["attachments"], hub["attachments"]);
    assert_eq!(peer["last_seen_at"], crate::core::identity::iso(1000));
    let inventory = Arc::new(ChannelInventory::new(remote_channels_identity(&client)));
    inventory.cached.lock().map_err(|e| e.to_string())?.value = json!({"status":"error","detail":"Temporary inventory failure","channels":[{"id":"messenger","status":"ready"}]});
    service
        .channel_inventories
        .insert("work".into(), inventory.clone());
    let settings = service.settings_for_hub("work");
    assert_eq!(
        settings["channel_inventory_error"],
        "Temporary inventory failure"
    );
    assert_eq!(settings["channels"].as_array().unwrap().len(), 1);
    assert_eq!(settings["channels"][0]["id"], "messenger");
    inventory.cached.lock().map_err(|e| e.to_string())?.value =
        json!({"status":"error","detail":"Initial failure"});
    let settings = service.settings_for_hub("work");
    assert_eq!(settings["channels"], json!([]));
    assert_eq!(settings["channel_inventory_error"], "Initial failure");
    assert!(service.settings_for_hub("local")["channel_inventory_error"].is_null());
    *client.status.lock().map_err(|e| e.to_string())? = "connected".into();
    *client.rejected_bearer.lock().map_err(|e| e.to_string())? = Some("rejected".into());
    let snapshot = service.snapshot()?;
    let hub = snapshot["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "work")
        .unwrap();
    let peer = snapshot["my_remote_listeners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["hub"] == "work")
        .unwrap();
    assert_eq!(hub["human_state_status"], "stale");
    assert_eq!(hub["human_can_set_scope"], false);
    assert_eq!(hub["human_state_status"], peer["human_state_status"]);
    client.closed.store(true, Ordering::SeqCst);
    service.shutdown();
    fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    Ok(())
}
#[test]
fn an_unrepresentable_receipt_closes_the_stream_instead_of_silently_dropping_it() {
    let mailbox = UiMailbox::default();
    mailbox.emit(json!({"op":"error","request_id":"x".repeat(FRAME_LIMIT+1),"error":"Cannot fit"}));
    assert!(mailbox.closed.load(Ordering::SeqCst));
    assert!(mailbox.next().is_none());
}
#[test]
fn correlated_hub_save_never_sheds_the_row_needed_for_settlement() -> Result<(), String> {
    let hubs = (0..150)
        .map(|i| json!({"name":format!("hub-{i}"),"url":"https://id.example.com"}))
        .collect::<Vec<_>>();
    let frame: Value = serde_json::from_slice(&bounded_frame(json!({"op":"settings","request_id":"save","source_op":"save_hub","settings":{"hubs":hubs}}))?).map_err(|e| e.to_string())?;
    assert_eq!(frame["settings"]["hubs"][149]["name"], "hub-149");
    let hubs = (0..150).map(|i| json!({"name":format!("hub-{i}"),"attachments":(0..30).map(|_|json!({"display_name":"long".repeat(100)})).collect::<Vec<_>>()})).collect::<Vec<_>>();
    let frame: Value = serde_json::from_slice(&bounded_frame(json!({"op":"settings","request_id":"save","source_op":"save_hub","settings":{"hubs":hubs}}))?).map_err(|e| e.to_string())?;
    assert_eq!(frame["op"], "error");
    assert_eq!(frame["request_id"], "save");
    assert_eq!(frame["source_op"], "save_hub");
    Ok(())
}
#[test]
fn root_hub_shedding_retains_correlated_receipt_and_terminates() -> Result<(), String> {
    let f = json!({"op":"settings","request_id":"receipt","hub":"work","action":"save_hub_service","hubs":(0..80).map(|i|json!({"name":format!("hub-{i}"),"attachments":(0..20).map(|_|json!({"display_name":"long".repeat(100)})).collect::<Vec<_>>()})).collect::<Vec<_>>()});
    let bytes = bounded_frame(f)?;
    assert!(bytes.len() <= FRAME_LIMIT);
    let frame: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    assert_eq!(frame["request_id"], "receipt");
    assert_eq!(frame["hub"], "work");
    Ok(())
}
#[test]
fn channel_inventory_diagnostic_survives_collection_shedding() -> Result<(), String> {
    let frame = json!({"op":"state","settings":{"channel_inventory_error":"Inventory unavailable","channels":(0..16).map(|i| json!({"id":format!("channel-{i}"),"fields":(0..12).map(|_|json!({"detail":"x".repeat(4096)})).collect::<Vec<_>>()})).collect::<Vec<_>>()}});
    let bytes = bounded_frame(frame)?;
    assert!(bytes.len() < FRAME_LIMIT);
    let frame: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    assert_eq!(frame["truncated"], true);
    assert_eq!(
        frame["settings"]["channel_inventory_error"],
        "Inventory unavailable"
    );
    Ok(())
}

#[test]
fn explicit_nested_pids_preserve_endpoint_errors_and_provider_fallback() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("cito-explicit-{}", random_uuid_v4()));
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    let mut plugins = vec![];
    for id in ["a-bound", "z-fallback"] {
        let path = config.state_dir.join("plugins").join(id);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("plugin.json"),json!({"id":id,"kind":"agent","displayName":id,"pidBound":id=="a-bound","command":["python3","./driver.py"]}).to_string()).unwrap();
        fs::write(path.join("driver.py"),r#"import json,sys,pathlib,os
req=json.loads(sys.stdin.readline());root=pathlib.Path(__file__).parent
op=req['op'];result={'ok':True}
if op=='resolve_caller':
 if root.name=='a-bound':
  code='BINDING_REQUIRED' if (root/'unmatched').exists() else 'ENDPOINT_UNAVAILABLE'
  result={'ok':False,'error_code':code,'message':'Matching endpoint is unavailable'}
 else: result={'ok':True,'caller':{'harness':root.name,'session_id':'live','cwd':'/workspace','title':'Fallback','endpoint_pid':os.getppid()}}
elif op=='discover_sessions': result['sessions']=[]
elif op=='setup_inspect': result.update(installed=True,configured=True,paths=[])
print(json.dumps(result))
"#).unwrap();
        plugins.push(crate::plugins::parse_manifest_dir(&path, false)?);
    }
    let mut service = DaemonService::open(
        &config,
        &LoadedPlugins {
            plugins,
            rejected_ids: Default::default(),
        },
    )?;
    let pid = std::process::id();
    for meta in [
        json!({"endpoint_pid":pid}),
        json!({"_stdio":{"endpoint_pid":pid}}),
        json!({"_meta":{"endpoint_pid":pid}}),
    ] {
        let error = service
            .tool(
                "cito_send",
                &json!({"to":"@self","message":"Must not downgrade"}),
                &meta,
            )
            .unwrap_err();
        assert!(error.starts_with("ENDPOINT_UNAVAILABLE:"), "{error}");
    }
    assert!(service
        .state
        .db
        .query("SELECT * FROM local_sent_idempotency", &[])?
        .is_empty());
    fs::write(config.state_dir.join("plugins/a-bound/unmatched"), "").unwrap();
    let status = service.tool(
        "cito_status",
        &json!({}),
        &json!({"_stdio":{"endpoint_pid":pid}}),
    )?;
    assert_eq!(status["registered"], false, "{status}");
    let caller = service
        .caller(&json!({"_stdio":{"endpoint_pid":pid}}), true)?
        .unwrap();
    assert_eq!(caller["harness"], "z-fallback");
    service.shutdown();
    drop(service);
    fs::remove_dir_all(dir).unwrap();
    Ok(())
}

#[test]
fn canonical_editable_values_survive_ui_projection() -> Result<(), String> {
    let title = "\u{4e2d}".repeat(64);
    let long = "x".repeat(6000);
    let frame = json!({"op":"state","listeners":[{"title":title}],
        "settings":{"hubs":[{"url":format!("https://id.example.com/{long}"),"issuer":long,"client_id":long}],
        "channels":[{"fields":[{"value":long}]}]}});
    let projected: Value = serde_json::from_slice(&bounded_frame(frame.clone())?).unwrap();
    assert_eq!(projected, frame);
    let invite = json!({"op":"hub_invite","url":long,"token":long,"issuer":long,"client_id":long});
    let projected: Value = serde_json::from_slice(&bounded_frame(invite.clone())?).unwrap();
    assert_eq!(projected, invite);
    Ok(())
}

#[test]
fn mcp_metadata_never_grants_human_authority() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("cito-authority-{}", random_uuid_v4()));
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    let mut service = DaemonService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        },
    )?;
    for tool in [
        "cito_listen",
        "cito_unlisten",
        "cito_send",
        "cito_inbox",
        "cito_status",
    ] {
        assert!(service
            .tool(tool, &json!({}), &json!({"expected_harness":"human"}))
            .unwrap_err()
            .starts_with("FORBIDDEN:"));
        assert!(service
            .tool(tool, &json!({"harness":"human"}), &json!({}))
            .unwrap_err()
            .starts_with("FORBIDDEN:"));
    }
    service.shutdown();
    drop(service);
    fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    Ok(())
}

#[test]
fn bounded_empty_activity_with_multibyte_listeners() -> Result<(), String> {
    let f = json!({"op":"state","activity":[],"listeners":(0..150).map(|_|json!({"title":"🦀".repeat(10000),"about":"🦀".repeat(10000)})).collect::<Vec<_>>(),"unread_count":200});
    let bytes = bounded_frame(f)?;
    assert!(bytes.len() < FRAME_LIMIT);
    let frame: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    assert_eq!(frame["unread_count"], 200);
    assert_eq!(frame["listeners"].as_array().map(Vec::len), Some(64));
    let title = "é".repeat(128);
    let frame: Value = serde_json::from_slice(&bounded_frame(json!({"title":title}))?)
        .map_err(|e| e.to_string())?;
    // Valid editable titles preserve their canonical UTF-8 bytes.
    let projected = frame["title"].as_str().ok_or("Missing title")?;
    assert_eq!(projected, title);
    assert!(projected.len() <= 512);
    Ok(())
}
