use super::*;

#[test]
fn paused_cli_and_mcp_status_discover_sessions_started_after_idle() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"local":{"url":"local://","autoListen":false}}}));
    f.provider("delivered_unconfirmed");
    let provider = f.state.join("plugins/fixture");
    let save = |sid: &str| {
        fs::write(provider.join("options.json"),json!({"session_id":sid,"sessions":[{"harness":"fixture","session_id":sid,"title":"New work","cwd":"/workspace","needs_reload":true}],"status":"alive","outcome":"delivered_unconfirmed"}).to_string()).unwrap()
    };
    save("before-idle");
    f.daemon(&[]);
    let calls = || {
        fs::read_to_string(provider.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|request| request["op"] == "discover_sessions")
            .count()
    };
    assert_eq!(
        calls(),
        0,
        "A closed daemon with Auto-Listen off starts no background discovery"
    );
    save("after-idle");
    let started = std::time::Instant::now();
    let (ok, output, error) = f.command(NATIVE, &["status", "--json"], "");
    assert!(ok, "{error}");
    let snapshot: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(snapshot["available"][0]["session_id"], "after-idle");
    assert!(started.elapsed() < std::time::Duration::from_millis(1500));
    save("after-cli");
    let before = calls();
    let started = std::time::Instant::now();
    let snapshot = f.tool(
        "cito_status",
        json!({}),
        json!({"expected_harness":"fixture","expected_session_id":"after-cli"}),
    );
    assert_eq!(snapshot["registered"], false);
    assert_eq!(calls(),before+1,"MCP status must request fresh discovery even though its session-scoped DTO omits the available-session list");
    assert!(started.elapsed() < std::time::Duration::from_millis(1500));

    let options_path = provider.join("options.json");
    let mut options: Value = read_json(&options_path);
    options["discover_delay_ms"] = json!(2000);
    fs::write(options_path, options.to_string()).unwrap();
    let resolve_calls = || {
        fs::read_to_string(provider.join("calls.jsonl"))
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|request| request["op"] == "resolve_caller")
            .count()
    };
    let before_resolve = resolve_calls();
    let before_discovery = calls();
    let started = std::time::Instant::now();
    let error = f.tool_result(
        "cito_status",
        json!({}),
        json!({"expected_harness":"fixture","expected_session_id":"after-cli"}),
    );
    assert_eq!(error["isError"], true, "{error}");
    let message = error.to_string();
    assert!(!message.contains("UNSUPPORTED_HARNESS"), "{error}");
    assert!(message.contains("TOOL_TIMEOUT"), "{error}");
    assert_eq!(calls(), before_discovery + 1);
    assert_eq!(
        resolve_calls(),
        before_resolve,
        "Discovery consumed the caller budget"
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(1900));
}

#[test]
fn u1_discovery_hides_unconfigured_apps_and_refreshes_setup_lifecycle() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    // Discovery finds the live socket; subsequent probes also verify the process entrypoint.
    let script = f.dir.join("pi.js");
    fs::write(&script, include_str!("../endpoint.cjs")).unwrap();
    let endpoint = f.spawn(
        "node",
        &[script.to_str().unwrap()],
        &[("FIXTURE_HARNESS", "pi")],
    );
    wait(|| f.frames_for(endpoint, "ready").next().is_some());
    assert_eq!(
        f.plugin(PI, json!({"op":"discover_sessions"}))["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    f.daemon(&[]);
    let available = || f.snapshot()["available"].clone();
    assert_eq!(available(), json!([]));
    assert!(!f.snapshot()["discovery"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["harness"] == "pi"));
    f.action_ok(json!({"op":"setup_apply","harness":"pi"}));
    let candidates = available();
    let candidate = candidates
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["harness"] == "pi")
        .expect("fast discovery must be visible on the first snapshot")
        .clone();
    assert!(candidate["endpoint_pid"].is_number());
    let snapshot = f.snapshot();
    assert_eq!(snapshot["hubs"][0]["auto_listen"], false);
    // A different principal's pause must not suppress this principal's enrollment.
    let db = f.db("state.sqlite");
    db.execute("INSERT INTO local_listeners(hub,listener_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status) VALUES(\'local\',\'c_0123456789abcdef\',\'none#bob\',\'bob\',\'other-host\',\'other-host\',\'pi\',?,\'/tmp\',\'Other session\',\'\',\'self\',\'UNLISTENED\')", &[candidate["session_id"].clone()]).unwrap();
    f.action_ok(json!({"op":"set_auto_listen","hub":"local","enabled":true}));
    wait(|| {
        f.snapshot()["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["harness"] == "pi" && v["scope"] == "workspace")
    });
    let snapshot = f.snapshot();
    assert_eq!(snapshot["hubs"][0]["auto_listen"], true);
    assert_eq!(snapshot["settings"]["hubs"][0]["auto_listen"], true);
    assert!(
        snapshot["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["harness"] == "pi" && v["scope"] == "workspace"),
        "{snapshot}"
    );
    db.execute(
        "DELETE FROM local_listeners WHERE principal_id=\'none#bob\'",
        &[],
    )
    .unwrap();
    let saved: Value = read_json(&f.config);
    assert_eq!(saved["hubs"]["local"]["autoListen"], true);
    f.action_ok(json!({"op":"unlisten_session","hub":"local","harness":"pi","session_id":candidate["session_id"]}));
    f.action_ok(json!({"op":"refresh_available"}));
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='pi'")[0]["status"],
        "UNLISTENED"
    );
    assert_eq!(f.snapshot()["hubs"][0]["auto_listen"], true);
    assert_eq!(
        f.rows("SELECT user_stopped FROM local_listeners WHERE harness='pi'")[0]["user_stopped"],
        1
    );
    // Re-enabling requests fresh discovery before restoring agent enrollment,
    // while preserving the independent Human pause on the same Hub.
    f.action_ok(
        json!({"op":"unlisten_session","hub":"local","harness":"human","session_id":"default"}),
    );
    f.action_ok(json!({"op":"set_auto_listen","hub":"local","enabled":true}));
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='human'")[0]["status"],
        "UNLISTENED"
    );
    wait(|| {
        f.rows("SELECT status FROM local_listeners WHERE harness=\'pi\'")[0]["status"] == "ACTIVE"
    });
    f.action_ok(json!({"op":"unlisten_all"}));
    assert_eq!(f.snapshot()["hubs"][0]["auto_listen"], false);
    f.action_ok(json!({"op":"set_auto_listen","hub":"local","enabled":true}));
    f.action_ok(json!({"op":"setup_remove_app","harness":"pi"}));
    assert_eq!(available(), json!([]));
    wait(|| {
        !f.snapshot()["discovery"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["harness"] == "pi")
    });
    f.action_ok(json!({"op":"setup_apply","harness":"pi"}));
    f.action_ok(json!({"op":"setup_disconnect_all"}));
    assert_eq!(available(), json!([]));
}

