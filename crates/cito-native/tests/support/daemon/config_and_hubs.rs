use super::*;

#[test]
fn background_oidc_recovery_does_not_block_health_or_a_healthy_hub() {
    use std::{
        io::Write,
        net::TcpListener,
        time::{Duration, Instant},
    };
    let _lock = serial();
    let issuer = TcpListener::bind("127.0.0.1:0").unwrap();
    let issuer_url = format!("http://{}", issuer.local_addr().unwrap());
    let (accepted, ready) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let (finished, done) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = issuer.accept().unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        accepted.send(()).unwrap();
        let _ = released.recv_timeout(Duration::from_secs(5));
        let _ = stream.write_all(
            b"HTTP/1.1 503 Unavailable\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
        let _ = finished.send(());
    });
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let saved = f.action(json!({"op":"setup_save_hub","name":"work","url":"http://127.0.0.1:1","authMode":"oidc","issuer":issuer_url,"clientId":"client"}));
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let in_flight = done.try_recv().is_err();
    let start = Instant::now();
    let health = http(f.port, "/health", None);
    let latency = start.elapsed();
    let start = Instant::now();
    let snapshot = f.action(json!({"op":"get_snapshot"}));
    let snapshot_latency = start.elapsed();
    let _ = release.send(());
    worker.join().unwrap();
    assert_eq!(saved.0, 200);
    assert!(in_flight, "Probe runs during a blocked OIDC request");
    assert_eq!(health.0, 200);
    assert!(latency < Duration::from_millis(500), "{latency:?}");
    assert_eq!(snapshot.0, 200);
    assert!(
        snapshot_latency < Duration::from_millis(500),
        "{snapshot_latency:?}"
    );
    assert!(snapshot.1["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["name"] == "local" && h["status"] == "connected"));
}
#[test]
fn gui_purge_removes_credentials_idempotency_hub_history_and_personal_profile_data() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    let human = f
        .rows("SELECT * FROM local_listeners WHERE harness='human'")
        .remove(0);
    f.tool("cito_listen", json!({"about":"Delete this session"}), meta);
    f.tool(
        "cito_send",
        json!({"to":human["public_id"],"message":"Delete this history"}),
        json!({}),
    );
    assert!(!f.rows("SELECT * FROM local_sent_idempotency").is_empty());
    let updated = f.action(json!({"op":"human_inbox_update","hub":"local","paused":true,"about":"Delete this profile"}));
    assert_eq!(updated.0, 200);
    fs::write(
        f.state.join("oidc-token-work.json"),
        "delete this credential",
    )
    .unwrap();
    fs::create_dir_all(f.state.join("activity/fixture")).unwrap();
    fs::write(f.state.join("activity/fixture/source.json"), "{}").unwrap();
    let result = f.action(json!({"op":"setup_disconnect_all","purge_data":true}));
    assert_eq!(result.0, 200, "{}", result.1);
    assert!(!f.state.join("oidc-token-work.json").exists());
    assert!(!f.state.join("activity").exists());
    for table in [
        "local_signals",
        "local_sent_idempotency",
        "pending_hub_acks",
        "pending_hub_detaches",
        "session_custom_titles",
    ] {
        assert!(
            f.rows(&format!("SELECT * FROM {table}")).is_empty(),
            "{table}"
        );
    }
    let hub = f.db("hub.sqlite");
    assert!(hub
        .query("SELECT * FROM hub_signals", &[])
        .unwrap()
        .is_empty());
    let fresh = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(fresh["hubs"][0]["human_paused"], false);
    assert_eq!(fresh["hubs"][0]["human_about"], "Human");
    assert_eq!(fresh["hubs"][0]["human_control_revision"], 1);
}
#[test]
fn u8_token_auth_generates_and_persists_token_while_sharing_is_off() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[("CITO_TAILSCALE_HOST", "mac-studio.tailnet.ts.net.")]);
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":false,"auth_mode":"token"}))
            .0,
        200
    );
    let host = f.action(json!({"op":"get_snapshot"})).1["settings"]["host_hub"].clone();
    assert_eq!(host["enabled"], false);
    assert_eq!(host["running"], false);
    assert_eq!(host["token"].as_str().unwrap().len(), 64);
    assert_eq!(
        host["endpoint_url"],
        "http://mac-studio.tailnet.ts.net:4291"
    );
    assert!(host["invite_command"]
        .as_str()
        .unwrap()
        .contains("--url http://mac-studio.tailnet.ts.net:4291"));
    assert!(host["invite_command"]
        .as_str()
        .unwrap()
        .contains("--hub mac-studio "));
    assert_eq!(host["tailscale_url"], host["endpoint_url"]);
    assert!(host["lan_url"].as_str().unwrap().starts_with("http://"));
    assert_eq!(host["tailscale_invite_command"], host["invite_command"]);
    let saved: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    assert_eq!(saved["host_hub"]["token"], host["token"]);
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":false,"auth_mode":"token"}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"get_snapshot"})).1["settings"]["host_hub"]["token"],
        host["token"]
    );
    // An explicit listener interface must not advertise an unrelated Tailscale address.
    for bind in ["127.0.0.1", "192.0.2.10", "::1"] {
        assert_eq!(
            f.action(json!({"op":"set_host_hub","enabled":false,"bind":bind}))
                .0,
            200
        );
        let host = f.action(json!({"op":"get_snapshot"})).1["settings"]["host_hub"].clone();
        let authority = if bind.contains(':') {
            format!("[{bind}]")
        } else {
            bind.into()
        };
        assert_eq!(host["endpoint_url"], format!("http://{authority}:4291"));
        assert_eq!(host["lan_url"], host["endpoint_url"]);
        assert!(host["tailscale_url"].is_null());
        assert!(host["tailscale_invite_command"].is_null());
    }
}

