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

use crate::core::hub::{TestDirectory, TestEngine};

struct TestService(Option<DaemonService>);
impl TestService {
    fn open(config: &NativeConfig, plugins: &LoadedPlugins) -> Result<Self, String> {
        DaemonService::open(config, plugins).map(|service| Self(Some(service)))
    }
    fn replace_slot(&mut self, client: HubClient) {
        if let Some(old) = self.slots.insert(client.name.clone(), client) {
            old.close();
        }
    }
    fn into_inner(mut self) -> DaemonService {
        self.0.take().unwrap()
    }
}
impl std::ops::Deref for TestService {
    type Target = DaemonService;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for TestService {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().unwrap()
    }
}
impl Drop for TestService {
    fn drop(&mut self) {
        if let Some(service) = self.0.as_mut() {
            service.shutdown();
        }
    }
}

struct ServiceCleanup {
    service: Arc<Mutex<DaemonService>>,
}
impl Drop for ServiceCleanup {
    fn drop(&mut self) {
        self.service
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .shutdown();
    }
}

#[test]
fn ui_frame_bounds_preserve_cwd_receipt_authority_and_mailbox_liveness() {
    let cwd = format!("/workspace/{}", "segment/".repeat(80));
    let bytes = bounded_frame(json!({"op":"state","available":[{"cwd":cwd}]})).unwrap();
    let frame: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(frame["available"][0]["cwd"], cwd);
    for key in [
        "token",
        "invite_command",
        "tailscale_invite_command",
        "lan_invite_command",
    ] {
        let value = "x".repeat(8192);
        let frame: Value =
            serde_json::from_slice(&bounded_frame(json!({key:value})).unwrap()).unwrap();
        assert_eq!(frame[key], value);
    }
    let authority = json!({"hub":"work","generation":7});
    let bytes=bounded_frame(json!({"op":"settings","ok":true,"committed":true,"request_id":"save","receipt_authority":authority,"warning":"Saved under the original Hub authority","settings":{"cwd":"x".repeat(FRAME_LIMIT)}})).unwrap();
    let receipt: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(receipt["receipt_authority"], authority);
    assert_eq!(receipt["warning"], "Saved under the original Hub authority");
    let mailbox = UiMailbox::default();
    assert!(mailbox.emit(json!({"op":"settings","cwd":"x".repeat(FRAME_LIMIT)})));
    let reduced: Value = serde_json::from_slice(&mailbox.next().unwrap()).unwrap();
    assert_eq!(reduced["truncated"], true);
    assert_eq!(reduced["truncated_collections"], json!([]));
    assert!(reduced["cwd"].as_str().unwrap().ends_with('…'));
    assert!(!mailbox.closed.load(Ordering::SeqCst));
    mailbox.emit(json!({"op":"state","status":"idle"}));
    assert_eq!(mailbox.pending.lock().unwrap().len(), 1);
}

#[test]
fn composer_prearm_failures_settle_once_and_config_producers_share_lock() -> Result<(), String> {
    let dir = TestDirectory(std::env::temp_dir().join(format!("cito-prearm-{}", random_uuid_v4())));
    fs::create_dir_all(dir.join("home")).map_err(|e| e.to_string())?;
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    fs::write(&config.config_path, "{}").unwrap();
    let service = Arc::new(Mutex::new(DaemonService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: HashSet::new(),
        },
    )?));
    let _cleanup = ServiceCleanup {
        service: service.clone(),
    };
    for generation in [None, Some(u64::MAX)] {
        fs::write(&config.config_path, "{").unwrap();
        for op in ["send_signal", "send_reply_from_human"] {
            let mailbox = Arc::new(UiMailbox::default());
            service.lock().unwrap().ui = Some(mailbox.clone());
            assert!(http_actions::dispatch_action(
                &service,
                &json!({"op":op,"request_id":"composer"}),
                generation
            )
            .is_err());
            let mut count = 0;
            while !mailbox.pending.lock().unwrap().is_empty() {
                let frame: Value = serde_json::from_slice(&mailbox.next().unwrap()).unwrap();
                assert_ne!(frame["op"], "error", "{frame}");
                if frame["op"] == "composer_result" {
                    count += 1;
                    assert_eq!(frame["ok"], false);
                    assert_eq!(frame["request_id"], "composer");
                    assert!(frame["error"].is_string());
                }
            }
            assert_eq!(count, 1);
        }
    }
    fs::write(&config.config_path, "{\"count\":0}").unwrap();
    let alias = dir.join("alias.json");
    std::os::unix::fs::symlink(&config.config_path, &alias).unwrap();
    // Hold setup inside its transform. Daemon must not enter until setup commits.
    for path in [&config.config_path, &alias] {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (daemon_tx, daemon_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let setup = scope.spawn(move || {
                crate::setup::mutate_file("cito", path, false, Some(0o600), |old| {
                    let mut raw: Value = serde_json::from_str(old.unwrap()).unwrap();
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    raw["count"] = json!(raw["count"].as_u64().unwrap() + 1);
                    Ok((
                        "updated".into(),
                        Some(raw.to_string()),
                        "Updated counter".into(),
                    ))
                })
            });
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let service = service.clone();
            let daemon = scope.spawn(move || {
                service.lock().unwrap().mutate_config(|raw| {
                    daemon_tx.send(()).unwrap();
                    raw["count"] = json!(raw["count"].as_u64().unwrap() + 1);
                    Ok(())
                })
            });
            assert!(
                daemon_rx.recv_timeout(Duration::from_millis(100)).is_err(),
                "Daemon bypassed setup lock"
            );
            release_tx.send(()).unwrap();
            setup.join().unwrap().unwrap();
            daemon.join().unwrap().unwrap();
            daemon_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        });
    }
    let raw: Value = serde_json::from_slice(&fs::read(&config.config_path).unwrap()).unwrap();
    assert_eq!(raw["count"], 4);
    assert!(alias.is_symlink());
    Ok(())
}

#[test]
fn action_failures_emit_correlated_settings_without_outer_errors() -> Result<(), String> {
    let dir = TestDirectory(
        std::env::temp_dir().join(format!("cito-frame-settlement-{}", random_uuid_v4())),
    );
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
    let service = Arc::new(Mutex::new(DaemonService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: HashSet::new(),
        },
    )?));
    let _cleanup = ServiceCleanup {
        service: service.clone(),
    };
    let result = (|| {
        {
            let mut s = service.lock().unwrap();
            assert!(s
                .tool(
                    "cito_status",
                    &json!({}),
                    &json!({"expected_session_id":" \t "})
                )
                .unwrap_err()
                .starts_with("INVALID_INPUT:"));
            assert!(s
                .tool(
                    "cito_status",
                    &json!({}),
                    &json!({"expected_harness":" human "})
                )
                .unwrap_err()
                .starts_with("FORBIDDEN:"));
            s.listen(
                "local",
                &json!({"about":"Old receiver"}),
                &json!({"harness":"fixture","session_id":"old","endpoint_pid":12345}),
            )?;
            s.state.db.execute("CREATE TRIGGER fail_retirement BEFORE UPDATE OF status ON local_listeners WHEN OLD.session_id='old' BEGIN SELECT RAISE(ABORT,'retirement unavailable'); END", &[])?;
            let registered = s.listen(
                "local",
                &json!({"about":"New receiver"}),
                &json!({"harness":"fixture","session_id":"new","endpoint_pid":12345}),
            )?;
            assert_eq!(registered["committed"], true, "{registered}");
            assert!(
                string(&registered, "warning").contains("retirement unavailable"),
                "{registered}"
            );
            s.state.db.execute("DROP TRIGGER fail_retirement", &[])?;
            s.slot("local")?.closed.store(true, Ordering::SeqCst);
            let renamed = s.rename_session("fixture", "new", "Custom title")?;
            assert!(!renamed["warnings"].as_array().unwrap().is_empty());
            assert!(renamed["warning"].is_string());
            s.slot("local")?.closed.store(false, Ordering::SeqCst);
            s.mutate_config(|raw| {
                raw["hubs"] = json!({"local":{"url":"local://"},"work":{"url":"local://"}});
                Ok(())
            })?;
            s.reload_slots()?;
            let node = s
                .state
                .active()?
                .into_iter()
                .find(|row| row["session_id"] == "new")
                .unwrap();
            s.state.db.execute("CREATE TRIGGER fail_source_detach BEFORE UPDATE OF status ON local_listeners WHEN OLD.hub='local' BEGIN SELECT RAISE(ABORT,'source detach unavailable'); END",&[])?;
            let switched = s.action(&json!({"op":"switch_hub","from_hub":"local","to_hub":"work","listener_id":node["listener_id"]}))?;
            assert_eq!(switched["committed"], true, "{switched}");
            assert!(
                string(&switched, "warning").contains("source detach unavailable"),
                "{switched}"
            );
            assert!(s
                .state
                .active()?
                .iter()
                .any(|row| row["hub"] == "work" && row["session_id"] == "new"));
            s.state.db.execute("DROP TRIGGER fail_source_detach", &[])?;
        }
        for action in [
            json!({"op":"channel_test","channel_id":"missing","hub":"local","request_id":"test"}),
            json!({"op":"channel_save","channel_id":"missing","hub":"local","request_id":"save"}),
            json!({"op":"set_host_hub","enabled":true,"bind":"invalid address","request_id":"host"}),
        ] {
            let mailbox = Arc::new(UiMailbox::default());
            service.lock().unwrap().ui = Some(mailbox.clone());
            assert!(http_actions::dispatch_action(&service, &action, None).is_err());
            let mut settlements = 0;
            while !mailbox.pending.lock().unwrap().is_empty() {
                let frame: Value =
                    serde_json::from_slice(&mailbox.next().unwrap()).map_err(|e| e.to_string())?;
                assert_ne!(frame["op"], "error", "{frame}");
                if frame["op"] == "settings" {
                    settlements += 1;
                    assert_eq!(frame["ok"], false);
                    assert_eq!(frame["request_id"], action["request_id"]);
                    assert_eq!(frame["source_op"], action["op"]);
                    assert!(frame["error"].is_string(), "{frame}");
                }
            }
            assert_eq!(settlements, 1);
        }
        Ok(())
    })();
    result
}

