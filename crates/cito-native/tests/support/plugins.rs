use super::support::*;
use std::os::unix::net::UnixListener;

#[test]
fn live_bridge_rejections_survive_identification_and_wake() {
    use std::io::{BufRead, BufReader, Write};
    let _lock = serial();
    let f = Fixture::new(json!({}));
    fs::create_dir_all(f.state.join("sockets")).unwrap();
    for (id, bin) in [("pi", PI), ("claude", CLAUDE)] {
        let path = f
            .state
            .join(format!("sockets/{id}-pid-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
            let mut count = 0;
            while count < if id == "pi" { 5 } else { 2 } && std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                            .unwrap();
                        let mut line = String::new();
                        BufReader::new(stream.try_clone().unwrap())
                            .read_line(&mut line)
                            .unwrap();
                        let response = if count == 4 {
                            json!({"ok":true})
                        } else {
                            json!({"ok":false,"error_code":"BRIDGE_REJECTED","message":"Session is unavailable"})
                        };
                        writeln!(stream, "{response}").unwrap();
                        count += 1;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
            count
        });
        let identify=f.plugin(bin,json!({"op":"verify","endpoint_pid":std::process::id(),"session_id":"session","cached_caller":{},"meta":{}}));
        let wake=f.plugin(bin,json!({"op":"wake","endpoint_pid":std::process::id(),"session_id":"session","text":"hello"}));
        if id == "pi" {
            assert_eq!(
                identify["error_code"], "PI_BRIDGE_UNAVAILABLE",
                "{identify}"
            );
            let resolve = f.plugin(
                bin,
                json!({"op":"resolve_caller","endpoint_pid":std::process::id()}),
            );
            assert_eq!(resolve["error_code"], "PI_BRIDGE_UNAVAILABLE", "{resolve}");
            assert!(resolve["message"]
                .as_str()
                .unwrap()
                .contains("Session is unavailable"));
            let probe = f.plugin(bin, json!({"op":"probe","endpoint_pid":std::process::id()}));
            assert_eq!(probe["status"], "transient_error", "{probe}");
            assert!(probe["reason"]
                .as_str()
                .unwrap()
                .contains("Session is unavailable"));
            let closed = f.plugin(bin, json!({"op":"probe","endpoint_pid":std::process::id()}));
            assert_eq!(closed["status"], "closed_definitive", "{closed}");
        }
        assert_eq!(server.join().unwrap(), if id == "pi" { 5 } else { 2 });
        fs::remove_file(path).unwrap();
        assert_eq!(identify["ok"], false, "{id}: {identify}");
        assert!(
            identify.to_string().contains("Session is unavailable"),
            "{id}: {identify}"
        );
        assert_eq!(wake["outcome"], "driver_unavailable", "{id}: {wake}");
        assert!(
            wake["reason"]
                .as_str()
                .unwrap()
                .contains("Session is unavailable"),
            "{id}: {wake}"
        );
    }
}
#[test]
fn socket_discovery_retains_verified_sessions_across_750ms_timeouts() {
    use std::io::{BufRead, BufReader, Write};
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    fs::create_dir_all(f.state.join("sockets")).unwrap();
    for (id, bin) in [("pi", PI), ("claude", CLAUDE)] {
        let script = f.dir.join(format!("{id}.mjs"));
        fs::write(&script, "console.log('ready'); setInterval(()=>{},1000);").unwrap();
        let process = f.spawn("node", &[script.to_str().unwrap()], &[]);
        wait(|| f.output(process, false).contains("ready"));
        let pid = f.pid(process);
        if id == "claude" {
            f.env.insert("CITO_ENDPOINT_PID".into(), pid.to_string());
            f.plugin(
                HOOK,
                json!({"hook_event_name":"SessionStart","session_id":"live-session","cwd":"/repo"}),
            );
            f.env.remove("CITO_ENDPOINT_PID");
        }
        let path = f.state.join(format!("sockets/{id}-pid-{pid}.sock"));
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            for turn in 0..3 {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(pair) => break pair,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(std::time::Duration::from_millis(2))
                        }
                        Err(e) => panic!("Socket fixture: {e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                match turn {
                    0 => writeln!(stream,"{}",json!({"ok":true,"session_id":"live-session","cwd":"/repo","session_name":"Live work","updated_at_ms":1000})).unwrap(),
                    1 => { release_rx.recv_timeout(std::time::Duration::from_secs(4)).unwrap(); },
                    _ => writeln!(stream,"{}",json!({"ok":false,"message":"Session closed"})).unwrap(),
                }
            }
        });
        let first = f.plugin(bin, json!({"op":"discover_sessions"}));
        assert_eq!(
            first["sessions"][0]["session_id"], "live-session",
            "{id}: {first}"
        );
        let slow = f.plugin(bin, json!({"op":"discover_sessions"}));
        release_tx.send(()).unwrap();
        assert_eq!(slow["ok"], true, "{id}: {slow}");
        assert_eq!(
            slow["sessions"][0]["session_id"], "live-session",
            "{id}: {slow}"
        );
        assert_eq!(slow["sessions"][0]["updated_at_ms"], 1000);
        assert_ne!(slow["sessions"][0]["needs_reload"], true);
        let rejected = f.plugin(bin, json!({"op":"discover_sessions"}));
        server.join().unwrap();
        assert_eq!(
            rejected["sessions"],
            json!([]),
            "A responding rejection must supersede cached identity: {rejected}"
        );
        fs::remove_file(path).unwrap();
        f.stop(process);
    }
}

