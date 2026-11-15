use super::*;

struct Fixture {
    root: std::path::PathBuf,
    service: DaemonService,
}
impl Fixture {
    fn new() -> Result<Self, String> {
        let root = std::env::temp_dir().join(format!("cito-demand-{}", random_uuid_v4()));
        fs::create_dir_all(root.join("home")).map_err(|e| e.to_string())?;
        let script = root.join("provider.py");
        fs::write(&script, r#"import json, pathlib, sys
root = pathlib.Path(__file__).parent
request = json.loads(sys.stdin.readline())
harness = sys.argv[1]
with (root / 'calls.jsonl').open('a') as output:
    output.write(json.dumps(dict(harness=harness, request=request)) + '\n')
result = dict(ok=True)
if request['op'] == 'setup_inspect': result.update(installed=True, configured=harness=='agent', paths=[])
elif request['op'] == 'discover_sessions':
    result['sessions'] = [dict(harness=harness, session_id=(root/'session').read_text(), cwd='/workspace', title='Work')]
elif request['op'] == 'probe': result['status'] = 'closed_definitive' if request['session_id']=='closed' else 'alive'
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
        let service = DaemonService::open(&config, &plugins)?;
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
        self.service.shutdown();
        let _ = fs::remove_dir_all(&self.root);
    }
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
    while f.service.probes.values().any(|probe| {
        probe
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    f.service.schedule_probes()?;
    let calls = f.calls("probe");
    assert_eq!(
        calls.len(),
        2,
        "Two Hub bindings share a probe, while different sessions keep separate observations"
    );
    let rows = f.service.state.db.query(
        "SELECT session_id,status FROM local_listeners WHERE harness='agent'",
        &[],
    )?;
    assert_eq!(rows.len(), 4);
    for row in rows {
        assert_eq!(
            row["status"],
            if row["session_id"] == "closed" {
                "UNLISTENED"
            } else {
                "ACTIVE"
            }
        );
    }
    assert!(f
        .service
        .probes
        .values()
        .any(|probe| probe.calm > 0 && probe.due > Instant::now() + Duration::from_secs(1)));
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
    f.service.state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) VALUES('local','new','target',1,1,'bob','host','Sender','Work','now',?,'RECEIVED')", &[json!(now_ms()+60_000)])?;
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