#[test]
fn codex_reopen_restores_retained_sessions_outside_recent_and_menu_slices() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"local":{"url":"local://","autoListen":true}}}));
    let source =
        cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite")).unwrap();
    source
        .execute(
            "CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,recency_at_ms INTEGER)",
            &[],
        )
        .unwrap();
    let now = cito_native::plugins::now_ms();
    for id in ["pinned", "automatic", "stopped"] {
        source
            .execute(
                "INSERT INTO threads VALUES(?,'/repo',0,?)",
                &[json!(id), json!(now)],
            )
            .unwrap();
    }
    assert_eq!(f.plugin(CODEX, json!({"op":"setup_apply"}))["ok"], true);
    let app = f.dir.join("codex.mjs");
    fs::write(&app, "console.log('ready'); setInterval(()=>{},1000);").unwrap();
    let process = spawn_interactive(&f, "node", &[app.to_str().unwrap()], &[]);
    wait(|| f.output(process, false).contains("ready"));
    // Use this build's real provider, independently of checked-in release staging.
    install_provider_command(&f, "codex", json!([CODEX]));
    f.daemon(&[]);
    wait(|| {
        f.rows("SELECT * FROM local_listeners WHERE harness='codex' AND status='ACTIVE'")
            .len()
            == 3
    });
    let pinned = f
        .rows("SELECT * FROM local_listeners WHERE harness='codex' AND session_id='pinned'")[0]
        .clone();
    f.action_ok(json!({"op":"set_listen_mode","hub":"local","listener_id":pinned["listener_id"],"listen_mode":"manual"}));
    f.action_ok(
        json!({"op":"unlisten_session","hub":"local","harness":"codex","session_id":"stopped"}),
    );
    // Integration close/reopen is deterministic even with unrelated host providers.
    f.action_ok(json!({"op":"setup_remove_app","harness":"codex"}));
    f.stop(process);
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE harness='codex' AND status='ACTIVE'")
        .is_empty());
    let old = now - 1_800_000;
    source
        .execute("UPDATE threads SET recency_at_ms=?", &[json!(old)])
        .unwrap();
    f.db("state.sqlite").execute("UPDATE local_listeners SET last_activity_at_ms=?,last_activity_revision=? WHERE harness='codex'", &[json!(old),json!(old.to_string())]).unwrap();
    for i in 0..8 {
        source
            .execute(
                "INSERT INTO threads VALUES(?,'/repo',0,?)",
                &[json!(format!("menu-{i}")), json!(old + 1000 + i)],
            )
            .unwrap();
    }
    let reopened = spawn_interactive(&f, "node", &[app.to_str().unwrap()], &[]);
    wait(|| f.output(reopened, false).contains("ready"));
    let sliced = f.plugin(
        CODEX,
        json!({"op":"discover_sessions","endpoint_pid":f.pid(reopened)}),
    );
    assert_eq!(sliced["sessions"].as_array().unwrap().len(), 8);
    assert!(sliced["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["session_id"].as_str().unwrap().starts_with("menu-")));
    f.action_ok(json!({"op":"setup_apply","harness":"codex"}));
    f.action_ok(json!({"op":"refresh_available"}));
    wait(|| {
        f.rows("SELECT * FROM local_listeners WHERE harness='codex' AND status='ACTIVE'")
            .len()
            == 2
    });
    let rows = f.rows("SELECT * FROM local_listeners WHERE harness='codex'");
    for (id, mode) in [("pinned", "manual"), ("automatic", "auto")] {
        let row = rows.iter().find(|r| r["session_id"] == id).unwrap();
        assert_eq!(row["status"], "ACTIVE", "{row}");
        assert_eq!(row["listen_mode"], mode);
        assert_eq!(
            row["last_activity_at_ms"], old,
            "Reopen must preserve idle age"
        );
    }
    let stopped = rows.iter().find(|r| r["session_id"] == "stopped").unwrap();
    assert_eq!(stopped["status"], "UNLISTENED");
    assert_eq!(stopped["user_stopped"], 1);
}
#[test]
fn cli_disable_stops_active_listeners_and_invalidates_cached_discovery() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    f.tool("cito_listen", json!({"about":"Disconnect me"}), meta);
    assert!(!f
        .rows("SELECT * FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
        .is_empty());
    let options = f.state.join("plugins/fixture/options.json");
    let mut config: Value = read_json(&options);
    config["configured"] = json!(false);
    fs::write(&options, config.to_string()).unwrap();
    let (ok, out, error) = f.command(NATIVE, &["setup", "--disable", "fixture", "--json"], "");
    assert!(ok, "{out} {error}");
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
        .is_empty());
    assert!(!f.snapshot()["available"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["harness"] == "fixture"));
    assert!(
        fs::read_to_string(f.state.join("plugins/fixture/calls.jsonl"))
            .unwrap()
            .lines()
            .any(|l| serde_json::from_str::<Value>(l).unwrap()["op"] == "setup_remove")
    );
}
#[test]
fn daemon_plugins_strip_inherited_caller_identity_but_honor_explicit_request_environment() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    for key in [
        "CODEX_THREAD_ID",
        "CLAUDE_SESSION_ID",
        "PI_CODING_AGENT_SESSION_FILE",
        "CITO_ENDPOINT_PID",
    ] {
        f.env.insert(key.into(), "inherited".into());
    }
    let meta = f.provider("delivered");
    f.daemon(&[]);
    f.tool("cito_listen", json!({"about":"Inspect environment"}), meta);
    let calls_path = f.state.join("plugins/fixture/calls.jsonl");
    let records = || {
        fs::read_to_string(&calls_path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect::<Vec<_>>()
    };
    for op in ["setup_apply", "setup_remove_app"] {
        f.action_ok(json!({"op":op,"harness":"fixture"}));
    }
    let calls = records();
    for op in ["setup_inspect", "setup_apply", "setup_remove"] {
        let setup: Vec<_> = calls.iter().filter(|r| r["op"] == op).collect();
        assert!(!setup.is_empty(), "Missing {op}");
        assert!(
            setup.iter().all(|r| r["process_env"] == json!({})),
            "{setup:?}"
        );
    }
    let resolve = calls
        .iter()
        .filter(|r| r["op"] == "resolve_caller")
        .collect::<Vec<_>>();
    assert!(!resolve.is_empty());
    assert!(
        resolve.iter().all(|r| r["process_env"] == json!({})),
        "{resolve:?}"
    );
    let dir = f.state.join("plugins/fixture");
    let plugin = cito_native::plugins::parse_manifest_dir(&dir, false).unwrap();
    let explicit = cito_native::plugins::plugin_request(&plugin,&json!({"op":"discover_sessions","state_dir":f.state,"env":{"HOME":f.home,"CITO_STATE_DIR":f.state,"CITO_CONFIG_PATH":f.config,"CODEX_THREAD_ID":"requested"}})).unwrap();
    assert_eq!(explicit["ok"], true);
    assert!(records()
        .iter()
        .any(|r| r["op"] == "discover_sessions"
            && r["process_env"]["CODEX_THREAD_ID"] == "requested"));
}
#[test]
fn uninstall_requires_exclusive_owner_lock_even_when_health_is_unreachable() {
    use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let marker = f.state.join("retained-data");
    fs::write(&marker, "preserve").unwrap();
    let runtime = f.state.join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    let staged = runtime.join("staged-data");
    fs::write(&staged, "staged").unwrap();
    let config_lock = fs::File::create(f.state.join("config.json.lock")).unwrap();
    let staging_lock = fs::File::create(runtime.join("staging.lock")).unwrap();
    let owner = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(f.state.join("daemon.lock"))
        .unwrap();
    // SAFETY: this test owns the descriptor until the explicit unlock below.
    assert_eq!(
        unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let (ok, _, error) = f.command(
        NATIVE,
        &["setup", "--uninstall", "--purge-data", "--json"],
        "",
    );
    assert!(!ok && error.contains("owner lock is held"), "{error}");
    assert_eq!(fs::read_to_string(&marker).unwrap(), "preserve");
    assert_eq!(fs::read_to_string(&staged).unwrap(), "staged");
    assert!(f.config.exists());
    // SAFETY: release the same owned file descriptor; the lock inode stays stable.
    assert_eq!(unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_UN) }, 0);
    let (ok, out, error) = f.command(
        NATIVE,
        &["setup", "--uninstall", "--purge-data", "--json"],
        "",
    );
    assert!(ok, "{out} {error}");
    assert!(!marker.exists());
    assert!(!f.config.exists());
    let entries = |dir: &std::path::Path| {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        entries(&f.state),
        ["config.json.lock", "daemon.lock", "runtime"]
    );
    assert_eq!(entries(&runtime), ["staging.lock"]);
    // A recreated pathname would permit a second writer to lock a different inode.
    for (file, path) in [
        (owner, f.state.join("daemon.lock")),
        (config_lock, f.state.join("config.json.lock")),
        (staging_lock, runtime.join("staging.lock")),
    ] {
        let before = file.metadata().unwrap();
        let after = fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (after.dev(), after.ino()),
            (before.dev(), before.ino()),
            "{}",
            path.display()
        );
    }
}
// Health is available before initial setup inspections finish. Establish the
// settled projection before measuring cache reuse or changing the provider.
fn assert_initial_fixture_setup(f: &Fixture, configured: bool) {
    let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
    assert_eq!(status, 200);
    let app = snapshot["settings"]["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|app| app["id"] == "fixture")
        .unwrap();
    assert_eq!(app["configured"], configured, "{app}");
}

#[test]
fn setup_inspection_cache_and_hub_rename_migrate_persistent_state() {
    let _lock = serial();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":"https://hub.example.com","enabled":false}}}),
    );
    f.provider("delivered");
    f.daemon(&[]);
    assert_initial_fixture_setup(&f, true);
    let count = || {
        fs::read_to_string(f.state.join("plugins/fixture/calls.jsonl"))
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|r| r["op"] == "setup_inspect")
            .count()
    };
    let initial = count();
    assert_eq!(initial, 1);
    for _ in 0..3 {
        f.action_ok(json!({"op":"get_snapshot"}));
    }
    assert_eq!(count(), initial, "Snapshots reuse setup inspection");
    f.action_ok(json!({"op":"setup_apply","harness":"fixture"}));
    assert_eq!(count(), initial + 1);
    f.action_ok(json!({"op":"setup_remove_app","harness":"fixture"}));
    assert_eq!(count(), initial + 2);
    let db = f.db("state.sqlite");
    db.execute("INSERT INTO local_listeners(hub,listener_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status,hub_url) SELECT 'work',listener_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,'UNLISTENED','https://hub.example.com' FROM local_listeners WHERE hub='local' AND harness='human'",&[]).unwrap();
    db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) SELECT 'work','pending',listener_id,1,0,'bob','laptop','','Keep','now',9999999999999,'STORED_PASSIVE' FROM local_listeners WHERE hub='work'; INSERT INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms,hub_url,principal_id) SELECT hub,listener_id,'pending','device:human',1,hub_url,principal_id FROM local_listeners WHERE hub='work'; INSERT INTO pending_hub_detaches(hub,listener_id,hub_url) SELECT hub,listener_id,hub_url FROM local_listeners WHERE hub='work'",&[]).unwrap();
    fs::write(f.state.join("oidc-token-work.json"), "retained credential").unwrap();
    f.action_ok(json!({"op":"setup_save_hub","edit":true,"original_name":"work","name":"cloud","url":"https://hub.example.com","enabled":false}));
    for table in [
        "local_listeners",
        "local_signals",
        "pending_hub_acks",
        "pending_hub_detaches",
    ] {
        assert_eq!(
            db.query(&format!("SELECT hub FROM {table} WHERE hub='cloud'"), &[])
                .unwrap()
                .len(),
            1,
            "{table}"
        );
        assert!(db
            .query(&format!("SELECT hub FROM {table} WHERE hub='work'"), &[])
            .unwrap()
            .is_empty());
    }
    assert!(!f.state.join("oidc-token-work.json").exists());
    assert_eq!(
        fs::read_to_string(f.state.join("oidc-token-cloud.json")).unwrap(),
        "retained credential"
    );
    db.execute("CREATE TRIGGER reject_rename BEFORE UPDATE ON local_listeners WHEN NEW.hub='blocked' BEGIN SELECT RAISE(ABORT,'injected rename failure'); END",&[]).unwrap();
    let failed = f.action(json!({"op":"setup_save_hub","edit":true,"original_name":"cloud","name":"blocked","url":"https://hub.example.com","enabled":false}));
    assert_eq!(failed.0, 400);
    assert!(failed.1.to_string().contains("injected rename failure"));
    let config: Value = read_json(&f.config);
    assert!(config["hubs"]["cloud"].is_object());
    assert!(config["hubs"].get("blocked").is_none());
    assert!(f.state.join("oidc-token-cloud.json").exists());
    assert!(!f.state.join("oidc-token-blocked.json").exists());
    assert_eq!(
        db.query("SELECT * FROM local_listeners WHERE hub='cloud'", &[])
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn setup_refresh_preserves_cached_settings_without_blocking_hub_or_mcp_and_fences_stale_generations(
) {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    assert_initial_fixture_setup(&f, true);
    let options_path = f.state.join("plugins/fixture/options.json");
    let calls_path = f.state.join("plugins/fixture/calls.jsonl");
    let gate = f.dir.join("release-setup");
    let mut options: Value = read_json(&options_path);
    options["configured"] = json!(false);
    options["setup_gate"] = json!(gate);
    fs::write(&options_path, options.to_string()).unwrap();
    let count = |op: &str| {
        fs::read_to_string(&calls_path)
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|r| r["op"] == op)
            .count()
    };
    let port = f.port;
    std::thread::scope(|scope| {
        let first = scope.spawn(move || {
            http(
                port,
                "/ui",
                Some(&json!({"op":"setup_apply","harness":"fixture"})),
            )
        });
        wait(|| count("setup_inspect") >= 2);
        let start = std::time::Instant::now();
        let snapshot = f.action(json!({"op":"get_snapshot"}));
        let peers = f.tool("cito_find", json!({}), meta);
        let elapsed = start.elapsed();
        let cached = snapshot.1["settings"]["apps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == "fixture")
            .unwrap()["configured"]
            .clone();
        options["configured"] = json!(true);
        fs::write(&options_path, options.to_string()).unwrap();
        let second = scope.spawn(move || {
            http(
                port,
                "/ui",
                Some(&json!({"op":"setup_apply","harness":"fixture"})),
            )
        });
        wait(|| count("setup_apply") >= 2);
        fs::write(&gate, "").unwrap();
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert_eq!(snapshot.0, 200);
        assert_eq!(peers["ok"], true);
        assert_eq!(cached, true, "Refresh must retain the previous projection");
        assert!(
            elapsed < std::time::Duration::from_millis(800),
            "Blocked setup held service: {elapsed:?}"
        );
        assert_eq!(first.0, 200);
        assert_eq!(second.0, 200);
    });
    assert!(
        count("setup_inspect") >= 3,
        "A superseded inspection must refresh at the current generation"
    );
    let snapshot = f.snapshot();
    assert_eq!(
        snapshot["settings"]["apps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == "fixture")
            .unwrap()["configured"],
        true
    );
}
#[test]
fn setup_apply_uses_reloaded_command_and_leaves_snapshot_available() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    let dir = f.state.join("plugins/fixture");
    let old = dir.join("driver.py");
    let driver = r#"import json, pathlib, sys, time, os
root = pathlib.Path(__file__).parent
old = pathlib.Path(__file__).name == 'driver.py'
request = json.loads(sys.stdin.readline())
op = request['op']
reply = dict(ok=True, installed=True, configured=True, paths=[], actions=[])
def gate(entered, release):
    (root/entered).touch()
    deadline = time.monotonic() + 3
    while not (root/release).exists():
        if time.monotonic() >= deadline: sys.exit(1)
        time.sleep(.005)
if op == 'setup_apply' and not old: gate('entered','release')
if op == 'resolve_caller': reply['caller'] = dict(harness='fixture',session_id='session-1',cwd=str(root),title='Current',endpoint_pid=os.getppid())
if op == 'probe': reply.update(status='alive', current_session_id='session-1')
if op == 'discover_sessions':
    if old and (root/'arm').exists(): gate('old-flight','finish-old')
    reply['sessions'] = [dict(harness='fixture',session_id='old-session' if old else 'new-session',cwd=str(root))]
if op == 'update_badge' and request['badge']:
    (root/('old-badge' if old else 'new-badge')).write_text(request['badge'])
print(json.dumps(reply))
"#;
    fs::write(&old, driver).unwrap();
    f.daemon(&[]);
    assert_eq!(
        f.tool("cito_listen", json!({"about":"Badge target"}), meta)["ok"],
        true
    );
    wait(|| dir.join("old-badge").exists());
    assert!(f.snapshot()["available"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["session_id"] == "old-session"));
    fs::write(dir.join("arm"), b"").unwrap();
    f.action_ok(json!({"op":"set_auto_listen","hub":"local","enabled":true}));
    wait(|| dir.join("old-flight").exists());
    let manifest = dir.join("plugin.json");
    let mut changed: Value = read_json(&manifest);
    changed["command"] = json!(["python3", "./updated.py"]);
    fs::write(dir.join("updated.py"), driver).unwrap();
    fs::write(manifest, changed.to_string()).unwrap();
    let port = f.port;
    std::thread::scope(|scope| {
        let pending = scope.spawn(move || {
            http(
                port,
                "/ui",
                Some(&json!({"op":"setup_apply","harness":"fixture"})),
            )
        });
        wait(|| dir.join("entered").exists());
        fs::remove_file(&old).unwrap();
        fs::write(dir.join("finish-old"), b"").unwrap();
        let snapshot = f.action(json!({"op":"get_snapshot"}));
        fs::write(dir.join("release"), b"done").unwrap();
        assert_eq!(snapshot.0, 200);
        let available = snapshot.1["available"].as_array().unwrap();
        assert!(
            available.iter().any(|r| r["session_id"] == "new-session"),
            "{available:?}"
        );
        assert!(available.iter().all(|r| r["session_id"] != "old-session"));
        wait(|| dir.join("new-badge").exists());
        assert_eq!(
            fs::read(dir.join("old-badge")).unwrap(),
            fs::read(dir.join("new-badge")).unwrap()
        );
        let receipt = pending.join().unwrap();
        assert_eq!(receipt.0, 200, "{}", receipt.1);
        assert_eq!(receipt.1["committed"], true);
    });
}

#[test]
fn setup_receipts_wait_for_post_mutation_inspections() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.provider("delivered");
    let options_path = f.state.join("plugins/fixture/options.json");
    let calls_path = f.state.join("plugins/fixture/calls.jsonl");
    let mut options: Value = read_json(&options_path);
    options["configured"] = json!(false);
    fs::write(&options_path, options.to_string()).unwrap();
    f.recording_daemon();
    assert_initial_fixture_setup(&f, false);
    let frames = || ui_frames(&f);
    let inspections = || {
        fs::read_to_string(&calls_path)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|row| row["op"] == "setup_inspect")
            .count()
    };
    let configured = |frame: &Value| {
        frame["settings"]["apps"]
            .as_array()
            .unwrap_or_else(|| panic!("Missing settings apps in {frame}"))
            .iter()
            .find(|app| app["id"] == "fixture")
            .unwrap()["configured"]
            .clone()
    };
    for (index, (op, expected)) in [
        ("setup_apply", true),
        ("setup_remove_app", false),
        ("setup_apply", true),
        ("setup_disconnect_all", false),
    ]
    .into_iter()
    .enumerate()
    {
        let request_id = format!("setup-{index}");
        let marker = format!("while-blocked-{index}");
        let gate = f.dir.join(format!("release-setup-{index}"));
        let before = inspections();
        options["configured"] = json!(expected);
        options["setup_gate"] = json!(gate);
        options["setup_gate_timeout_ms"] = json!(5000);
        fs::write(&options_path, options.to_string()).unwrap();
        let action = json!({"op":op,"harness":"fixture","request_id":request_id});
        let port = f.port;
        std::thread::scope(|scope| {
            let pending = scope.spawn(move || http(port, "/ui", Some(&action)));
            wait(|| inspections() > before);
            let snapshot = f.snapshot();
            // This unrelated correlated frame is an ordered mailbox barrier.
            f.action_ok(json!({"op":"set_user","user":"alice","request_id":marker}));
            wait(|| frames().iter().any(|frame| frame["request_id"] == marker));
            let premature = frames()
                .into_iter()
                .filter(|frame| frame["request_id"] == request_id)
                .map(|frame| json!({"op":frame["op"],"configured":configured(&frame)}))
                .collect::<Vec<_>>();
            fs::write(&gate, "").unwrap();
            let response = pending.join().unwrap();
            assert_eq!(response.0, 200, "{response:?}");
            assert_eq!(
                configured(&snapshot),
                !expected,
                "Cached projection must remain available while refreshing"
            );
            assert!(
                premature.is_empty(),
                "{op} acknowledged before inspection settled: {premature:?}"
            );
        });
        wait(|| {
            ["settings", "state"].into_iter().all(|op| {
                frames()
                    .iter()
                    .any(|frame| frame["request_id"] == request_id && frame["op"] == op)
            })
        });
        let receipts = frames()
            .into_iter()
            .filter(|frame| frame["request_id"] == request_id)
            .collect::<Vec<_>>();
        assert_eq!(
            receipts.len(),
            2,
            "One settings receipt and one state receipt per successful action"
        );
        for receipt in receipts {
            assert_eq!(receipt["ok"], true);
            assert_eq!(receipt["source_op"], op);
            assert_eq!(
                configured(&receipt),
                expected,
                "{op}: {} receipt",
                receipt["op"]
            );
        }
    }
}
#[test]
fn u1_synthetic_sessions_follow_explicit_configuration_without_global_hardcoding() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (_, port) = host.hub();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":format!("http://127.0.0.1:{port}"),"autoListen":false}},"defaultHub":"local"}),
    );
    f.provider("delivered");
    let daemon = f.daemon(&[]);
    let snapshot = || f.snapshot();
    let local_auto_listen = || {
        snapshot()["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hub| hub["name"] == "local")
            .unwrap()["auto_listen"]
            .clone()
    };
    wait(|| {
        snapshot()["available"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["harness"] == "fixture")
    });
    let options = f.state.join("plugins/fixture/options.json");
    let mut config: Value = read_json(&options);
    config["configured"] = json!(false);
    fs::write(&options, config.to_string()).unwrap();
    f.action_ok(json!({"op":"setup_apply","harness":"fixture"}));
    assert_eq!(snapshot()["available"], json!([]));
    config["configured"] = json!(true);
    fs::write(&options, config.to_string()).unwrap();
    f.action_ok(json!({"op":"setup_apply","harness":"fixture"}));
    assert!(snapshot()["available"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["harness"] == "fixture"));
    f.action_ok(json!({"op":"set_auto_listen","hub":"local","enabled":true}));
    f.action_ok(
        json!({"op":"unlisten_session","hub":"local","harness":"fixture","session_id":"session-1"}),
    );
    assert_eq!(local_auto_listen(), true);
    f.stop(daemon);
    let mut preferences: Value = read_json(&f.config);
    preferences["hubs"]["local"]["autoListen"] = json!(true);
    fs::write(&f.config, preferences.to_string()).unwrap();
    config["session_id"] = json!("live");
    let mut sessions: Vec<Value> = (0..8).map(|i| json!({"harness":"fixture","session_id":format!("historical-{i}"),"title":"History","cwd":"/tmp"})).collect();
    sessions.extend([
        json!({"harness":"fixture","session_id":"reload","title":"Reload","cwd":"/tmp","endpoint_pid":std::process::id(),"needs_reload":true}),
        json!({"harness":"fixture","session_id":"live","title":"Live","cwd":"/tmp","endpoint_pid":std::process::id()}),
        json!({"harness":"fixture","session_id":"departed","title":"Departed","cwd":"/tmp","endpoint_pid":i32::MAX})
    ]);
    config["sessions"] = json!(sessions);
    fs::write(&options, config.to_string()).unwrap();
    // Only background polling drives discovery/enrollment after restart.
    f.daemon(&[]);
    wait(|| {
        snapshot()["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["harness"] == "fixture" && v["session_id"] == "live")
    });
    let rows = f
        .rows("SELECT session_id FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'");
    assert_eq!(rows, vec![json!({"session_id":"live"})]);
    assert_eq!(local_auto_listen(), true);
    let before = f.rows(
        "SELECT owner_token FROM local_listeners WHERE harness=\'fixture\' AND session_id=\'live\'",
    )[0]
    .clone();
    config["configured"] = json!(false);
    config["status"] = json!("closed_definitive");
    fs::write(&options, config.to_string()).unwrap();
    wait(|| {
        f.rows(
            "SELECT status FROM local_listeners WHERE harness=\'fixture\' AND session_id=\'live\'",
        )[0]["status"]
            == "UNLISTENED"
    });
    f.action_ok(
        json!({"op":"unlisten_session","hub":"local","harness":"fixture","session_id":"live"}),
    );
    assert_eq!(
        local_auto_listen(),
        true,
        "An inactive-row pause must not disable Auto-Listen"
    );
    config["configured"] = json!(true);
    config["status"] = json!("alive");
    fs::write(&options, config.to_string()).unwrap();
    f.action_ok(json!({"op":"refresh_available"}));
    assert_eq!(f.rows("SELECT user_stopped FROM local_listeners WHERE harness='fixture' AND session_id='live'")[0]["user_stopped"],1);
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='fixture' AND session_id='live'")
            [0]["status"],
        "UNLISTENED"
    );
    f.action_ok(json!({"op":"set_auto_listen","hub":"local","enabled":true}));
    wait(|| {
        snapshot()["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["harness"] == "fixture" && r["session_id"] == "live")
    });
    assert_ne!(f.rows("SELECT owner_token FROM local_listeners WHERE harness=\'fixture\' AND session_id=\'live\'")[0]["owner_token"], before["owner_token"], "A recovered endpoint needs a new local binding");
    let recovered = f.rows(
        "SELECT owner_token FROM local_listeners WHERE harness=\'fixture\' AND session_id=\'live\'",
    )[0]
    .clone();
    // The PID stays alive while the endpoint switches identities between discovery passes.
    config["session_id"] = json!("replacement");
    config["sessions"] = json!([{"harness":"fixture","session_id":"replacement","title":"New session","cwd":"/tmp","endpoint_pid":std::process::id()}]);
    config["status"] = json!("switched");
    // Let the concurrent probe revoke after the enrollment pass snapshots rows.
    config["setup_delay_ms"] = json!(200);
    fs::write(&options, config.to_string()).unwrap();
    wait(|| {
        f.rows(
            "SELECT status FROM local_listeners WHERE harness=\'fixture\' AND session_id=\'live\'",
        )[0]["status"]
            == "UNLISTENED"
    });
    assert!(f.rows("SELECT updated_at FROM local_listeners WHERE harness=\'fixture\' AND session_id=\'live\'")[0]["updated_at"].as_u64().unwrap()>0);
    config["status"] = json!("alive");
    config.as_object_mut().unwrap().remove("setup_delay_ms");
    fs::write(&options, config.to_string()).unwrap();
    // Wait for the next confirmed discovery, not a fixed sleep or explicit cache refresh.
    wait(|| {
        snapshot()["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["harness"] == "fixture" && r["session_id"] == "replacement")
    });
    let old = &f.rows("SELECT status,owner_token FROM local_listeners WHERE harness=\'fixture\' AND session_id=\'live\'")[0];
    assert_eq!(
        old["owner_token"], recovered["owner_token"],
        "No intermediate re-enrollment from the pre-revocation cache"
    );
    assert_eq!(old["status"], "UNLISTENED");
    // Exercise wake-dispatch revocation independently of the probe path above.
    let dispatched=f.rows("SELECT listener_id,owner_token FROM local_listeners WHERE harness='fixture' AND session_id='replacement'")[0].clone();
    f.db("state.sqlite").execute("UPDATE local_listeners SET updated_at=1 WHERE harness='fixture' AND session_id='replacement'",&[]).unwrap();
    config["outcome"] = json!("session_switched");
    config["sessions"] = json!([{"harness":"fixture","session_id":"dispatch-replacement","title":"After dispatch","cwd":"/tmp","endpoint_pid":std::process::id()}]);
    fs::write(&options, config.to_string()).unwrap();
    let (status,result)=f.action(json!({"op":"send_signal","hub":"local","to":dispatched["listener_id"],"message":"Dispatch identity switch","request_id":"dispatch-switch-send"}));
    assert_eq!(status, 200, "{result}");
    wait(|| {
        f.rows("SELECT status FROM local_listeners WHERE harness='fixture' AND session_id='replacement'")[0]["status"]=="UNLISTENED"
    });
    assert!(f.rows("SELECT updated_at FROM local_listeners WHERE harness='fixture' AND session_id='replacement'")[0]["updated_at"].as_u64().unwrap()>1);
    assert!(
        !f.rows("SELECT signal_id FROM local_signals WHERE held_reason='session_switched'")
            .is_empty(),
        "The wake response, not a probe, revoked this binding"
    );
    config["session_id"] = json!("dispatch-replacement");
    config["outcome"] = json!("delivered");
    fs::write(&options, config.to_string()).unwrap();
    wait(|| {
        snapshot()["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["harness"] == "fixture" && r["session_id"] == "dispatch-replacement")
    });
    let old=&f.rows("SELECT status,owner_token FROM local_listeners WHERE harness='fixture' AND session_id='replacement'")[0];
    assert_eq!(old["status"], "UNLISTENED");
    assert_eq!(
        old["owner_token"], dispatched["owner_token"],
        "Dispatch revocation must reject the pre-revocation discovery cache"
    );
    // Cache evidence from S cannot enroll on a destination that has no S row.
    f.action_ok(json!({"op":"set_auto_listen","hub":"local","enabled":false}));
    f.action_ok(json!({"op":"refresh_available"}));
    let discoveries = || {
        fs::read_to_string(f.state.join("plugins/fixture/calls.jsonl"))
            .unwrap()
            .lines()
            .filter(|line| {
                serde_json::from_str::<Value>(line).unwrap()["op"] == "discover_sessions"
            })
            .count()
    };
    let confirmed = discoveries();
    config["status"] = json!("closed_definitive");
    config["discover_error"] = json!(true);
    fs::write(&options, config.to_string()).unwrap();
    wait(|| {
        f.rows("SELECT status FROM local_listeners WHERE hub='local' AND harness='fixture' AND session_id='dispatch-replacement'")[0]["status"]=="UNLISTENED"
    });
    assert_eq!(
        discoveries(),
        confirmed,
        "The cached evidence must predate this revocation"
    );
    config["status"] = json!("alive");
    fs::write(&options, config.to_string()).unwrap();
    assert!(f
        .rows("SELECT listener_id FROM local_listeners WHERE hub='work' AND harness='fixture'")
        .is_empty());
    f.action_ok(json!({"op":"set_auto_listen","hub":"work","enabled":true}));
    assert!(
        f.rows("SELECT listener_id FROM local_listeners WHERE hub='work' AND harness='fixture'")
            .is_empty(),
        "Hub B must not enroll from Hub A's pre-revocation cache"
    );
    config["discover_error"] = json!(false);
    fs::write(&options, config.to_string()).unwrap();
    f.action_ok(json!({"op":"refresh_available"}));
    wait(|| {
        f.rows("SELECT status FROM local_listeners WHERE hub='work' AND harness='fixture' AND session_id='dispatch-replacement'").iter().any(|row|row["status"]=="ACTIVE")
    });
    assert!(discoveries() > confirmed);
}

#[test]
fn user_plugin_overrides_take_precedence_and_fall_back_after_removal() {
    if std::env::var_os("CITO_TEST_PLUGIN_PRIORITY").is_some() {
        let (config, _) = cito_native::plugins::load_native_config().unwrap();
        let dirs = [
            config.home_dir.join("first/pi"),
            config.home_dir.join("second/pi"),
            config.state_dir.join("plugins/pi"),
        ];
        for (index, expected) in [
            "First override",
            "Second override",
            "User override",
            "builtin",
        ]
        .into_iter()
        .enumerate()
        {
            let (_, plugins) = cito_native::plugins::load_native_config().unwrap();
            let plugin = plugins.find_by_id("pi").unwrap();
            assert_eq!(plugins.plugins.iter().filter(|p| p.id == "pi").count(), 1);
            assert_eq!(plugin.is_builtin, index == 3);
            cito_native::setup::ensure_cli_shims_native(&config.state_dir, &plugins).unwrap();
            if index < 3 {
                assert_eq!(plugin.display_name, expected);
                assert_eq!(plugin.plugin_dir, dirs[index]);
                let actual =
                    cito_native::plugins::plugin_request(plugin, &json!({"op":"probe"})).unwrap();
                assert_eq!(actual["source"], expected);
                assert_eq!(
                    actual["cwd"],
                    json!(fs::canonicalize(&dirs[index]).unwrap())
                );
                assert_eq!(
                    cito_native::setup::try_inspect_plugin_setup(plugin).unwrap(),
                    (true, true, vec![dirs[index].display().to_string()])
                );
                assert!(!config.state_dir.join("runtime/plugins/pi").exists());
                let manifest: Value = read_json(config.state_dir.join("plugins/pi/plugin.json"));
                assert_eq!(
                    manifest["displayName"], "User override",
                    "Staging cannot overwrite a user plugin"
                );
                fs::remove_dir_all(&dirs[index]).unwrap();
            } else {
                assert_eq!(plugin.display_name, "Pi Coding Agent");
                assert!(config
                    .state_dir
                    .join("runtime/plugins/pi/plugin.json")
                    .is_file());
                assert!(!config.state_dir.join("plugins/pi").exists());
                assert!(cito_native::setup::is_staged_plugin_intact(
                    &config.state_dir,
                    plugin
                ));
            }
        }
        return;
    }
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let driver = f.dir.join("override.py");
    script(&driver, "# purpose: Report the actual selected plugin command and setup source.\n# usage: python3 override.py DISPLAY_NAME\n# effects: Reads one request; emits JSON only.\n# requires: Python standard library.\nimport json, os, sys\nr=json.loads(sys.stdin.readline())\nprint(json.dumps({'ok':True,'installed':True,'configured':True,'source':sys.argv[1],'cwd':os.getcwd(),'paths':[r.get('plugin_dir','')]}))\n");
    for (dir, display) in [
        (f.home.join("first/pi"), "First override"),
        (f.home.join("second/pi"), "Second override"),
        (f.state.join("plugins/pi"), "User override"),
    ] {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("plugin.json"), json!({"id":"pi","kind":"agent","displayName":display,"command":["python3",driver,display]}).to_string()).unwrap();
    }
    f.env
        .insert("CITO_PLUGIN_PATH".into(), " ~/first : ~/second ".into());
    f.env.insert("CITO_TEST_PLUGIN_PRIORITY".into(), "1".into());
    let (ok, out, err) = f.command(
        std::env::current_exe().unwrap().to_str().unwrap(),
        &[
            "--exact",
            "daemon::discovery_and_setup::user_plugin_overrides_take_precedence_and_fall_back_after_removal",
            "--nocapture",
        ],
        "",
    );
    assert!(ok, "{out}\n{err}");
}

#[test]
fn legacy_cleanup_rejects_invalid_native_config_without_deleting_plugins() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    f.env.remove("CITO_USER");
    let legacy = f.state.join("plugins/pi");
    fs::create_dir_all(&legacy).unwrap();
    let manifest = legacy_manifest("pi");
    fs::write(legacy.join("plugin.json"), &manifest).unwrap();
    for raw in [
        "{",
        "[]",
        "{\"daemonPort\":0}",
        "{\"user\":\"invalid user\"}",
        "{\"hubs\":{\"work\":{\"url\":\"invalid\"}}}",
    ] {
        fs::write(&f.config, raw).unwrap();
        let (ok, out, err) = f.command(NATIVE, &["daemon"], "");
        assert!(!ok, "{raw}: {out} {err}");
        assert_eq!(
            fs::read_to_string(legacy.join("plugin.json")).unwrap(),
            manifest
        );
        assert_eq!(fs::read_to_string(&f.config).unwrap(), raw);
    }
    fs::write(&f.config, "{}").unwrap();
    f.env.insert("CITO_DAEMON_PORT".into(), "invalid".into());
    let (ok, out, err) = f.command(NATIVE, &["daemon"], "");
    assert!(!ok, "{out} {err}");
    assert!(err.contains("CITO_DAEMON_PORT"), "{err}");
    assert_eq!(
        fs::read_to_string(legacy.join("plugin.json")).unwrap(),
        manifest
    );
}

#[test]
fn bundled_plugin_sources_survive_user_root_and_child_symlinks() {
    if std::env::var_os("CITO_TEST_SOURCE_ALIASES").is_some() {
        use cito_native::{core::hub_plugins, plugins, setup};
        let state = std::path::PathBuf::from(std::env::var("CITO_STATE_DIR").unwrap());
        let source = std::path::PathBuf::from(std::env::var("CITO_BUILTIN_PLUGINS_DIR").unwrap());
        let user = state.join("plugins");
        assert_eq!(
            setup::resolve_bundled_plugin_dir("telegram"),
            source.join("telegram")
        );
        let originals: Vec<_> = ["telegram", "pi"]
            .into_iter()
            .map(|id| (id, fs::read(source.join(id).join("plugin.json")).unwrap()))
            .collect();
        for parent_alias in [true, false] {
            for native in [true, false] {
                if parent_alias {
                    std::os::unix::fs::symlink(&source, &user).unwrap();
                } else {
                    fs::create_dir_all(&user).unwrap();
                    for (id, _) in &originals {
                        std::os::unix::fs::symlink(source.join(id), user.join(id)).unwrap();
                    }
                }
                let readonly = plugins::load_plugins_readonly(&state);
                assert!(readonly.find_by_id("human").is_none());
                assert!(user.join("telegram").exists());
                if native {
                    let loaded = plugins::load_plugins(&state);
                    for (id, _) in &originals {
                        let plugin = loaded.find_by_id(id).unwrap();
                        assert!(plugin.is_builtin, "{id}");
                        assert_eq!(plugin.plugin_dir, source.join(id));
                    }
                } else {
                    let channels = hub_plugins::discover(
                        &[(user.clone(), false), (source.clone(), true)],
                        Some(&user),
                    )
                    .unwrap();
                    assert_eq!(
                        channels.iter().find(|p| p.id == "telegram").unwrap().dir,
                        source.join("telegram")
                    );
                }
                for (id, original) in &originals {
                    assert_eq!(
                        fs::read(source.join(id).join("plugin.json")).unwrap(),
                        *original
                    );
                }
                if parent_alias {
                    assert!(user.is_symlink());
                    fs::remove_file(&user).unwrap();
                } else {
                    assert!(fs::read_dir(&user).unwrap().next().is_none());
                    fs::remove_dir(&user).unwrap();
                }
            }
        }
        return;
    }
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    // Relocate our own libtest binary so the real packaged-root resolver uses sandbox data.
    // No system binary, desktop application, daemon, or network service is launched.
    let package = f.dir.join("package");
    let executable = package.join("bin/plugin-test");
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let source = package.join("plugins");
    for id in ["telegram", "pi"] {
        fs::create_dir_all(source.join(id)).unwrap();
        fs::copy(
            root().join("plugins").join(id).join("plugin.json"),
            source.join(id).join("plugin.json"),
        )
        .unwrap();
    }
    f.env.insert(
        "CITO_BUILTIN_PLUGINS_DIR".into(),
        source.display().to_string(),
    );
    f.env.insert("CITO_TEST_SOURCE_ALIASES".into(), "1".into());
    let (ok, out, err) = f.command(
        executable.to_str().unwrap(),
        &[
            "--exact",
            "daemon::discovery_and_setup::bundled_plugin_sources_survive_user_root_and_child_symlinks",
            "--nocapture",
        ],
        "",
    );
    assert!(ok, "{out} {err}");
}

fn legacy_manifest(id: &str) -> String {
    let shipped = fs::read(root().join("plugins").join(id).join("plugin.json")).unwrap();
    let mut manifest: Value = serde_json::from_slice(&shipped).unwrap();
    manifest["version"] = json!("0.20.3");
    manifest.to_string()
}

#[test]
fn legacy_staged_plugins_migrate_without_discarding_custom_overrides() {
    if let Ok(mode) = std::env::var("CITO_TEST_LEGACY_PLUGINS") {
        use cito_native::{core::hub_plugins, plugins};
        let state = std::path::PathBuf::from(std::env::var("CITO_STATE_DIR").unwrap());
        let user_root = state.join("plugins");
        let ids = ["pi", "codex", "claude", "telegram", "human"];
        if mode == "native" {
            let config_path = std::path::PathBuf::from(std::env::var("CITO_CONFIG_PATH").unwrap());
            let original_config = fs::read(&config_path).unwrap();
            let legacy_manifest = fs::read(user_root.join("pi/plugin.json")).unwrap();
            for hub in ["local", "work"] {
                for user in [
                    json!("invalid user"),
                    json!("self"),
                    json!(""),
                    json!(" "),
                    json!(" alice "),
                    json!(42),
                    json!(false),
                    json!([]),
                    json!({}),
                ] {
                    let mut raw =
                        json!({"hubs":{"local":{"url":"local://"},"work":{"url":"local://"}}});
                    raw["hubs"][hub]["user"] = user;
                    fs::write(&config_path, raw.to_string()).unwrap();
                    assert_eq!(
                        plugins::load_native_config().unwrap_err(),
                        format!("Invalid user for hub {hub}")
                    );
                    assert_eq!(
                        fs::read(user_root.join("pi/plugin.json")).unwrap(),
                        legacy_manifest
                    );
                    for id in ids {
                        assert!(user_root.join(id).is_dir(), "rejected config removed {id}");
                    }
                }
            }
            fs::write(
                &config_path,
                json!({"hubs":{"local":{"url":"local://","user":null}}}).to_string(),
            )
            .unwrap();
            let (config, loaded) = plugins::load_native_config().unwrap();
            assert_eq!(config.effective_user("local").unwrap(), config.user);
            fs::write(config_path, original_config).unwrap();
            assert!(loaded.find_by_id("human").is_none());
            for id in ids.into_iter().filter(|id| *id != "human") {
                let plugin = loaded.find_by_id(id).unwrap();
                assert!(plugin.is_builtin, "{id}");
                let manifest: Value = read_json(plugin.plugin_dir.join("plugin.json"));
                assert_eq!(manifest["version"], plugins::CITO_VERSION);
            }
        } else {
            let loaded = hub_plugins::discover(
                &plugins::plugin_roots_for_executable(
                    &state,
                    std::env::current_exe().ok().as_deref(),
                ),
                Some(&user_root),
            )
            .unwrap();
            let channel = loaded.iter().find(|p| p.id == "telegram").unwrap();
            assert_eq!(channel.manifest["version"], plugins::CITO_VERSION);
            assert!(channel.dir.ends_with("plugins/telegram"));
            assert_ne!(channel.dir, user_root.join("telegram"));
            assert!(!loaded.iter().any(|p| p.id == "human"));
        }
        for id in ids {
            assert!(!user_root.join(id).exists(), "legacy copy remains: {id}");
        }
        let external = std::path::PathBuf::from(std::env::var("CITO_PLUGIN_DIR").unwrap());
        assert!(external.join("human/plugin.json").is_file());
        assert!(external.join("alias/plugin.json").is_file());
        // The companion stays reserved even for a current-version custom command.
        let companion = user_root.join("human");
        fs::create_dir_all(&companion).unwrap();
        fs::write(companion.join("plugin.json"), json!({"id":"human","kind":"human",
            "displayName":"Custom companion","version":plugins::CITO_VERSION,"command":["./run.sh"]}).to_string()).unwrap();
        let loaded = plugins::load_plugins(&state);
        assert!(loaded.find_by_id("human").is_none());
        assert!(!companion.exists());
        // Custom commands survive any version; native-shaped commands survive at the current version.
        for (version, command) in [
            (Value::Null, json!(["./run.sh"])),
            (json!("custom"), json!(["node", "custom.js"])),
            (
                json!(plugins::CITO_VERSION),
                json!(["./bin/cito-plugin-custom"]),
            ),
        ] {
            for (id, kind) in [("pi", "agent"), ("telegram", "human-channel")] {
                let dir = user_root.join(id);
                fs::create_dir_all(&dir).unwrap();
                fs::write(
                    dir.join("plugin.json"),
                    json!({"id":id,"kind":kind,
                    "displayName":"Custom override","version":version,"command":command})
                    .to_string(),
                )
                .unwrap();
            }
            let loaded = plugins::load_plugins(&state);
            for id in ["pi", "telegram"] {
                let plugin = loaded.find_by_id(id).unwrap();
                assert!(!plugin.is_builtin);
                assert_eq!(plugin.plugin_dir, user_root.join(id));
            }
            let channels = hub_plugins::discover(
                &plugins::plugin_roots_for_executable(
                    &state,
                    std::env::current_exe().ok().as_deref(),
                ),
                Some(&user_root),
            )
            .unwrap();
            assert_eq!(
                channels.iter().find(|p| p.id == "telegram").unwrap().dir,
                user_root.join("telegram")
            );
        }
        return;
    }
    let _lock = serial();
    for mode in ["native", "hub"] {
        let mut f = Fixture::new(json!({}));
        for id in ["pi", "codex", "claude", "telegram"] {
            let dir = f.state.join("plugins").join(id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("plugin.json"), legacy_manifest(id)).unwrap();
        }
        // The retired companion no longer has a shipped manifest.
        let retired = f.state.join("plugins/human");
        fs::create_dir_all(&retired).unwrap();
        fs::write(retired.join("plugin.json"), r#"{"id":"human","kind":"human-channel","displayName":"Human","version":"0.20.3","command":["./bin/cito-plugin-human"]}"#).unwrap();
        let external = f.dir.join("external");
        for name in ["human", "alias"] {
            let dir = external.join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("plugin.json"),
                json!({"id":"human","kind":"human-channel",
                "displayName":"External companion","command":["./run.sh"]})
                .to_string(),
            )
            .unwrap();
        }
        f.env
            .insert("CITO_PLUGIN_PATH".into(), external.display().to_string());
        f.env
            .insert("CITO_PLUGIN_DIR".into(), external.display().to_string());
        f.env.insert("CITO_TEST_LEGACY_PLUGINS".into(), mode.into());
        let (ok, out, err) = f.command(
            std::env::current_exe().unwrap().to_str().unwrap(),
            &[
                "--exact",
                "daemon::discovery_and_setup::legacy_staged_plugins_migrate_without_discarding_custom_overrides",
                "--nocapture",
            ],
            "",
        );
        assert!(ok, "{mode}: {out}\n{err}");
    }
}

#[test]
fn lifecycle_setup_repairs_deleted_builtin_loaded_from_staged_plugins() {
    if std::env::var_os("CITO_TEST_STAGED_PLUGINS").is_some() {
        // Re-enter under the fixture environment without changing process-global
        // variables in the parent test runner.
        let (config, plugins) = cito_native::plugins::load_native_config().unwrap();
        let custom = plugins.find_by_id("custom-agent").unwrap();
        let custom_dir = config.state_dir.join("plugins/custom-agent");
        let cwd = fs::canonicalize(&custom_dir).unwrap();
        assert!(!custom.is_builtin);
        assert_eq!(custom.plugin_dir, custom_dir);
        let response =
            cito_native::plugins::plugin_request(custom, &json!({"op":"setup_inspect"})).unwrap();
        assert_eq!(response["cwd"], json!(cwd));
        assert_eq!(response["executable"], json!(custom_dir.join("./run.sh")));
        assert_eq!(
            cito_native::setup::try_inspect_plugin_setup(custom).unwrap(),
            (true, true, vec![cwd.display().to_string()])
        );
        assert!(plugins.find_by_id("pi").unwrap().is_builtin);
        return;
    }

    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let daemon = f.daemon(&[]);
    f.action_ok(json!({"op":"setup_apply","harness":"pi"}));
    f.stop(daemon);
    let custom = f.state.join("plugins/custom-agent");
    fs::create_dir_all(&custom).unwrap();
    fs::write(
        custom.join("plugin.json"),
        json!({"id":"custom-agent","kind":"agent","displayName":"Custom Agent","command":["./run.sh"]}).to_string(),
    ).unwrap();
    script(
        &custom.join("run.sh"),
        "#!/bin/sh\nread -r request\nprintf '{\"ok\":true,\"installed\":true,\"configured\":true,\"paths\":[\"%s\"],\"cwd\":\"%s\",\"executable\":\"%s\"}\\n' \"$(pwd -P)\" \"$(pwd -P)\" \"$0\"\n",
    );
    let check_plugins = |f: &mut Fixture| {
        f.env.insert("CITO_TEST_STAGED_PLUGINS".into(), "1".into());
        let (ok, out, err) = f.command(
            std::env::current_exe().unwrap().to_str().unwrap(),
            &[
                "--exact",
                "daemon::discovery_and_setup::lifecycle_setup_repairs_deleted_builtin_loaded_from_staged_plugins",
                "--nocapture",
            ],
            "",
        );
        f.env.remove("CITO_TEST_STAGED_PLUGINS");
        assert!(ok, "{out}\n{err}");
    };
    f.env.insert(
        "CITO_BUILTIN_PLUGINS_DIR".into(),
        f.state.join("runtime/plugins").display().to_string(),
    );
    check_plugins(&mut f);
    let mut daemon = f.daemon(&[]);
    let app = |f: &Fixture, id: &str| {
        let snapshot = f.snapshot();
        let apps: Vec<_> = snapshot["settings"]["apps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["id"] == id)
            .collect();
        assert_eq!(apps.len(), 1, "descriptor must remain unique: {id}");
        apps[0].clone()
    };
    assert_eq!(app(&f, "pi")["status"], "ready");
    assert_eq!(app(&f, "custom-agent")["status"], "ready");
    // Repairing one app must not repair another app's deleted staged files.
    let codex_manifest = f.state.join("runtime/plugins/codex/plugin.json");
    fs::remove_file(&codex_manifest).unwrap();
    // Repairs must use the distribution even though the loaded descriptor is staged.
    for asset in [
        "cito-bridge.ts",
        "package.json",
        "plugin.json",
        "bin/cito-plugin-pi",
    ] {
        let staged = f.state.join("runtime/plugins/pi").join(asset);
        fs::remove_file(&staged).unwrap();
        assert_eq!(app(&f, "pi")["status"], "repair_needed", "{asset}");
        f.action_ok(json!({"op":"setup_apply","harness":"pi"}));
        assert_eq!(
            fs::read(&staged).unwrap(),
            fs::read(root().join("plugins/pi").join(asset)).unwrap()
        );
        assert_eq!(app(&f, "pi")["status"], "ready", "{asset}");
    }
    assert!(!codex_manifest.exists());
    f.action_ok(json!({"op":"setup_apply","harness":"codex"}));
    assert!(codex_manifest.exists());
    let alias = f.dir.join("staged-plugins");
    std::os::unix::fs::symlink(f.state.join("runtime/plugins"), &alias).unwrap();
    for builtin_root in [
        f.state.join("runtime/plugins"),
        alias,
        f.state.join("plugins"),
    ] {
        f.env.insert(
            "CITO_BUILTIN_PLUGINS_DIR".into(),
            builtin_root.display().to_string(),
        );
        for missing_manifest in [true, false] {
            f.stop(daemon);
            let staged = f.state.join("runtime/plugins/pi");
            if missing_manifest {
                fs::remove_file(staged.join("plugin.json")).unwrap();
            } else {
                fs::remove_dir_all(&staged).unwrap();
            }
            // A new process must rediscover the descriptor, including through an alias.
            check_plugins(&mut f);
            daemon = f.daemon(&[]);
            assert_eq!(app(&f, "pi")["status"], "ready");
            let custom_app = app(&f, "custom-agent");
            assert_eq!(custom_app["status"], "ready");
            assert_eq!(
                custom_app["paths"],
                json!([fs::canonicalize(&custom).unwrap()])
            );
            f.action_ok(json!({"op":"setup_apply","harness":"pi"}));
            for asset in ["plugin.json", "bin/cito-plugin-pi", "cito-bridge.ts"] {
                assert_eq!(
                    fs::read(staged.join(asset)).unwrap(),
                    fs::read(root().join("plugins/pi").join(asset)).unwrap()
                );
            }
            assert_eq!(app(&f, "pi")["status"], "ready");
            assert_eq!(app(&f, "pi")["configured"], true);
        }
    }
}

#[test]
fn lifecycle_setup_repairs_shims_disconnects_listeners_and_keeps_daemon_ui_alive() {
    let _lock = serial();
    let f = Fixture::new(
        json!({"default_hub":"local","hubs":{"local":{"url":"local://"},"work":{"url":"https://hub.example.com","enabled":false}}}),
    );
    let meta = f.provider("delivered_unconfirmed");
    let channel = f.state.join("plugins/channel");
    fs::create_dir_all(&channel).unwrap();
    fs::write(
        channel.join("plugin.json"),
        json!({
            "id":"channel","kind":"human-channel","scope":"hub","displayName":"Messenger",
            "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],
            "fields":[{"key":"credential","scope":"hub","type":"secret","required":true}]
        })
        .to_string(),
    )
    .unwrap();
    let daemon = f.recording_daemon();
    wait(|| f.home.join("ui-pid").exists());
    enroll(&f, meta.clone());
    f.action_ok(json!({"op":"setup_apply","harness":"pi","request_id":"app-setup"}));
    let app = |snapshot: Value| {
        snapshot["settings"]["apps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "pi")
            .unwrap()
            .clone()
    };
    assert_eq!(app(f.snapshot())["status"], "ready");
    fs::remove_dir_all(f.state.join("bin")).unwrap();
    fs::remove_dir_all(f.state.join("runtime/plugins")).unwrap();
    assert!(f.state.join("plugins/fixture/plugin.json").is_file());
    assert_eq!(app(f.snapshot())["status"], "repair_needed");
    f.action_ok(json!({"op":"setup_apply","harness":"pi"}));
    assert!(f.state.join("bin/cito-mcp").is_file());
    assert!(f.state.join("runtime/plugins/pi/plugin.json").is_file());
    assert!(app(f.snapshot())["detail"]
        .as_str()
        .unwrap()
        .contains("/reload"));
    enroll(&f, meta.clone());
    f.action_ok(json!({"op":"setup_remove_app","harness":"fixture","request_id":"app-remove"}));
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
        .is_empty());
    for (op, request) in [
        ("setup_apply", "app-setup"),
        ("setup_remove_app", "app-remove"),
    ] {
        wait(|| {
            ui_frames(&f).into_iter().any(|frame| {
                frame["op"] == "settings"
                    && frame["ok"] == true
                    && frame["source_op"] == op
                    && frame["request_id"] == request
                    && frame["settings"]["apps"].is_array()
            })
        });
    }
    enroll(&f, meta);
    f.action_ok(json!({"op":"rename_session","harness":"fixture","session_id":"session-1","title":"Backend API"}));
    let row = f.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    f.db("state.sqlite").execute(
        "INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) VALUES('local','purge-history',?,1,?,'bob','laptop','Sender','Retain until purge','2030-01-01T00:00:00.000Z',4102444800000,'STORED_PASSIVE')",
        &[row["listener_id"].clone(), row["binding_generation"].clone()],
    ).unwrap();
    f.action_ok(json!({"op":"save_hub_service","hub":"local","channel_id":"channel","values":{"credential":"saved-gateway"}}));
    let service_config = || {
        let rows = f
            .db("hub.sqlite")
            .query(
                "SELECT config_json FROM hub_plugin_services WHERE plugin_id='channel'",
                &[],
            )
            .unwrap();
        serde_json::from_str::<Value>(rows[0]["config_json"].as_str().unwrap()).unwrap()
    };
    assert_eq!(service_config()["credential"], "saved-gateway");
    let (ok, out, err) = f.command(NATIVE, &["setup", "--disconnect-all", "--json"], "");
    assert!(ok, "{out} {err}");
    assert_eq!(
        f.rows("SELECT title FROM session_custom_titles")[0]["title"],
        "Backend API"
    );
    assert_eq!(service_config()["credential"], "saved-gateway");
    assert!(!f
        .rows("SELECT * FROM local_signals WHERE signal_id='purge-history'")
        .is_empty());
    let config: Value = read_json(&f.config);
    assert!(config["hubs"].get("work").is_some());
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE harness!='human' AND status='ACTIVE'")
        .is_empty());
    assert!(!f
        .rows("SELECT * FROM local_listeners WHERE harness='human' AND status='ACTIVE'")
        .is_empty());
    assert!(!f.exited(daemon));
    let ui_pid = fs::read_to_string(f.home.join("ui-pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert!(cito_native::plugins::pid_alive(ui_pid));
    assert_eq!(http(f.port, "/health", None).0, 200);
    assert!(f.state.join("bin/cito-mcp").is_file());
    assert!(f.state.join("runtime/plugins/pi").is_dir());
    let host: Value = read_json(f.home.join(".pi/agent/settings.json"));
    assert!(
        host["packages"].is_null() || host["packages"] == json!([]),
        "{host}"
    );
    f.action_ok(json!({"op":"setup_disconnect_all","purge_data":true}));
    assert!(f.rows("SELECT * FROM session_custom_titles").is_empty());
    let config: Value = read_json(&f.config);
    assert_eq!(config["default_hub"], "local");
    assert!(config.get("defaultHub").is_none());
    assert_eq!(config["hubs"], json!({}));
    assert!(f
        .db("hub.sqlite")
        .query(
            "SELECT * FROM hub_plugin_services WHERE plugin_id='channel'",
            &[]
        )
        .unwrap()
        .is_empty());
    assert!(f.rows("SELECT * FROM local_signals").is_empty());
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE harness!='human'")
        .is_empty());
    assert!(!f
        .rows("SELECT * FROM local_listeners WHERE harness='human'")
        .is_empty());
    assert!(!f.exited(daemon));
    assert!(cito_native::plugins::pid_alive(ui_pid));
}
#[test]
fn v29_malformed_inspection_stays_unknown_and_surfaces_error() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.provider("delivered");
    fs::write(f.state.join("plugins/fixture/options.json"), "{").unwrap();
    f.recording_daemon();
    f.action(json!({"op":"open_settings"}));
    let mut settings = Value::Null;
    wait(|| {
        settings = ui_frames(&f)
            .into_iter()
            .rfind(|v| v["op"] == "settings")
            .unwrap_or(Value::Null);
        settings.is_object()
    });
    let app = settings["settings"]["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "fixture")
        .unwrap();
    assert_eq!(app["status"], "error", "{settings}");
    assert!(app["configured"].is_null());
    assert!(!app["detail"].as_str().unwrap().is_empty());
}

#[test]
fn cli_validation_dry_run_and_selective_harness_configuration() {
    let _lock = serial();
    let plugins = cito_native::plugins::LoadedPlugins {
        plugins: ["pi", "codex", "claude"]
            .into_iter()
            .map(|id| {
                cito_native::plugins::parse_manifest_dir(&root().join("plugins").join(id), true)
                    .unwrap()
            })
            .collect(),
        rejected_ids: Default::default(),
    };
    for id in ["pi", "codex", "claude"] {
        for flag in [format!("--{id}"), format!("--{id}=session-1")] {
            let (input, meta) = cito_native::core::cli::parse_cli(
                "rename",
                &["Backend API".into(), flag.clone()],
                &plugins,
            )
            .unwrap();
            assert_eq!(input["title"], "Backend API");
            assert_eq!(meta["expected_harness"], id);
            assert_eq!(
                meta["expected_session_id"],
                if flag.contains('=') {
                    json!("session-1")
                } else {
                    Value::Null
                }
            );
        }
    }
    let f = Fixture::new(json!({}));
    let legacy = f.state.join("plugins/pi");
    fs::create_dir_all(&legacy).unwrap();
    let manifest = legacy_manifest("pi");
    fs::write(legacy.join("plugin.json"), &manifest).unwrap();
    fs::write(legacy.join("keep"), "legacy payload").unwrap();
    let before = fs::read(&f.config).unwrap();
    for (args, reason) in [
        (vec!["setup", "--bogus"], "Unknown flag"),
        (vec!["setup", "--enable", "missing"], "Unknown harness"),
        (
            vec!["setup", "--hub", "work", "--url", "invalid"],
            "Enter a Hub URL using HTTP or HTTPS",
        ),
        (vec!["setup", "--enable-hub", "missing"], "Unknown hub"),
        (
            vec!["setup", "--share-port", "invalid"],
            "port must be an integer",
        ),
        (vec!["setup", "--invite"], "Missing value"),
        (
            vec!["setup", "--invite", "invalid", "--disconnect-all"],
            "must be used separately",
        ),
    ] {
        let (ok, out, err) = f.command(NATIVE, &args, "");
        assert!(!ok, "{args:?}: {out} {err}");
        assert!(err.contains(reason), "{args:?}: {err}");
        assert_eq!(
            fs::read_to_string(legacy.join("plugin.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read_to_string(legacy.join("keep")).unwrap(),
            "legacy payload"
        );
        assert_eq!(fs::read(&f.config).unwrap(), before);
    }
    for args in [
        vec![
            "setup",
            "--enable",
            "pi",
            "--dry-run",
            "--json",
            "--no-login-item",
        ],
        vec!["setup", "--dry-run", "--build-app", "--json"],
        vec!["setup", "--status", "--json"],
        vec!["setup", "--status", "--build-app", "--json"],
        vec!["setup", "--invite", "https://hub.example.com", "--dry-run"],
        vec!["setup", "--disconnect-all", "--purge-data", "--dry-run"],
        vec!["setup", "--print-invite", "--dry-run"],
    ] {
        let (ok, out, err) = f.command(NATIVE, &args, "");
        assert!(ok, "{args:?}: {out} {err}");
        assert_eq!(
            fs::read_to_string(legacy.join("plugin.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read_to_string(legacy.join("keep")).unwrap(),
            "legacy payload"
        );
        assert_eq!(fs::read(&f.config).unwrap(), before);
    }
    assert!(!f.home.join(".pi").exists());
    fs::write(&f.config, "{\"daemonPort\":0}").unwrap();
    let (ok, out, err) = f.command(NATIVE, &["setup", "--share-hub", "--json"], "");
    assert!(ok, "{out} {err}");
    let resulting_config: Value = read_json(&f.config);
    assert_eq!(resulting_config["daemonPort"], 0);
    assert_eq!(resulting_config["host_hub"]["enabled"], true);
    assert_eq!(
        fs::read_to_string(legacy.join("plugin.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(legacy.join("keep")).unwrap(),
        "legacy payload"
    );
    fs::write(&f.config, &before).unwrap();
    assert!(
        f.command(
            NATIVE,
            &["setup", "--enable", "pi", "--json", "--no-login-item"],
            ""
        )
        .0
    );
    assert!(!legacy.exists());
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
        format!("cito v{}", env!("CARGO_PKG_VERSION"))
    );
    let before_invalid = fs::read(&f.config).unwrap();
    for (args, reason) in [
        (
            vec!["listen", "--session", "orphan"],
            "--session requires --harness",
        ),
        (vec!["listen", "--harness", "missing"], "Unknown harness"),
    ] {
        let (ok, out, err) = f.command(NATIVE, &args, "");
        assert!(!ok && err.contains(reason), "{args:?}: {out} {err}");
        assert_eq!(fs::read(&f.config).unwrap(), before_invalid);
    }
}