#[test]
fn u2_live_process_discovery_is_configured_owned_and_collision_safe() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let script = f.dir.join("pi.mjs");
    fs::write(
        &script,
        "console.log('ready'); setInterval(() => {}, 1000);",
    )
    .unwrap();
    let first = f.spawn("node", &[script.to_str().unwrap()], &[]);
    let second = f.spawn("node", &[script.to_str().unwrap()], &[]);
    wait(|| f.output(first, false).contains("ready") && f.output(second, false).contains("ready"));
    let cwd = fs::canonicalize(&f.dir).unwrap();
    let encoded = format!(
        "--{}--",
        cwd.to_str()
            .unwrap()
            .trim_start_matches('/')
            .replace('/', "-")
    );
    let sessions = f.home.join(".pi/agent/sessions").join(encoded);
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("latest.jsonl"),
        "{\"type\":\"session\",\"id\":\"live-session\",\"name\":\"Live work\"}\n",
    )
    .unwrap();
    let discover = |f: &Fixture| f.plugin(PI, json!({"op":"discover_sessions"}));
    assert_eq!(f.plugin(PI, json!({"op":"setup_apply"}))["ok"], true);
    assert_eq!(
        discover(&f)["sessions"],
        json!([]),
        "fixture disables ambient process discovery"
    );
    f.env.insert("CITO_PI_PROCESS_DISCOVERY".into(), "1".into());
    let found = discover(&f);
    let own: Vec<_> = found["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| {
            [u64::from(f.pid(first)), u64::from(f.pid(second))]
                .contains(&v["endpoint_pid"].as_u64().unwrap())
        })
        .collect();
    assert_eq!(own.len(), 2, "{found}");
    assert_ne!(own[0]["session_id"], own[1]["session_id"]);
    assert!(own
        .iter()
        .any(|v| v["session_id"] == "live-session" && v["title"] == "Live work"));
    assert!(own
        .iter()
        .any(|v| v["session_id"] == format!("pid-{}", v["endpoint_pid"])));
    for row in own {
        let resolved = f.plugin(
            PI,
            json!({"op":"resolve_caller","endpoint_pid":row["endpoint_pid"],"cwd":cwd}),
        );
        assert_eq!(resolved["ok"], true, "{resolved}");
        // Caller resolution scans the same complete candidate set before selecting a PID.
        assert_eq!(resolved["caller"]["session_id"], row["session_id"]);
        let probe = f.plugin(
            PI,
            json!({"op":"probe","endpoint_pid":row["endpoint_pid"],"session_id":row["session_id"]}),
        );
        assert_eq!(probe["status"], "alive", "{probe}");
    }
    // One failed bridge cannot discard healthy endpoint discovery. Probes still
    // report that individual endpoint's transport failure as transient.
    fs::create_dir_all(f.state.join("sockets")).unwrap();
    for (id, bin) in [("pi", PI), ("claude", CLAUDE)] {
        let live = f
            .state
            .join(format!("sockets/{id}-pid-{}.sock", f.pid(first)));
        drop(UnixListener::bind(&live).unwrap());
        let failed = f.plugin(bin, json!({"op":"discover_sessions"}));
        assert_eq!(failed["ok"], true, "{id}: {failed}");
        assert!(failed["sessions"].is_array());
        if id == "pi" {
            assert!(failed["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["endpoint_pid"] == f.pid(second)));
        }
        if id == "pi" {
            f.env.insert("CITO_PI_REQUIRE_BRIDGE".into(), "1".into());
            let probe = f.plugin(
                PI,
                json!({"op":"probe","endpoint_pid":f.pid(first),"session_id":"live-session"}),
            );
            assert_eq!(probe["status"], "transient_error", "{probe}");
            f.env.remove("CITO_PI_REQUIRE_BRIDGE");
        }
        fs::remove_file(&live).unwrap();
        let listener = UnixListener::bind(&live).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                            .unwrap();
                        let mut line = String::new();
                        BufReader::new(stream.try_clone().unwrap())
                            .read_line(&mut line)
                            .unwrap();
                        assert_eq!(
                            serde_json::from_str::<Value>(&line).unwrap()["op"],
                            "identify"
                        );
                        stream.write_all(b"{\"ok\":false,\"error\":\"DELIVERY_FAILED\",\"message\":\"Cannot persist bridge identity\"}\n").unwrap();
                        break;
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(5))
                    }
                    Err(error) => panic!("Bridge fixture accept failed: {error}"),
                }
            }
        });
        let rejected = f.plugin(bin, json!({"op":"discover_sessions"}));
        server.join().unwrap();
        assert_eq!(rejected["ok"], true, "{id}: {rejected}");
        assert!(rejected["sessions"].is_array());
        if id == "pi" {
            assert!(rejected["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["endpoint_pid"] == f.pid(second)));
        }
        fs::remove_file(live).unwrap();
        let dead = f.state.join(format!("sockets/{id}-pid-{}.sock", i32::MAX));
        drop(UnixListener::bind(&dead).unwrap());
        let found = f.plugin(bin, json!({"op":"discover_sessions"}));
        assert_eq!(found["ok"], true, "{id}: {found}");
        fs::remove_file(dead).unwrap();
    }
    fs::remove_dir(f.state.join("sockets")).unwrap();
    fs::write(f.state.join("sockets"), "not a directory").unwrap();
    for bin in [PI, CLAUDE] {
        let failed = f.plugin(bin, json!({"op":"discover_sessions"}));
        assert_eq!(failed["ok"], false, "{failed}");
        assert!(failed["sessions"].is_null());
    }
    fs::remove_file(f.state.join("sockets")).unwrap();
    let settings_path = f.home.join(".pi/agent/settings.json");
    let original_settings = fs::read(&settings_path).unwrap();
    fs::write(&settings_path, "{").unwrap();
    assert_eq!(
        discover(&f)["ok"],
        false,
        "Setup inspection errors must not become empty discovery"
    );
    fs::write(settings_path, original_settings).unwrap();
    // Interactive discovery excludes background invocations, independently of caller resolution.
    let flags: &[&[&str]] = &[
        &["--mode", "rpc"],
        &["--mode", "json"],
        &["--print"],
        &["-p"],
        &["--no-session"],
        &["--version"],
        &["--help"],
    ];
    let mut background = Vec::new();
    for flags in flags {
        let mut args = vec![script.to_str().unwrap()];
        args.extend_from_slice(flags);
        let index = f.spawn("node", &args, &[]);
        wait(|| f.output(index, false).contains("ready"));
        background.push(index);
    }
    let interactive = discover(&f);
    assert!(interactive["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| !background
            .iter()
            .any(|index| r["endpoint_pid"] == f.pid(*index))));
    assert_eq!(
        f.plugin(
            PI,
            json!({"op":"resolve_caller","endpoint_pid":f.pid(background[0]),"cwd":cwd})
        )["ok"],
        true
    );
    f.env.remove("CITO_PI_ALLOW_HEADLESS_DISCOVERY");
    let no_tty = discover(&f);
    assert!(no_tty["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["endpoint_pid"] != f.pid(first) && r["endpoint_pid"] != f.pid(second)));
    assert_eq!(
        f.plugin(
            PI,
            json!({"op":"resolve_caller","endpoint_pid":f.pid(first),"cwd":cwd})
        )["ok"],
        true
    );
    f.env
        .insert("CITO_PI_ALLOW_HEADLESS_DISCOVERY".into(), "1".into());
    // Production readiness gating is distinct from the explicit process-only test mode.
    f.env.insert("CITO_PI_REQUIRE_BRIDGE".into(), "1".into());
    let gated = discover(&f);
    for p in [f.pid(first), f.pid(second)] {
        let row = gated["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["endpoint_pid"] == p)
            .unwrap();
        assert_eq!(row["needs_reload"], true);
        assert_eq!(
            row["reload_hint"],
            "Run /reload in Pi to enable 1-click listen"
        );
        assert_eq!(
            f.plugin(PI, json!({"op":"resolve_caller","endpoint_pid":p}))["ok"],
            false
        );
        assert_eq!(
            f.plugin(
                PI,
                json!({"op":"probe","endpoint_pid":p,"session_id":row["session_id"]})
            )["status"],
            "closed_definitive"
        );
        assert_eq!(f.plugin(PI,json!({"op":"update_badge","endpoint_pid":p,"session_id":row["session_id"],"badge":null}))["ok"],true);
    }
    let custom = f.dir.join("first/pi");
    assert_eq!(
        f.plugin(PI, json!({"op":"setup_apply","plugin_dir":custom}))["ok"],
        true
    );
    assert!(
        discover(&f)["sessions"].as_array().unwrap().is_empty(),
        "A different registered package must not match the default source"
    );
    let custom_discovery = f.plugin(PI, json!({"op":"discover_sessions","plugin_dir":custom}));
    for pid in [f.pid(first), f.pid(second)] {
        let row = custom_discovery["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["endpoint_pid"] == pid)
            .unwrap();
        assert_eq!(row["needs_reload"], true, "{custom_discovery}");
    }
    assert_eq!(
        f.plugin(PI, json!({"op":"setup_remove","plugin_dir":custom}))["ok"],
        true
    );
    assert_eq!(f.plugin(PI, json!({"op":"setup_apply"}))["ok"], true);
    f.env.remove("CITO_PI_REQUIRE_BRIDGE");
    // Socket-backed identities take precedence, including when resolving a process-only PID.
    let endpoint_script = f.dir.join("endpoint.cjs");
    fs::write(&endpoint_script, include_str!("endpoint.cjs")).unwrap();
    let socket_owner = f.spawn(
        "node",
        &[endpoint_script.to_str().unwrap()],
        &[("FIXTURE_HARNESS", "pi")],
    );
    wait(|| f.frames(socket_owner).iter().any(|v| v["op"] == "ready"));
    fs::write(
        sessions.join("latest.jsonl"),
        "{\"type\":\"session\",\"id\":\"session-a\"}\n",
    )
    .unwrap();
    for process in [first, second] {
        let response = f.plugin(
            PI,
            json!({"op":"resolve_caller","endpoint_pid":f.pid(process)}),
        );
        assert_eq!(
            response["caller"]["session_id"],
            format!("pid-{}", f.pid(process)),
            "{response}"
        );
    }
    f.stop(socket_owner);
    fs::remove_file(sessions.join("latest.jsonl")).unwrap();
    let response = f.plugin(
        PI,
        json!({"op":"resolve_caller","endpoint_pid":f.pid(first),"cwd":cwd}),
    );
    assert_eq!(
        response["caller"]["session_id"],
        format!("pid-{}", f.pid(first))
    );
    assert_eq!(f.plugin(PI, json!({"op":"setup_remove"}))["ok"], true);
    assert_eq!(discover(&f)["sessions"], json!([]));
    assert_eq!(
        f.plugin(
            PI,
            json!({"op":"probe","endpoint_pid":f.pid(first),"session_id":"live-session"})
        )["status"],
        "closed_definitive"
    );
}

#[test]
fn cached_fingerprint_does_not_authorize_an_unrelated_process_command() {
    let _lock = serial();
    for (id, bin) in [("pi", PI), ("codex", CODEX), ("claude", CLAUDE)] {
        let f = Fixture::new(json!({}));
        let path = f.dir.join("unrelated.cjs");
        fs::write(&path, include_str!("endpoint.cjs")).unwrap();
        let p = f.spawn(
            "node",
            &[path.to_str().unwrap()],
            &[("FIXTURE_HARNESS", id)],
        );
        wait(|| f.frames(p).iter().any(|v| v["op"] == "ready"));
        if id == "codex" {
            let db = cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite"))
                .unwrap();
            db.execute("CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,updated_at INTEGER);INSERT INTO threads VALUES('session-a','/repo',0,1)",&[]).unwrap();
        }
        let mut request = json!({"op":"resolve_caller","endpoint_pid":f.pid(p),"cwd":"/repo","require_open":true});
        if id == "codex" {
            request["meta"] = json!({"threadId":"session-a"});
        }
        let response = f.plugin(bin, request.clone());
        assert_eq!(response["ok"], true, "{response}");
        f.send(p, json!({"op":"offline"}));
        wait(|| f.frames(p).iter().any(|v| v["op"] == "offline"));
        if id == "codex" {
            fs::remove_file(f.home.join(".codex/state_5.sqlite")).unwrap();
        }
        request["require_open"] = json!(false);
        request["cached_caller"] = response["caller"].clone();
        request["meta"] = json!({"expected_harness":id,"expected_session_id":"session-a"});
        let denied = f.plugin(bin, request);
        assert_eq!(denied["error_code"], "BINDING_REQUIRED");
        assert!(denied["message"]
            .as_str()
            .unwrap()
            .contains("process command does not match"));
    }
}
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
};

fn socket(path: &std::path::Path, request: Value) -> Value {
    let mut stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
#[test]
fn pi_badge_clear_converges_for_missing_or_departed_bridges() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let endpoint = f.spawn("/bin/cat", &[], &[]);
    let path = f
        .state
        .join(format!("sockets/pi-pid-{}.sock", f.pid(endpoint)));
    let update = |badge: Value| {
        f.plugin(
            PI,
            json!({"op":"update_badge","endpoint_pid":f.pid(endpoint),"session_id":"session-a","badge":badge}),
        )
    };

    // A process discovered before its bridge starts has nothing to clear.
    assert!(!path.exists());
    assert_eq!(update(Value::Null), json!({"ok":true}));
    assert_eq!(update(json!("badge"))["ok"], false);

    fs::create_dir_all(path.parent().unwrap()).unwrap();
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    // A live endpoint's transport failure must remain retryable.
    assert_eq!(update(Value::Null)["ok"], false);
    assert!(path.exists());

    f.stop(endpoint);
    assert_eq!(update(json!("badge"))["ok"], false);
    assert!(path.exists(), "badge writes must not clean up the socket");
    assert_eq!(update(Value::Null), json!({"ok":true}));
    assert!(
        !path.exists(),
        "a departed endpoint's stale socket is removed"
    );
    assert_eq!(update(Value::Null), json!({"ok":true}));
}

#[test]
fn pi_package_setup_preserves_unrelated_settings_and_removes_legacy_mcp() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let pi = f.home.join(".pi/agent");
    fs::create_dir_all(pi.join("extensions")).unwrap();
    fs::create_dir_all(pi.join("mcp")).unwrap();
    let package = f.state.join("runtime/plugins/pi");
    let before = json!({"theme":"dark","packages":["npm:@tefx/cito","npm:other",f.state.join("plugins/pi")]});
    fs::write(pi.join("settings.json"), before.to_string()).unwrap();
    fs::write(pi.join("extensions/cito-bridge.ts"), "legacy").unwrap();
    for p in ["mcp.json", "mcp/global.mcp.json"] {
        fs::write(pi.join(p),json!({"mcpServers":{"cito":{"command":"old"},"other":{"command":"keep"}},"extra":true}).to_string()).unwrap();
    }
    assert_eq!(
        f.plugin(PI, json!({"op":"setup_apply","dry_run":true}))["ok"],
        true
    );
    assert_eq!(
        serde_json::from_str::<Value>(&fs::read_to_string(pi.join("settings.json")).unwrap())
            .unwrap(),
        before
    );
    assert_eq!(f.plugin(PI, json!({"op":"setup_apply"}))["ok"], true);
    let v: Value =
        serde_json::from_str(&fs::read_to_string(pi.join("settings.json")).unwrap()).unwrap();
    assert_eq!(v, json!({"theme":"dark","packages":["npm:other",package]}));
    assert!(!pi.join("extensions/cito-bridge.ts").exists());
    for p in ["mcp.json", "mcp/global.mcp.json"] {
        let v: Value = serde_json::from_str(&fs::read_to_string(pi.join(p)).unwrap()).unwrap();
        assert_eq!(
            v,
            json!({"mcpServers":{"other":{"command":"keep"}},"extra":true})
        );
    }
    assert_eq!(
        f.plugin(PI, json!({"op":"setup_inspect"}))["configured"],
        true
    );
    assert!(f.plugin(PI, json!({"op":"setup_apply"}))["actions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["status"] == "unchanged"));
    assert_eq!(f.plugin(PI, json!({"op":"setup_remove"}))["ok"], true);
    let v: Value =
        serde_json::from_str(&fs::read_to_string(pi.join("settings.json")).unwrap()).unwrap();
    assert_eq!(v, json!({"theme":"dark","packages":["npm:other"]}));
}
#[test]
fn setup_uses_selected_plugin_sources_and_rejects_stale_registrations() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let package = f.home.join("custom/pi");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("package.json"), "{}").unwrap();
    let settings = f.home.join(".pi/agent/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let legacy = f.state.join("plugins/pi");
    let runtime = f.state.join("runtime/plugins/pi");
    for old in [&legacy, &runtime] {
        fs::write(&settings, json!({"packages":[old,"npm:other"]}).to_string()).unwrap();
        assert_eq!(
            f.plugin(PI, json!({"op":"setup_inspect"}))["configured"],
            true
        );
        assert_eq!(
            f.plugin(PI, json!({"op":"setup_inspect","plugin_dir":null}))["configured"],
            true
        );
        assert_eq!(
            f.plugin(PI, json!({"op":"setup_inspect","plugin_dir":"~/custom/pi"}))["configured"],
            false
        );
    }
    assert_eq!(
        f.plugin(PI, json!({"op":"setup_apply","plugin_dir":"~/custom/pi"}))["ok"],
        true
    );
    let configured: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(configured["packages"], json!(["npm:other", package]));
    assert_eq!(
        f.plugin(PI, json!({"op":"setup_inspect","plugin_dir":"~/custom/pi"}))["configured"],
        true
    );
    fs::remove_file(package.join("package.json")).unwrap();
    assert_eq!(
        f.plugin(PI, json!({"op":"setup_inspect","plugin_dir":"~/custom/pi"}))["configured"],
        true,
        "Missing staged assets must not erase host registration"
    );
    assert_eq!(
        f.plugin(PI, json!({"op":"setup_remove","plugin_dir":"~/custom/pi"}))["ok"],
        true
    );
    let configured: Value = serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
    assert_eq!(configured["packages"], json!(["npm:other"]));

    let hook = f.home.join("custom/claude/bin/cito-claude-hook");
    script(&hook, "#!/bin/sh\nexit 0\n");
    assert_eq!(
        f.plugin(
            CLAUDE,
            json!({"op":"setup_apply","plugin_dir":"~/custom/claude"})
        )["ok"],
        true
    );
    let inspect = || {
        f.plugin(
            CLAUDE,
            json!({"op":"setup_inspect","plugin_dir":"~/custom/claude"}),
        )["configured"]
            .clone()
    };
    assert_eq!(inspect(), true);
    let settings = f.home.join(".claude/settings.json");
    let original: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    for event in ["Stop", "SessionStart", "UserPromptSubmit"] {
        assert_eq!(
            original["hooks"][event][0]["hooks"][0]["command"],
            json!(hook)
        );
        let mut stale = original.clone();
        stale["hooks"][event][0]["hooks"][0]["command"] =
            json!(f.state.join("plugins/claude/bin/cito-claude-hook"));
        fs::write(&settings, stale.to_string()).unwrap();
        assert_eq!(inspect(), false, "{event} must reference the selected hook");
    }
    fs::write(&settings, original.to_string()).unwrap();
    fs::remove_file(&hook).unwrap();
    assert_eq!(
        inspect(),
        true,
        "Missing staged assets must not erase host registration"
    );
    assert_eq!(
        f.plugin(
            CLAUDE,
            json!({"op":"setup_remove","plugin_dir":"~/custom/claude"})
        )["ok"],
        true
    );
    let removed: Value = serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
    assert!(removed.get("hooks").is_none());
}

#[test]
fn codex_multiline_toml_and_complete_subtree_removal() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = f.home.join(".codex/config.toml");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let prefix="model = \"default\"\nitems = [\n  \"one\", # comment\n  \"two\",\n]\nprompt = \"\"\"\n[mcp_servers.cito.fake]\nThis is a multiline string.\n\"\"\"\nliteral = '''\n[still.not.a.table]\n'''\n";
    fs::write(&p,format!("{prefix}[mcp_servers.cito]\ncommand = \"old\"\n[mcp_servers.cito.env]\nKEEP = \"old\"\n[mcp_servers.other]\ncommand = \"other\"\n")).unwrap();
    let original = fs::read(&p).unwrap();
    let conflict = f.plugin(CODEX, json!({"op":"setup_apply"}));
    assert_eq!(conflict["actions"][0]["status"], "conflict");
    assert_eq!(fs::read(&p).unwrap(), original);
    assert_eq!(
        f.plugin(CODEX, json!({"op":"setup_apply","force":true}))["ok"],
        true
    );
    assert_eq!(
        f.plugin(CODEX, json!({"op":"setup_inspect"}))["configured"],
        true
    );
    assert!(fs::read_to_string(&p).unwrap().contains(prefix));
    assert_eq!(f.plugin(CODEX, json!({"op":"setup_remove"}))["ok"], true);
    let text = fs::read_to_string(&p).unwrap();
    assert!(text.contains(prefix));
    assert!(!text.contains("KEEP ="));
    assert!(text.contains("[mcp_servers.other]"));
    for command in ["cito-mcp", "/opt/cito/bin/cito-mcp"] {
        for enabled in [true, false] {
            fs::write(&p,format!("[mcp_servers.cito]\ncommand = {command:?}\nenabled = {enabled}\n[mcp_servers.cito.env]\nKEEP = \"retained\"\n")).unwrap();
            assert_eq!(
                f.plugin(CODEX, json!({"op":"setup_inspect"}))["configured"],
                enabled
            );
            assert_eq!(f.plugin(CODEX, json!({"op":"setup_apply"}))["ok"], true);
            assert_eq!(
                f.plugin(CODEX, json!({"op":"setup_inspect"}))["configured"],
                true
            );
            assert!(fs::read_to_string(&p)
                .unwrap()
                .contains("KEEP = \"retained\""));
            assert_eq!(f.plugin(CODEX, json!({"op":"setup_remove"}))["ok"], true);
            assert!(!fs::read_to_string(&p).unwrap().contains("mcp_servers"));
        }
    }
}
#[test]
fn codex_activity_is_durable_unbounded_and_wal_errors_reach_probe() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let home = f.dir.join("source #?%");
    fs::create_dir_all(&home).unwrap();
    f.env.insert(
        "CITO_CODEX_HOME".into(),
        home.to_string_lossy().into_owned(),
    );
    assert_eq!(f.plugin(CODEX, json!({"op":"setup_apply"}))["ok"], true);
    let app = f.dir.join("codex.mjs");
    fs::write(&app, "console.log('ready'); setInterval(()=>{},1000);").unwrap();
    let process = f.spawn("node", &[app.to_str().unwrap()], &[]);
    wait(|| f.output(process, false).contains("ready"));
    let source = f.dir.join("source.sqlite");
    let db = cito_native::database::Database::open(&source).unwrap();
    db.execute("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,recency_at_ms INTEGER,rollout_path TEXT); PRAGMA wal_checkpoint(TRUNCATE)",&[]).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let rollout = f.dir.join("rollout.jsonl");
    fs::write(&rollout,"{\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"payload\":{\"type\":\"user_message\",\"message\":\"User work\"}}\n").unwrap();
    for i in 0..12 {
        db.execute(
            "INSERT INTO threads VALUES(?,'/repo',0,?,NULL)",
            &[json!(format!("recent-{i}")), json!(now + i)],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO threads VALUES('thread','/repo',0,?,?)",
        &[json!(now), json!(rollout)],
    )
    .unwrap();
    let path = home.join("state_5.sqlite");
    fs::copy(&source, &path).unwrap();
    fs::copy(
        source.with_extension("sqlite-wal"),
        path.with_extension("sqlite-wal"),
    )
    .unwrap();
    assert!(!path.with_extension("sqlite-shm").exists());
    let discover = |f: &Fixture| f.plugin(CODEX, json!({"op":"discover_sessions"}));
    let first = discover(&f);
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["sessions"].as_array().unwrap().len(), 13);
    let activity = |v: &Value| {
        v["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["session_id"] == "thread")
            .unwrap()["activity"]
            .clone()
    };
    let before = activity(&first);
    assert_eq!(before["kind"], "user_turn");
    assert_eq!(before["at_ms"], 1_767_225_600_000u64);
    assert_eq!(
        activity(&discover(&f)),
        before,
        "Provider restarts and refreshes preserve source revisions"
    );
    let binary = f.dir.join("queue");
    script(&binary, "#!/bin/sh\nexit 0\n");
    f.env.insert(
        "CITO_CODEX_BIN".into(),
        binary.to_string_lossy().into_owned(),
    );
    let wake = "Cito · Incoming work";
    let queued = f.plugin(
        CODEX,
        json!({"op":"wake","session_id":"thread","text":wake,"checkpoint_only":true}),
    );
    assert_eq!(queued["queue_written"], true, "{queued}");
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(&rollout).unwrap();
    writeln!(file,"{}",json!({"timestamp":"2026-01-01T00:01:00.000Z","payload":{"type":"user_message","message":wake}})).unwrap();
    assert_eq!(
        activity(&discover(&f)),
        before,
        "Cito effects cannot renew qualifying user activity"
    );
    writeln!(file,"{}",json!({"timestamp":"2026-01-01T00:02:00.000Z","payload":{"type":"user_message","message":"Next user turn"}})).unwrap();
    // Tool output after a user turn can exceed a fixed tail slice. It must not
    // hide the qualifying turn or become new activity on subsequent refreshes.
    writeln!(file,"{}",json!({"timestamp":"2026-01-01T00:03:00.000Z","payload":{"type":"tool_output","text":"x".repeat(300_000)}})).unwrap();
    let target = cito_native::database::Database::open(&path).unwrap();
    target
        .execute(
            "UPDATE threads SET recency_at_ms=? WHERE id='thread'",
            &[json!(now + 1)],
        )
        .unwrap();
    let after = activity(&discover(&f));
    assert!(after["revision"].as_u64() > before["revision"].as_u64());
    assert_eq!(after["at_ms"], 1_767_225_720_000u64);
    target
        .execute(
            "UPDATE threads SET recency_at_ms=? WHERE id='thread'",
            &[json!(now - 10)],
        )
        .unwrap();
    assert_eq!(
        activity(&discover(&f))["revision"],
        after["revision"],
        "Durable source high-water revisions cannot regress after a provider timestamp rollback"
    );
    assert_eq!(activity(&discover(&f))["at_ms"], after["at_ms"]);
    drop(file);
    target
        .execute("UPDATE threads SET archived=1 WHERE id='thread'", &[])
        .unwrap();
    assert_eq!(
        f.plugin(CODEX, json!({"op":"probe","session_id":"thread"}))["status"],
        "closed_definitive"
    );
    target.execute("ALTER TABLE threads RENAME TO saved_threads; CREATE VIEW threads AS SELECT id,cwd,missing_function(archived) AS archived FROM saved_threads",&[]).unwrap();
    let rejected = f.plugin(CODEX, json!({"op":"probe","session_id":"thread"}));
    assert_eq!(
        rejected["ok"], false,
        "An archived lookup error cannot become alive: {rejected}"
    );
    assert!(rejected["status"].is_null());
    let text = rejected.to_string();
    assert!(!text.contains("missing_function") && !text.contains("SQLite"));
}
#[test]
fn codex_no_rollout_effect_is_delivery_bound_and_new_user_revision_recovers() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let source =
        cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite")).unwrap();
    source
        .execute(
            "CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,recency_at_ms INTEGER)",
            &[],
        )
        .unwrap();
    let now = cito_native::plugins::now_ms();
    source
        .execute(
            "INSERT INTO threads VALUES('thread','/repo',0,?)",
            &[json!(now)],
        )
        .unwrap();
    // A live, fixture-owned process stands in for the provider.
    let app = f.dir.join("codex.mjs");
    fs::write(&app, "console.log('ready'); setInterval(()=>{},1000);").unwrap();
    let app = f.spawn("node", &[app.to_str().unwrap()], &[]);
    wait(|| f.output(app, false).contains("ready"));
    let activity = |f: &Fixture| {
        f.plugin(
            CODEX,
            json!({"op":"discover_sessions","endpoint_pid":f.pid(app)}),
        )["sessions"][0]["activity"]
            .clone()
    };
    f.env.insert(
        "CITO_CODEX_BIN".into(),
        f.dir.join("missing").display().to_string(),
    );
    let failed = f.plugin(
        CODEX,
        json!({"op":"wake","session_id":"thread","text":"Wake","checkpoint_only":true}),
    );
    assert_eq!(failed["outcome"], "driver_unavailable", "{failed}");
    assert_eq!(activity(&f)["kind"], "unknown");
    let binary = f.dir.join("queue");
    script(&binary, "#!/bin/sh\nexit 0\n");
    f.env
        .insert("CITO_CODEX_BIN".into(), binary.display().to_string());
    let sent = f.plugin(
        CODEX,
        json!({"op":"wake","session_id":"thread","text":"Wake","checkpoint_only":true}),
    );
    assert_eq!(sent["queue_confirmed"], true, "{sent}");
    let effect = activity(&f);
    assert_eq!(effect["kind"], "cito_effect");
    assert_eq!(activity(&f), effect, "Refresh cannot invent activity");
    source
        .execute("UPDATE threads SET recency_at_ms=?", &[json!(now + 1)])
        .unwrap();
    let user = activity(&f);
    assert_eq!(user["kind"], "user_turn", "{user}");
    assert_eq!(user["revision"], now + 1);
    assert_eq!(activity(&f), user);
    // Fail attribution after insertion, rather than preventing the new in-flight
    // guard from being established before the queue command starts.
    script(
        &binary,
        "#!/bin/sh\nrm -rf \"$CITO_STATE_DIR/activity\"\nprintf broken > \"$CITO_STATE_DIR/activity\"\nexit 0\n",
    );
    let confirmed = f.plugin(
        CODEX,
        json!({"op":"wake","session_id":"thread","text":"Another wake","checkpoint_only":true}),
    );
    assert_eq!(confirmed["queue_written"], true, "{confirmed}");
    assert_eq!(confirmed["queue_confirmed"], true, "{confirmed}");
    assert!(confirmed["reason"]
        .as_str()
        .unwrap()
        .contains("attribution"));
    script(
        &binary,
        "#!/bin/sh\ntouch \"$TMPDIR/queue-called\"\nexit 0\n",
    );
    let unguarded = f.plugin(
        CODEX,
        json!({"op":"wake","session_id":"thread","text":"Unguarded wake","checkpoint_only":true}),
    );
    assert_eq!(unguarded["queue_written"], false, "{unguarded}");
    assert!(
        !f.dir.join("queue-called").exists(),
        "A wake must establish its guard before insertion"
    );
}
#[test]
fn codex_inflight_wake_defers_source_checkpoint_until_delivery_settles() {
    let _lock = serial();
    for (with_rollout, prior_effect, deliver) in [
        (false, false, true),
        (false, true, true),
        (true, false, true),
        (false, false, false),
        (true, false, false),
    ] {
        let mut f = Fixture::new(json!({}));
        let source =
            cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite")).unwrap();
        source.execute("CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,recency_at_ms INTEGER,rollout_path TEXT)", &[]).unwrap();
        let now = cito_native::plugins::now_ms();
        let rollout = f.dir.join("rollout.jsonl");
        fs::write(&rollout, "{\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"payload\":{\"type\":\"user_message\",\"message\":\"Previous user turn\"}}\n").unwrap();
        source
            .execute(
                "INSERT INTO threads VALUES('thread','/repo',0,?,?)",
                &[
                    json!(now),
                    if with_rollout {
                        json!(rollout)
                    } else {
                        Value::Null
                    },
                ],
            )
            .unwrap();
        let app = f.dir.join("codex.mjs");
        fs::write(&app, "console.log('ready'); setInterval(()=>{},1000);").unwrap();
        let app = f.spawn("node", &[app.to_str().unwrap()], &[]);
        wait(|| f.output(app, false).contains("ready"));
        // Seed an older fallback attribution so the race covers both unknown
        // fallback activity and a previously recovered user_turn.
        let binary = f.dir.join("queue");
        script(&binary, "#!/bin/sh\nexit 0\n");
        f.env
            .insert("CITO_CODEX_BIN".into(), binary.display().to_string());
        if prior_effect {
            let seed = f.plugin(CODEX, json!({"op":"wake","session_id":"thread","text":"Earlier wake","checkpoint_only":true}));
            assert_eq!(seed["queue_confirmed"], true, "{seed}");
        }
        source
            .execute("UPDATE threads SET recency_at_ms=?", &[json!(now + 1)])
            .unwrap();
        let discover = |f: &Fixture| {
            let response = f.plugin(
                CODEX,
                json!({"op":"discover_sessions","endpoint_pid":f.pid(app)}),
            );
            assert_eq!(response["ok"], true, "{response}");
            response["sessions"][0]["activity"].clone()
        };
        let before = discover(&f);
        assert_eq!(
            before["kind"],
            if with_rollout || prior_effect {
                "user_turn"
            } else {
                "unknown"
            }
        );
        let cursor = fs::read_dir(f.state.join("activity/codex"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.to_string_lossy().ends_with(".source.json"))
            .unwrap();
        let checkpoint = fs::read(&cursor).unwrap();
        let marker = f.dir.join("published");
        let release = f.dir.join("release");
        script(
            &binary,
            r#"#!/usr/bin/env python3
import json, os, pathlib, sqlite3, sys, time
root = pathlib.Path(os.environ["TMPDIR"])
if os.environ["DELIVER"] == "1":
    with sqlite3.connect(pathlib.Path(os.environ["HOME"]) / ".codex/state_5.sqlite") as db:
        db.execute("UPDATE threads SET recency_at_ms=?", (int(os.environ["WAKE_REVISION"]),))
    if os.environ["WITH_ROLLOUT"] == "1":
        with (root / "rollout.jsonl").open("a") as file:
            file.write(json.dumps({"timestamp":"2026-01-01T00:01:00.000Z", "payload":{"type":"user_message", "message":sys.argv[-1]}}) + "\n")
(root / "published").touch()
deadline = time.monotonic() + 8
while not (root / "release").exists():
    if time.monotonic() >= deadline:
        sys.exit(2)
    time.sleep(0.01)
sys.exit(0 if os.environ["DELIVER"] == "1" else 1)
"#,
        );
        f.env
            .insert("DELIVER".into(), if deliver { "1" } else { "0" }.into());
        f.env.insert(
            "WITH_ROLLOUT".into(),
            if with_rollout { "1" } else { "0" }.into(),
        );
        f.env.insert("WAKE_REVISION".into(), (now + 2).to_string());
        let wake = f.spawn(CODEX, &[], &[]);
        f.send(wake, json!({"op":"wake","session_id":"thread","text":"Cito wake","checkpoint_only":true,"state_dir":f.state,"env":f.env}));
        f.eof(wake);
        wait(|| {
            assert!(!f.exited(wake), "{}", f.output(wake, true));
            marker.exists()
        });
        assert!(!release.exists());
        let inflight = discover(&f);
        assert_eq!(inflight["kind"], "cito_effect", "{inflight}");
        assert_eq!(
            inflight["revision"],
            if deliver { now + 2 } else { now + 1 }
        );
        assert_eq!(
            fs::read(&cursor).unwrap(),
            checkpoint,
            "In-flight discovery must not checkpoint a queue record"
        );
        assert!(
            !f.exited(wake),
            "Discovery must run before queue completion"
        );
        assert!(f.frames(wake).is_empty());
        fs::write(&release, "").unwrap();
        wait(|| f.exited(wake));
        let response = f.frames(wake).pop().unwrap();
        if deliver {
            assert_eq!(response["queue_confirmed"], true, "{response}");
            let settled = discover(&f);
            if with_rollout {
                assert_eq!(settled["kind"], "user_turn");
                assert_eq!(
                    settled["at_ms"], before["at_ms"],
                    "Wake cannot become a new user turn"
                );
            } else {
                assert_eq!(settled["kind"], "cito_effect", "{settled}");
            }
        } else {
            assert_eq!(response["queue_written"], false, "{response}");
            assert_eq!(
                discover(&f),
                before,
                "Failed delivery must release the guard without adding an effect"
            );
            let ledger = cursor.with_extension("").with_extension("json");
            if ledger.exists() {
                let ledger: Value = serde_json::from_slice(&fs::read(ledger).unwrap()).unwrap();
                assert!(ledger["effects"].as_array().is_none_or(Vec::is_empty));
                assert!(ledger["fallback"].is_null());
            }
        }
        source
            .execute("UPDATE threads SET recency_at_ms=?", &[json!(now + 3)])
            .unwrap();
        if with_rollout {
            use std::io::Write;
            let mut file = fs::OpenOptions::new().append(true).open(&rollout).unwrap();
            writeln!(file, "{}", json!({"timestamp":"2026-01-01T00:02:00.000Z","payload":{"type":"user_message","message":"Next genuine user turn"}})).unwrap();
        }
        let user = discover(&f);
        assert_eq!(
            user["kind"],
            if deliver || with_rollout {
                "user_turn"
            } else {
                "unknown"
            }
        );
        assert_eq!(user["revision"], now + 3);
        if with_rollout {
            assert_eq!(user["at_ms"], 1_767_225_720_000u64);
        }
        assert_eq!(
            discover(&f),
            user,
            "Repeated discovery must preserve settled attribution"
        );
    }
}
#[test]
fn codex_metadata_validation_and_sqlite_thread_projection() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    for meta in [
        json!({"openai/threadId":"thread-a"}),
        json!({"threadId":"thread-a"}),
        json!({"codexThreadId":"thread-a"}),
    ] {
        assert_eq!(
            f.plugin(
                CODEX,
                json!({"op":"resolve_caller","meta":meta,"cwd":"/repo"})
            )["caller"]["session_id"],
            "thread-a"
        );
    }
    for headers in [
        json!({"x-codex-turn-metadata":"{\"thread_id\":\"thread-a\"}"}),
        json!({"x-codex-turn-metadata":{"session_id":"thread-a"}}),
    ] {
        assert_eq!(
            f.plugin(CODEX, json!({"op":"resolve_caller","headers":headers}))["caller"]
                ["session_id"],
            "thread-a"
        );
    }
    for meta in [
        json!({"codexThreadId":"a","openai/threadId":"b"}),
        json!({"codexThreadId":"a","pi_pid":123}),
        json!({"endpoint_pid":1,"caller_pid":2}),
    ] {
        assert_eq!(
            f.plugin(CODEX, json!({"op":"resolve_caller","meta":meta}))["error_code"],
            "INVALID_INPUT"
        );
    }
    let db = cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite")).unwrap();
    db.execute("CREATE TABLE metadata(key TEXT)", &[]).unwrap();
    let absent_table = f.plugin(
        CODEX,
        json!({"op":"resolve_caller","meta":{"threadId":"schema-pending"},"cwd":"/repo"}),
    );
    assert_eq!(
        absent_table["caller"]["session_id"], "schema-pending",
        "An absent threads table is not a database IO failure: {absent_table}"
    );
    db.execute("CREATE TABLE threads(id TEXT, cwd TEXT, archived INTEGER, recency_at_ms INTEGER, title TEXT, git_branch TEXT, git_origin_url TEXT); INSERT INTO threads VALUES('active','/repo',0,200,'Active work','main','git@example.com:acme/service.git'),('archived','/repo',1,300,'Archived','old','https://example.com/acme/service.git')",&[]).unwrap();
    let result = f.plugin(
        CODEX,
        json!({"op":"resolve_caller","cli_cmd":"listen","cwd":"/repo"}),
    );
    assert_eq!(result["caller"]["session_id"], "active");
    assert_eq!(result["caller"]["repo"], "acme/service");
    assert_eq!(result["caller"]["branch"], "main");
    assert_eq!(
        f.plugin(
            CODEX,
            json!({"op":"resolve_caller","meta":{"threadId":"archived"},"require_open":true})
        )["error_code"],
        "SESSION_NOT_OPEN"
    );
    assert_eq!(
        f.plugin(
            CODEX,
            json!({"op":"resolve_caller","meta":{"threadId":"archived"},"require_open":false})
        )["caller"]["session_id"],
        "archived"
    );
    assert_eq!(
        f.plugin(CODEX, json!({"op":"probe","session_id":"archived"}))["status"],
        "closed_definitive"
    );
    // A signed interpreter running a user-space CLI script exercises process
    // discovery without launching a desktop app or touching host services.
    let cli = f.dir.join("codex.js");
    fs::write(
        &cli,
        "console.log(JSON.stringify({op:'ready'})); setInterval(()=>{},1000);",
    )
    .unwrap();
    let process = f.spawn("node", &[cli.to_str().unwrap(), "app-server"], &[]);
    wait(|| f.frames(process).iter().any(|v| v["op"] == "ready"));
    let git = f.dir.join("tools/git");
    script(&git, "#!/bin/sh\necho git >> \"$HOME/git-calls\"\nexit 1\n");
    f.env.insert(
        "PATH".into(),
        format!("{}:{}", git.parent().unwrap().display(), f.env["PATH"]),
    );
    let failed_process = f.plugin(
        CODEX,
        json!({"op":"discover_sessions","endpoint_pid":i32::MAX}),
    );
    assert_eq!(
        failed_process["ok"], false,
        "Process inspection errors must not look empty: {failed_process}"
    );
    assert!(failed_process["sessions"].is_null());
    let discover = || f.plugin(CODEX, json!({"op":"discover_sessions"}));
    assert_eq!(f.plugin(CODEX, json!({"op":"setup_apply"}))["ok"], true);
    db.execute("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE", &[])
        .unwrap();
    let started = std::time::Instant::now();
    let locked = discover();
    let probe = f.plugin(CODEX, json!({"op":"probe","session_id":"active"}));
    assert_eq!(probe["ok"], true, "{probe}");
    assert_eq!(probe["status"], "transient_error", "{probe}");
    for request in [
        json!({"op":"resolve_caller","cli_cmd":"listen","cwd":"/repo"}),
        json!({"op":"resolve_caller","meta":{"threadId":"active"},"cwd":"/repo"}),
    ] {
        let failed = f.plugin(CODEX, request);
        assert_eq!(
            failed["ok"], false,
            "A locked database must not fabricate a caller: {failed}"
        );
        assert!(failed["caller"].is_null());
    }
    db.execute("ROLLBACK", &[]).unwrap();
    assert_eq!(
        locked["ok"], false,
        "A locked database must not look empty: {locked}"
    );
    assert!(started.elapsed() >= std::time::Duration::from_millis(900));
    // One read-only connection observes a coherent committed WAL snapshot while
    // a separate writer keeps an uncommitted transaction open.
    db.execute("PRAGMA journal_mode=WAL; BEGIN IMMEDIATE; UPDATE threads SET title='Uncommitted' WHERE id='active'",&[]).unwrap();
    let started = std::time::Instant::now();
    let wal = discover();
    let elapsed = started.elapsed();
    db.execute("ROLLBACK", &[]).unwrap();
    assert_eq!(wal["sessions"][0]["title"], "Active work");
    assert!(
        elapsed < std::time::Duration::from_millis(1500),
        "{elapsed:?}"
    );
    // Older schemas omit the optional source columns.
    assert_eq!(discover()["sessions"][0]["session_id"], "active");
    db.execute(
        "ALTER TABLE threads ADD COLUMN thread_source TEXT; ALTER TABLE threads ADD COLUMN source TEXT",
        &[],
    )
    .unwrap();
    db.transaction(|c| {
        for i in 0..35 {
            let (thread_source, source) = if i % 2 == 0 {
                (json!("subagent"), Value::Null)
            } else {
                (json!("user"), json!("{\"subagent\":true}"))
            };
            c.execute("INSERT INTO threads(id,cwd,archived,recency_at_ms,title,thread_source,source) VALUES(?, '/repo', 0, ?, 'Subagent', ?, ?)", &[json!(format!("subagent-{i}")),json!(1000+i),thread_source,source])?;
        }
        for (id, cwd) in [(Value::Null,json!("/repo")),(json!(""),json!("/repo")),(json!("missing-cwd"),Value::Null),(json!("empty-cwd"),json!("")),(json!(" \t"),json!("/repo")),(json!("blank-cwd"),json!(" \n"))] {
            c.execute("INSERT INTO threads(id,cwd,archived,recency_at_ms) VALUES(?, ?, 0, 2000)", &[id,cwd])?;
        }
        Ok(())
    }).unwrap();
    // Eligibility must precede the Menu slice: 35 newer subagents and invalid rows
    // cannot hide the older user thread, whose source columns are both NULL.
    let discovered = discover();
    assert_eq!(discovered["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(discovered["sessions"][0]["session_id"], "active");
    assert_eq!(discovered["sessions"][0]["title"], "Active work");
    assert_eq!(discovered["sessions"][0]["branch"], "main");
    assert_eq!(discovered["sessions"][0]["repo"], "acme/service");
    assert!(
        !f.home.join("git-calls").exists(),
        "SQLite metadata must bypass git"
    );
    db.execute("UPDATE threads SET thread_source='' WHERE id='active'", &[])
        .unwrap();
    assert_eq!(discover()["sessions"][0]["session_id"], "active");
    db.transaction(|c| {
        for i in 0..40 {
            c.execute("INSERT INTO threads(id,cwd,archived,recency_at_ms,title,thread_source,source) VALUES(?, '/other', 0, ?, 'User', 'user', 'cli')", &[json!(format!("user-{i}")),json!(3000+i)])?;
        }
        Ok(())
    }).unwrap();
    let discovered = f.plugin(CODEX, json!({"op":"discover_sessions"}));
    let sessions = discovered["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 8);
    assert_eq!(sessions[0]["session_id"], "user-39");
    // Implicit caller resolution remains unbounded and can address older threads.
    assert_eq!(
        f.plugin(
            CODEX,
            json!({"op":"resolve_caller","cli_cmd":"listen","cwd":"/repo"})
        )["caller"]["session_id"],
        "active"
    );
    // Explicit metadata lookup remains available as well.
    assert_eq!(
        f.plugin(
            CODEX,
            json!({"op":"resolve_caller","meta":{"threadId":"active"}})
        )["caller"]["session_id"],
        "active"
    );
    db.execute("UPDATE threads SET archived=1 WHERE id='active'", &[])
        .unwrap();
    assert_eq!(
        f.plugin(CODEX, json!({"op":"probe","session_id":"active"}))["status"],
        "closed_definitive"
    );
    drop(db);
    fs::remove_file(f.home.join(".codex/state_5.sqlite")).unwrap();
    assert_eq!(discover()["sessions"], json!([]));
    fs::write(f.home.join(".codex/state_5.sqlite"), "not a database").unwrap();
    assert_eq!(
        discover()["ok"],
        false,
        "Real IO/format errors must reach the discovery cache owner"
    );
    assert_eq!(
        f.plugin(CODEX, json!({"op":"probe","session_id":"active"}))["ok"],
        false,
        "Structural database errors cannot fabricate liveness"
    );
    for request in [
        json!({"op":"resolve_caller","cli_cmd":"listen","cwd":"/repo"}),
        json!({"op":"resolve_caller","meta":{"threadId":"active"},"cwd":"/repo"}),
    ] {
        let failed = f.plugin(CODEX, request);
        assert_eq!(
            failed["ok"], false,
            "Corruption must not fall back to caller metadata: {failed}"
        );
        assert!(failed["caller"].is_null());
    }
}
#[test]
fn codex_queue_literal_arguments_and_open_retry_do_not_duplicate_insertion() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let queue = f.dir.join("queue");
    script(
        &queue,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$HOME/queue-args\"\n",
    );
    f.env
        .insert("CITO_CODEX_BIN".into(), queue.display().to_string());
    let request = json!({"op":"wake","session_id":"thread-a","text":"a message with spaces"});
    assert_eq!(f.plugin(CODEX, request.clone())["outcome"], "delivered");
    assert_eq!(
        fs::read_to_string(f.home.join("queue-args")).unwrap(),
        "queue\n--resume\n--thread\nthread-a\n--message\na message with spaces\n"
    );
    assert!(fs::read_to_string(f.home.join("mock-calls"))
        .unwrap()
        .contains("codex://threads/wake/thread-a"));
    let mut retry = request;
    retry["queue_written"] = json!(true);
    let result = f.plugin(CODEX, retry.clone());
    assert_eq!(result["outcome"], "delivered_unconfirmed");
    assert_eq!(result["open_confirmed"], true);
    script(
        std::path::Path::new(&f.env["CITO_OPEN_BIN"]),
        "#!/bin/sh\nexit 1\n",
    );
    assert_eq!(f.plugin(CODEX, retry)["open_confirmed"], false);
    assert_eq!(
        fs::read_to_string(f.home.join("queue-args"))
            .unwrap()
            .lines()
            .count(),
        6
    );
}
#[test]
fn setup_remove_dry_run_preserves_live_plugin_runtime() {
    use std::os::unix::net::UnixListener;
    let _lock = serial();
    let f = Fixture::new(json!({}));
    fs::create_dir_all(f.state.join("sockets")).unwrap();
    fs::create_dir_all(f.state.join("claude/wakes")).unwrap();
    let queue = f.state.join("claude/wakes/session-a.jsonl");
    fs::write(&queue, b"retained wake\n").unwrap();
    for (id, bin) in [("pi", PI), ("claude", CLAUDE)] {
        let socket = f.state.join(format!("sockets/{id}-pid-123.sock"));
        let _listener = UnixListener::bind(&socket).unwrap();
        assert_eq!(f.plugin(bin, json!({"op":"setup_apply"}))["ok"], true);
        assert_eq!(
            f.plugin(bin, json!({"op":"setup_remove","dry_run":true}))["ok"],
            true
        );
        assert!(socket.exists());
        assert_eq!(fs::read(&queue).unwrap(), b"retained wake\n");
        assert_eq!(
            f.plugin(bin, json!({"op":"setup_inspect"}))["configured"],
            true
        );
        assert_eq!(f.plugin(bin, json!({"op":"setup_remove"}))["ok"], true);
        assert!(!socket.exists());
    }
    assert!(!queue.exists());
}
#[test]
fn claude_probe_resolves_hook_only_sessions_and_drain_read_failure_preserves_alias() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let script = f.dir.join("claude.mjs");
    fs::write(
        &script,
        "console.log('ready'); setInterval(() => {}, 1000);",
    )
    .unwrap();
    let endpoint = f.spawn("node", &[script.to_str().unwrap()], &[]);
    wait(|| f.output(endpoint, false).contains("ready"));
    f.env
        .insert("CITO_ENDPOINT_PID".into(), f.pid(endpoint).to_string());
    let transcripts = f.home.join(".claude/projects/-repo");
    fs::create_dir_all(&transcripts).unwrap();
    let sid = "11111111-1111-1111-1111-111111111111";
    fs::write(transcripts.join(format!("{sid}.jsonl")), b"").unwrap();
    f.plugin(
        HOOK,
        json!({"hook_event_name":"SessionStart","session_id":sid,"cwd":"/repo"}),
    );
    assert!(!f
        .state
        .join(format!("sockets/claude-pid-{}.sock", f.pid(endpoint)))
        .exists());
    for (expected, status) in [(sid, "alive"), ("old-session", "switched")] {
        let result = f.plugin(
            CLAUDE,
            json!({"op":"probe","session_id":expected,"cwd":"/repo","endpoint_pid":f.pid(endpoint)}),
        );
        assert_eq!(result["status"], status, "{result}");
    }
    let wakes = f.state.join("claude/wakes");
    fs::create_dir_all(&wakes).unwrap();
    fs::create_dir_all(f.state.join("claude/aliases")).unwrap();
    fs::write(f.state.join("claude/aliases").join(sid), b"claude-123").unwrap();
    let alias = wakes.join("claude-123.jsonl");
    fs::write(
        &alias,
        "{\"batch_id\":\"alias\",\"content\":\"keep alias\"}\n",
    )
    .unwrap();
    let broken = wakes.join(format!("{sid}.jsonl.draining-0"));
    fs::write(&broken, [255]).unwrap();
    assert_eq!(
        f.plugin(
            HOOK,
            json!({"hook_event_name":"UserPromptSubmit","session_id":sid,"cwd":"/repo"})
        ),
        json!({})
    );
    assert!(alias.exists());
    assert!(broken.exists());
    fs::remove_file(broken).unwrap();
    let result = f.plugin(
        HOOK,
        json!({"hook_event_name":"UserPromptSubmit","session_id":sid,"cwd":"/repo"}),
    );
    assert_eq!(
        result["hookSpecificOutput"]["additionalContext"],
        "keep alias"
    );
    assert!(
        alias.exists(),
        "Unconfirmed ACK continuation keeps the durable entry"
    );
    let row: Value = serde_json::from_str(fs::read_to_string(&alias).unwrap().trim()).unwrap();
    assert_eq!(row["output_delivered"], true);
    assert_eq!(
        f.plugin(
            HOOK,
            json!({"hook_event_name":"UserPromptSubmit","session_id":sid,"cwd":"/repo"})
        ),
        json!({})
    );
}
#[test]
fn claude_setup_preserves_foreign_hooks_and_installs_absolute_paths() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let path = f.home.join(".claude/settings.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path,json!({"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"/old/cito-claude-hook"},{"type":"command","command":"keep"}]}]}}).to_string()).unwrap();
    assert_eq!(f.plugin(CLAUDE, json!({"op":"setup_apply"}))["ok"], true);
    let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        v["mcpServers"]["cito"]["command"],
        json!(f.state.join("bin/cito-mcp"))
    );
    assert!(v.to_string().contains(
        f.state
            .join("runtime/plugins/claude/bin/cito-claude-hook")
            .to_str()
            .unwrap()
    ));
    assert_eq!(f.plugin(CLAUDE, json!({"op":"setup_remove"}))["ok"], true);
    let v: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        v,
        json!({"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep"}]}]}})
    );
}
#[test]
fn claude_flock_torn_lines_concurrent_drain_and_passive_stop() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let queue = f.state.join("claude/wakes/session-a.jsonl");
    fs::create_dir_all(queue.parent().unwrap()).unwrap();
    fs::write(&queue, "{\"batch_id\":\"torn\"").unwrap();
    // Bounded four-way concurrency exercises the file lock without creating a process storm.
    let mut contexts = String::new();
    for chunk in 0..6 {
        let mut children = Vec::new();
        for i in chunk * 4..chunk * 4 + 4 {
            let p = f.spawn(CLAUDE, &[], &[]);
            f.send(p,json!({"op":"wake","state_dir":f.state,"env":f.env,"session_id":"session-a","batch_id":format!("b-{i}"),"text":format!("token-{i:03}"),"wake":false}));
            f.eof(p);
            children.push(p);
        }
        let result = f.plugin(
            HOOK,
            json!({"hook_event_name":"UserPromptSubmit","session_id":"session-a","cwd":"/repo"}),
        );
        contexts.push_str(
            result["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap_or(""),
        );
        for p in children {
            wait(|| f.exited(p));
            f.stop(p);
        }
    }
    let result = f.plugin(
        HOOK,
        json!({"hook_event_name":"Stop","session_id":"session-a","cwd":"/repo"}),
    );
    assert!(result.get("decision").is_none());
    contexts.push_str(
        result["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or(""),
    );
    for i in 0..24 {
        assert_eq!(
            contexts.matches(&format!("token-{i:03}")).count(),
            1,
            "{contexts}"
        );
    }
    f.plugin(CLAUDE,json!({"op":"wake","session_id":"session-a","batch_id":"wake","text":"continue","wake":true}));
    assert_eq!(
        f.plugin(
            HOOK,
            json!({"hook_event_name":"Stop","session_id":"session-a"})
        ),
        json!({"decision":"block","reason":"continue"})
    );
    assert_eq!(
        f.plugin(
            HOOK,
            json!({"hook_event_name":"Stop","session_id":"session-a"})
        ),
        json!({})
    );
}
#[test]
fn claude_offline_hook_retires_output_only_after_identity_bound_durable_ack() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let local = cito_native::core::state::LocalState::open(&f.state).unwrap();
    local.db.execute("INSERT INTO local_listeners(hub,listener_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status,hub_url) VALUES('work','c_offline','none#alice','alice','host','laptop','claude','session-a','/repo','Work','','workspace','ACTIVE','https://hub.example.com'); INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,batch_id,queue_written,queue_confirmed,hub_url,principal_id) VALUES('work','offline-signal','c_offline',1,1,'bob','desktop','','Delivered exactly once','now',9999999999999,'DISPATCHED','offline-batch',1,1,'https://hub.example.com','none#alice'); UPDATE local_listeners SET hub_url='https://replacement.example.com',principal_id='none#bob' WHERE listener_id='c_offline'",&[]).unwrap();
    f.plugin(CLAUDE,json!({"op":"wake","session_id":"session-a","batch_id":"offline-batch","text":"Delivered exactly once","wake":false}));
    let output = f.plugin(
        HOOK,
        json!({"hook_event_name":"UserPromptSubmit","session_id":"session-a","cwd":"/repo"}),
    );
    assert_eq!(
        output["hookSpecificOutput"]["additionalContext"],
        "Delivered exactly once"
    );
    let acks = local
        .db
        .query("SELECT * FROM pending_hub_acks", &[])
        .unwrap();
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0]["hub_url"], "https://hub.example.com");
    assert_eq!(acks[0]["principal_id"], "none#alice");
    assert_eq!(acks[0]["signal_id"], "offline-signal");
    assert!(!f.state.join("claude/wakes/session-a.jsonl").exists());
    assert_eq!(
        f.plugin(
            HOOK,
            json!({"hook_event_name":"UserPromptSubmit","session_id":"session-a","cwd":"/repo"})
        ),
        json!({})
    );
}
#[test]
fn claude_hook_ack_prefers_environment_daemon_port_over_config() {
    use std::{io::Read, net::TcpListener, time::Duration};

    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let environment = TcpListener::bind("127.0.0.1:0").unwrap();
    let configured = TcpListener::bind("127.0.0.1:0").unwrap();
    environment.set_nonblocking(true).unwrap();
    configured.set_nonblocking(true).unwrap();
    f.env.insert(
        "CITO_DAEMON_PORT".into(),
        format!(" {} ", environment.local_addr().unwrap().port()),
    );
    fs::write(
        &f.config,
        json!({"daemonPort":configured.local_addr().unwrap().port()}).to_string(),
    )
    .unwrap();
    f.plugin(CLAUDE, json!({"op":"wake","session_id":"session-a","batch_id":"ack-port","text":"pending","wake":false}));
    let hook = f.spawn(HOOK, &[], &[]);
    f.send(
        hook,
        json!({"hook_event_name":"UserPromptSubmit","session_id":"session-a","cwd":"/repo"}),
    );
    f.eof(hook);
    let mut connection = None;
    wait(|| match environment.accept() {
        Ok((stream, _)) => {
            connection = Some(stream);
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            assert!(
                !f.exited(hook),
                "hook exited without acknowledging to CITO_DAEMON_PORT: {}",
                f.output(hook, true)
            );
            false
        }
        Err(e) => panic!("environment listener: {e}"),
    });
    let stream = connection.unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "POST /mcp HTTP/1.1\r\n");
    let mut length = None;
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<u64>().unwrap());
            }
        }
    }
    let ack: Value = serde_json::from_reader(reader.by_ref().take(length.unwrap())).unwrap();
    assert_eq!(
        ack,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"cito_inbox","arguments":{"ack_all":false,"ack_ids":["ack-port"]},"_meta":{"claudeSessionId":"session-a","cwd":"/repo"}}})
    );
    reader
        .get_mut()
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
        .unwrap();
    wait(|| f.exited(hook));
    f.stop(hook);
    assert_eq!(
        f.frames(hook),
        vec![
            json!({"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"pending"}})
        ]
    );
    assert_eq!(f.output(hook, true), "");
    assert!(matches!(configured.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}
#[test]
fn claude_sidecar_socket_dedup_session_fencing_and_eof_cleanup() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let endpoint = std::process::id();
    let p = f.spawn(CLAUDE, &[], &[]);
    let mut env = f.env.clone();
    env.insert("CLAUDE_SESSION_ID".into(), "session-a".into());
    f.send(p,json!({"op":"stdio_sidecar","state_dir":f.state,"env":env,"endpoint_pid":endpoint,"cwd":"/repo"}));
    wait(|| f.frames(p).iter().any(|v| v["op"] == "ready"));
    assert_eq!(
        f.frames(p)[0]["capabilities_patch"]["experimental"]["claude/channel"],
        json!({})
    );
    let path = f.state.join(format!("sockets/claude-pid-{endpoint}.sock"));
    assert_eq!(
        socket(&path, json!({"op":"identify"}))["session_id"],
        "session-a"
    );
    socket(
        &path,
        json!({"op":"status","session_id":"session-a","badge":"badge"}),
    );
    let frame = json!({"op":"deliver","session_id":"session-a","batch_id":"batch","content":"literal <card>","wake":true});
    assert_eq!(socket(&path, frame.clone())["deduplicated"], false);
    assert_eq!(socket(&path, frame)["deduplicated"], true);
    assert_eq!(
        socket(
            &path,
            json!({"op":"deliver","session_id":"other","batch_id":"batch","content":"stale","wake":true})
        )["error"],
        "SESSION_MISMATCH"
    );
    assert_eq!(
        fs::read_to_string(f.state.join("claude/wakes/session-a.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    f.eof(p);
    wait(|| f.exited(p));
    assert!(!path.exists());
    assert!(!f.state.join("claude/badges/session-a").exists());
}
#[test]
fn pi_bridge_real_typescript_host_contract_without_npm() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let file = root().join("crates/cito-native/tests/support/pi-bridge.mjs");
    let (ok, out, err) = f.command(
        "node",
        &["--experimental-strip-types", file.to_str().unwrap()],
        "",
    );
    assert!(ok, "{out}\n{err}");
}
#[test]
fn empty_and_unbound_hooks_are_silent_and_malformed_input_preserves_queue() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for input in ["", "{\"hook_event_name\":\"Stop\",\"cwd\":\"/repo\"}\n"] {
        let (ok, out, err) = f.command(HOOK, &[], input);
        assert!(ok);
        assert_eq!(out, "{}\n");
        assert_eq!(err, "");
    }
    f.plugin(CLAUDE, json!({"op":"wake","session_id":"session-a","batch_id":"preserve","text":"pending","wake":true}));
    let queue = f.state.join("claude/wakes/session-a.jsonl");
    let before = fs::read(&queue).unwrap();
    let (ok, out, err) = f.command(HOOK, &[], "not-json\n");
    assert!(ok);
    assert_eq!(out, "{}\n");
    assert!(err.starts_with("cito-claude-hook:"));
    assert_eq!(fs::read(queue).unwrap(), before);
}
#[test]
fn explicit_pid_claims_reject_invalid_conflicting_and_unreachable_endpoints() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for bin in [PI, CODEX, CLAUDE] {
        for meta in [
            json!({"pi_pid":0}),
            json!({"pi_pid":-1}),
            json!({"pi_pid":0.5}),
            json!({"pi_pid":"12"}),
            json!({"pi_pid":123,"caller_pid":124}),
        ] {
            assert_ne!(
                f.plugin(bin, json!({"op":"resolve_caller","meta":meta}))["ok"],
                true
            );
        }
    }
    assert_eq!(
        f.plugin(
            PI,
            json!({"op":"resolve_caller","meta":{"pi_pid":2147483647}})
        )["ok"],
        false
    );
}

#[test]
fn telegram_token_rotation_discards_pending_updates_and_restarts_offset_zero() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let api = root().join("crates/cito-native/tests/support/telegram-api.py");
    let server = f.spawn("python3", &[api.to_str().unwrap()], &[]);
    wait(|| !f.frames(server).is_empty());
    let port = f.frames(server)[0]["port"].as_u64().unwrap();
    let p = f.spawn(TELEGRAM, &[], &[]);
    let config =
        |token: &str| json!({"botToken":token,"apiBaseUrl":format!("http://127.0.0.1:{port}")});
    f.send(
        p,
        json!({"op":"configure","hub_config":config("old"),"cursor":"10"}),
    );
    wait(|| f.frames(p).iter().any(|v| v["op"] == "inbound_reply"));
    // Both pairing and reply updates await acknowledgments; a new token must discard them.
    f.send(
        p,
        json!({"op":"configure","hub_config":config("rotated"),"cursor":"999"}),
    );
    wait(|| {
        fs::read_to_string(f.home.join("api-polls.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .any(|v| v["path"] == "/botrotated/getUpdates" && v["offset"] == 0)
    });
    f.send(
        p,
        json!({"op":"pair_ack","code":"cito-abcdefghij","ok":true,"owner_user":"alice"}),
    );
    f.send(
        p,
        json!({"op":"reply_ack","update_id":"15","ok":true,"target_display":"sender"}),
    );
    f.send(p,json!({"op":"format_pair","req_id":"barrier","code":"cito-new","hub_config":config("rotated")}));
    wait(|| f.frames(p).iter().any(|v| v["req_id"] == "barrier"));
    assert!(
        !f.home.join("api-sends.jsonl").exists(),
        "stale identity maps must not send confirmations for the previous token"
    );
    f.eof(p);
    wait(|| f.exited(p));
}
#[test]
fn telegram_poll_health_reports_conflicts_and_credentials_without_mutating_webhooks() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let api = root().join("crates/cito-native/tests/support/telegram-api.py");
    let server = f.spawn("python3", &[api.to_str().unwrap()], &[]);
    wait(|| !f.frames(server).is_empty());
    let port = f.frames(server)[0]["port"].as_u64().unwrap();
    let p = f.spawn(TELEGRAM, &[], &[]);
    for (token, expected) in [("poll409", "webhook"), ("poll401", "replace the Bot Token")] {
        let before = fs::read_to_string(f.home.join("api-methods.jsonl"))
            .unwrap_or_default()
            .lines()
            .count();
        f.send(p,json!({"op":"configure","hub_config":{"botToken":token,"apiBaseUrl":format!("http://127.0.0.1:{port}")}}));
        wait(|| {
            f.frames(p).iter().any(|v| {
                v["op"] == "health" && v["error"].as_str().is_some_and(|e| e.contains(expected))
            })
        });
        let calls = fs::read_to_string(f.home.join("api-methods.jsonl"))
            .unwrap()
            .lines()
            .count()
            - before;
        assert_eq!(calls,if token=="poll409" {3} else {1},"Transient poll failures need three consecutive failures; auth errors surface immediately");
    }
    f.send(p,json!({"op":"configure","hub_config":{"botToken":"healthy","apiBaseUrl":format!("http://127.0.0.1:{port}")}}));
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "health" && v["error"].is_null())
    });
    let methods = fs::read_to_string(f.home.join("api-methods.jsonl")).unwrap();
    assert!(!methods.contains("deleteWebhook"));
    f.eof(p);
    wait(|| f.exited(p));
}
#[test]
fn telegram_persistent_pair_reply_cursor_and_unicode_chunk_contract() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let api = root().join("crates/cito-native/tests/support/telegram-api.py");
    let server = f.spawn("python3", &[api.to_str().unwrap()], &[]);
    wait(|| !f.frames(server).is_empty());
    let port = f.frames(server)[0]["port"].as_u64().unwrap();
    let config = json!({"botToken":"test","apiBaseUrl":format!("http://127.0.0.1:{port}")});
    let p = f.spawn(TELEGRAM, &[], &[]);
    f.send(
        p,
        json!({"op":"format_pair","req_id":"pair","code":"cito-abcdefghij","hub_config":config}),
    );
    wait(|| f.frames(p).iter().any(|v| v["op"] == "rpc_result"));
    assert_eq!(
        f.frames(p)
            .iter()
            .find(|v| v["op"] == "rpc_result")
            .unwrap()["instruction"],
        "Send \"/start cito-abcdefghij\" to @example_bot"
    );
    let pairing = f
        .frames(p)
        .into_iter()
        .find(|v| v["op"] == "rpc_result")
        .unwrap();
    assert_eq!(pairing["bot_username"], "example_bot");
    assert_eq!(
        pairing["pairing_url"],
        "https://t.me/example_bot?start=cito-abcdefghij"
    );
    f.send(
        p,
        json!({"op":"configure","hub_config":config,"bindings":[],"cursor":"10"}),
    );
    wait(|| f.frames(p).iter().any(|v| v["op"] == "pair_redeem"));
    wait(|| f.frames(p).iter().any(|v| v["op"] == "inbound_reply"));
    assert!(f
        .frames(p)
        .iter()
        .filter(|v| v["op"] == "cursor_save")
        .all(|v| v["cursor"].as_str().unwrap().parse::<u64>().unwrap() <= 12));
    f.send(
        p,
        json!({"op":"pair_ack","code":"cito-abcdefghij","ok":true,"owner_user":"alice"}),
    );
    f.send(
        p,
        json!({"op":"reply_ack","update_id":"15","ok":true,"target_display":"alice-agent"}),
    );
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "cursor_save" && v["cursor"] == "16")
    });
    let message = format!(
        "start:{}|first-boundary|{}|second-boundary|{}",
        "😀".repeat(4070),
        "b".repeat(4090),
        "🚀".repeat(850)
    );
    let expected = format!("Cito · work\n\n{message}");
    f.send(p,json!({"op":"deliver","delivery_id":"large","config":{"chatId":"large"},"signal":{"from":{"title":"work"},"message":message}}));
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "delivered" && v["progress"] != true)
    });
    let frame = f
        .frames(p)
        .into_iter()
        .find(|v| v["op"] == "delivered" && v["progress"] != true)
        .unwrap();
    assert_eq!(frame["ok"], true);
    assert_eq!(frame["external_msg_ids"].as_array().unwrap().len(), 3);
    let sent = fs::read_to_string(f.home.join("api-sends.jsonl")).unwrap();
    let chunks = sent
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|v| v["chat_id"] == "large")
        .map(|v| v["text"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(chunks.len(), 3);
    assert!(chunks.iter().all(|text| text.chars().count() <= 4096));
    assert_eq!(chunks.concat(), expected);
    let outcome = |delivery_id: &str| {
        wait(|| {
            f.frames(p).iter().any(|v| {
                v["delivery_id"] == delivery_id && v["op"] == "delivered" && v["progress"] != true
            })
        });
        f.frames(p)
            .into_iter()
            .find(|v| {
                v["delivery_id"] == delivery_id && v["op"] == "delivered" && v["progress"] != true
            })
            .unwrap()
    };
    for status in [400, 401, 403, 404, 408, 429, 500, 503] {
        let delivery_id = format!("status-{status}");
        f.send(p,json!({"op":"deliver","delivery_id":delivery_id,"config":{"chatId":format!("status:{status}")},"signal":{"message":"Review"}}));
        let frame = outcome(&delivery_id);
        assert_eq!(frame["ok"], false);
        assert_eq!(frame["retryable"], matches!(status, 408 | 429 | 500..=599));
        assert_eq!(frame["effect_unknown"], false);
        assert_eq!(
            frame["error"],
            format!("Telegram sendMessage HTTP {status}: Rejected by fixture")
        );
    }
    f.send(p,json!({"op":"deliver","delivery_id":"uncertain","config":{"chatId":"uncertain"},"signal":{"message":"Review"}}));
    let frame = outcome("uncertain");
    assert_eq!(frame["effect_unknown"], true);
    assert_eq!(frame["retryable"], false);
    let message = format!(
        "start:{}|first-boundary|{}|second-boundary|end",
        "a".repeat(4070),
        "b".repeat(4800)
    );
    let expected = format!("Cito · sender\n\n{message}");
    f.send(p,json!({"op":"deliver","delivery_id":"partial","config":{"chatId":"partial"},"signal":{"message":message}}));
    let partial = outcome("partial");
    assert_eq!(partial["ok"], false);
    assert_eq!(partial["retryable"], true);
    assert_eq!(partial["effect_unknown"], false);
    assert_eq!(partial["external_msg_ids"].as_array().unwrap().len(), 1);
    f.send(p,json!({"op":"deliver","delivery_id":"resumed","already_sent_ids":partial["external_msg_ids"],"config":{"chatId":"partial"},"signal":{"message":message}}));
    let resumed = outcome("resumed");
    assert_eq!(resumed["ok"], true);
    assert_eq!(resumed["external_msg_ids"].as_array().unwrap().len(), 3);
    assert_eq!(
        resumed["external_msg_ids"][0],
        partial["external_msg_ids"][0]
    );
    let sent = fs::read_to_string(f.home.join("api-sends.jsonl")).unwrap();
    let chunks = sent
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|v| v["chat_id"] == "partial")
        .map(|v| v["text"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks.concat(), expected);
    assert_eq!(
        chunks[1..].concat(),
        expected.chars().skip(4096).collect::<String>()
    );
    f.eof(p);
    wait(|| f.exited(p));
}

#[test]
fn cached_pid_identity_requires_same_live_invocation_and_plugin_process_command() {
    let _lock = serial();
    for (id, bin, entry) in [
        (
            "pi",
            PI,
            "node_modules/@example/pi-coding-agent/dist/cli.js",
        ),
        ("codex", CODEX, "node_modules/@example/codex/bin/codex.js"),
        ("claude", CLAUDE, "node_modules/@example/claude-code/cli.js"),
    ] {
        let f = Fixture::new(json!({}));
        let path = f.dir.join(entry);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, include_str!("endpoint.cjs")).unwrap();
        let p = f.spawn(
            "node",
            &[path.to_str().unwrap()],
            &[("FIXTURE_HARNESS", id)],
        );
        wait(|| f.frames(p).iter().any(|v| v["op"] == "ready"));
        let endpoint = f.pid(p);
        if id == "codex" {
            let db = cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite"))
                .unwrap();
            db.execute("CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,updated_at INTEGER);INSERT INTO threads VALUES('session-a','/repo',0,1)",&[]).unwrap();
        }
        let mut request = json!({"op":"resolve_caller","endpoint_pid":endpoint,"cwd":"/repo","require_open":true});
        if id == "codex" {
            request["meta"] = json!({"threadId":"session-a"});
        }
        let resolved = f.plugin(bin, request.clone());
        assert_eq!(resolved["ok"], true, "{resolved}");
        let cached = resolved["caller"].clone();
        assert!(cached["process_identity"]["command"]
            .as_str()
            .unwrap()
            .contains(path.to_str().unwrap()));
        f.send(p, json!({"op":"offline"}));
        wait(|| f.frames(p).iter().any(|v| v["op"] == "offline"));
        if id == "codex" {
            fs::remove_file(f.home.join(".codex/state_5.sqlite")).unwrap();
        }
        request["require_open"] = json!(false);
        request["cached_caller"] = cached.clone();
        request["meta"] = json!({"expected_harness":id,"expected_session_id":"session-a"});
        let offline = f.plugin(bin, request.clone());
        if id == "pi" {
            assert_eq!(offline["error_code"], "PI_BRIDGE_UNAVAILABLE");
        } else {
            assert_eq!(offline["caller"], cached);
        }
        for key in ["command", "started_at"] {
            let mut stale = request.clone();
            stale["cached_caller"]["process_identity"][key] = json!("another invocation");
            assert_eq!(f.plugin(bin, stale)["error_code"], "BINDING_REQUIRED");
        }
        let mut wrong = request.clone();
        wrong["meta"]["expected_session_id"] = json!("other");
        assert_eq!(f.plugin(bin, wrong)["error_code"], "INVALID_INPUT");
        f.stop(p);
        assert_eq!(f.plugin(bin, request)["error_code"], "BINDING_REQUIRED");
    }
}