#[test]
fn lifecycle_sign_in_enables_disabled_hub_before_bounded_discovery_failure() {
    let _lock = serial();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let server = std::thread::spawn(move || {
        use std::io::Write;
        while !flag.load(std::sync::atomic::Ordering::SeqCst) {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                );
            } else {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    });
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":format!("http://127.0.0.1:{port}"),"enabled":false,"authMode":"oidc","issuer":format!("http://127.0.0.1:{port}"),"clientId":"client"}},"default_hub":"local"}),
    );
    f.daemon(&[]);
    let result = f.action(json!({"op":"login_hub","hub":"work"}));
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    server.join().unwrap();
    assert_eq!(result.0, 400, "{result:?}");
    let raw: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    assert_eq!(raw["hubs"]["work"]["enabled"], true);
    assert!(
        !f.home.join("mock-calls").exists(),
        "discovery failure must never open a browser"
    );
}
#[test]
fn lifecycle_hub_edit_preserves_secrets_disabled_state_default_and_host_error_rollback() {
    let _lock = serial();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":"http://127.0.0.1:1","authMode":"token","token":"saved","enabled":false,"clientSecret":"retained"}},"default_hub":"local"}),
    );
    f.daemon(&[]);
    assert_eq!(f.action(json!({"op":"setup_save_hub","edit":true,"original_name":"work","name":"renamed","url":"http://127.0.0.1:2","auth_mode":"token","token":"","clientSecret":""})).0,200);
    let raw: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    assert_eq!(raw["default_hub"], "local");
    assert!(raw["hubs"].get("work").is_none());
    assert_eq!(raw["hubs"]["renamed"]["token"], "saved");
    assert_eq!(raw["hubs"]["renamed"]["clientSecret"], "retained");
    assert_eq!(raw["hubs"]["renamed"]["enabled"], false);
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    assert!(!snapshot["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["name"] == "renamed"));
    let h = snapshot["settings"]["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "renamed")
        .unwrap();
    assert_eq!(h["status"], "disconnected");
    assert_eq!(h["connected"], false);
    assert_eq!(f.action(json!({"op":"set_host_hub","enabled":true,"port":0,"bind":"127.0.0.1","auth_mode":"none"})).0,200);
    let running = f.action(json!({"op":"get_snapshot"})).1["settings"]["host_hub"].clone();
    let blocked = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = blocked.local_addr().unwrap().port();
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":true,"port":port}))
            .0,
        400
    );
    let failed = f.action(json!({"op":"get_snapshot"})).1["settings"]["host_hub"].clone();
    assert_eq!(failed["port"], running["port"]);
    assert!(failed["error"].is_string());
    assert_eq!(failed["running"], true);
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":true,"port":0,"auth_mode":"token"}))
            .0,
        200
    );
    let success = f.action(json!({"op":"get_snapshot"})).1["settings"]["host_hub"].clone();
    assert!(success["error"].is_null());
    assert_eq!(success["auth_mode"], "token");
}
#[test]
fn live_user_reload_and_auth_mode_canonicalization_survive_restart() {
    let _lock = serial();
    let mut f =
        Fixture::new(json!({"user":"alice","hubs":{"local":{"url":"local://","authMode":"NONE"}}}));
    f.env.remove("CITO_USER");
    let daemon = f.daemon(&[]);
    let a = enroll(&f, human());
    assert!(a["listener_id"].as_str().unwrap().starts_with("alice-"));
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    config["user"] = json!("bob");
    fs::write(&f.config, config.to_string()).unwrap();
    let b = enroll(&f, human());
    assert!(
        b["listener_id"].as_str().unwrap().starts_with("bob-"),
        "{b}"
    );
    f.stop(daemon);
    f.daemon(&[]);
    assert!(enroll(&f, human())["listener_id"]
        .as_str()
        .unwrap()
        .starts_with("bob-"));
    // Disabled slots lose their in-memory predecessor, but retained bindings still
    // belong to the original principal and URL across edits and restarts.
    assert_eq!(
        f.action(
            json!({"op":"unlisten_session","hub":"local","harness":"human","session_id":"default"})
        )
        .0,
        200
    );
    config["hubs"]["local"]["enabled"] = json!(false);
    fs::write(&f.config, config.to_string()).unwrap();
    f.action(json!({"op":"get_snapshot"}));
    config["user"] = json!("carol");
    config["hubs"]["local"]["enabled"] = json!(true);
    fs::write(&f.config, config.to_string()).unwrap();
    assert!(enroll(&f, human())["listener_id"]
        .as_str()
        .unwrap()
        .starts_with("carol-"));
    assert!(f
        .rows("SELECT status FROM local_listeners WHERE principal_id=\'none#bob\'")
        .iter()
        .all(|r| r["status"] == "FENCED_INACTIVE"));
    config["hubs"]["local"]["enabled"] = json!(false);
    fs::write(&f.config, config.to_string()).unwrap();
    f.action(json!({"op":"get_snapshot"}));
    config["hubs"]["local"]["url"] = json!("http://127.0.0.1:0");
    config["hubs"]["local"]["enabled"] = json!(true);
    fs::write(&f.config, config.to_string()).unwrap();
    f.action(json!({"op":"get_snapshot"}));
    let rows =
        f.rows("SELECT status,hub_url FROM local_listeners WHERE principal_id=\'none#carol\'");
    assert!(!rows.is_empty());
    assert!(rows
        .iter()
        .all(|r| r["status"] == "FENCED_INACTIVE" && r["hub_url"] == "local://"));
}
#[test]
fn ui_invites_and_config_mutations_validate_before_persistent_effects() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let before = fs::read(&f.config).unwrap();
    for invite in ["[]", "42", "true", "\"invite\"", "null"] {
        let (s, v) = f.action(json!({"op":"paste_hub_invite","invite":invite}));
        assert_eq!(s, 400);
        assert_eq!(v["error"], "INVALID_INPUT: Invalid hub invite");
        assert_eq!(fs::read(&f.config).unwrap(), before);
    }
    assert_eq!(f.action(json!({"op":"paste_hub_invite","invite":"{\"name\":\"work\",\"url\":\"local://\",\"authMode\":\"none\"}"})).0,200);
    assert_eq!(
        f.action(json!({"op":"set_auto_listen","hub":"local","enabled":false}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"setup_save_hub","name":"other","url":"local://","auth_mode":"none"}))
            .0,
        200
    );
    let config: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    assert_eq!(config["hubs"]["local"]["autoListen"], false);
    assert_eq!(config["hubs"]["other"]["url"], "local://");
    for (name,invite) in [
        ("remote",format!("http://127.0.0.1:{}",port())),
        ("joined",format!("cito setup --hub joined --url http://127.0.0.1:{} --auth token --token fixture-token --default",port())),
        ("json",json!({"name":"json","url":format!("http://127.0.0.1:{}",port()),"auth_mode":"none"}).to_string()),
        ("equals",format!("cito setup --hub=equals --url=http://127.0.0.1:{} --default",port())),
    ] {
        let (ok,out,err)=f.command(NATIVE,&["setup","--invite",&invite,"--json"],""); assert!(ok,"{out} {err}");
        let config:Value=serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
        assert!(config["hubs"][name].is_object()); assert_eq!(config["default_hub"],name);
    }
    let (status, error) = f.action(json!({"op":"test_hub","hub":"json"}));
    assert_eq!(status, 400);
    let state = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(
        state["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == "json")
            .unwrap()["error"],
        error["error"]
    );
    assert_eq!(
        f.action(json!({"op":"setup_save_hub","edit":true,"original_name":"json","name":"json","url":"local://","auth_mode":"none"}))
            .0,
        200
    );
    assert_eq!(f.action(json!({"op":"test_hub","hub":"json"})).0, 200);
    let state = f.action(json!({"op":"get_snapshot"})).1;
    assert!(state["settings"]["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "json")
        .unwrap()["error"]
        .is_null());

    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":true,"port":0,"bind":"127.0.0.1"}))
            .0,
        200
    );
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    let hosted = &snapshot["settings"]["host_hub"];
    let (ok, out, err) = f.command(NATIVE, &["setup", "--print-invite", "--json"], "");
    assert!(ok, "{out} {err}");
    let printed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(printed["invite_command"], hosted["invite_command"]);
    let (ok, _, err) = f.command(
        NATIVE,
        &[
            "setup",
            "--invite",
            hosted["endpoint_url"].as_str().unwrap(),
        ],
        "",
    );
    assert!(!ok);
    assert!(
        err.contains("This invite code belongs to this Mac's own shared Hub"),
        "{err}"
    );
    let port = hosted["port"].as_u64().unwrap();
    let committed = fs::read(&f.config).unwrap();
    let hostname = f.env["CITO_HOST_NAME"].clone();
    for host in [
        "127.0.0.1",
        "localhost",
        "[::1]",
        &hostname,
        &format!("{}.local", hostname.trim_end_matches(".local")),
    ] {
        let invite =
            json!({"name":"self-invite","url":format!("http://{host}:{port}"),"auth_mode":"none"});
        let (status, error) =
            f.action(json!({"op":"paste_hub_invite","invite":invite.to_string()}));
        assert_eq!(status, 400, "{host}: {error}");
        assert_eq!(error["error"], "INVALID_INPUT: This invite code belongs to this Mac's own shared Hub (already active as 'local'). Paste it in Cito on your other Mac to connect.");
        assert_eq!(fs::read(&f.config).unwrap(), committed);
        let (ok, out, err) = f.command(
            NATIVE,
            &["setup", "--invite", &invite.to_string(), "--json"],
            "",
        );
        assert!(!ok, "{host}: {out}");
        assert!(
            err.contains("This invite code belongs to this Mac's own shared Hub")
                && !err.contains("INVALID_INPUT:"),
            "{host}: {err}"
        );
        assert_eq!(fs::read(&f.config).unwrap(), committed);
    }
    let root = ureq::get(hosted["endpoint_url"].as_str().unwrap())
        .call()
        .unwrap();
    assert_eq!(
        root.header("Content-Type"),
        Some("text/html; charset=utf-8")
    );
    let landing = root.into_string().unwrap();
    assert!(landing.contains("On another Mac"));
    assert!(landing.contains("Join from Clipboard"));
    // The same host on another port remains a valid remote Hub.
    let different = if port == 65535 { port - 1 } else { port + 1 };
    let invite = json!({"name":"different-port","url":format!("http://127.0.0.1:{different}"),"auth_mode":"none"});
    assert_eq!(
        f.action(json!({"op":"paste_hub_invite","invite":invite.to_string()}))
            .0,
        200
    );
}
#[test]
fn r1_background_hub_recovery_clears_transient_errors_but_preserves_validation_errors() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (server, port) = host.hub();
    let f = Fixture::new(
        json!({"hubs":{"shared":{"url":format!("http://127.0.0.1:{port}")}},"defaultHub":"shared"}),
    );
    let recorder = f.recording_ui();
    f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", recorder.to_str().unwrap()),
    ]);
    host.stop(server);
    let (status, error) = f.action(json!({"op":"test_hub","hub":"shared"}));
    assert_eq!(status, 400);
    assert!(error["error"]
        .as_str()
        .unwrap()
        .starts_with("NATS_UNAVAILABLE:"));
    let failed = f.action(json!({"op":"get_snapshot"})).1;
    for hubs in [&failed["hubs"], &failed["settings"]["hubs"]] {
        assert_eq!(
            hubs.as_array()
                .unwrap()
                .iter()
                .find(|h| h["name"] == "shared")
                .unwrap()["error"],
            error["error"]
        );
    }
    let frames_path = f.home.join("ui-frames");
    wait(|| {
        fs::read_to_string(&frames_path)
            .unwrap_or_default()
            .contains(error["error"].as_str().unwrap())
    });
    let before = fs::read_to_string(&frames_path).unwrap().len();
    host.spawn(
        NATIVE,
        &[
            "hub",
            "--bind",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--data-dir",
            host.state.join("hub").to_str().unwrap(),
            "--auth",
            "none",
        ],
        &[],
    );
    // No UI or MCP request drives recovery: the daemon's background transport
    // work must publish a fresh state before we read either projection.
    wait(|| {
        fs::read_to_string(&frames_path)
            .unwrap_or_default()
            .get(before..)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|frame| {
                frame["op"] == "state"
                    && frame["hubs"].as_array().is_some_and(|hubs| {
                        hubs.iter().any(|h| {
                            h["name"] == "shared"
                                && h["status"] == "connected"
                                && h["error"].is_null()
                        })
                    })
            })
    });
    let recovered = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(
        f.action(json!({"op":"open_settings","hub":"shared"})).0,
        200
    );
    let mut settings = Value::Null;
    wait(|| {
        settings = fs::read_to_string(&frames_path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .rfind(|frame| frame["op"] == "settings" && frame["hub"] == "shared")
            .map(|frame| frame["settings"].clone())
            .unwrap_or(Value::Null);
        settings.is_object()
    });
    for hubs in [
        &recovered["hubs"],
        &recovered["settings"]["hubs"],
        &settings["hubs"],
    ] {
        assert!(hubs
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == "shared")
            .unwrap()["error"]
            .is_null());
    }
    let (status, invalid) =
        f.action(json!({"op":"setup_save_hub","name":"shared","url":"invalid"}));
    assert_eq!(status, 400);
    assert!(invalid["error"]
        .as_str()
        .unwrap()
        .starts_with("INVALID_INPUT:"));
    // A healthy connection must not hide a rejected configuration edit, even
    // after another background poll has observed the recovered transport.
    assert_eq!(f.action(json!({"op":"refresh_available"})).0, 200);
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(
        snapshot["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == "shared")
            .unwrap()["error"],
        invalid["error"]
    );
}