#[test]
fn credential_save_restricts_existing_external_config_mode() -> Result<(), String> {
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-config-mode-{}", random_uuid_v4())));
    fs::create_dir_all(dir.join("external")).map_err(|e| e.to_string())?;
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("external/config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    fs::write(&config.config_path, "{}").map_err(|e| e.to_string())?;
    fs::set_permissions(&config.config_path, fs::Permissions::from_mode(0o644))
        .map_err(|e| e.to_string())?;
    let mut service = TestService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        },
    )?;
    let saved = service.mutate_config(|raw| {
        raw["token"] = json!("secret");
        Ok(())
    });
    service.shutdown();
    saved?;
    let stored: Value =
        serde_json::from_str(&fs::read_to_string(&config.config_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    assert_eq!(stored["token"], "secret");
    assert_eq!(
        fs::metadata(&config.config_path)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    Ok(())
}

#[test]
fn authoritative_commits_survive_secondary_local_projection_failures() -> Result<(), String> {
    let dir = TestDirectory(
        std::env::temp_dir().join(format!("cito-commit-projection-{}", random_uuid_v4())),
    );
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
    let mut service = TestService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: HashSet::new(),
        },
    )?;
    let result = (|| -> Result<(), String> {
        service.local.set_plugin_roots(&[])?;
        let registered = service.action(&json!({"op":"cli_human_device","attach":true}))?;
        let id = string(&registered, "canonical_listener_id");
        assert!(!id.is_empty(), "{registered}");
        let client = service.slot("local")?.clone();
        let agent = service.listen(
            "local",
            &json!({"about":"Receiver","scope":"self"}),
            &json!({"harness":"fixture","session_id":"session","title":"Receiver"}),
        )?;
        let agent_id = string(&agent, "canonical_listener_id");
        assert!(!agent_id.is_empty(), "{agent}");
        service.state.db.execute("CREATE TRIGGER fail_projection BEFORE UPDATE ON local_listeners BEGIN SELECT RAISE(ABORT,'injected projection failure'); END",&[])?;
        for action in [
            json!({"op":"human_inbox_update","scope":"self"}),
            json!({"op":"set_scope","listener_id":id,"scope":"workspace"}),
            json!({"op":"set_scope","listener_id":agent_id,"scope":"workspace"}),
        ] {
            let target = action["listener_id"].as_str().unwrap_or(id);
            let original = service
                .state
                .listener("local", target)?
                .ok_or("Missing local receiver")?;
            let result = service.action(&action)?;
            assert_eq!(result["committed"], true, "{result}");
            assert!(
                string(&result, "warning").contains("injected projection failure"),
                "{result}"
            );
            assert_eq!(
                client
                    .kv_get(target)?
                    .ok_or("Missing authoritative record")?
                    .0["scope"],
                action["scope"]
            );
            assert_eq!(
                service.state.listener("local", target)?.unwrap()["scope"],
                original["scope"]
            );
        }
        let before = client.kv_get(id)?.unwrap().0;
        let rejected = service
            .action(&json!({"op":"human_inbox_update","paused":true,"expected_control_revision":0}))
            .unwrap_err();
        assert!(
            !rejected.contains("injected projection failure"),
            "{rejected}"
        );
        assert_eq!(client.kv_get(id)?.unwrap().0, before);
        service
            .state
            .db
            .execute("DROP TRIGGER fail_projection", &[])?;
        service.mutate_config(|raw| {
            raw["hubs"]["local"]["autoListen"] = json!(false);
            Ok(())
        })?;
        service.slots.get_mut("local").unwrap().config["autoListen"] = json!(false);
        service.state.db.execute(
            "UPDATE local_listeners SET user_stopped=1,listen_mode='auto' WHERE hub='local' AND harness='fixture'",
            &[],
        )?;
        service.state.db.execute("CREATE TRIGGER fail_auto_projection BEFORE UPDATE OF user_stopped ON local_listeners BEGIN SELECT RAISE(ABORT,'injected auto-listen projection failure'); END", &[])?;
        let enabled = service.action(&json!({"op":"set_auto_listen","enabled":true}))?;
        assert_eq!(enabled["committed"], true, "{enabled}");
        assert_eq!(enabled["ok"], true, "{enabled}");
        assert!(
            string(&enabled, "warning").contains("injected auto-listen projection failure"),
            "{enabled}"
        );
        assert_eq!(service.raw_config()?["hubs"]["local"]["autoListen"], true);
        assert_eq!(service.slot("local")?.config["autoListen"], true);
        assert_eq!(
            service.state.listener("local", agent_id)?.unwrap()["user_stopped"],
            1
        );
        let rejected = service.action(&json!({"op":"set_auto_listen","enabled":"yes"}));
        assert!(rejected.unwrap_err().starts_with("INVALID_INPUT:"));
        assert_eq!(service.raw_config()?["hubs"]["local"]["autoListen"], true);
        assert_eq!(
            service.state.listener("local", agent_id)?.unwrap()["status"],
            "ACTIVE"
        );
        let disabled = service.action(&json!({"op":"set_auto_listen","enabled":false}))?;
        assert_eq!(disabled["committed"], true, "{disabled}");
        assert!(
            string(&disabled, "warning").contains("injected auto-listen projection failure"),
            "{disabled}"
        );
        assert_eq!(service.raw_config()?["hubs"]["local"]["autoListen"], false);
        assert_eq!(service.slot("local")?.config["autoListen"], false);
        assert_eq!(
            service.state.listener("local", agent_id)?.unwrap()["status"],
            "ACTIVE"
        );
        service
            .state
            .db
            .execute("DROP TRIGGER fail_auto_projection", &[])?;
        service.state.db.execute("CREATE TRIGGER fail_projection BEFORE UPDATE ON local_listeners BEGIN SELECT RAISE(ABORT,'injected projection failure'); END",&[])?;
        let saved = service.action(&json!({"op":"set_user","user":"bob"}))?;
        assert_eq!(saved["committed"], true, "{saved}");
        assert!(
            string(&saved, "warning").contains("injected projection failure"),
            "{saved}"
        );
        assert_eq!(service.raw_config()?["user"], "bob");
        assert!(Arc::ptr_eq(&service.slot("local")?.closed, &client.closed));
        assert!(
            !client.closed.load(Ordering::SeqCst),
            "Failed reload must retain the old event owner until replacement"
        );
        service
            .state
            .db
            .execute("DROP TRIGGER fail_projection", &[])?;
        // A queued probe from the retired slot cannot commit even if the
        // successor retains the same listener generation and owner token.
        let queued = service.state.listener("local", agent_id)?.unwrap();
        let state = service.state.clone();
        let (ready, rx) = std::sync::mpsc::channel();
        thread::scope(|threads| -> Result<(), String> {
            let writer = threads.spawn(|| -> Result<(), String> {
                let _guard = client.registration.lock().unwrap();
                ready.send(()).unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                while !client.closed.load(Ordering::SeqCst) && Instant::now() < deadline {
                    client.wait_for_event(client.event_revision(), deadline);
                }
                assert!(client.closed.load(Ordering::SeqCst));
                let error = super::super::receiver::apply_probe(
                    &state,
                    &client,
                    &queued,
                    &json!({"ok":true,"status":"alive"}),
                )
                .unwrap_err();
                assert!(error.starts_with("STALE_GENERATION:"), "{error}");
                Ok(())
            });
            rx.recv().unwrap();
            service.reload_slots()?;
            writer.join().unwrap()?;
            Ok(())
        })?;
        assert_eq!(
            state.listener("local", agent_id)?.unwrap()["status"],
            "FENCED_INACTIVE"
        );
        Ok(())
    })();
    result
}

#[test]
fn delayed_remote_account_rpc_allows_local_requests_and_rejects_replaced_authority(
) -> Result<(), String> {
    let dir = TestDirectory(std::env::temp_dir().join(format!("cito-rpc-{}", random_uuid_v4())));
    fs::create_dir_all(dir.join("home")).unwrap();
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
        r#"{"default_hub":"local","hubs":{"local":{"url":"local://"},"work":{"url":"local://"}}}"#,
    )
    .unwrap();
    let mut s = TestService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        },
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut remote = s.slot("work")?.config.clone();
    remote["url"] = json!(url);
    s.mutate_config(|raw| {
        raw["hubs"]["work"]["url"] = json!(url);
        Ok(())
    })?;
    s.state.db.execute(
        "UPDATE local_listeners SET hub_url=? WHERE hub='work'",
        &[json!(url)],
    )?;
    s.replace_slot(HubClient::new("work".into(), remote, "alice", None));
    let service = Arc::new(Mutex::new(s.into_inner()));
    let _cleanup = ServiceCleanup {
        service: service.clone(),
    };
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    thread::scope(|threads| -> Result<(), String> {
        let server = threads.spawn(move || -> Result<(), String> {
            let mut stream = crate::core::http_boundary::accept_test_client(&listener)?;
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let head = crate::core::http_boundary::read_head(&mut reader, 16_384)
                .map_err(|(_, error)| error)?;
            let mut bytes = vec![0; head.length];
            reader.read_exact(&mut bytes).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap()["op"],
                "account_info"
            );
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(3))
                .map_err(|e| e.to_string())?;
            respond(
                &mut stream,
                200,
                &json!({"ok":true,"result":{"original":true}}),
            );
            Ok(())
        });
        let pending = threads.spawn(|| {
            http_actions::dispatch_action(&service, &json!({"op":"test_hub","hub":"work"}), None)
        });
        ready
            .recv_timeout(Duration::from_secs(3))
            .map_err(|e| e.to_string())?;
        http_actions::dispatch_action(&service, &json!({"op":"test_hub","hub":"local"}), None)?;
        http_actions::dispatch_action(
            &service,
            &json!({"op":"setup_toggle_hub","hub":"work","enabled":false}),
            None,
        )?;
        release.send(()).map_err(|e| e.to_string())?;
        server.join().unwrap()?;
        assert!(pending
            .join()
            .unwrap()
            .unwrap_err()
            .starts_with("STALE_GENERATION:"));
        assert!(!service.lock().unwrap().last_hub_errors.contains_key("work"));
        Ok(())
    })
}

