use super::*;

pub(super) struct Fixture {
    root: std::path::PathBuf,
    pub(super) service: DaemonService,
}
impl Fixture {
    pub(super) fn new() -> Result<Self, String> {
        let root = std::env::temp_dir().join(format!("cito-demand-{}", random_uuid_v4()));
        fs::create_dir_all(root.join("home")).map_err(|e| e.to_string())?;
        let script = root.join("provider.py");
        fs::write(&script, r#"import json, pathlib, sys
root = pathlib.Path(__file__).parent
request = json.loads(sys.stdin.readline())
harness = sys.argv[1]
with (root / 'calls.jsonl').open('a') as output:
    output.write(json.dumps(dict(harness=harness, request=request)) + '\n')
if request['op'] in ('discover_sessions', 'resolve_caller') and (root/'delay').exists():
    import time
    time.sleep(json.loads((root/'delay').read_text())[request['op']])
result = dict(ok=True)
if request['op'] == 'setup_inspect': result.update(installed=True, configured=harness=='agent' and not (root/'unconfigured').exists(), paths=[])
elif request['op'] == 'discover_sessions':
    result['sessions'] = [dict(harness=harness, session_id=(root/'session').read_text(), cwd='/workspace', title='Work')]
elif request['op'] == 'resolve_caller': result['caller'] = dict(harness=harness, session_id='initial', cwd='/workspace')
elif request['op'] == 'probe':
    gate = root / ('probe-' + request['session_id'])
    if gate.exists():
        with gate.open('rb', buffering=0) as pending: pending.read(1)
    result['status'] = 'closed_definitive' if request['session_id']=='closed' else 'alive'
print(json.dumps(result))
"#).map_err(|e| e.to_string())?;
        fs::write(root.join("session"), "initial").map_err(|e| e.to_string())?;
        let mut plugins = LoadedPlugins {
            plugins: Vec::new(),
            rejected_ids: HashSet::new(),
        };
        for id in ["agent", "disabled"] {
            let dir = root.join("plugins").join(id);
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            fs::write(dir.join("plugin.json"), json!({"id":id,"kind":"agent-harness","displayName":id,"command":["python3",script,id]}).to_string()).map_err(|e|e.to_string())?;
            plugins.plugins.push(crate::plugins::PluginDescriptor {
                id: id.into(),
                kind: "agent-harness".into(),
                display_name: id.into(),
                tier: "test".into(),
                order: 0,
                sf_symbol: "terminal".into(),
                tint: "blue".into(),
                cli_flag: false,
                pid_bound: true,
                auto_listen: Default::default(),
                plugin_dir: dir,
                is_builtin: false,
                command: vec![
                    "python3".into(),
                    script.to_string_lossy().into_owned(),
                    id.into(),
                ],
                channel_env: None,
                fields: Vec::new(),
                post_setup_hint: None,
                post_disconnect_hint: None,
            });
        }
        let config = NativeConfig {
            state_dir: root.join("state"),
            config_path: root.join("config.json"),
            home_dir: root.join("home"),
            daemon_port: 0,
            user: "alice".into(),
            default_hub: "local".into(),
        };
        fs::write(&config.config_path,json!({"hubs":{"local":{"url":"local://","autoListen":false},"work":{"url":"local://","autoListen":false}}}).to_string()).map_err(|e|e.to_string())?;
        let mut service = DaemonService::open(&config, &plugins)?;
        service.refresh_initial_setup_until(Instant::now() + Duration::from_secs(3));
        service.local.set_plugin_roots(&[])?;
        Ok(Self { root, service })
    }
    fn calls(&self, op: &str) -> Vec<Value> {
        fs::read_to_string(self.root.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|row| row["request"]["op"] == op)
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        for sid in ["shared", "closed"] {
            if let Ok(mut gate) = fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(self.root.join(format!("probe-{sid}")))
            {
                let _ = gate.write_all(b"x");
            }
        }
        self.service.shutdown();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn session_actions_validate_selectors_titles_and_retained_sessions() -> Result<(), String> {
    let mut f = Fixture::new()?;
    let mut raw = f.service.raw_config()?;
    raw["hubs"]["disabled"] = json!({"url":"local://","enabled":false});
    fs::write(&f.service.config.config_path, raw.to_string()).map_err(|e| e.to_string())?;
    for op in ["unlisten", "unlisten_session", "unlisten_all"] {
        for hub in [json!(17), json!("missing"), json!("disabled")] {
            assert!(f
                .service
                .action(&json!({"op":op,"hub":hub,"harness":"agent","session_id":"initial"}))
                .unwrap_err()
                .starts_with("INVALID_INPUT:"));
        }
        assert!(f
            .service
            .action(&json!({"op":op,"harness":"human","session_id":"other"}))
            .is_err());
    }
    assert!(f
        .service
        .action(
            &json!({"op":"listen_session","harness":"agent","session_id":"initial","title":" "})
        )
        .is_err());
    let listened=f.service.action(&json!({"op":"listen_session","harness":"agent","session_id":"initial","title":"Review","tag":"review"}))?;
    let id = string(&listened, "canonical_listener_id");
    let row = f.service.state.listener("local", id)?.unwrap();
    assert_eq!(row["title"], "Review");
    assert_eq!(row["about"], "Review");
    assert_eq!(row["tag"], "review");
    f.service.retire_id("local", id, "hard_exit")?;
    f.service.action(
        &json!({"op":"rename_session","harness":"agent","session_id":"initial","title":"Retained"}),
    )?;
    assert_eq!(
        f.service.state.custom_title("agent", "initial")?,
        Some("Retained".into())
    );
    f.service
        .action(&json!({"op":"unlisten","hub":"local","listener_id":id}))?;
    assert_eq!(
        f.service.state.listener("local", id)?.unwrap()["user_stopped"],
        1
    );
    Ok(())
}

#[test]
fn save_hub_honors_outer_default_selection_before_nested_config() -> Result<(), String> {
    let mut f = Fixture::new()?;
    for (name, outer, nested, expected) in [
        ("personal", Some(true), false, "personal"),
        ("cloud", Some(false), true, "personal"),
        ("shared", None, true, "shared"),
    ] {
        let mut action =
            json!({"op":"save_hub","name":name,"config":{"url":"local://","set_default":nested}});
        if let Some(outer) = outer {
            action["set_default"] = json!(outer);
        }
        f.service.action(&action)?;
        assert_eq!(f.service.raw_config()?["default_hub"], expected);
    }
    Ok(())
}

#[test]
fn hub_saves_cancel_only_the_edited_and_saved_profiles() -> Result<(), String> {
    let mut f = Fixture::new()?;
    let s = &mut f.service;
    s.action(&json!({"op":"save_hub","name":"work","url":"local://","set_default":true}))?;
    let work = s.logins.entry("work".into()).or_default().clone();
    s.active_settings_hub = Some("work".into());
    for action in [
        json!({"op":"setup_save_hub","name":"personal","url":"local://","user":"bob"}),
        json!({"op":"save_hub","edit":true,"original_name":"","name":"personal","hub":"work","autoListen":false}),
    ] {
        let personal = s.logins.entry("personal".into()).or_default().clone();
        s.action(&action)?;
        assert!(personal.cancelled.load(Ordering::SeqCst));
        assert!(!s.logins.contains_key("personal"));
        assert!(!work.cancelled.load(Ordering::SeqCst));
        assert_eq!(s.active_settings_hub.as_deref(), Some("work"));
        assert_eq!(s.raw_config()?["hubs"]["personal"]["user"], "bob");
    }
    for (original, name, selected) in [
        ("personal", "office", "personal"),
        ("office", "personal", "personal"),
    ] {
        s.active_settings_hub = Some(selected.into());
        let from = s.logins.entry(original.into()).or_default().clone();
        let to = s.logins.entry(name.into()).or_default().clone();
        s.action(&json!({"op":"setup_save_hub","edit":true,"original_name":original,"name":name}))?;
        assert!(from.cancelled.load(Ordering::SeqCst) && to.cancelled.load(Ordering::SeqCst));
        assert!(s.active_settings_hub.is_none());
        assert!(Arc::ptr_eq(&s.logins["work"], &work));
        assert!(!work.cancelled.load(Ordering::SeqCst));
    }
    Ok(())
}

#[test]
fn discovery_demand_restores_only_configured_manual_hard_exits() -> Result<(), String> {
    for (status, stopped, mode, reason, harness, disconnected, enabled, expected) in [
        (
            "UNLISTENED",
            0,
            "manual",
            "hard_exit",
            "agent",
            false,
            true,
            true,
        ),
        (
            "UNLISTENED",
            0,
            "manual",
            "disconnected",
            "agent",
            false,
            true,
            false,
        ),
        (
            "UNLISTENED",
            1,
            "manual",
            "hard_exit",
            "agent",
            false,
            true,
            false,
        ),
        (
            "FENCED_INACTIVE",
            0,
            "manual",
            "hard_exit",
            "agent",
            false,
            true,
            false,
        ),
        (
            "UNLISTENED",
            0,
            "auto",
            "hard_exit",
            "agent",
            false,
            true,
            false,
        ),
        (
            "UNLISTENED",
            0,
            "manual",
            "hard_exit",
            "disabled",
            false,
            true,
            false,
        ),
        (
            "UNLISTENED",
            0,
            "manual",
            "hard_exit",
            "agent",
            true,
            true,
            false,
        ),
        (
            "UNLISTENED",
            0,
            "manual",
            "hard_exit",
            "agent",
            false,
            false,
            false,
        ),
    ] {
        let mut f = Fixture::new()?;
        f.service.state.db.execute(
            "INSERT INTO local_listeners(hub,listener_id,harness,session_id,status,user_stopped,listen_mode,retirement_reason,principal_id,owner_user,host_id,host_name,cwd,title,about,scope) VALUES('work','target',?,'session',?,?,?,?,'none#alice','alice','host','host','','Target','','self')",
            &[json!(harness),json!(status),json!(stopped),json!(mode),json!(reason)],
        )?;
        if disconnected {
            f.service
                .last_app_action
                .insert(harness.into(), "disconnect".into());
        }
        if !enabled {
            f.service.slots.remove("work").unwrap().close();
        }
        f.service.update_discovery_demand();
        let activity = f.service.session_activity.as_mut().unwrap();
        activity.tick(&f.service.access.state, &[])?;
        assert_eq!(
            activity.discovery_pending(),
            expected,
            "{status} {stopped} {mode} {reason} {harness} {disconnected} {enabled}"
        );
        activity.shutdown()?;
    }
    Ok(())
}

#[test]
fn paused_discovery_skips_unconfigured_providers_and_refreshes_on_snapshot_and_menu_open(
) -> Result<(), String> {
    let mut f = Fixture::new()?;
    f.service.poll()?;
    assert!(f.calls("discover_sessions").is_empty());
    for (op, session) in [
        ("get_snapshot", "first"),
        ("open_menu", "second"),
        ("refresh_available", "third"),
    ] {
        fs::write(f.root.join("session"), session).map_err(|e| e.to_string())?;
        let start = Instant::now();
        let reply = f.service.action(&json!({"op":op}))?;
        assert!(
            start.elapsed() < Duration::from_millis(1500),
            "On-demand discovery exceeded its caller budget"
        );
        let snapshot = if op == "get_snapshot" {
            reply
        } else {
            f.service.snapshot()?
        };
        assert_eq!(snapshot["available"][0]["session_id"], session);
        assert!(f
            .calls("discover_sessions")
            .iter()
            .all(|row| row["harness"] == "agent"));
        f.service.action(&json!({"op":"close_menu"}))?;
        let count = f.calls("discover_sessions").len();
        f.service.poll()?;
        assert_eq!(
            f.calls("discover_sessions").len(),
            count,
            "Closed surfaces with Auto-Listen off must not restart discovery"
        );
    }
    Ok(())
}

#[test]
fn shared_endpoint_probes_keep_session_results_and_generation_fences() -> Result<(), String> {
    let mut f = Fixture::new()?;
    for sid in ["shared", "closed"] {
        let path = std::ffi::CString::new(
            f.root
                .join(format!("probe-{sid}"))
                .to_string_lossy()
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
    for hub in ["local", "work"] {
        for sid in ["shared", "closed"] {
            super::super::registration::register_listener(
                &f.service.state,
                f.service.slot(hub)?,
                &f.service.host_id,
                &json!({"about":"Work"}),
                &json!({"harness":"agent","session_id":sid,"endpoint_pid":std::process::id(),"cwd":"/workspace","title":"Work"}),
            )?;
        }
    }
    f.service.schedule_probes()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while f.calls("probe").len() < 2 {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let previous = f.service.state.db.query("SELECT * FROM local_listeners WHERE hub='work' AND harness='agent' AND session_id='closed'", &[])?[0].clone();
    f.service
        .retire_id("work", string(&previous, "listener_id"), "hard_exit")?;
    super::super::registration::register_listener(
        &f.service.state,
        f.service.slot("work")?,
        &f.service.host_id,
        &json!({"about":"New binding"}),
        &json!({"harness":"agent","session_id":"closed","endpoint_pid":std::process::id(),"cwd":"/workspace","title":"Work"}),
    )?;
    let current = f
        .service
        .state
        .listener("work", string(&previous, "listener_id"))?
        .unwrap();
    assert!(current["binding_generation"].as_u64() > previous["binding_generation"].as_u64());
    use std::io::Write;
    for sid in ["shared", "closed"] {
        fs::OpenOptions::new()
            .write(true)
            .open(f.root.join(format!("probe-{sid}")))
            .map_err(|e| e.to_string())?
            .write_all(b"x")
            .map_err(|e| e.to_string())?;
    }
    while f.service.probes.values().any(|probe| {
        probe
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let calls = f.calls("probe");
    assert_eq!(
        calls.len(),
        2,
        "Two Hub bindings share a probe, while different sessions keep separate observations"
    );
    let rows = f.service.state.db.query(
        "SELECT hub,session_id,status FROM local_listeners WHERE harness='agent'",
        &[],
    )?;
    assert_eq!(rows.len(), 4);
    for row in rows {
        assert_eq!(
            row["status"],
            if row["session_id"] == "closed" && row["hub"] != "work" {
                "UNLISTENED"
            } else {
                "ACTIVE"
            }
        );
    }
    for sid in ["shared", "closed"] {
        fs::remove_file(f.root.join(format!("probe-{sid}"))).map_err(|e| e.to_string())?;
    }
    f.service.schedule_probes()?;
    assert!(f
        .service
        .probes
        .values()
        .any(|probe| probe.calm > 0 && probe.due > Instant::now() + Duration::from_secs(1)));
    Ok(())
}

#[test]
fn serves_health_during_setup_and_cleans_up_on_fatal_io() -> Result<(), String> {
    use std::process::{Command, Stdio};
    // The daemon installs process-wide signal handlers. Isolate them from other tests.
    let Some(home) = std::env::var_os("CITO_RUNTIME_TEST_HOME") else {
        let dir = crate::core::hub::TestDirectory(
            std::env::temp_dir().join(format!("cito-runtime-{}", random_uuid_v4())),
        );
        fs::create_dir_all(dir.join("bundled")).unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "core::daemon::event_tests::serves_health_during_setup_and_cleans_up_on_fatal_io",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", &dir.0)
        .env("CITO_RUNTIME_TEST_HOME", &dir.0)
        .env("CITO_BUILTIN_PLUGINS_DIR", dir.join("bundled"))
        .env("CITO_DISABLE_UI", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let output = crate::support::run_command_with_timeout(cmd, Duration::from_secs(15))?
            .ok_or("Daemon cleanup timed out")?;
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for fault in ["accept", "poll"] {
            let state = dir.join(fault);
            let health: Value = serde_json::from_slice(
                &fs::read(state.join("plugins/agent/health.json"))
                    .map_err(|e| format!("{fault}: setup did not receive /health: {e}"))?,
            )
            .unwrap();
            assert_eq!(health["version"], CITO_VERSION);
            assert_eq!(health["stateDir"], json!(state));
        }
        return Ok(());
    };
    let home = std::path::PathBuf::from(home);
    for fault in ["accept", "poll"] {
        let state = home.join(fault);
        let plugin = state.join("plugins/agent");
        fs::create_dir_all(&plugin).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let config = NativeConfig {
            state_dir: state.clone(),
            config_path: state.join("config.json"),
            home_dir: home.clone(),
            daemon_port: port,
            user: "alice".into(),
            default_hub: "local".into(),
        };
        let observed = plugin.join("health.json");
        // The inspection cannot finish until the production HTTP handler answers.
        fs::write(plugin.join("plugin.json"), json!({"id":"agent","kind":"agent","displayName":"Agent","command":["python3","./driver.py",port.to_string()]}).to_string()).unwrap();
        fs::write(plugin.join("driver.py"), r#"import http.client, json, pathlib, sys
if json.loads(sys.stdin.readline())['op'] == 'setup_inspect':
    http = http.client.HTTPConnection('127.0.0.1', int(sys.argv[1]), timeout=1.5)
    http.request('GET', '/health')
    response = http.getresponse()
    if response.status == 200: pathlib.Path(__file__).with_name('health.json').write_bytes(response.read())
    http.close()
print(json.dumps(dict(ok=True, configured=False, installed=True, paths=[])))
"#).unwrap();
        let plugins = LoadedPlugins {
            plugins: vec![crate::plugins::parse_manifest_dir(&plugin, false)?],
            rejected_ids: Default::default(),
        };
        let deadline = Instant::now() + Duration::from_secs(4);
        let fail = || observed.exists() || Instant::now() >= deadline;
        let error = run_daemon_inner(
            &config,
            &plugins,
            |listener| {
                if fault == "accept" && fail() {
                    return Err(std::io::Error::other("test failure"));
                }
                listener.accept()
            },
            |fds, due| {
                if fault == "poll" && fail() {
                    return Err("test failure".into());
                }
                crate::support::poll_until(fds, due)
            },
        )
        .unwrap_err();
        assert_eq!(error, format!("{fault}: test failure"));
        assert_eq!(SIGNAL_PIPE.load(Ordering::SeqCst), -1);
        assert!(LocalState::open(&state)?
            .db
            .query("SELECT * FROM daemon_owner", &[])?
            .is_empty());
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok());
    }
    Ok(())
}

#[test]
fn contention_iterations_back_off_after_the_poll_deadline_expires() -> Result<(), String> {
    let service = Mutex::new(());
    let _held = service.lock().unwrap();
    let last_poll = Instant::now() - Duration::from_secs(2);
    let start = Instant::now();
    let mut iterations = 0;
    while start.elapsed() < Duration::from_millis(140) {
        assert!(service.try_lock().is_err());
        crate::support::poll_until(&mut [], Some(poll_deadline(last_poll)))?;
        iterations += 1;
    }
    assert!(
        iterations <= 3,
        "Expired periodic deadline caused {iterations} contention iterations"
    );
    Ok(())
}

#[test]
fn status_refresh_and_caller_share_one_deadline() -> Result<(), String> {
    let mut f = Fixture::new()?;
    // Each provider delay fits the request budget; their sum exceeds it. Leave
    // enough first-stage slack for process startup before asserting both calls.
    fs::write(
        f.root.join("delay"),
        json!({"discover_sessions":0.2,"resolve_caller":1.4}).to_string(),
    )
    .map_err(|e| e.to_string())?;
    let started = Instant::now();
    assert!(f
        .service
        .tool(
            "cito_status",
            &json!({}),
            &json!({"expected_harness":"agent"})
        )
        .is_err());
    assert!(
        started.elapsed() < Duration::from_millis(1900),
        "Provider deadlines stacked: {:?}",
        started.elapsed()
    );
    assert_eq!(f.calls("discover_sessions").len(), 1);
    assert_eq!(f.calls("resolve_caller").len(), 1);
    Ok(())
}

#[test]
fn disconnected_ui_revokes_all_visibility_demand_before_replacement() -> Result<(), String> {
    let mut f = Fixture::new()?;
    f.service.action(&json!({"op":"open_menu"}))?;
    f.service.directory_open = true;
    f.service.active_settings_hub = Some("local".into());
    let count = f.calls("discover_sessions").len();
    f.service.disconnect_ui();
    assert!(!f.service.menu_open);
    assert!(!f.service.directory_open);
    assert!(f.service.active_settings_hub.is_none());
    f.service
        .session_activity
        .as_mut()
        .unwrap()
        .mark_stale("agent");
    f.service.poll()?;
    assert_eq!(f.calls("discover_sessions").len(), count);
    f.service.action(&json!({"op":"open_menu"}))?;
    assert_eq!(f.calls("discover_sessions").len(), count + 1);
    Ok(())
}

#[test]
fn replaced_ui_reader_cannot_restore_visibility_demand() -> Result<(), String> {
    let mut f = Fixture::new()?;
    f.service.shutdown();
    let service = Arc::new(Mutex::new(DaemonService::open(
        &f.service.config,
        &f.service.plugins,
    )?));
    let result = (|| {
        let (old_generation, current_generation) = {
            let mut s = service.lock().unwrap();
            s.set_ui(Arc::new(UiMailbox::default()));
            let old_generation = s.ui_generation;
            s.set_ui(Arc::new(UiMailbox::default()));
            assert!(s.ui_generation > old_generation);
            (old_generation, s.ui_generation)
        };
        // The old reader resumes only after the replacement mailbox is installed.
        for action in [
            json!({"op":"open_menu"}),
            json!({"op":"open_directory"}),
            json!({"op":"open_settings"}),
        ] {
            assert!(
                http_actions::dispatch_action(&service, &action, Some(old_generation)).is_err()
            );
            let s = service.lock().unwrap();
            assert!(!s.menu_open);
            assert!(!s.directory_open);
            assert!(s.active_settings_hub.is_none());
        }
        http_actions::dispatch_action(
            &service,
            &json!({"op":"open_menu"}),
            Some(current_generation),
        )?;
        assert!(service.lock().unwrap().menu_open);
        service.lock().unwrap().disconnect_ui();
        assert!(http_actions::dispatch_action(
            &service,
            &json!({"op":"open_menu"}),
            Some(current_generation),
        )
        .is_err());
        assert!(!service.lock().unwrap().menu_open);
        // HTTP control remains independent of the companion connection.
        http_actions::dispatch_action(&service, &json!({"op":"open_menu"}), None)?;
        assert!(service.lock().unwrap().menu_open);
        Ok(())
    })();
    service.lock().unwrap().shutdown();
    result
}

#[test]
fn visibility_close_actions_emit_no_full_snapshot() -> Result<(), String> {
    let mut f = Fixture::new()?;
    f.service.ui = Some(Arc::new(UiMailbox::default()));
    for close in ["close_menu", "close_settings", "close_directory"] {
        match close {
            "close_menu" => f.service.menu_open = true,
            "close_settings" => f.service.active_settings_hub = Some("local".into()),
            _ => f.service.directory_open = true,
        }
        f.service.push_ui();
        f.service.ui.take().unwrap().close();
        let mailbox = Arc::new(UiMailbox::default());
        f.service.ui = Some(mailbox.clone());
        f.service.action(&json!({"op":close}))?;
        // A close emits a cheap status projection without a correlated snapshot.
        let frame: Value = serde_json::from_slice(&mailbox.next().ok_or("Missing close frame")?)
            .map_err(|e| e.to_string())?;
        assert!(frame.get("activity").is_none(), "{close}: {frame}");
        assert!(frame.get("settings").is_none(), "{close}: {frame}");
        assert!(frame.get("source_op").is_none(), "{close}: {frame}");
    }
    Ok(())
}

#[test]
fn receiver_liveness_uses_authority_without_a_poll_worker() -> Result<(), String> {
    let f = Fixture::new()?;
    assert!(f.service.receivers.is_empty());
    let row = f
        .service
        .state
        .active()?
        .into_iter()
        .find(|row| row["harness"] == "human")
        .ok_or("Missing Human attachment")?;
    let id = row["listener_id"].clone();
    assert!(f
        .service
        .live_receivers()?
        .iter()
        .any(|row| row["listener_id"] == id));
    for sql in [
        "UPDATE local_listeners SET principal_id='none#other' WHERE listener_id=?",
        "UPDATE local_listeners SET hub_url='https://other.example.com' WHERE listener_id=?",
    ] {
        f.service.state.db.execute(sql, std::slice::from_ref(&id))?;
        assert!(!f
            .service
            .live_receivers()?
            .iter()
            .any(|row| row["listener_id"] == id));
        f.service.state.db.execute(
            "UPDATE local_listeners SET principal_id=?,hub_url=? WHERE listener_id=?",
            &[
                row["principal_id"].clone(),
                row["hub_url"].clone(),
                id.clone(),
            ],
        )?;
    }
    Ok(())
}

#[test]
fn open_settings_emits_inventory_changes_and_skips_identical_completions() -> Result<(), String> {
    let mut f = Fixture::new()?;
    let mailbox = Arc::new(UiMailbox::default());
    let slot = f.service.slot("work")?.clone();
    let inventory = f.service.channel_inventory(&slot);
    // Startup enrolls Human nodes after starting event readers. Settle their independent
    // revision changes before testing an inventory-only change to the Settings frame.
    for slot in f.service.slots.values() {
        slot.close();
    }
    for event in f.service.events.drain(..) {
        event.join().map_err(|_| "Hub event reader panicked")?;
    }
    f.service.active_settings_hub = Some("work".into());
    f.service.ui = Some(mailbox.clone());
    f.service.push_ui();
    assert!(mailbox.next().is_some());
    inventory.cached.lock().unwrap().completed_gen += 1;
    f.service.push_ui();
    assert!(mailbox.pending.lock().unwrap().is_empty());
    inventory.cached.lock().unwrap().value =
        json!({"status":"error","detail":"Service unavailable"});
    f.service.push_ui();
    assert!(!mailbox.pending.lock().unwrap().is_empty());
    let frame: Value = serde_json::from_slice(&mailbox.next().unwrap()).unwrap();
    assert_eq!(frame["op"], "state");
    mailbox.close();
    inventory.cached.lock().unwrap().value = json!({"channels":[]});
    let last = f.service.last_ui_revision.lock().unwrap().clone();
    f.service.push_ui();
    assert_eq!(*f.service.last_ui_revision.lock().unwrap(), last);
    Ok(())
}

#[test]
fn closed_ui_emits_light_status_without_activity_history_or_inventory() -> Result<(), String> {
    let mut f = Fixture::new()?;
    let mailbox = Arc::new(UiMailbox::default());
    f.service.ui = Some(mailbox.clone());
    f.service.push_ui();
    let initial: Value = serde_json::from_slice(&mailbox.next().ok_or("Missing initial state")?)
        .map_err(|e| e.to_string())?;
    assert!(initial.get("settings").is_some());
    f.service.state.db.execute("INSERT INTO local_listeners(hub,listener_id,principal_id,hub_url,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status) VALUES('local','target','none#alice','local://','alice','host','host','fixture','s','','Target','','self','ACTIVE')", &[])?;
    f.service.state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,hub_url,principal_id) VALUES('local','new','target',1,1,'bob','host','Sender','Work','now',?,'RECEIVED','local://','none#alice')", &[json!(now_ms()+60_000)])?;
    f.service.push_ui();
    let status: Value = serde_json::from_slice(&mailbox.next().ok_or("Missing status state")?)
        .map_err(|e| e.to_string())?;
    assert_eq!(status["op"], "state");
    assert_eq!(status["unread_count"], 1);
    assert_eq!(status["activity_has_any"], true);
    assert!(status.get("activity").is_none());
    assert!(status.get("settings").is_none());
    f.service.menu_open = true;
    f.service.push_ui();
    let full: Value = serde_json::from_slice(&mailbox.next().ok_or("Missing open-surface state")?)
        .map_err(|e| e.to_string())?;
    assert_eq!(full["activity"].as_array().map(Vec::len), Some(1));
    assert!(full.get("settings").is_some());
    Ok(())
}