#[test]
fn removing_or_disabling_default_hubs_persists_a_restartable_fallback() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for (key, op, remaining, expected) in [
        (
            "default_hub",
            "setup_remove_hub",
            json!({"local":{"url":"local://"}}),
            "local",
        ),
        (
            "defaultHub",
            "remove_hub",
            json!({"local":{"url":"local://"}}),
            "local",
        ),
        ("default_hub", "remove_hub", json!({}), "local"),
        (
            "defaultHub",
            "setup_remove_hub",
            json!({"personal":{"url":"local://","enabled":false},"shared":{"url":"local://"}}),
            "shared",
        ),
        (
            "default_hub",
            "remove_hub",
            json!({"local":{"url":"local://","enabled":false},"shared":{"url":"local://","enabled":true},"zeta":{"url":"local://"}}),
            "shared",
        ),
        (
            "defaultHub",
            "setup_remove_hub",
            json!({"local":{"url":"local://","disabled":true},"shared":{"url":"local://"}}),
            "shared",
        ),
        (
            "default_hub",
            "setup_toggle_hub",
            json!({"local":{"url":"local://"},"shared":{"url":"local://"}}),
            "local",
        ),
        (
            "defaultHub",
            "hub_action",
            json!({"local":{"url":"local://","enabled":false},"shared":{"url":"local://","enabled":true},"zeta":{"url":"local://"}}),
            "shared",
        ),
    ] {
        let mut initial = json!({"hubs":remaining});
        initial["hubs"]["work"] = json!({"url":"local://"});
        initial[key] = json!("work");
        fs::write(&f.config, initial.to_string()).unwrap();
        let daemon = f.daemon(&[]);
        assert_eq!(f.action(json!({"op":"get_snapshot"})).1["hub"], "work");
        let (status, response) =
            f.action(json!({"op":op,"hub":"work","enabled":false,"action":"disconnect"}));
        assert_eq!(status, 200, "{op}: {response}");
        let saved: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
        assert_eq!(saved["default_hub"], expected, "{op}: {saved}");
        assert!(saved.get("defaultHub").is_none());
        if matches!(op, "setup_toggle_hub" | "hub_action") {
            assert_eq!(saved["hubs"]["work"]["enabled"], false);
        } else {
            assert!(saved["hubs"].get("work").is_none());
        }
        assert_eq!(f.action(json!({"op":"get_snapshot"})).1["hub"], expected);
        let (ok, out, err) = f.command(NATIVE, &["setup", "--status", "--json"], "");
        assert!(ok, "{err}");
        let inspection: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(inspection["defaultHub"], expected);
        assert!(inspection["hubs"].get(expected).is_some());
        let (ok, out, err) = f.command(NATIVE, &["find", "--user", "absent", "--json"], "");
        assert!(ok, "{err}");
        let found: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(found["ok"], true);
        assert_eq!(found["listeners"], json!([]));
        f.stop(daemon);
        let restarted = f.daemon(&[]);
        let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
        assert_eq!(status, 200, "{snapshot}");
        assert_eq!(snapshot["hub"], expected);
        f.stop(restarted);
    }
}
#[test]
fn dual_default_hub_aliases_allow_cli_removal_and_restart() {
    let _lock = serial();
    let f = Fixture::new(json!({
        "defaultHub":"work",
        "default_hub":"work",
        "hubs":{"local":{"url":"local://"},"work":{"url":"local://"}}
    }));
    let (ok, out, err) = f.command(
        NATIVE,
        &["setup", "--remove-hub", "work", "--no-login-item", "--json"],
        "",
    );
    assert!(ok, "{out}\n{err}");
    let saved: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    assert_eq!(saved["default_hub"], "local");
    assert!(saved.get("defaultHub").is_none());
    assert!(saved["hubs"].get("work").is_none());
    let (ok, out, err) = f.command(NATIVE, &["setup", "--status", "--json"], "");
    assert!(ok, "{out}\n{err}");
    let inspection: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(inspection["defaultHub"], "local");
    assert_eq!(inspection["hubs"]["local"]["url"], "local://");
    let daemon = f.daemon(&[]);
    let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
    assert_eq!(status, 200, "{snapshot}");
    assert_eq!(snapshot["hub"], "local");
    let (ok, out, err) = f.command(NATIVE, &["find", "--user", "absent", "--json"], "");
    assert!(ok, "{out}\n{err}");
    let found: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(found["ok"], true);
    assert_eq!(found["listeners"], json!([]));
    f.stop(daemon);
}
#[test]
fn reloading_disabled_defaults_selects_enabled_hubs_unless_overridden() {
    let _lock = serial();
    let f = Fixture::new(json!({"default_hub":"work","hubs":{"work":{"url":"local://"}}}));
    let daemon = f.daemon(&[]);
    assert_eq!(f.action(json!({"op":"get_snapshot"})).1["hub"], "work");
    for (key, hubs, expected) in [
        (
            "default_hub",
            json!({"work":{"url":"local://","enabled":false},"local":{"url":"local://"},"alpha":{"url":"local://"}}),
            "local",
        ),
        (
            "defaultHub",
            json!({"work":{"url":"local://","disabled":true},"local":{"url":"local://","enabled":false},"shared":{"url":"local://"},"zeta":{"url":"local://"}}),
            "shared",
        ),
    ] {
        let mut config = json!({"hubs":hubs});
        config[key] = json!("work");
        fs::write(&f.config, config.to_string()).unwrap();
        let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
        assert_eq!(status, 200, "{snapshot}");
        assert_eq!(snapshot["hub"], expected);
        assert!(snapshot["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["enabled"] != false && h["name"] != "work"));
        assert!(snapshot["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["name"] == "work" && h["enabled"] == false));
        let found = f.tool("cito_find", json!({"user":"absent"}), json!({}));
        assert_eq!(found["ok"], true, "{found}");
        assert_eq!(found["listeners"], json!([]));
        assert_eq!(fs::read_to_string(&f.config).unwrap(), config.to_string());
    }
    f.stop(daemon);
    let overridden = f.daemon(&[("CITO_HUB", "work")]);
    assert_eq!(f.action(json!({"op":"get_snapshot"})).1["hub"], "work");
    f.stop(overridden);
}
#[test]
fn disabled_hub_alias_reenable_paths_activate_slots_across_restart() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for op in ["setup_toggle_hub", "hub_action", "cli"] {
        let initial = json!({"defaultHub":"local","hubs":{
            "local":{"url":"local://","disabled":true},
            "work":{"url":"local://","enabled":true,"disabled":true}
        }});
        fs::write(&f.config, initial.to_string()).unwrap();
        let daemon = f.daemon(&[]);
        let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
        assert_eq!(status, 200, "{snapshot}");
        let work = snapshot["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hub| hub["name"] == "work")
            .unwrap();
        assert_eq!(work["enabled"], false, "{op}: {work}");
        assert_eq!(snapshot["hubs"], json!([]));
        assert!(
            !snapshot["settings"]["channels"]
                .as_array()
                .unwrap()
                .is_empty(),
            "disabled local still exposes channel inventory"
        );
        let unavailable = f.tool_result(
            "cito_find",
            json!({"hub":"work","user":"absent"}),
            json!({}),
        );
        assert_eq!(unavailable["isError"], true, "{unavailable}");
        if op == "cli" {
            let (ok, out, err) = f.command(
                NATIVE,
                &["setup", "--enable-hub", "work", "--json", "--no-login-item"],
                "",
            );
            assert!(ok, "{out}\n{err}");
        } else {
            let (status, response) =
                f.action(json!({"op":op,"hub":"work","enabled":true,"action":"reconnect"}));
            assert_eq!(status, 200, "{op}: {response}");
        }
        let saved: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
        assert_eq!(saved["hubs"]["work"]["enabled"], true, "{op}: {saved}");
        assert!(
            saved["hubs"]["work"].get("disabled").is_none(),
            "{op}: {saved}"
        );
        assert_eq!(saved["default_hub"], "work", "{op}: {saved}");
        assert!(saved.get("defaultHub").is_none());
        for restart in [false, true] {
            let running = if restart { f.daemon(&[]) } else { daemon };
            let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
            assert_eq!(status, 200, "{snapshot}");
            assert_eq!(snapshot["hub"], "work", "{op}: {snapshot}");
            let work = snapshot["settings"]["hubs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|hub| hub["name"] == "work")
                .unwrap();
            assert_eq!(work["enabled"], true, "{op}: {work}");
            let found = f.tool(
                "cito_find",
                json!({"hub":"work","user":"absent"}),
                json!({}),
            );
            assert_eq!(found["ok"], true, "{op}: {found}");
            assert_eq!(found["listeners"], json!([]));
            f.stop(running);
        }
    }
}
#[test]
fn disabled_hub_alias_saves_preserve_effective_enablement() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"local":{"url":"local://"}}}));
    f.daemon(&[]);
    for op in ["setup_save_hub", "save_hub"] {
        for (fields, enabled) in [
            (json!({"disabled":true}), false),
            (json!({"enabled":true,"disabled":true}), false),
            (json!({"enabled":false,"disabled":false}), false),
            (json!({"enabled":true,"disabled":false}), true),
        ] {
            let mut cfg = fields;
            cfg["url"] = json!("local://");
            let action = if op == "setup_save_hub" {
                cfg["op"] = json!(op);
                cfg["name"] = json!("work");
                cfg
            } else {
                json!({"op":op,"name":"work","config":cfg})
            };
            let (status, response) = f.action(action.clone());
            assert_eq!(status, 200, "{action}: {response}");
            let saved: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
            assert_eq!(
                saved["hubs"]["work"]["enabled"], enabled,
                "{action}: {saved}"
            );
            assert!(
                saved["hubs"]["work"].get("disabled").is_none(),
                "{action}: {saved}"
            );
            let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
            assert_eq!(status, 200, "{snapshot}");
            let work = snapshot["settings"]["hubs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|hub| hub["name"] == "work")
                .unwrap();
            assert_eq!(work["enabled"], enabled, "{action}: {work}");
            let result = f.tool_result(
                "cito_find",
                json!({"hub":"work","user":"absent"}),
                json!({}),
            );
            assert_eq!(result["isError"] == true, !enabled, "{action}: {result}");
        }
    }
}
#[test]
fn hub_saves_reject_invalid_candidates_without_poisoning_daemon() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"local":{"url":"local://"}}}));
    let daemon = f.daemon(&[]);
    let before = fs::read(&f.config).unwrap();
    let invalid = [
        json!({"user":"alice smith"}),
        json!({"user":""}),
        json!({"user":"a".repeat(65)}),
        json!({"user":"alice@work"}),
        json!({"user":42}),
        json!({"user":false}),
        json!({"user":[]}),
        json!({"user":{}}),
        json!({"authMode":"token"}),
        json!({"authMode":"token","token":"   "}),
        json!({"authMode":"token","token":42}),
        json!({"authMode":"oidc","client_id":"client"}),
        json!({"authMode":"oidc","issuer":"http://id.example.com","client_id":"client"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com?query=1","client_id":"client"}),
        json!({"authMode":"oidc","issuer":42,"client_id":"client"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","client_id":"   "}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":42}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","scopes":"openid"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","scopes":["profile"]}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","scopes":["openid","invalid scope"]}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","redirectPort":65536}),
    ];
    for op in ["setup_save_hub", "save_hub"] {
        for fields in &invalid {
            let mut cfg = json!({"url":"local://","auth_mode":"none","set_default":true});
            cfg.as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let action = if op == "setup_save_hub" {
                cfg["op"] = json!(op);
                cfg["name"] = json!("work");
                cfg
            } else {
                json!({"op":op,"name":"work","config":cfg})
            };
            let (status, response) = f.action(action.clone());
            assert_eq!(status, 400, "{action}: {response}");
            assert!(
                response["error"]
                    .as_str()
                    .unwrap()
                    .starts_with("INVALID_INPUT:"),
                "{response}"
            );
            assert_eq!(fs::read(&f.config).unwrap(), before, "{action}");
            let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
            assert_eq!(status, 200, "{snapshot}");
            assert_eq!(snapshot["hub"], "local");
            assert_eq!(snapshot["activity"], json!([]));
            assert_eq!(snapshot["unread_count"], 0);
        }
    }
    for user in [
        Value::Null,
        json!("a"),
        json!("Alice.Smith_1-"),
        json!("a".repeat(64)),
    ] {
        let (status, response) = f.action(
            json!({"op":"save_hub","name":"work","url":"local://","auth_mode":"none","user":user}),
        );
        assert_eq!(status, 200, "{response}");
        let config: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
        assert_eq!(config["hubs"]["work"]["user"], user);
    }
    for fields in [
        json!({"authMode":"token","token":"test-token"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","client_id":"client","scopes":["openid","profile"]}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","redirectPort":0}),
        json!({"authMode":"oidc","oidcIssuer":"https://id.example.com","oidcClientId":"client","oidcScopes":["openid"]}),
    ] {
        let mut cfg = fields.clone();
        cfg["url"] = json!("local://");
        cfg["enabled"] = json!(false);
        let (status, response) =
            f.action(json!({"op":"setup_save_hub","name":"work","config":cfg}));
        assert_eq!(status, 200, "{response}");
        let config: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
        for (key, value) in fields.as_object().unwrap() {
            let canonical = match key.as_str() {
                "client_id" | "oidcClientId" => "clientId",
                "oidcIssuer" => "issuer",
                "oidcScopes" => "scopes",
                "redirect_port" | "oidcRedirectPort" => "redirectPort",
                "auth_mode" | "auth" => "authMode",
                key => key,
            };
            assert_eq!(&config["hubs"]["work"][canonical], value);
        }
        for key in [
            "client_id",
            "oidcIssuer",
            "oidcClientId",
            "oidcScopes",
            "redirect_port",
            "oidcRedirectPort",
            "auth_mode",
            "auth",
            "request_id",
        ] {
            assert!(config["hubs"]["work"].get(key).is_none());
        }
        let (ok, out, err) = f.command(NATIVE, &["setup", "--status", "--json"], "");
        assert!(ok, "{fields}: {err}");
        let inspection: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(inspection["hubs"]["work"]["authMode"], fields["authMode"]);
        assert_eq!(inspection["hubs"]["work"]["enabled"], false);
        let (ok, out, err) = f.command(NATIVE, &["find", "--user", "absent", "--json"], "");
        assert!(ok, "{fields}: {err}");
        let found: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(found["ok"], true);
        assert_eq!(found["listeners"], json!([]));
    }
    f.stop(daemon);
    f.daemon(&[]);
    assert_eq!(
        f.action(json!({"op":"set_auto_listen","hub":"local","enabled":false}))
            .0,
        200
    );
    let config: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    assert_eq!(config["hubs"]["local"]["autoListen"], false);
}