#[test]
fn data_reset_waits_for_retired_workers_outside_service() -> Result<(), String> {
    let dir = TestDirectory(std::env::temp_dir().join(format!("cito-reset-{}", random_uuid_v4())));
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    fs::create_dir_all(&config.home_dir).unwrap();
    let mut s = TestService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        },
    )?;
    let slot = s.slot("local")?.clone();
    let (closed, arrived) = std::sync::mpsc::channel();
    let (release, done) = std::sync::mpsc::channel();
    s.receivers.insert(
        "directory:local".into(),
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !slot.closed.load(Ordering::Acquire) && Instant::now() < deadline {
                slot.wait_for_event(slot.event_revision(), deadline);
            }
            if slot.closed.load(Ordering::Acquire) {
                closed.send(()).unwrap();
                let _ = done.recv_timeout(Duration::from_secs(2));
            }
        }),
    );
    let service = Arc::new(Mutex::new(s.into_inner()));
    let _cleanup = ServiceCleanup {
        service: service.clone(),
    };
    thread::scope(|threads| -> Result<(), String> {
        let reset = threads.spawn(|| {
            http_actions::dispatch_action(
                &service,
                &json!({"op":"setup_disconnect_all","purge_data":true}),
                None,
            )
        });
        arrived
            .recv_timeout(Duration::from_secs(4))
            .map_err(|e| e.to_string())?;
        let start = Instant::now();
        let error =
            http_actions::dispatch_action(&service, &json!({"op":"test_hub","hub":"local"}), None)
                .unwrap_err();
        assert!(error.contains("reset in progress"), "{error}");
        assert!(start.elapsed() < Duration::from_millis(300));
        release.send(()).unwrap();
        assert_eq!(reset.join().unwrap()?["committed"], true);
        http_actions::dispatch_action(&service, &json!({"op":"test_hub","hub":"local"}), None)?;
        Ok(())
    })
}

#[test]
fn mcp_http_releases_service_for_directory_enrollment_publication_and_retirement(
) -> Result<(), String> {
    use std::io::Write;
    for (tool, mut args, gate) in [
        ("directory_worker", json!({}), "kv_snapshot"),
        ("cito_find", json!({"hub":"work"}), "kv_snapshot"),
        // listen's final directory read starts after both Hub and local enrollment commit.
        (
            "cito_listen",
            json!({"about":"Review","hub":"work"}),
            "kv_snapshot",
        ),
        (
            "cito_send",
            json!({"to":"work:@self","message":"Review"}),
            "js_publish",
        ),
        ("cito_unlisten", json!({"hub":"work"}), "kv_get"),
        (
            "query_directory",
            json!({"hub":"work","request_id":"query"}),
            "kv_snapshot",
        ),
        (
            "cli_human_device",
            json!({"hub":"work","attach":true}),
            "kv_snapshot",
        ),
        ("unlisten_all", json!({"hub":"work"}), "kv_get"),
        (
            "send_signal",
            json!({"to":"work:@self","message":"Review"}),
            "send_reply_from_human",
        ),
    ] {
        let dir =
            TestDirectory(std::env::temp_dir().join(format!("cito-mcp-rpc-{}", random_uuid_v4())));
        fs::create_dir_all(dir.join("home")).unwrap();
        let config = NativeConfig {
            state_dir: dir.join("state"),
            config_path: dir.join("config.json"),
            home_dir: dir.join("home"),
            daemon_port: 0,
            user: "alice".into(),
            default_hub: "local".into(),
        };
        fs::write(&config.config_path,r#"{"default_hub":"local","hubs":{"local":{"url":"local://"},"work":{"url":"local://"}}}"#).unwrap();
        let provider = dir.join("plugins/fixture");
        fs::create_dir_all(&provider).unwrap();
        fs::write(provider.join("plugin.json"),json!({"id":"fixture","kind":"agent","displayName":"Fixture","command":["python3","-c","import sys,json; json.load(sys.stdin); print(json.dumps({'ok':True,'caller':{'harness':'fixture','session_id':'session','cwd':'/workspace'}}))"]}).to_string()).unwrap();
        let mut s = TestService::open(
            &config,
            &LoadedPlugins {
                plugins: vec![crate::plugins::parse_manifest_dir(&provider, false)?],
                rejected_ids: Default::default(),
            },
        )?;
        if matches!(
            tool,
            "directory_worker" | "cito_unlisten" | "unlisten_all" | "send_signal"
        ) {
            let binding = s.listen(
                "work",
                &json!({"about":"Stop me"}),
                &json!({"harness":"fixture","session_id":"session"}),
            )?;
            if tool == "send_signal" {
                args["to"] = binding["listener_id"].clone();
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mut cfg = s.slot("work")?.config.clone();
        cfg["url"] = json!(url);
        s.mutate_config(|raw| {
            raw["hubs"]["work"]["url"] = json!(url);
            Ok(())
        })?;
        s.state.db.execute(
            "UPDATE local_listeners SET hub_url=? WHERE hub='work'",
            &[json!(url)],
        )?;
        let mut remote = HubClient::new("work".into(), cfg, "alice", None);
        remote.registration = s.state.registration_lock("work")?;
        let principal = remote.identity.clone();
        let engine = s.local.clone();
        s.replace_slot(remote);
        let service = Arc::new(Mutex::new(s.into_inner()));
        let _cleanup = ServiceCleanup {
            service: service.clone(),
        };
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        thread::scope(|threads| -> Result<(), String> {
            let server = threads.spawn(move || -> Result<(), String> {
                loop {
                    let mut stream = crate::core::http_boundary::accept_test_client(&listener)?;
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let head = crate::core::http_boundary::read_head(&mut reader, 16384)
                        .map_err(|(_, e)| e)?;
                    let mut bytes = vec![0; head.length];
                    reader.read_exact(&mut bytes).unwrap();
                    let req: Value = serde_json::from_slice(&bytes).unwrap();
                    let op = string(&req, "op");
                    let result = engine.rpc(op, &req["params"], Some(&principal))?;
                    if op == gate {
                        entered.send(()).unwrap();
                        wait.recv_timeout(Duration::from_secs(4))
                            .map_err(|e| e.to_string())?;
                    }
                    respond(&mut stream, 200, &json!({"ok":true,"result":result}));
                    if op == gate {
                        return Ok(());
                    }
                }
            });
            if tool == "directory_worker" {
                let (old, id) = {
                    let mut s = service.lock().unwrap();
                    let id = s.session_node_id("work", "fixture", "session")?;
                    s.action(&json!({"op":"set_auto_listen","hub":"work","enabled":true}))?;
                    s.action(&json!({"op":"set_listen_mode","hub":"work","listener_id":id,"listen_mode":"auto"}))?;
                    assert_eq!(
                        s.state.listener("work", &id)?.unwrap()["listen_mode"],
                        "auto"
                    );
                    let old = s.slot("work")?.clone();
                    // The witness is the scheduled directory worker; do not gate a
                    // receiver or an enrollment's own independent RPC instead.
                    for row in s.state.active()? {
                        let hub = string(&row, "hub");
                        let revision = s.slot(hub)?.event_revision();
                        s.observations.insert(
                            format!("{hub}:{}", string(&row, "listener_id")),
                            (revision, Instant::now() + Duration::from_secs(30)),
                        );
                    }
                    s.poll()?;
                    assert!(s.receivers.contains_key("directory:work"));
                    (old, id)
                };
                ready
                    .recv_timeout(Duration::from_secs(3))
                    .map_err(|e| e.to_string())?;
                for action in [
                    json!({"op":"set_listen_mode","hub":"work","listener_id":id,"listen_mode":"manual"}),
                    json!({"op":"set_auto_listen","hub":"work","enabled":false}),
                    json!({"op":"test_hub","hub":"local"}),
                ] {
                    let start = Instant::now();
                    http_actions::dispatch_action(&service, &action, None)?;
                    assert!(
                        start.elapsed() < Duration::from_millis(300),
                        "{action}: {:?}",
                        start.elapsed()
                    );
                }
                let (held, entered) = std::sync::mpsc::channel();
                let (done, hold) = std::sync::mpsc::channel();
                let lock = old.registration.clone();
                threads.spawn(move || {
                    let _reg = lock.lock().unwrap();
                    held.send(()).unwrap();
                    let _ = hold.recv_timeout(Duration::from_secs(2));
                });
                entered
                    .recv_timeout(Duration::from_secs(1))
                    .map_err(|e| e.to_string())?;
                let start = Instant::now();
                {
                    let mut s = service.lock().unwrap();
                    assert_eq!(
                        s.state.listener("work", &id)?.unwrap()["listen_mode"],
                        "manual"
                    );
                    s.mutate_config(|v| {
                        v["hubs"]["work"]["url"] = json!("local://");
                        Ok(())
                    })?;
                    s.reload_slots()?;
                    assert!(old.closed.load(Ordering::Acquire));
                    assert!(!Arc::ptr_eq(&old.closed, &s.slot("work")?.closed));
                    assert_eq!(s.raw_config()?["hubs"]["work"]["autoListen"], false);
                }
                assert!(start.elapsed() < Duration::from_millis(300));
                done.send(()).map_err(|e| e.to_string())?;
                release.send(()).map_err(|e| e.to_string())?;
                return server.join().unwrap();
            }
            let local = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = local.local_addr().unwrap();
            let mut client = TcpStream::connect(addr).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let stream = crate::core::http_boundary::accept_test_client(&local)?;
            let mcp = tool.starts_with("cito_");
            let (path, body) = if mcp {
                (
                    "/mcp",
                    json!({"id":1,"jsonrpc":"2.0","method":"tools/call","params":{"name":tool,"arguments":args,"_meta":{"expected_harness":"fixture"}}}),
                )
            } else {
                let mut action = args;
                action["op"] = json!(tool);
                ("/action", action)
            };
            let body = body.to_string();
            write!(client,"POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
            let shared = service.clone();
            let worker = threads
                .spawn(move || http_actions::handle_http(stream, shared, Arc::new(json!({}))));
            ready
                .recv_timeout(Duration::from_secs(4))
                .map_err(|e| format!("{tool} never entered {gate}: {e}"))?;
            http_actions::dispatch_action(&service, &json!({"op":"test_hub","hub":"local"}), None)?;
            let disable = json!({"op":"setup_toggle_hub","hub":"work","enabled":false});
            if matches!(tool, "cito_send" | "send_signal") {
                // Return the committed publication, then switch authority before
                // the request can re-enter the service to project that result.
                let mut s = service.lock().unwrap();
                release.send(()).map_err(|_| "Publication held service")?;
                let deadline = Instant::now() + Duration::from_secs(3);
                while s.state.db.query("SELECT 1 FROM local_sent_idempotency WHERE hub='work' AND status='wake_accepted'",&[])?.is_empty() {
                    assert!(Instant::now()<deadline,"publication was never recorded");
                    thread::yield_now();
                }
                s.action(&disable)?;
            } else {
                http_actions::dispatch_action(&service, &disable, None)?;
                release
                    .send(())
                    .map_err(|_| format!("{tool} held service across {gate}"))?;
            }
            server.join().unwrap()?;
            worker.join().unwrap();
            let raw = crate::plugins::read_http_response(&mut client)?;
            let rpc: Value = serde_json::from_str(&raw.body).unwrap();
            let result: Value = if mcp {
                serde_json::from_str(string(&rpc["result"]["content"][0], "text")).unwrap()
            } else {
                rpc
            };
            if matches!(tool, "cito_listen" | "cli_human_device") {
                assert_eq!(result["committed"], true, "{result}");
                assert!(string(&result, "warning").contains("STALE_GENERATION"));
            } else if matches!(tool, "cito_send" | "send_signal") {
                assert_eq!(result["committed"], true, "{result}");
                assert!(string(&result, "signal_id").starts_with("work:sig_"));
                assert!(string(&result, "maintenance_warning").contains("STALE_GENERATION"));
            } else if tool == "cito_unlisten" {
                assert!(
                    result["committed"] == true || result["error"]["code"] == "DETACH_PENDING",
                    "{result}"
                );
            } else if tool == "unlisten_all" {
                assert!(
                    result["committed"] == true
                        || string(&result, "error").starts_with("DETACH_PENDING:"),
                    "{result}"
                );
                assert_eq!(service.lock().unwrap().state.db.query("SELECT user_stopped FROM local_listeners WHERE hub='work' AND harness='fixture'",&[])?[0]["user_stopped"],1);
            } else if tool == "query_directory" {
                assert!(
                    string(&result, "error").starts_with("STALE_GENERATION:"),
                    "{result}"
                );
            } else {
                assert_eq!(
                    result["error"]["code"], "STALE_GENERATION",
                    "{tool}: {result}"
                );
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn invite_join_settles_each_request_and_records_its_saved_hub() -> Result<(), String> {
    let dir = TestDirectory(std::env::temp_dir().join(format!("cito-invite-{}", random_uuid_v4())));
    fs::create_dir_all(dir.join("home")).unwrap();
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
        r#"{"hubs":{"local":{"url":"local://"},"work":{"url":"local://"}}}"#,
    )
    .unwrap();
    let service = Arc::new(Mutex::new(DaemonService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        },
    )?));
    let _cleanup = ServiceCleanup {
        service: service.clone(),
    };
    let derived = super::host_hub::hub_name_from_url("local://");
    for (request, target) in [
        ("parse", "work"),
        ("existing", "work"),
        ("derived", derived.as_str()),
        ("reject", "work"),
        ("warn", "warning"),
    ] {
        let mailbox = Arc::new(UiMailbox::default());
        let mut invite = json!({"name":target,"url":"local://","set_default":false});
        if request == "derived" {
            invite.as_object_mut().unwrap().remove("name");
        }
        if request == "reject" {
            invite["authMode"] = json!("token");
        }
        {
            let mut s = service.lock().unwrap();
            s.ui = Some(mailbox.clone());
            s.last_hub_errors
                .insert(target.to_owned(), ("old error".into(), false));
            if request == "warn" {
                s.state.db.execute("CREATE TRIGGER fail_warning_enrollment BEFORE INSERT ON local_listeners WHEN NEW.hub='warning' BEGIN SELECT RAISE(ABORT,'projection unavailable'); END",&[])?;
            }
        }
        let result = http_actions::dispatch_action(
            &service,
            &json!({"op":"paste_hub_invite","invite":invite.to_string(),"request_id":request,"parse_only":request=="parse"}),
            None,
        );
        let mut frames = Vec::new();
        while !mailbox.pending.lock().unwrap().is_empty() {
            let frame: Value = serde_json::from_slice(&mailbox.next().unwrap()).unwrap();
            if frame["request_id"] == request {
                assert_eq!(frame["source_op"], "paste_hub_invite");
                frames.push(frame);
            }
        }
        let s = service.lock().unwrap();
        match request {
            "parse" => {
                assert!(result?["committed"].is_null());
                assert_eq!(
                    frames.iter().map(|v| string(v, "op")).collect::<Vec<_>>(),
                    ["hub_invite"]
                );
                assert_eq!(s.last_hub_errors[target].0, "old error");
            }
            "reject" => {
                let error = result.unwrap_err();
                assert_eq!(s.last_hub_errors[target].0, error);
                assert_eq!(frames.len(), 1);
                assert_eq!(frames[0]["error"], error);
            }
            _ => {
                let result = result?;
                assert_eq!(result["committed"], true);
                assert_eq!(s.raw_config()?["hubs"][target]["url"], "local://");
                assert!(!s.last_hub_errors.contains_key(target));
                assert_eq!(
                    frames.iter().map(|v| string(v, "op")).collect::<Vec<_>>(),
                    ["settings", "state"]
                );
                assert!(frames.iter().all(|v| v["committed"] == true));
                assert_eq!(frames[0]["warning"], result["warning"]);
                assert_eq!(result["warning"].is_string(), request == "warn");
            }
        }
    }
    Ok(())
}

#[test]
fn hub_discovery_releases_service_and_config_locks_and_fences_concurrent_edits(
) -> Result<(), String> {
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::mpsc;
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-discovery-{}", random_uuid_v4())));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    fs::write(&config.config_path, "{}").map_err(|e| e.to_string())?;
    let service = Arc::new(Mutex::new(DaemonService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        },
    )?));
    let _cleanup = ServiceCleanup {
        service: service.clone(),
    };
    let result = (|| -> Result<(), String> {
        for op in ["setup_save_hub", "save_hub", "paste_hub_invite"] {
            let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
            let url = format!(
                "http://{}",
                listener.local_addr().map_err(|e| e.to_string())?
            );
            let (started, observed) = mpsc::sync_channel(1);
            let (release, wait) = mpsc::sync_channel(1);
            let server = thread::spawn(move || -> Result<(), String> {
                let mut stream = crate::core::http_boundary::accept_test_client(&listener)?;
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .map_err(|e| e.to_string())?;
                let mut bytes = [0; 4096];
                stream.read(&mut bytes).map_err(|e| e.to_string())?;
                started.send(()).map_err(|e| e.to_string())?;
                wait.recv_timeout(Duration::from_secs(3))
                    .map_err(|e| e.to_string())?;
                let body = json!({"auth_mode":"oidc","oidc":{"issuer":"https://id.example.com","client_id":"client","redirect_port":4280}}).to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .map_err(|e| e.to_string())
            });
            let mut action = json!({"op":op,"name":"work","url":url,"enabled":false});
            if op == "paste_hub_invite" {
                action = json!({"op":op,"invite":action.to_string()});
            }
            let worker_service = service.clone();
            let worker = thread::spawn(move || {
                super::http_actions::dispatch_action(&worker_service, &action, None)
            });
            observed
                .recv_timeout(Duration::from_secs(3))
                .map_err(|e| e.to_string())?;
            let s = service
                .try_lock()
                .map_err(|_| "Discovery held the service mutex")?;
            crate::plugins::with_config_lock(&config.config_path, || Ok(()))?;
            let concurrent = op == "save_hub";
            if concurrent {
                s.mutate_config(|raw| {
                    raw["theme"] = json!("dark");
                    Ok(())
                })?;
            }
            release.send(()).map_err(|e| e.to_string())?;
            drop(s);
            let saved = worker.join().map_err(|_| "Discovery worker panicked")?;
            server.join().map_err(|_| "Health fixture panicked")??;
            if concurrent {
                assert!(saved.unwrap_err().contains("Configuration changed"));
            } else {
                assert_eq!(saved?["committed"], true);
                let s = service.lock().map_err(|_| "Daemon lock poisoned")?;
                assert_eq!(s.raw_config()?["hubs"]["work"]["redirectPort"], 4280);
                assert_eq!(
                    s.settings()["hubs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|hub| hub["name"] == "work")
                        .unwrap()["redirect_port"],
                    4280
                );
            }
        }
        Ok(())
    })();
    result
}

#[test]
fn ipc_marks_only_collections_shed_to_fit_the_wire_budget() {
    let frame = json!({"op":"state","activity":vec![json!({"message":"short"});81],"listeners":vec![json!({});65]});
    let bounded: Value = serde_json::from_slice(&bounded_frame(frame).unwrap()).unwrap();
    assert_eq!(bounded["activity"].as_array().unwrap().len(), 80);
    assert_eq!(bounded["listeners"].as_array().unwrap().len(), 64);
    assert!(bounded.get("truncated").is_none());
    assert!(bounded.get("truncated_collections").is_none());

    // The final newline counts toward the budget. One extra byte sheds the
    // activity row; peers still fit with the resulting diagnostic fields.
    let mut edge =
        json!({"cwd":"", "activity":[{"message":"a".repeat(128)}], "peers":[{"name":"alice"}]});
    let padding = FRAME_LIMIT - 2 - serde_json::to_vec(&edge).unwrap().len();
    edge["cwd"] = json!("x".repeat(padding));
    assert_eq!(
        bounded_frame(edge.clone()).unwrap(),
        serde_json::to_vec(&edge).unwrap()
    );
    edge["cwd"] = json!("x".repeat(padding + 1));
    let partial: Value = serde_json::from_slice(&bounded_frame(edge.clone()).unwrap()).unwrap();
    assert_eq!(partial["truncated"], true);
    assert_eq!(partial["truncated_collections"], json!(["activity"]));
    assert_eq!(partial["activity"], json!([]));
    assert_eq!(partial["peers"], edge["peers"]);
    assert_eq!(partial["cwd"], edge["cwd"]);

    let keys = [
        "activity",
        "peers",
        "my_remote_listeners",
        "available",
        "listeners",
        "harnesses",
        "hubs",
        "apps",
        "channels",
    ];
    let mut frame = json!({"settings":{"hubs":[1]},"cwd":"x".repeat(FRAME_LIMIT)});
    for key in keys {
        let root = if matches!(key, "apps" | "channels") {
            &mut frame["settings"]
        } else {
            &mut frame
        };
        root[key] = json!([1, 2]);
    }
    let bytes = bounded_frame(frame).unwrap();
    assert!(bytes.len() + 1 < FRAME_LIMIT);
    let bounded: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(bounded["truncated"], true);
    let mut shed = keys.to_vec();
    shed.push("settings.hubs");
    assert_eq!(bounded["truncated_collections"], json!(shed));
    assert_eq!(bounded["settings"]["hubs"], json!([]));
    for key in keys {
        assert_eq!(
            bounded.get(key).unwrap_or(&bounded["settings"][key]),
            &json!([])
        );
    }
    let bounded: Value = serde_json::from_slice(
        &ui_ipc::bounded_frame(json!({"op":"state","activity":[{"message":"x".repeat(513)}],"listeners":[{"cwd":"x".repeat(129),"title":"x".repeat(513),"about":"x".repeat(257)}]})).unwrap(),
    )
    .unwrap();
    assert!(bounded.get("truncated").is_none());
}

fn settle_setup_inspections(service: &mut DaemonService) {
    service.refresh_setup_inspections();
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

fn take_state_frame(ui: &UiMailbox) -> Value {
    ui.pending
        .lock()
        .unwrap()
        .drain(..)
        .map(|(_, bytes)| serde_json::from_slice::<Value>(&bytes).unwrap())
        .rfind(|frame| frame["op"] == "state")
        .expect("Missing state frame")
}

#[test]
fn background_push_projects_current_human_channels_and_complete_ordered_hubs() -> Result<(), String>
{
    let mut fixture = event_tests::Fixture::new()?;
    let service = &mut fixture.service;
    service.mutate_config(|raw| {
        raw["hubs"]["archive"] = json!({"url":"local://","enabled":false});
        raw["hubs"]["work"]["redirect_port"] = json!(4321);
        Ok(())
    })?;
    service.reload_slots()?;
    let ui = Arc::new(UiMailbox::default());
    service.ui = Some(ui.clone());
    service.push_ui();
    let initial = take_state_frame(&ui);
    let names = |rows: &Value| {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&initial["settings"]["hubs"]),
        ["archive", "local", "work"]
    );
    assert_eq!(names(&initial["hubs"]), ["local", "work"]);
    assert_eq!(initial["hubs"][1]["redirect_port"], 4321);
    let channels = |frame: &Value| {
        frame["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["hub"] == "work" && r["harness"] == "human")
            .unwrap()["channels"]
            .clone()
    };
    assert_ne!(channels(&initial), json!(["Laptop"]));
    service.slot("work")?.human_request("human_device_attach", &json!({"channel_id":"second:human","host_id":"second","host_name":"Laptop","binding_generation":1,"owner_token":"fixture"}))?;
    service.push_ui();
    let status = take_state_frame(&ui);
    let full = service.snapshot()?;
    assert!(status.get("settings").is_none());
    for frame in [&status, &full] {
        assert!(frame.get("sessions").is_none());
        assert_eq!(channels(frame), json!(["Laptop"]));
    }
    assert_eq!(status["hubs"], full["hubs"]);
    assert_eq!(
        status["hubs"].as_array().unwrap(),
        &full["settings"]["hubs"].as_array().unwrap()[1..]
    );
    Ok(())
}

#[test]
fn setup_completion_invalidates_discovery_and_app_action_invalidates_settings() -> Result<(), String>
{
    let mut fixture = event_tests::Fixture::new()?;
    let service = &mut fixture.service;
    service.refresh_available()?;
    for slot in service.slots.values() {
        slot.close();
    }
    for event in service.events.drain(..) {
        event.join().unwrap();
    }
    let ui = Arc::new(UiMailbox::default());
    service.ui = Some(ui.clone());
    service.push_ui();
    let initial = take_state_frame(&ui);
    for key in ["available", "discovery"] {
        assert_eq!(initial[key][0]["harness"], "agent", "{initial}");
    }
    fs::write(
        service.config.config_path.with_file_name("unconfigured"),
        "",
    )
    .unwrap();
    service.invalidate_setup("agent");
    settle_setup_inspections(service);
    service.push_ui();
    let closed = take_state_frame(&ui);
    assert!(closed.get("settings").is_none());
    for key in ["available", "discovery"] {
        assert_eq!(closed[key], json!([]));
    }
    service.state.db.execute(
        "UPDATE local_listeners SET title='Updated' WHERE harness='human'",
        &[],
    )?;
    service.push_ui();
    assert_eq!(take_state_frame(&ui)["available"], json!([]));
    service.menu_open = true;
    service.push_ui();
    take_state_frame(&ui);
    service
        .last_app_action
        .insert("agent".into(), "disconnect".into());
    service.push_ui();
    let frame = take_state_frame(&ui);
    let app = frame["settings"]["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "agent")
        .unwrap();
    assert!(string(app, "detail").contains("Disconnected"));
    service.push_ui();
    assert!(ui.pending.lock().unwrap().is_empty());
    Ok(())
}

#[test]
fn external_hub_removal_reconciles_runtime_caches_without_purging_disabled_profiles(
) -> Result<(), String> {
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-removed-hub-{}", random_uuid_v4())));
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
        r#"{"hubs":{"local":{"url":"local://"},"work":{"url":"local://","enabled":false}}}"#,
    )
    .map_err(|e| e.to_string())?;
    let plugins = LoadedPlugins {
        plugins: vec![],
        rejected_ids: HashSet::new(),
    };
    let mut service = TestService::open(&config, &plugins)?;
    let result = (|| -> Result<(), String> {
        for hub in ["local", "work"] {
            service
                .last_hub_errors
                .insert(hub.into(), ("Retained error".into(), false));
            service.auth_backoff.insert(hub.into(), Default::default());
            service
                .observations
                .insert(format!("{hub}:node"), (1, Instant::now()));
            service.channel_inventories.insert(
                hub.into(),
                Arc::new(ChannelInventory::new(json!({"hub":hub}))),
            );
            fs::write(
                config.state_dir.join(format!("oidc-token-{hub}.json")),
                "credential",
            )
            .map_err(|e| e.to_string())?;
        }
        service.reload_slots()?;
        assert!(service.last_hub_errors.contains_key("work"));
        assert!(config.state_dir.join("oidc-token-work.json").exists());
        service.mutate_config(|raw| {
            raw["hubs"].as_object_mut().unwrap().remove("work");
            Ok(())
        })?;
        service.reload_slots()?;
        assert!(!service.last_hub_errors.contains_key("work"));
        assert!(!service.auth_backoff.contains_key("work"));
        assert!(!service.observations.contains_key("work:node"));
        assert!(!service.channel_inventories.contains_key("work"));
        assert!(!config.state_dir.join("oidc-token-work.json").exists());
        assert!(service.last_hub_errors.contains_key("local"));
        assert!(service.auth_backoff.contains_key("local"));
        assert!(service.observations.contains_key("local:node"));
        assert!(service.channel_inventories.contains_key("local"));
        assert!(config.state_dir.join("oidc-token-local.json").exists());
        Ok(())
    })();
    result
}

fn poll_config(dir: &TestDirectory, profile: Value) -> NativeConfig {
    fs::create_dir_all(dir.join("home")).unwrap();
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    fs::write(&config.config_path, profile.to_string()).unwrap();
    config
}

#[test]
fn poll_skips_contended_enrollment_without_delaying_directory_or_next_enrollment(
) -> Result<(), String> {
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-poll-lock-{}", random_uuid_v4())));
    let config = poll_config(&dir, json!({}));
    let mut service = TestService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        },
    )?;
    let slot = service.slot("local")?.clone();
    let row = service.state.active()?.pop().unwrap();
    let id = string(&row, "listener_id");
    service.state.db.execute(
        "DELETE FROM local_listeners WHERE listener_id=?",
        &[json!(id)],
    )?;
    // Change the Hub directly so only a directory read can populate this client's cache.
    service.local.rpc("human_device_attach", &json!({"channel_id":"second:human","host_id":"second","host_name":"Laptop","binding_generation":1,"owner_token":"fixture"}), Some(&slot.identity))?;
    let observed = || {
        slot.cached_directory()
            .iter()
            .any(|r| r["channels"]["second:human"].is_object())
    };
    assert!(!observed());
    thread::scope(|scope| -> Result<(), String> {
        let guard = slot.registration.lock().unwrap();
        let (finished, result) = std::sync::mpsc::channel();
        let service = &mut *service;
        scope.spawn(move || {
            let _ = finished.send(service.poll());
        });
        result
            .recv_timeout(Duration::from_secs(3))
            .map_err(|_| "Poll waited for enrollment")??;
        let deadline = Instant::now() + Duration::from_secs(3);
        while !observed() {
            assert!(
                Instant::now() < deadline,
                "Directory RPC stalled behind enrollment"
            );
            thread::yield_now();
        }
        assert_eq!(
            slot.kv_get(id)?.unwrap().0["channels"][format!("{}:human", string(&row, "host_id"))]
                ["binding_generation"],
            row["binding_generation"]
        );
        drop(guard);
        Ok(())
    })?;
    assert!(service.state.listener("local", id)?.is_none());
    // No backoff: the first uncontended poll may enroll immediately.
    if let Some(worker) = service.receivers.remove("directory:local") {
        worker.join().unwrap();
    }
    service.poll()?;
    assert_eq!(
        service.state.listener("local", id)?.unwrap()["status"],
        "ACTIVE"
    );
    Ok(())
}

#[test]
fn poll_dispatches_due_wake_retry_without_live_sse() -> Result<(), String> {
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-poll-wake-{}", random_uuid_v4())));
    let engine = TestEngine(HubEngine::open(&dir.join("hub"))?);
    let server = engine.start_server(
        "127.0.0.1",
        0,
        crate::core::auth::HubAuth::new("none", None, None, None)?,
    )?;
    let config = poll_config(
        &dir,
        json!({"hubs":{"work":{"url":format!("http://{}",server.address)}}}),
    );
    let provider = dir.join("fixture");
    fs::create_dir_all(&provider).unwrap();
    fs::write(provider.join("plugin.json"), json!({"id":"fixture","kind":"agent","displayName":"Fixture","command":["python3","-c","import json,sys; r=json.load(sys.stdin); print(json.dumps({'ok':True,'outcome':'delivered'} if r.get('queue_written') else {'ok':True,'queue_written':True,'continue_wake':True}))"]}).to_string()).unwrap();
    let mut service = TestService::open(
        &config,
        &LoadedPlugins {
            plugins: vec![crate::plugins::parse_manifest_dir(&provider, false)?],
            rejected_ids: Default::default(),
        },
    )?;
    if let Some(worker) = service.receivers.remove("auto:work") {
        worker.join().unwrap();
    }
    // Model a disconnected stream while preserving the real HTTP/RPC Hub and its bindings.
    let old = service.slot("work")?.clone();
    let mut slot = HubClient::new(old.name.clone(), old.config.clone(), "alice", None);
    slot.registration = old.registration;
    service.replace_slot(slot.clone());
    let binding = service.listen(
        "work",
        &json!({"about":"Retry test"}),
        &json!({"harness":"fixture","session_id":"review"}),
    )?;
    let id = string(&binding, "canonical_listener_id");
    let key = format!("work:{id}");
    let sent = service.tool(
        "cito_send",
        &json!({"to":format!("work:{id}"),"message":"Wake once"}),
        &json!({}),
    )?;
    assert_eq!(sent["ok"], true, "{sent}");
    service.poll()?;
    service
        .receivers
        .remove(&key)
        .expect("initial receiver")
        .join()
        .unwrap();
    let rows = || {
        service.state.db.query(
            "SELECT state,queue_written FROM local_signals WHERE hub='work' AND listener_id=?",
            &[json!(id)],
        )
    };
    assert_eq!(rows()?, [json!({"state":"DISPATCHING","queue_written":1})]);
    assert_eq!(slot.capabilities()["sse_live"], false);
    let set_due = |state: &LocalState, due| -> Result<(), String> {
        assert_eq!(
            state.db.execute(
                "UPDATE wake_retries SET retry_at_ms=? WHERE hub='work' AND listener_id=?",
                &[json!(due), json!(id)]
            )?,
            1
        );
        Ok(())
    };
    set_due(&service.state, now_ms() + 60_000)?;
    service.poll()?;
    assert!(
        !service.receivers.contains_key(&key),
        "Regular interval and retry are not due"
    );
    set_due(&service.state, 0)?;
    service.poll()?;
    service
        .receivers
        .remove(&key)
        .expect("due retry receiver")
        .join()
        .unwrap();
    assert_eq!(
        service.state.db.query(
            "SELECT state FROM local_signals WHERE hub='work' AND listener_id=?",
            &[json!(id)]
        )?,
        [json!({"state":"DISPATCHED"})]
    );
    assert!(service
        .state
        .db
        .query(
            "SELECT 1 FROM wake_retries WHERE hub='work' AND listener_id=?",
            &[json!(id)]
        )?
        .is_empty());
    Ok(())
}

#[test]
fn targeted_probe_restores_manual_badge_with_closed_surfaces_and_no_auto_listen(
) -> Result<(), String> {
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-probe-badge-{}", random_uuid_v4())));
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
        r#"{"hubs":{"local":{"url":"local://","autoListen":false}}}"#,
    )
    .map_err(|e| e.to_string())?;
    let script = dir.join("provider.py");
    fs::write(&script, r#"import json, pathlib, sys
root = pathlib.Path(__file__).parent
req = json.loads(sys.stdin.readline())
op = req['op']
result = dict(ok=True)
if op == 'setup_inspect': result.update(installed=True, configured=True, paths=[])
elif op == 'discover_sessions':
    (root/'discovered').touch()
    result['sessions'] = []
elif op == 'probe':
    result.update(status='alive', current_session_id=req['session_id'], badge=json.loads((root/'badge.json').read_text()) if (root/'badge.json').exists() else None)
elif op == 'update_badge':
    (root/'badge.json').write_text(json.dumps(req['badge']))
    with (root/'effects').open('a') as log: log.write(json.dumps(req['badge'])+'\n')
print(json.dumps(result))
"#).map_err(|e| e.to_string())?;
    let plugin = crate::plugins::PluginDescriptor {
        id: "fixture".into(),
        kind: "agent".into(),
        display_name: "Fixture".into(),
        tier: "test".into(),
        order: 0,
        sf_symbol: "terminal".into(),
        tint: "blue".into(),
        cli_flag: false,
        pid_bound: false,
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
    };
    let plugins = LoadedPlugins {
        plugins: vec![plugin],
        rejected_ids: HashSet::new(),
    };
    let mut service = TestService::open(&config, &plugins)?;
    service.local.set_plugin_roots(&[])?;
    let result = (|| -> Result<(), String> {
        settle_setup_inspections(&mut service);
        service.listen(
            "local",
            &json!({"about":"Manual"}),
            &json!({"harness":"fixture","session_id":"session","title":"Work","cwd":*dir}),
        )?;
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            service.poll()?;
            if fs::read_to_string(dir.join("badge.json"))
                .ok()
                .is_some_and(|s| s.contains("Cito"))
            {
                break;
            }
            assert!(Instant::now() < deadline, "Initial badge did not arrive");
            thread::sleep(Duration::from_millis(5));
        }
        // Let the original effect and targeted probe settle before modeling reload.
        for probe in service.probes.values_mut() {
            if let Some(worker) = probe.worker.take() {
                let _ = worker.join();
            }
            probe.due = Instant::now();
        }
        let expected = fs::read_to_string(dir.join("badge.json")).map_err(|e| e.to_string())?;
        fs::write(dir.join("badge.json"), "null").map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            service.poll()?;
            if fs::read_to_string(dir.join("badge.json")).ok().as_deref() == Some(expected.as_str())
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Targeted observation did not restore badge"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !service.menu_open && !service.directory_open && service.active_settings_hub.is_none()
        );
        assert_eq!(service.slot("local")?.config["autoListen"], false);
        assert!(
            !dir.join("discovered").exists(),
            "The daemon's real demand must keep background discovery off"
        );
        Ok(())
    })();
    result
}

#[test]
fn activity_auto_listen_pin_stop_idle_and_reopen_follow_orthogonal_intent() -> Result<(), String> {
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-lifecycle-{}", random_uuid_v4())));
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
    let mut service = TestService::open(&config, &plugins)?;
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
        service.action(&json!({"op":"set_auto_listen","hub":"local","enabled":false}))?;
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
        service.action(&json!({"op":"set_auto_listen","hub":"local","enabled":true}))?;
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
        let client = service.slot("local")?;
        let channel = format!("{}:fixture", service.host_id);
        assert!(DaemonService::heartbeat_agent(
            &service.state,
            client,
            &channel,
            &manual
        )?);
        assert_eq!(client.kv_get(&id)?.unwrap().0["listen_mode"], "auto");
        assert!(!DaemonService::heartbeat_agent(
            &service.state,
            client,
            &channel,
            &stopped
        )?);
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
        service.state.db.execute(
            "INSERT OR REPLACE INTO wake_retries VALUES('local',?,?)",
            &[json!(id), json!(now_ms() + 30_000)],
        )?;
        let lock = service.state.registration_lock("local")?;
        let (ready, rx) = std::sync::mpsc::channel();
        let (release, done) = std::sync::mpsc::channel();
        thread::scope(|scope| -> Result<(), String> {
            scope.spawn(move || {
                let _guard = lock.lock().unwrap();
                ready.send(()).unwrap();
                let _ = done.recv_timeout(Duration::from_secs(2));
            });
            rx.recv().unwrap();
            service.auto_listen()?;
            assert_eq!(
                service.state.listener("local", &id)?.unwrap()["status"],
                "ACTIVE"
            );
            release.send(()).unwrap();
            Ok(())
        })?;
        service.auto_listen()?;
        assert!(
            service
                .state
                .db
                .query(
                    "SELECT * FROM wake_retries WHERE hub='local' AND listener_id=?",
                    &[json!(id)]
                )?
                .is_empty(),
            "Idle retirement must clear scheduling left by prior work"
        );
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
            service.action(&json!({"op":"set_auto_listen","hub":"local","enabled":true}))?;
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
    result
}

#[test]
fn oidc_pre_expiry_recovery_preserves_slot_and_inventory_until_verified_replacement(
) -> Result<(), String> {
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-renewal-{}", random_uuid_v4())));
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
    let mut service = TestService::open(
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
    service.replace_slot(old);
    service
        .receivers
        .insert("auto:work".into(), thread::spawn(|| {}));
    let (tx, rx) = std::sync::mpsc::channel();
    service.oidc_recoveries.insert(
        "work".into(),
        OidcRecovery {
            config: hub.clone(),
            rejected: None,
            cancelled: Arc::new(super::super::login::Cancellation::default()),
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
    let expires = now_ms() / 1000 + 300;
    let token = format!(
        "header.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({"exp":expires as f64 + 0.5}).to_string())
    );
    tx.send(Ok((token.clone(), identity)))
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(1);
    while !service.oidc_recoveries["work"].worker.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    service.reload_slots()?;
    assert_eq!(service.slots["work"].expires_at, expires);
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
    let cancelled = Arc::new(super::super::login::Cancellation::default());
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
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-daemon-unit-{}", random_uuid_v4())));
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
    let mut service = TestService::open(&config, &plugins)?;
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
        assert_eq!(updated["committed"], true);
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
        let args = json!({"to":target["listener_id"],"message":"Review | ready","idempotency_key":"same|key"});
        service.state.db.execute("CREATE TRIGGER reject_sent_maintenance BEFORE UPDATE ON local_sent_idempotency BEGIN SELECT RAISE(ABORT,'maintenance unavailable'); END", &[])?;
        let committed_args = json!({"to":target["listener_id"],"message":"Committed despite local maintenance","idempotency_key":"maintenance"});
        let committed = service.send("local", &committed_args, &json!({}))?;
        assert_eq!(committed["ok"], true);
        assert!(committed["seq"].as_u64().is_some_and(|seq| seq > 0));
        assert!(committed["maintenance_warning"]
            .as_str()
            .unwrap()
            .contains("local sent history"));
        service
            .state
            .db
            .execute("DROP TRIGGER reject_sent_maintenance", &[])?;
        let recovered = service.send("local", &committed_args, &json!({}))?;
        assert_eq!(recovered["signal_id"], committed["signal_id"]);
        assert_eq!(recovered["seq"], committed["seq"]);
        assert!(recovered["maintenance_warning"].is_null());
        let first = service.send_from("local", &args, None)?;
        let endpoint = format!(
            "{}|{}|unbound|default",
            string(&service.slot("local")?.identity, "principal_id"),
            service.host_id
        );
        let old_key = crate::core::identity::hash(
            &json!([
                endpoint,
                "local://",
                target["listener_id"],
                args["idempotency_key"]
            ])
            .to_string(),
        );
        assert_eq!(
            service.state.db.execute(
                "UPDATE local_sent_idempotency SET dedup_id=? WHERE seq=?",
                &[json!(old_key), first["seq"].clone()]
            )?,
            1
        );
        assert_eq!(
            service.send_from("local", &args, None)?["signal_id"],
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
            3
        );
        service
            .slots
            .get_mut("local")
            .ok_or("Missing local slot")?
            .config["url"] = json!("local://");
        service.listen(
            "local",
            &json!({"about":"Historical manual binding"}),
            &json!({"harness":"fixture","session_id":"closed","title":"Closed"}),
        )?;
        let closed = service.session_node_id("local", "fixture", "closed")?;
        service.retire_id("local", &closed, "hard_exit")?;
        service.action(&json!({"op":"unlisten_all"}))?;
        let stopped = service.state.listener("local", &closed)?.unwrap();
        assert_eq!(stopped["user_stopped"], 1);
        assert!(stopped["retirement_reason"].is_null());
        let remaining = service.state.active()?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0]["harness"], "human");
        let remote = service
            .slot("local")?
            .kv_get(string(&remaining[0], "listener_id"))?
            .ok_or("Missing personal registration")?
            .0;
        assert_eq!(remote["human_paused"], false);
        assert_eq!(
            remote["channels"][format!("{}:human", service.host_id)]["state"],
            "ACTIVE"
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
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-addresses-{}", random_uuid_v4())));
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
    let mut service = TestService::open(&config, &plugins)?;
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
            let config_before = service.raw_config()?;
            for (action, expected) in [
                (
                    json!({"op":"hub_action","hub":"missing","action":"reconnect"}),
                    "NOT_FOUND:",
                ),
                (
                    json!({"op":"hub_action","hub":"missing","action":"disconnect"}),
                    "NOT_FOUND:",
                ),
                (
                    json!({"op":"setup_toggle_hub","hub":"missing","enabled":false}),
                    "NOT_FOUND:",
                ),
                (
                    json!({"op":"setup_toggle_hub","hub":"local","enabled":0}),
                    "INVALID_INPUT:",
                ),
                (
                    json!({"op":"hub_action","hub":42,"action":"reconnect"}),
                    "INVALID_INPUT:",
                ),
                (
                    json!({"op":"hub_action","hub":" ","action":"disconnect"}),
                    "INVALID_INPUT:",
                ),
            ] {
                let error = service.action(&action).unwrap_err();
                assert!(error.starts_with(expected), "{action}: {error}");
            }
            assert_eq!(service.raw_config()?, config_before);
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
        let activity = service.activity_dto(vec![json!({"hub":"local","listener_id":"missing","from_listener_id":"alice-abcd123456789012","from_user":"alice","from_harness":"fixture","message":"hello"})], &service.address_book());
        assert_eq!(activity[0]["from_display_address"], "alice/fixture#abcd");
        assert!(activity[0]["target_display_address"].is_null());
        // Exercise late revocation in both destination iteration orders without
        // a network server or sleeps: setup inspection performs the local transition.
        let other = TestEngine(HubEngine::open(&dir.join("other-hub"))?);
        // The two local aliases have independent injected engines. Run the real
        // policy commit and drain without reloading their transport fixtures.
        let policy = |s: &mut DaemonService, hub: &str, enabled: bool| -> Result<Value, String> {
            let lock = s.state.registration_lock(hub)?;
            let guard = lock.lock().unwrap();
            let mut detaches = mcp::Detaches::default();
            s.set_auto_listen_locked(hub, enabled, &mut detaches)??;
            drop(guard);
            detaches
                .settle(json!({"ok":true}), false)
                .execute(&s.access)
        };
        let fanout_result = (|| -> Result<(), String> {
            service.replace_slot(HubClient::new(
                "work".into(),
                json!({"url":"local://"}),
                "alice",
                Some(Arc::new(other.clone())),
            ));
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
    result['sessions'] = [dict(row, cwd=str(root)) for row in (candidates if isinstance(candidates, list) else [candidates])]
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
                            policy(&mut service, "work", false)?["warning"].is_string(),
                            "Unavailable detach must remain visible"
                        );
                        assert_eq!(service.slot("work")?.config["autoListen"], false);
                        policy(&mut service, "work", true)?;
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
                    policy(&mut service, &hub, true)?;
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
        fanout_result?;
        Ok(())
    })();
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
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-config-shape-{}", random_uuid_v4())));
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
        let mut service = TestService::open(&config, &plugins)?;
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
                assert_eq!(
                    service.action(&retry)?,
                    json!({"ok":true,"committed":true,"redirect_port":null})
                );
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
    result
}
#[test]
fn ui_acknowledgements_preserve_other_hubs_with_colliding_signal_ids() -> Result<(), String> {
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-ack-scope-{}", random_uuid_v4())));
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
    let mut service = TestService::open(&config, &plugins)?;
    let result = (|| {
        for hub in ["local", "work"] {
            service.state.db.execute("INSERT INTO local_listeners(hub,listener_id,principal_id,hub_url,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status) VALUES(?,'node','none#alice','local://','alice','host','host','fixture','s','','Target','','self','ACTIVE')", &[json!(hub)])?;
            service.state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,hub_url,principal_id) VALUES(?,'shared','node',1,1,'alice','host','','message','now',?,'RECEIVED','local://','none#alice')", &[json!(hub),json!(now_ms()+60_000)])?;
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
        service.activity_filter = "me".into();
        service.action(&json!({"op":"ack_signals","signal_ids":["work:shared"],"filter":"me","query":"not present"}))?;
        let queued = state.db.query(
            "SELECT channel_id FROM pending_hub_acks WHERE hub='work' AND signal_id='shared'",
            &[],
        )?;
        assert_eq!(
            queued[0]["channel_id"],
            format!("{}:fixture", service.host_id)
        );
        service.activity_filter = "all".into();
        assert!(unread()?.is_empty());
        for (id, message) in [("matching", "review ready"), ("unmatched", "build passed")] {
            state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,hub_url,principal_id) VALUES('local',?,'node',2,1,'alice','host','',?,'now',?,'RECEIVED','local://','none#alice')", &[json!(id),json!(message),json!(now_ms()+60_000)])?;
        }
        state.db.execute("CREATE TRIGGER reject_second_ack BEFORE INSERT ON pending_hub_acks WHEN NEW.signal_id IN ('matching','unmatched') AND EXISTS (SELECT 1 FROM pending_hub_acks WHERE signal_id IN ('matching','unmatched')) BEGIN SELECT RAISE(ABORT,'second ack rejected'); END", &[])?;
        assert_eq!(
            state
                .acknowledge_activity(&json!({"hub":"local"}), "all", "host", true)
                .unwrap_err(),
            "second ack rejected"
        );
        assert_eq!(unread()?.len(), 2);
        state.db.execute("DROP TRIGGER reject_second_ack", &[])?;
        let (_, matched, _) = state.query_activity("all", " review ")?;
        assert_eq!(matched, 1);
        service.action(&json!({"op":"ack_signals","ack_all":true,"query":" review "}))?;
        let remaining = state.db.query(
            "SELECT signal_id FROM local_signals WHERE acknowledged_at_ms IS NULL",
            &[],
        )?;
        assert_eq!(remaining, vec![json!({"signal_id":"unmatched"})]);
        service.action(
            &json!({"op":"ack_signals","signal_ids":["unmatched"],"filter":"me","query":"review"}),
        )?;
        assert!(unread()?.is_empty());

        state.db.execute("UPDATE local_listeners SET principal_id='oidc:https://id.example.com|alice' WHERE listener_id='node'",&[])?;
        state.db.execute("UPDATE local_signals SET principal_id='https://id.example.com#alice' WHERE listener_id='node'",&[])?;
        let (_, total, unread) = state.query_activity("all", "")?;
        assert_eq!(state.activity_counts()?, (total, unread));
        assert!(total > 0);
        let client = service.slot("local")?.clone();
        let human = super::super::registration::local_node(&state, &client, "human", "default")?
            .ok_or("Missing Human")?;
        let id = string(&human, "listener_id");
        state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,hub_url,principal_id) VALUES('local','clear-human',?,3,1,'alice','host','','clear me','now',?,'RECEIVED',?,?)", &[json!(id),json!(now_ms()+60_000),human["hub_url"].clone(),human["principal_id"].clone()])?;
        state.clear_activity_history(
            &json!({"hub":"local","signal_id":"clear-human"}),
            "all",
            &service.host_id,
        )?;
        let cleared = state.db.query("SELECT state,activity_hidden,acknowledged_at_ms FROM local_signals WHERE signal_id='clear-human'",&[])?;
        assert_eq!(cleared[0]["state"], "STORED_PASSIVE");
        assert_eq!(cleared[0]["activity_hidden"], 1);
        assert!(cleared[0]["acknowledged_at_ms"].is_number());
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
        service.replace_slot(remote);
        assert_eq!(
            service.human_projection("local")["human_state_status"],
            "unknown"
        );
        let settings = service.settings_for_hub("local");
        assert_eq!(settings["channels"][0]["inbox_paused"], true);
        assert_eq!(settings["channels"][0]["delivery_enabled"], false);
        Ok(())
    })();
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
    let dir = TestDirectory(
        std::env::temp_dir().join(format!("cito-host-validation-{}", random_uuid_v4())),
    );
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
        let mut service = TestService::open(&config, &plugins)?;
        let result = (|| {
            let host = json!({"enabled":true,"bind":"127.0.0.1","port":0,"auth_mode":"token","token":"previous-token"});
            let mut committed = json!({"host_hub":host,"other":7}).to_string();
            fs::write(&config.config_path, &committed).map_err(|e| e.to_string())?;
            service.reload_slots()?;
            let address = service.host_server.as_ref().unwrap().address;
            service.reload_slots()?;
            assert_eq!(service.host_server.as_ref().unwrap().address, address);
            let rpc = |address, token: &str| -> Result<(), String> {
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
                let invite = super::host_hub::parse_hub_invite(string(
                    &settings["host_hub"],
                    "invite_command",
                ))?;
                assert_eq!(invite["token"], "previous-token");
                assert_eq!(invite["auth_mode"], "token");
                assert_eq!(invite["url"], format!("http://{address}"));
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

            // An unchanged profile still has to recover a listener that has exited.
            let stopped = service.host_server.as_ref().unwrap();
            stopped.shutdown();
            assert!(!stopped.is_running());
            assert_eq!(service.settings()["host_hub"]["running"], false);
            assert!(TcpStream::connect(address).is_err());
            service.sync_host_hub()?;
            assert!(service.host_server.as_ref().unwrap().is_running());
            assert_eq!(service.settings()["host_hub"]["running"], true);
            rpc(address, "previous-token")?;

            // A write failure after binding must restore the old socket and credentials.
            // Obstruct the publication lock, independent of exclusive temporary names.
            let mut lock_name = config.config_path.as_os_str().to_os_string();
            lock_name.push(".lock");
            let tmp = std::path::PathBuf::from(lock_name);
            if tmp.is_file() {
                fs::remove_file(&tmp).map_err(|e| e.to_string())?;
            }
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
                &json!({"op":"set_host_hub","port":address.port(),"token":"--replacement=token"}),
            )?;
            assert_eq!(service.host_server.as_ref().unwrap().address, address);
            let hosted = &service.settings()["host_hub"];
            for key in ["invite_command", "lan_invite_command"] {
                let invite = super::host_hub::parse_hub_invite(string(hosted, key))?;
                assert_eq!(invite["token"], "--replacement=token");
                rpc(address, string(&invite, "token"))?;
            }
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
            rpc(replacement, "--replacement=token")?;
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
    result
}
#[test]
fn packaged_app_resolves_ui_and_honors_overrides() -> Result<(), String> {
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-ui-layout-{}", random_uuid_v4())));
    let root = dir.join("Cito.app/Contents");
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
}
#[test]
fn remediation_http_config_dtos_and_committed_receipts() -> Result<(), String> {
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-boundary-{}", random_uuid_v4())));
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
    let mut service = TestService::open(&config, &plugins)?;
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
        let listeners = snapshot["listeners"].as_array().unwrap();
        assert!(!listeners.is_empty());
        for row in listeners {
            assert!(row.get("owner_token").is_none());
            assert!(row.get("dispatcher_token").is_none());
            assert!(row.get("hub_url").is_none());
            assert_eq!(row["is_self_principal"], true);
            assert_eq!(row["can_set_scope"], false);
            for control in ["can_pin", "can_rename", "can_stop"] {
                assert_eq!(row[control], row["harness"] != "human", "{row}");
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
        assert!(
            service
                .state
                .db
                .query("SELECT * FROM wake_retries", &[])?
                .is_empty(),
            "Human admission must not schedule wake work"
        );
        // Clear must also reconcile a retained legacy retry row.
        service.state.db.execute(
            "INSERT INTO wake_retries VALUES('local',?,0)",
            &[row["listener_id"].clone()],
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
        let cleared = service.action(
            &json!({"op":"clear_activity_history","hub":"local","signal_id":"dto-signal"}),
        )?;
        assert_eq!(cleared["committed"], true);
        assert!(service
            .state
            .db
            .query("SELECT * FROM wake_retries", &[])?
            .is_empty());
        service.reload_slots()?;
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
        service.state.db.execute("CREATE TABLE wake_retries(hub TEXT NOT NULL,listener_id TEXT NOT NULL,retry_at_ms INTEGER NOT NULL,PRIMARY KEY(hub,listener_id))", &[])?;
        Ok(())
    })();
    let shared = Arc::new(Mutex::new(service.into_inner()));
    let _cleanup = ServiceCleanup {
        service: shared.clone(),
    };
    let http_result = (|| -> Result<(), String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        for (method, headers, status) in [
            ("GET /health?detail=true", format!("Host: {address}\r\n"), 200),
            ("GET /absent",format!("Host: {address}\r\n"),404),
            ("POST /mcp?truncated",format!("Host: {address}\r\nContent-Type: application/json\r\n"),400),
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
            let stream = crate::core::http_boundary::accept_test_client(&listener)?;
            let body = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}).to_string();
            let sent = if method.starts_with("GET") || method.ends_with("?truncated") { "" } else { &body };
            write!(client, "{method} HTTP/1.1\r\n{headers}Content-Length: {}\r\n\r\n{sent}",body.len()).map_err(|e| e.to_string())?;
            client.shutdown(std::net::Shutdown::Write).map_err(|e|e.to_string())?;
            // Queue the fixture's complete request before an early rejection can close the socket.
            let worker = thread::spawn(move || {
                super::http_actions::handle_http(stream, service, Arc::new(json!({})))
            });
            // Read the declared frame, not transport EOF: early request rejection
            // may close unread request bytes after delivering its complete response.
            let response = crate::plugins::read_http_response(&mut client)?;
            worker.join().map_err(|_| "HTTP worker panicked")?;
            assert_eq!(response.status, status, "{}", response.body);
            let _: Value = serde_json::from_str(&response.body).map_err(|e|e.to_string())?;
        }
        let mut committed =
            json!({"ok":true,"signal_id":"missing:sig_committed","seq":1,"status":"wake_accepted"});
        let slot = shared.lock().unwrap().slot("local")?.clone();
        super::mcp::wait_send_receipt(&slot, &mut committed);
        assert_eq!(committed["signal_id"], "missing:sig_committed");
        assert_eq!(committed["ok"], true);
        assert_eq!(committed["receipt_status"], "unknown");
        Ok(())
    })();
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
    let dir = TestDirectory(std::env::temp_dir().join(format!(
        "cito-projection-{}",
        crate::plugins::random_uuid_v4()
    )));
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
    let mut service = TestService::open(
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
        let mut stream = crate::core::http_boundary::accept_test_client(&listener).unwrap();
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
    service.replace_slot(client.clone());
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
    for current in [true, false] {
        let samples = service.hub_samples();
        *client.rejected_bearer.lock().unwrap() = current.then(|| "rejected".into());
        assert_ne!(client.human_observation_current(), current);
        let frame = service.snapshot_from(&samples)?;
        for (key, name_field) in [("hubs", "name"), ("my_remote_listeners", "hub")] {
            let projected = frame[key]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r[name_field] == "work")
                .unwrap();
            assert_eq!(
                projected["human_state_status"],
                if current { "current" } else { "stale" }
            );
            assert_eq!(projected["human_can_set_scope"], current);
        }
    }
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
    assert_eq!(frame["truncated_collections"], json!(["channels"]));
    assert_eq!(
        frame["settings"]["channel_inventory_error"],
        "Inventory unavailable"
    );
    Ok(())
}

#[test]
fn explicit_nested_pids_preserve_endpoint_errors_and_provider_fallback() -> Result<(), String> {
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-explicit-{}", random_uuid_v4())));
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
    let mut service = TestService::open(
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
    let dir =
        TestDirectory(std::env::temp_dir().join(format!("cito-authority-{}", random_uuid_v4())));
    let config = NativeConfig {
        state_dir: dir.join("state"),
        config_path: dir.join("config.json"),
        home_dir: dir.join("home"),
        daemon_port: 0,
        user: "alice".into(),
        default_hub: "local".into(),
    };
    let mut service = TestService::open(
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
