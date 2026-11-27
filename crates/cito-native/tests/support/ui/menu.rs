use super::*;

#[test]
fn appearance_overrides_are_scoped_to_one_observation() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, json!({"op":"test_appearance","appearance":"light"}));
    settlement::open_settings(&f, p, &settings(), "apps");

    let track = |snapshot: Value| snapshot.menu("header_hub_strip");
    let baseline = track(inspect(&f, p));
    assert!(baseline["background"][0].as_f64().unwrap() < 0.1);
    let png = |name: &str, appearance| {
        let path = f.home.join(name);
        capture_png(&f, p, "settings", &path, appearance, None);
        fs::read(path).unwrap()
    };
    let light = png("light.png", None);
    for op in ["test_inspect", "test_inspect_menu"] {
        let dark = track(inspect_command(&f, p, op, Some("dark")));
        assert!(dark["background"][0].as_f64().unwrap() > 0.9, "{op}");
        assert_eq!(dark["instance"], baseline["instance"], "{op}");
        let after = track(inspect(&f, p));
        assert_eq!(after["instance"], baseline["instance"], "{op}");
        assert_eq!(after["background"], baseline["background"], "{op}");
        assert!(png(&format!("after_{op}.png"), None) == light, "{op}");
    }

    let dark = png("dark_observation.png", Some("dark"));
    assert!(dark != light, "The PNG override must be visible");
    assert!(png("after_png.png", None) == light, "PNG override leaked");
    f.send(p, json!({"op":"test_appearance","appearance":"dark"}));
    assert_eq!(track(inspect(&f, p))["background"][0], 1.0);
    assert!(png("dark_baseline.png", None) == dark);
}

#[test]
fn pin_states_follow_hub_mode_and_idle_capability_without_squeezing_the_title() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let id = "personal:repo-abcdefghijkl";
    let title_text = "A deliberately long conversation title";
    for appearance in ["light", "dark"] {
        for hub_auto in [false, true] {
            for mode in ["auto", "manual"] {
                for timed in [false, true] {
                    let mut frame = state();
                    frame["hubs"][0]["auto_listen"] = json!(hub_auto);
                    frame["listeners"][0]["listen_mode"] = json!(mode);
                    frame["listeners"][0]["title"] = json!(title_text);
                    // Deliberately disagree with pid_bound to prove capability is independent.
                    frame["harnesses"][0]["pid_bound"] = json!(timed);
                    frame["harnesses"][0]["idle_timeout_ms"] =
                        if timed { json!(3_600_000) } else { Value::Null };
                    f.send(p, frame);
                    let s = inspect_as(&f, p, Some(appearance));
                    let pin = s.menu(&format!("pin:{id}"));
                    if !hub_auto && mode == "manual" {
                        assert!(pin.is_null());
                        continue;
                    }
                    assert_eq!(pin["width"], 22);
                    assert_eq!(pin["height"], 22);
                    assert_eq!(
                        pin["symbol"],
                        if mode == "auto" { "pin" } else { "pin.fill" }
                    );
                    assert_eq!(pin["border_width"], if mode == "auto" { 0 } else { 1 });
                    assert_eq!(
                        pin["accessibility_label"],
                        if mode == "auto" {
                            "Pin session"
                        } else {
                            "Unpin session"
                        }
                    );
                    let tooltip = pin["tooltip"].as_str().unwrap();
                    assert!(tooltip.contains(
                        "Pinned sessions stay listening when Auto-Listen is turned off."
                    ));
                    assert!(tooltip.contains(if timed {
                        "auto-retire after the idle timeout"
                    } else {
                        "endpoint is available; no idle timeout applies"
                    }));
                    let card = s.menu(&format!("session:{id}"));
                    let title = nodes(&card["children"])
                        .into_iter()
                        .find(|n| n["text"] == title_text)
                        .unwrap();
                    let rename = s.menu(&format!("rename:{id}"));
                    let title_end = title["x"].as_f64().unwrap() + title["width"].as_f64().unwrap();
                    let rename_x = rename["x"].as_f64().unwrap();
                    assert_eq!(rename_x - title_end, 6.0);
                    assert!(
                        rename_x + rename["width"].as_f64().unwrap() <= pin["x"].as_f64().unwrap(),
                        "title={title}; rename={rename}; pin={pin}; mode={mode}, auto={hub_auto}"
                    );
                    let scope = s.menu(&format!("scope:{id}"));
                    assert_eq!(scope["width"], 68);
                    assert!(s.menu(&format!("stop:{id}"))["width"].as_f64().unwrap() >= 44.0);
                    if mode == "manual" {
                        let tint = pin["children"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find_map(|n| n["tint"].as_array())
                            .unwrap();
                        assert!(
                            tint[2].as_f64().unwrap() > tint[0].as_f64().unwrap(),
                            "Pinned icon uses the blue info tint"
                        );
                    }
                    control(&f, p, &format!("pin:{id}"));
                    inspect(&f, p);
                    let action = f.frames_for(p, "set_listen_mode").next_back().unwrap();
                    assert_eq!(action["hub"], "personal");
                    assert_eq!(action["listener_id"], "c_abcdefghijkl");
                    assert_eq!(
                        action["listen_mode"],
                        if mode == "auto" { "manual" } else { "auto" }
                    );
                }
            }
        }
    }
}

#[test]
fn pencil_stays_next_to_a_short_title() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    frame["hubs"][0]["auto_listen"] = json!(true);
    frame["listeners"][0]["title"] = json!("Fix");
    f.send(p, frame);
    let s = inspect(&f, p);
    let card = s.menu("session:personal:repo-abcdefghijkl");
    let title = nodes(&card["children"])
        .into_iter()
        .find(|n| n["text"] == "Fix")
        .unwrap();
    let rename = s.menu("rename:personal:repo-abcdefghijkl");
    assert_eq!(title["width"], 44);
    assert_eq!(
        rename["x"].as_f64().unwrap(),
        title["x"].as_f64().unwrap() + 50.0
    );
}

#[test]
fn errors_unwrap_protocol_diagnostics_preserve_word_boundaries_and_expand_discovery() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    for (raw, expected) in [
        ("ACTIVE / UNLISTENED / DISCONNECTED_RETAINED / EXPIRED; INACTIVE / EXPIRED_UNSENT", "Active / Not listening / Temporarily offline / Offline; INACTIVE / EXPIRED_UNSENT"),
        ("personal: fixture_plugin: Discovery rejected: 10071: Cannot scan; BADGE_ERROR: Retry (os error 13)", "Cannot scan; Retry"),
        ("Badge rejected: 10037: Missing endpoint", "Missing endpoint"),
        ("Refresh failed: 10059: Not available; PLUGIN_EXEC_FAILED: Retry", "Refresh failed: Not available; Retry"),
        (r#"Discovery rejected: {"error_code":"PLUGIN_EXEC_FAILED","message":"10071: Cannot scan"}"#, "Cannot scan"),
        (r#"{"error":{"code":"FAILED","message":"Badge rejected: Cannot scan"}}"#, "Cannot scan"),
        (r#"{"error_code":"FAILED"}"#, "The request failed. Try again."),
        (r#"{"error_code": broken"#, "The request failed. Try again."),
        ("fixture: SQLITE_CANTOPEN: cannot open", "App database is unavailable. Check the app and its data location."),
        ("Discovery rejected: unable to open database file (SQLite 14)", "App database is unavailable. Check the app and its data location."),
        ("fixture: sqlite3 failed", "App database is unavailable. Check the app and its data location."),
        ("fixture: database is locked", "App database is unavailable. Check the app and its data location."),
    ] {
        let mut frame = state();
        frame["hubs"][0]["error"] = json!(raw);
        send_companion(&f, p, frame);
        let s = inspect(&f, p);
        assert_eq!(s.menu("hub_error:personal")["text"], format!("personal · {expected}"), "{raw}");
    }
    let vectors: Vec<[String; 2]> =
        serde_json::from_str(include_str!("../diagnostics.json")).unwrap();
    for [raw, expected] in vectors {
        let mut frame = state();
        frame["hubs"][0]["error"] = json!(raw);
        send_companion(&f, p, frame);
        let s = inspect(&f, p);
        assert_eq!(
            s.menu("hub_error:personal")["text"],
            format!("personal · {expected}"),
            "{raw}"
        );
    }
    let clean = "Cannot scan the session directory. Check the app data location and permissions, then refresh discovery to try again. ".repeat(4);
    for appearance in ["light", "dark"] {
        let mut frame = state();
        frame["discovery"] = json!([{"harness":"fixture","status":"error","error":format!("fixture: Discovery rejected: 10071: {clean}")}]);
        send_companion(&f, p, frame);
        let s = inspect_as(&f, p, Some(appearance));
        let row = s.menu("discovery:fixture");
        let label = nodes(&row["children"])
            .into_iter()
            .find(|n| {
                n["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Fixture · Cannot scan"))
            })
            .unwrap();
        assert_eq!(label["width"], 322);
        assert_eq!(label["tooltip"], format!("Fixture · {}", clean.trim()));
        assert!(label["height"].as_f64().unwrap() > 26.0);
        assert!(row["height"].as_f64().unwrap() >= label["height"].as_f64().unwrap() + 8.0);
    }
}

#[test]
fn hub_drawer_renders_the_presented_error_as_visible_text() {
    let _lock = serial();
    let (f, ui) = ui_fixture();
    let mut frame = state();
    frame["hubs"][0]["error"] = json!("personal: FAILED: Port already in use (os error 48)");
    send_companion(&f, ui, frame);
    let snapshot = inspect(&f, ui);
    let drawer = snapshot.menu("drawer:personal");
    assert_eq!(drawer["hidden"], false);
    assert!(nodes(&drawer["children"])
        .iter()
        .any(|n| n["text"] == "personal · Port already in use" && n["hidden"] != true));
    let mut frame = state();
    frame["hubs"][0]["error"] = json!("INVALID_INPUT: Hub URL must use HTTP or HTTPS");
    send_companion(&f, ui, frame);
    let snapshot = inspect(&f, ui);
    assert_eq!(
        snapshot.menu("hub_error:personal")["text"],
        "personal · Enter a Hub URL using HTTP or HTTPS, without credentials, query, or fragment."
    );
}
#[test]
fn stop_all_real_daemon_receipts_settle_mutation_and_idempotent_success() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    let mut cycle = ClosedLoop::new(&f);
    f.tool("cito_listen", json!({"about":"Stop this session"}), meta);
    cycle.until(|s| !s.menu("stop_all").is_null());
    cycle.control("stop_all");
    wait(|| {
        cycle.pump();
        cycle.frames.iter().any(|v| {
            v["source_op"] == "unlisten_all" && v["ok"] == true && v["request_id"].is_string()
        })
    });
    let action = cycle
        .results
        .iter()
        .find(|(a, _, _)| a["op"] == "unlisten_all")
        .unwrap();
    assert_eq!(action.1, 200);
    assert_eq!(action.2["removed"], 1);
    assert!(cycle
        .frames
        .iter()
        .any(|frame| frame["source_op"] == "unlisten_all"
            && frame["request_id"] == action.0["request_id"]));
    let (status, result) =
        f.action(json!({"op":"unlisten_all","hub":"local","request_id":"stop-noop"}));
    assert_eq!(status, 200);
    assert_eq!(result["removed"], 0);
    wait(|| {
        cycle.pump();
        cycle.frames.iter().any(|v| {
            v["source_op"] == "unlisten_all" && v["ok"] == true && v["request_id"] == "stop-noop"
        })
    });
}
#[test]
fn v41_menu_pid_bound_group_counts_discovery_and_scoped_pending_stop() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    frame["hubs"][0]["auto_listen"] = json!(true);
    frame["harnesses"][0]["pid_bound"] = json!(false);
    frame["available"] =
        json!([{"harness":"fixture","session_id":"recent","title":"Recent session","cwd":"/tmp"}]);
    send_companion(&f, p, frame.clone());
    let s = inspect(&f, p);
    assert_eq!(s.menu("toggle_group:fixture")["text"], "Fixture · 1 open");
    assert!(!s.menu("candidate:fixture:recent").is_null());
    control(&f, p, "toggle_group:fixture");
    control(&f, p, "target_hub:work");
    assert!(!inspect(&f, p).menu("candidate:fixture:recent").is_null());
    control(&f, p, "target_hub:personal");
    assert!(inspect(&f, p).menu("candidate:fixture:recent").is_null());
    frame["harnesses"][0]["pid_bound"] = json!(true);
    frame["available"][0]["endpoint_pid"] = json!(123);
    frame["available"].as_array_mut().unwrap().push(
        json!({"harness":"fixture","session_id":"older","title":"Older session","cwd":"/tmp"}),
    );
    frame["discovery"] = json!([{"harness":"fixture","status":"scanning"}]);
    send_companion(&f, p, frame.clone());
    let s = inspect(&f, p);
    assert_eq!(
        s.menu("toggle_group:fixture")["text"],
        "Fixture · 2 sessions"
    );
    assert!(nodes(&s.menu("discovery:fixture")["children"])
        .iter()
        .any(|n| n["text"] == "Fixture · Scanning sessions…"));
    frame["available"] = json!([]);
    frame["discovery"] = json!([{"harness":"fixture","status":"error","error":"personal: PLUGIN_EXEC_FAILED: Cannot scan (os error 13)"}]);
    send_companion(&f, p, frame.clone());
    let s = inspect(&f, p);
    assert!(s.menu("available_empty_target").is_null());
    assert!(nodes(&s.menu("discovery:fixture")["children"])
        .iter()
        .any(|n| n["text"] == "Fixture · Cannot scan"));
    control(&f, p, "stop_all");
    let s = inspect(&f, p);
    assert_eq!(s.menu("stop_all")["text"], "Stopping…");
    assert_eq!(s.menu("stop_all")["enabled"], false);
    let action = f.frames_for(p, "unlisten_all").next_back().unwrap();
    assert_eq!(action["hub"], "personal");
    frame["source_op"] = json!("unlisten_all");
    frame["request_id"] = action["request_id"].clone();
    send_companion(&f, p, frame);
    assert_eq!(inspect(&f, p).menu("stop_all")["enabled"], true);
}

#[test]
fn c1_c2_human_only_counter_and_real_disable_enable_normalize_title() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let mut l = ClosedLoop::new(&f);
    let s = l.until(|s| {
        nodes(&s["items"])
            .iter()
            .any(|n| n["text"] == "Personal inbox active")
    });
    assert_eq!(s["count"], 0);
    assert_eq!(s["button_title"], "");
    assert_eq!(s.menu("header_subtitle")["text"], "Personal inbox active");
    assert!(s.menu("header_hub_strip").is_null());
    assert!(s.menu("drawer:local").is_null());
    assert!(s.menu("stop_all").is_null());
    assert_eq!(s.menu("open_settings")["x"], 12);
    let quit = s.menu("quit");
    assert_eq!(
        quit["x"].as_f64().unwrap() + quit["width"].as_f64().unwrap(),
        368.0
    );
    assert_eq!(
        f.rows("SELECT title, about FROM local_listeners WHERE harness='human'"),
        vec![json!({"title":"Human","about":"Human"})]
    );
    let id = f.rows("SELECT public_id FROM local_listeners WHERE harness='human'")[0]["public_id"]
        .as_str()
        .unwrap()
        .to_string();
    l.control("open_settings");
    l.control("settings_tab:channels");
    let s = inspect(&f, l.ui);
    let row_id = "inbox_control_bar";
    let resting_height = s.settings(row_id)["height"].as_f64().unwrap();
    assert!(resting_height >= 48.0);
    let ready = l.until(|s| s.settings("toggle_inbox")["enabled"] == true);
    let pause = ready.settings("toggle_inbox");
    assert_eq!(pause["text"], "Pause Inbox");
    assert_eq!(
        pause["tooltip"],
        "Pause inbox alerts on local across all devices and external channels"
    );
    assert!(s.menu(&format!("switch_hub:{id}")).is_null());
    l.control("toggle_inbox");
    l.until(|s| s.settings("toggle_inbox")["enabled"] == true);
    l.control("toggle_inbox");
    let s = l.until(|s| s.settings("toggle_inbox")["text"] == "Pause Inbox");
    assert_eq!(s.settings(row_id)["height"], resting_height);
    assert!(nodes(&s["settings_items"])
        .iter()
        .any(|n| n["text"] == "You (@alice)"));
    assert_eq!(
        f.rows("SELECT title FROM local_listeners WHERE harness='human'")[0]["title"],
        "Human"
    );
    f.action_ok(json!({"op":"setup_save_hub","name":"archive","name_explicit":true,"url":"https://hub.example.com","auth_mode":"none","enabled":false}));
    l.control("open_settings");
    l.control("settings_tab:hubs");
    let s = l.until(|s| !s.settings("settings_hub:archive").is_null());
    assert!(s.settings("toggle_hub:local").is_null());
    assert_eq!(s.settings("toggle_hub:archive")["text"], "Enable");
    assert!(s.menu("hub:archive").is_null());
    let edit = s.settings("edit_hub:local");
    assert_eq!(
        edit["x"].as_f64().unwrap() + edit["width"].as_f64().unwrap(),
        s.settings("settings_hub:local")["width"].as_f64().unwrap() - 12.0
    );
    l.control("edit_hub:local");
    assert_eq!(inspect(&f, l.ui).settings("submit_add_hub")["text"], "Save");
    l.control("cancel_add_hub");
    l.control("settings_tab:channels");
    let channels = inspect(&f, l.ui);
    assert!(channels.settings("channel_hub_selector:archive").is_null());
    l.control("close_settings");
    for appearance in ["light", "dark"] {
        let s = inspect_as(&f, l.ui, Some(appearance));
        assert_eq!(s["low_contrast_labels"], json!([]), "{appearance}");
        render_png_as(
            &f,
            l.ui,
            "menu",
            &format!("c2_human_reenabled_{appearance}.png"),
            Some(appearance),
        );
    }
    let denied = f.tool_result(
        "cito_listen",
        json!({"about":"Human receiver"}),
        serde_json::from_str(HUMAN).unwrap(),
    );
    assert_eq!(denied["isError"], true);
    let denied: Value =
        serde_json::from_str(denied["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(denied["error"]["code"], "FORBIDDEN");
    l.control("open_directory");
    let s = l.until(|s| !s.directory(&format!("dir_title:{id}")).is_null());
    assert_eq!(
        s.directory(&format!("dir_title:{id}"))["text"],
        "@alice (You)"
    );
    assert_eq!(
        s.directory(&format!("dir_meta:{id}"))["text"],
        "Inbox · mac-studio"
    );
    f.action_ok(json!({"op":"human_inbox_update","hub":"local","about":"Release approvals"}));
    let revision = f.snapshot()["hubs"][0]["human_control_revision"].clone();
    wait(|| {
        l.pump();
        l.frames
            .iter()
            .rev()
            .find(|frame| frame["op"] == "state")
            .is_some_and(|frame| frame["hubs"][0]["human_control_revision"] == revision)
    });
    l.control("open_settings");
    l.control("settings_tab:channels");
    l.control("toggle_inbox");
    let paused = l.until(|s| s.settings("toggle_inbox")["text"] == "Resume Inbox");
    assert_eq!(
        paused.menu("inbox_paused_notice")["text"],
        "Inbox paused · Resume in Settings…"
    );
    let snapshot = f.snapshot();
    assert_eq!(snapshot["hubs"][0]["human_paused"], true);
    assert_eq!(snapshot["hubs"][0]["human_about"], "Release approvals");
    l.control("open_settings");
    l.control("settings_tab:channels");
    let paused_settings = l.until(|s| !s.settings("inbox_control_bar").is_null());
    assert_eq!(paused_settings["low_contrast_labels"], json!([]));
    l.control("toggle_inbox");
    let resumed = l.until(|s| {
        let toggle = s.settings("toggle_inbox");
        toggle["text"] == "Pause Inbox" && toggle["enabled"] == true
    });
    assert_eq!(resumed.settings(row_id)["height"], resting_height);
    l.control("close_settings");
    assert_eq!(f.snapshot()["hubs"][0]["human_about"], "Release approvals");
}

#[test]
fn c1_c3_c5_agent_counter_semantic_contrast_and_explicit_entries_in_both_appearances() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut human = listener();
    human["harness"] = json!("human");
    human["session_id"] = json!("default");
    human["title"] = json!(" ");
    human["about"] = json!("Human receiver");
    human["scoped_id"] = json!("personal:human");
    human["user"] = json!("bob");
    human["display_address"] = json!("@bob");
    let mut frame = state_with_settings(settings());
    frame["listeners"] = json!([]);
    f.send(p, frame.clone());
    let idle = inspect(&f, p);
    assert_eq!(idle["button_title"], "");
    assert_eq!(idle.menu("header_subtitle")["text"], "Not listening");
    for user in ["alice", "bob"] {
        human["user"] = json!(user);
        human["display_address"] = json!(format!("@{user}"));
        frame["listeners"] = json!([human.clone()]);
        frame["hubs"][0]["user"] = json!(user);
        frame["settings"]["hubs"][0]["user"] = json!(user);
        frame["hubs"][0]["human_paused"] = json!(false);
        frame["hubs"][0]["human_availability"] = json!("ACTIVE");
        frame["hubs"][0]["human_state_status"] = json!("current");
        frame["hubs"][0]["human_control_revision"] = json!(1);
        f.send(p, frame.clone());
        let s = inspect(&f, p);
        assert_eq!(s["count"], 0);
        assert_eq!(s["button_title"], "");
        assert!(s.menu("human_inbox_card").is_null());
        control(&f, p, "open_settings");
        control(&f, p, "settings_tab:channels");
        let s = inspect(&f, p);
        let row = s.settings("inbox_control_bar");
        assert!(row["height"].as_f64().unwrap() >= 48.0);
        let title = nodes(&row["children"])
            .into_iter()
            .find(|n| n["text"] == format!("You (@{user})"))
            .unwrap();
        assert_eq!(title["hidden"], false);
    }
    frame["unread_count"] = json!(3);
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p)["button_title"], " 3");
    let mut second = listener();
    second["hub"] = json!("work");
    second["scoped_id"] = json!("work:copy");
    second["display_address"] = json!("work:alice/fixture#abcd");
    frame["listeners"] = json!([human, listener(), second]);
    f.send(p, frame.clone());
    let s = inspect(&f, p);
    assert_eq!(s["count"], 1);
    assert_eq!(s["button_title"], " 3");
    assert_eq!(s.menu("header_subtitle")["text"], "1 session listening");
    let strip = s.menu("header_hub_strip");
    for hub in ["personal", "work"] {
        let pill = s.menu(&format!("hub:{hub}"));
        assert!(pill["x"].as_f64().unwrap() >= 0.0);
        assert!(
            pill["x"].as_f64().unwrap() + pill["width"].as_f64().unwrap()
                <= strip["width"].as_f64().unwrap()
        );
    }
    frame["unread_count"] = json!(0);
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p)["button_title"], "");
    assert_eq!(
        inspect(&f, p).menu("activity_unread_badge")["text"],
        "History"
    );
    frame["unread_count"] = json!(3);
    frame["available"] = json!([{"harness":"fixture","session_id":"pending","title":"Pending bridge","cwd":"/tmp","needs_reload":true}]);
    f.send(p, frame);
    control(&f, p, "open_directory");
    assert_eq!(
        inspect(&f, p)["directory_mode"],
        "directory",
        "Unread signals cannot hijack Directory entry"
    );
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"online_count":2}),
    );
    for appearance in ["light", "dark"] {
        control(&f, p, "copy:personal:repo-abcdefghijkl");
        let s = inspect_as(&f, p, Some(appearance));
        assert_eq!(
            s["low_contrast_labels"],
            json!([]),
            "menu/directory {appearance}"
        );
        let copied = s.menu("copy:personal:repo-abcdefghijkl");
        let expected_red = if appearance == "light" { 0.04 } else { 0.28 };
        assert!((copied["foreground"][0].as_f64().unwrap() - expected_red).abs() < 0.001);
        let dir = s.menu("open_directory");
        let act = s.menu("open_activity");
        assert_eq!(
            (
                dir["x"].clone(),
                dir["width"].clone(),
                dir["height"].clone()
            ),
            (json!(12), json!(174), json!(28))
        );
        assert_eq!(
            (act["x"].clone(), act["width"].clone()),
            (json!(194), json!(174))
        );
        assert_eq!(dir["text"], "Directory…");
        assert_eq!(act["text"], "Activity…");
        assert_eq!(act["symbol"], "bell.badge.fill");
        render_png_as(
            &f,
            p,
            "menu",
            &format!("c3_menu_{appearance}.png"),
            Some(appearance),
        );
        render_png_as(
            &f,
            p,
            "directory",
            &format!("c3_directory_{appearance}.png"),
            Some(appearance),
        );
        control(&f, p, "open_settings");
        for tab in ["apps", "channels", "hubs"] {
            control(&f, p, &format!("settings_tab:{tab}"));
            let s = inspect_as(&f, p, Some(appearance));
            assert_eq!(
                s["low_contrast_labels"],
                json!([]),
                "settings/{tab} {appearance}"
            );
            render_png_as(
                &f,
                p,
                "settings",
                &format!("c3_settings_{tab}_{appearance}.png"),
                Some(appearance),
            );
        }
        f.send(
            p,
            json!({"op":"error","source_op":"hub_action","error":"Connection unavailable"}),
        );
        let s = inspect_as(&f, p, Some(appearance));
        assert!(!s.settings("settings_error_banner").is_null());
        assert_eq!(
            s["low_contrast_labels"],
            json!([]),
            "settings error {appearance}"
        );
        control(&f, p, "dismiss_settings_error");
        control(&f, p, "close_settings");
        f.send(
            p,
            json!({"op":"error","source_op":"hub_action","error":"Connection unavailable"}),
        );
        let s = inspect_as(&f, p, Some(appearance));
        assert_eq!(s["settings_open"], false);
        assert!(s.settings("settings_error_banner").is_null());
        let row = s.menu("menu_error_row");
        let label = &row["children"][0];
        assert_eq!(label["text"], "Connection unavailable");
        assert_eq!(label["wraps"], true);
        assert_eq!(label["maximum_lines"], 0);
        assert_eq!(
            s["low_contrast_labels"],
            json!([]),
            "menu error {appearance}"
        );
        control(&f, p, "dismiss_menu_error");
        assert!(inspect(&f, p).menu("menu_error_row").is_null());
    }
    control(&f, p, "open_activity");
    assert_eq!(inspect(&f, p)["directory_mode"], "activity");
    assert!(f
        .frames(p)
        .iter()
        .any(|v| v["op"] == "open_directory" && v["mode"] == "activity"));
    assert!(f
        .frames(p)
        .iter()
        .any(|v| v["op"] == "open_directory" && v["mode"] == "directory"));
}

#[test]
fn dynamic_identity_and_error_labels_keep_full_tooltips_without_layout_overflow() {
    let _lock = serial();
    for length in [40, 64] {
        let (f, p) = ui_fixture();
        let handle = "w".repeat(length);
        let hub = "h".repeat(40);
        let display_name = "W".repeat(length);
        let clean_error = "Long diagnostic with recovery context. "
            .repeat(10)
            .trim_end()
            .to_owned();
        let error = format!("INVALID_INPUT: {clean_error} (os error 13)");
        let mut cfg = settings();
        cfg["settings_hub"] = json!(hub);
        cfg["hubs"][0]["name"] = json!(hub);
        cfg["hubs"][0]["user"] = json!(handle);
        cfg["channels"][0]["display_name"] = json!(display_name);
        cfg["channels"][0]["display_label"] = json!(handle);
        cfg["channels"][0]["fields"][1]["value"] = json!(handle);
        cfg["channels"][0]["fields"][1]["configured"] = json!(true);
        cfg["channels"][0]["configured"] = json!(true);
        cfg["channels"][0]["status"] = json!("ready");
        for app in cfg["apps"].as_array_mut().unwrap() {
            app["display_name"] = json!(display_name);
            app["detail"] = json!(error);
            app["paths"] = json!([]);
        }
        let mut frame = state();
        frame["hubs"][0]["name"] = json!(hub);
        frame["hubs"][0]["user"] = json!(handle);
        frame["listeners"] = json!([]);
        frame["settings"] = cfg;
        frame["harnesses"][0]["display_name"] = json!(display_name);
        frame["available"] =
            json!([{"harness":"fixture","session_id":"s2","title":"Session","cwd":"/tmp"}]);
        send_companion(&f, p, frame.clone());
        // Extreme input intentionally truncates. The strict V29 oracle must detect it
        // while every geometry, contrast, jargon, and address invariant stays intact.
        let stress_inspect = || {
            let snapshot = inspect_unchecked(&f, p, None);
            for key in [
                "overlapping_siblings",
                "clipped_views",
                "low_contrast_labels",
                "jargon_leaks",
                "address_mismatches",
            ] {
                assert_eq!(snapshot[key], json!([]), "{length}-character stress: {key}");
            }
            assert!(!snapshot["truncated_labels"].as_array().unwrap().is_empty());
            snapshot
        };
        let tooltip = |snapshot: &Value, surface: &str, text: &str| {
            assert!(
                nodes(&snapshot[surface])
                    .iter()
                    .any(|n| n["text"] == text && n["tooltip"] == text),
                "Missing full text/tooltip on {surface}: {text}"
            );
        };
        stress_inspect();
        let mut active = listener();
        active["hub"] = json!(hub);
        frame["listeners"] = json!([active]);
        send_companion(&f, p, frame.clone());
        send_companion(&f, p, json!({"op":"error","error":error}));
        let s = stress_inspect();
        tooltip(&s, "items", &clean_error);
        assert!(s.menu("switch_hub:personal:repo-abcdefghijkl").is_null());
        let pill = s.menu(&format!("hub:{hub}"));
        assert!(pill["tooltip"].as_str().unwrap().contains(&hub));
        assert!(pill["tooltip"].as_str().unwrap().contains("Connected"));
        control(&f, p, "open_settings");
        let s = stress_inspect();
        tooltip(&s, "settings_items", &display_name);
        let app_status = s.settings("settings_app_status:pi");
        assert_eq!(app_status["tooltip"], clean_error);
        assert_eq!(app_status["text"], format!("Connected · {clean_error}"));
        control(&f, p, "settings_tab:channels");
        let s = stress_inspect();
        tooltip(&s, "settings_items", &format!("You (@{handle})"));
        tooltip(&s, "settings_items", &display_name);
        assert_eq!(
            s.settings("channel_summary:channel")["text"],
            format!("Paired · {handle}")
        );
        let summary = s.settings("channel_summary:channel");
        assert!(summary["tooltip"].as_str().unwrap().contains(&handle));
        control(&f, p, "edit_user_channel:channel");
        let expanded = stress_inspect();
        let title = expanded.settings("stratum_user_title:channel");
        assert_eq!(title["text"], format!("Your Personal Account (@{handle})"));
        assert_eq!(title["tooltip"], title["text"]);
        send_companion(&f, p, json!({"op":"error","error":error}));
        let s = stress_inspect();
        tooltip(&s, "settings_items", &clean_error);
        control(&f, p, "open_directory");
        // Echo the current real request without invoking the normal no-truncation
        // fixture inspector: this test deliberately keeps overflowing chrome visible.
        stress_inspect();
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| matches!(a["op"].as_str(), Some("query_directory" | "open_directory")))
            .unwrap();
        let mut remote = peer("one");
        remote["display_address"] = json!(format!(
            "personal:{}",
            remote["display_address"].as_str().unwrap()
        ));
        send_companion(
            &f,
            p,
            json!({"op":"directory","request_id":request["request_id"],"query":request["query"],"scope":request["scope"],"harness":request["harness"],"peers":[remote]}),
        );
        control(&f, p, "msg_dir:personal:one");
        f.send(
            p,
            json!({"op":"test_composer_input","value":"Keep this draft"}),
        );
        control(&f, p, "composer_send");
        stress_inspect();
        let submitted = f.frames_for(p, "send_signal").next().unwrap();
        send_companion(
            &f,
            p,
            json!({"op":"composer_result","ok":false,"request_id":submitted["request_id"],"error":error}),
        );
        let s = stress_inspect();
        assert_eq!(s.directory("composer_input")["text"], "Keep this draft");
        tooltip(&s, "directory_items", &clean_error);
    }
}

#[test]
fn two_long_hub_pills_fit_and_pairing_warning_wraps_in_the_menu() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let names = [
        "personal-engineering-and-research-working-group",
        "workspace-engineering-and-research-working-group",
    ];
    let mut frame = state();
    let mut cfg = settings();
    for (index, name) in names.iter().enumerate() {
        frame["hubs"][index]["name"] = json!(name);
        cfg["hubs"][index]["name"] = json!(name);
    }
    frame["listeners"] = json!([]);
    frame["settings"] = cfg;
    f.send(p, frame);
    for name in names {
        control(&f, p, &format!("hub:{name}"));
        let snapshot = inspect(&f, p);
        let strip = snapshot.menu("header_hub_strip");
        assert_eq!(strip["content_x"], 0);
        for name in names {
            let pill = snapshot.menu(&format!("hub:{name}"));
            assert!(pill["x"].as_f64().unwrap() >= 2.0);
            assert!(
                pill["x"].as_f64().unwrap() + pill["width"].as_f64().unwrap()
                    <= strip["width"].as_f64().unwrap() - 2.0
            );
            assert!(pill["accessibility_label"].as_str().unwrap().contains(name));
            assert!(pill["label_measured_width"].as_f64() > pill["label_width"].as_f64());
        }
    }
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    let settings_snapshot = inspect(&f, p);
    for name in names {
        let label = settings_snapshot.settings(&format!("settings_hub_name:{name}"));
        assert_eq!(label["tooltip"], name);
        assert!(label["label_measured_width"].as_f64() > label["label_width"].as_f64());
    }
    control(&f, p, "close_settings");
    let frame = state_with_settings(settings());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "pair_channel:channel");
    inspect(&f, p);
    let pair = f.frames_for(p, "channel_pair").next_back().unwrap();
    f.send(p, json!({"op":"pair_code","hub":pair["hub"],"channel_id":"channel","request_id":pair["request_id"],"instruction":"Send the pairing command"}));
    control(&f, p, "close_settings");
    let warning = "Pairing started.\nThe provider accepted the pairing request.\nSettings could not refresh.\nOpen Settings again to see the latest status.";
    f.send(p, json!({"op":"settings","source_op":"channel_pair","hub":pair["hub"],"channel_id":"channel","request_id":pair["request_id"],"ok":true,"committed":true,"warning":warning}));
    let snapshot = inspect(&f, p);
    assert_eq!(snapshot["settings_open"], false);
    assert!(snapshot.settings("settings_error_banner").is_null());
    let row = snapshot.menu("menu_error_row");
    let label = nodes(&row["children"])
        .into_iter()
        .find(|child| child["text"] == warning)
        .unwrap();
    assert_eq!(label["wraps"], true);
    assert!(row["height"].as_f64().unwrap() > 36.0);
}

#[test]
fn two_hub_pills_share_unused_width_in_either_order() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let long = "engineering-and-research-working-group";
    for names in [["a", long], [long, "a"]] {
        let mut frame = state();
        frame["listeners"] = json!([]);
        for (index, name) in names.iter().enumerate() {
            frame["hubs"][index]["name"] = json!(name);
        }
        f.send(p, frame);
        let s = inspect(&f, p);
        let strip = s.menu("header_hub_strip");
        assert_eq!(strip["content_x"], 0);
        let wide = s.menu(&format!("hub:{long}"))["width"].as_f64().unwrap();
        let narrow = s.menu("hub:a")["width"].as_f64().unwrap();
        let budget = strip["width"].as_f64().unwrap() - 8.0;
        assert!(wide > budget / 2.0);
        assert!(wide + narrow <= budget);
        assert!(wide <= 180.0);
    }
}

#[test]
fn hub_drawer_wraps_status_when_packed_actions_need_the_width() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    f.send(p, frame.clone());
    control(&f, p, "hub:personal");
    let short = inspect(&f, p);
    assert_eq!(short.menu("drawer:personal")["height"], 32);
    assert_eq!(short.menu("login:personal")["text"], "Switch Account");
    frame["hubs"][0]["user"] = json!("alexmorgan");
    f.send(p, frame);
    let snapshot = inspect(&f, p);
    let status = snapshot.menu("hub_error:personal");
    assert_eq!(status["text"], "Connected as alexmorgan");
    assert_eq!(status["wraps"], true);
    assert!(snapshot.menu("drawer:personal")["height"].as_f64().unwrap() > 32.0);
}

#[test]
fn appkit_available_scroll_hub_selector_and_noop_state_diff() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    frame["available"]=json!((0..5).map(|i|json!({"harness":"fixture","session_id":format!("candidate-{i}"),"title":format!("Candidate {i}"),"cwd":if i == 1 { "~/Documents/A-very-long-ancestor/Projects/service" } else if i == 2 { "~/Documents/Projects/VeryLongParentDirectory/project-directory-with-long-name" } else if i == 3 { "/srv/Documents/A-very-long-ancestor/Projects/service" } else if i == 4 { "/srv/Documents/Projects/VeryLongParentDirectory/project-directory-with-long-name" } else { "/tmp/work" },"branch":"main","endpoint_pid":4242,"listening_hubs":["personal"]})).collect::<Vec<_>>());
    f.send(p, frame.clone());
    control(&f, p, "target_hub:work");
    for (style, native_style) in [("legacy", 0), ("overlay", 1)] {
        f.send(p, json!({"op":"test_tile_scroll_views","style":style}));
        let snapshot = inspect(&f, p);
        let scroll = snapshot.menu("available_scroll");
        let width = scroll["content_width"].as_f64().unwrap();
        let action = snapshot.menu("listen:fixture:candidate-0");
        assert_eq!(scroll["scroller_style"], native_style);
        assert_eq!(
            snapshot.menu("candidate:fixture:candidate-0")["width"],
            width
        );
        assert_eq!(
            action["x"].as_f64().unwrap() + action["width"].as_f64().unwrap(),
            width - 12.0
        );
        assert_eq!(snapshot["clipped_views"], json!([]), "{style}");
        assert_eq!(snapshot["overlapping_siblings"], json!([]), "{style}");
    }
    let s = inspect(&f, p);
    assert_eq!(s.menu("available_scroll")["height"], 176);
    let candidate = s.menu("candidate:fixture:candidate-0");
    assert_eq!(candidate["height"], 44);
    let title = s.menu("candidate_title:fixture:candidate-0");
    let path = s.menu("candidate_cwd:fixture:candidate-0");
    assert_eq!(title["width"], 244);
    assert_eq!(title["x"], 46);
    assert_eq!(path["width"], 244);
    assert_eq!(path["x"], 46);
    assert_eq!(title["y"], 23);
    assert_eq!(path["y"], 6);
    assert_eq!(path["text"], "/tmp/work · main");
    assert_eq!(
        s.menu("candidate_cwd:fixture:candidate-1")["text"],
        "~/…/Projects/service · main"
    );
    let leaf = s.menu("candidate_cwd:fixture:candidate-2");
    assert_eq!(leaf["text"], "~/…/project-directory-with-long-name · main");
    assert_eq!(
        leaf["tooltip"],
        "~/Documents/Projects/VeryLongParentDirectory/project-directory-with-long-name · main"
    );
    assert_eq!(
        s.menu("candidate_cwd:fixture:candidate-3")["text"],
        "/…/Projects/service · main"
    );
    assert_eq!(
        s.menu("candidate_cwd:fixture:candidate-4")["text"],
        "/…/project-directory-with-long-name · main"
    );
    assert_eq!(s["count"], 0);
    assert_eq!(s.menu("header_subtitle")["text"], "Not listening");
    assert_eq!(s["available_count"], 5);
    assert_eq!(s.menu("hub:work")["text"], "work");
    assert_eq!(s.menu("toggle_auto_listen")["text"], "Auto-Listen");
    control(&f, p, "toggle_auto_listen");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "set_auto_listen" && v["hub"] == "work" && v["enabled"] == true)
    });
    control(&f, p, "listen:fixture:candidate-0");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "listen_session" && v["hub"] == "work" && v["session_id"] == "candidate-0"
        })
    });
    assert_eq!(s.menu("header_hub_strip")["radius"], 13);
    assert_eq!(s.menu("hub:work")["radius"], 11);
    assert_eq!(s.menu("hub:personal")["border_width"], 0);
    // A semantic track fill must follow the view's appearance without rebuilding.
    for (appearance, dark) in [("dark", true), ("light", false), ("dark", true)] {
        let snapshot = inspect_as(&f, p, Some(appearance));
        let track = snapshot.menu("header_hub_strip");
        assert_eq!(track["draws_background"], true);
        let background = track["background"].clone();
        let red = background[0].as_f64().unwrap();
        assert!(
            if dark { red > 0.9 } else { red < 0.1 },
            "{appearance} Hub track: {background}"
        );
        assert!((background[3].as_f64().unwrap() - 0.06).abs() < 0.001);
    }
    control(&f, p, "target_hub:personal");
    assert!(!inspect(&f, p).menu("available_empty_target").is_null());
    f.send(
        p,
        json!({"op":"error","source_op":"hub_action","error":"OIDC cancelled"}),
    );
    let before = inspect(&f, p)["rebuilds"].as_u64().unwrap();
    // An unrelated error must not settle Listen or force an unchanged state to redraw.
    f.send(p, frame.clone());
    let after = inspect(&f, p)["rebuilds"].as_u64().unwrap();
    assert_eq!(after, before);
    // The next identical state is also idle.
    let before = json!(after);
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p)["rebuilds"], before);
    assert!(inspect(&f, p)
        .menu("switch_hub:personal:repo-abcdefghijkl")
        .is_null());
    control(&f, p, "hub:work");
    assert!(inspect(&f, p)
        .menu("session:personal:repo-abcdefghijkl")
        .is_null());
    let original_available = frame["available"].clone();
    let auto_before = inspect(&f, p).menu("toggle_auto_listen");
    // Auto-Listen explains reload blockers and seeds historical groups once.
    frame["hubs"][1]["auto_listen"] = json!(true);
    frame["available"] = json!([
        {"harness":"fixture","session_id":"reload","title":"Reload","cwd":"/tmp","endpoint_pid":4242,"needs_reload":true}
    ]);
    f.send(p, frame.clone());
    let s = inspect(&f, p);
    let auto_after = s.menu("toggle_auto_listen");
    assert_eq!(auto_after["width"], auto_before["width"]);
    assert_eq!(auto_after["x"], auto_before["x"]);
    assert!(nodes(&s["items"])
        .iter()
        .any(|n| n["text"] == "Auto-Listen is on — run /reload below to finish connecting"));
    frame["available"] = json!([
        {"harness":"fixture","session_id":"recent","title":"History","cwd":"/tmp"}
    ]);
    f.send(p, frame.clone());
    let s = inspect(&f, p);
    assert_eq!(s.menu("toggle_group:fixture")["symbol"], "chevron.right");
    assert!(s.menu("candidate:fixture:recent").is_null());
    control(&f, p, "toggle_group:fixture");
    let recent = inspect(&f, p).menu("listen:fixture:recent");
    assert_eq!(recent["enabled"], false);
    assert_eq!(recent["tooltip"], "Open this session to listen");
    control(&f, p, "listen:fixture:recent");
    inspect(&f, p);
    assert!(!f
        .frames_for(p, "listen_session")
        .any(|frame| frame["session_id"] == "recent"));
    frame["unread_count"] = json!(1);
    f.send(p, frame.clone());
    assert!(!inspect(&f, p).menu("candidate:fixture:recent").is_null());
    frame["available"] = json!([]);
    f.send(p, frame.clone());
    assert!(inspect(&f, p).menu("available_empty_target").is_null());
    // The sole enabled Hub can differ from the configured default. Listen must
    // still carry the selected network rather than relying on daemon fallback.
    frame["hubs"] = json!([hubs()[1].clone()]);
    frame["available"] = original_available;
    frame["listeners"] = json!([]);
    f.send(p, frame.clone());
    inspect(&f, p);
    control(&f, p, "listen:fixture:candidate-1");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "listen_session" && v["hub"] == "work" && v["session_id"] == "candidate-1"
        })
    });
    // No enabled networks leaves discovery visible, but cannot issue a Listen.
    frame["hubs"] = json!([]);
    f.send(p, frame);
    inspect(&f, p);
    control(&f, p, "listen:fixture:candidate-2");
    inspect(&f, p);
    assert!(!f
        .frames(p)
        .iter()
        .any(|v| { v["op"] == "listen_session" && v["session_id"] == "candidate-2" }));
}
#[test]
fn badge_artwork_uses_the_configured_plugin_directory() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let plugins = f.home.join("custom plugins");
    let icon = plugins.join("fixture/icon.png");
    fs::create_dir_all(icon.parent().unwrap()).unwrap();
    fs::copy(root().join("site/apple-touch-icon.png"), icon).unwrap();
    f.env.insert(
        "CITO_PLUGIN_DIR".into(),
        plugins.to_string_lossy().into_owned(),
    );
    let p = start(&f);
    f.send(p, state());
    let snapshot = inspect(&f, p);
    assert!(!snapshot.menu("plugin_icon:fixture").is_null());
    assert!(snapshot.menu("plugin_symbol:fixture").is_null());
}

#[test]
fn appkit_card_tree_geometry_icon_hover_copy_and_multi_hub_identity() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let icon = f.home.join("second/fixture/icon.png");
    fs::create_dir_all(icon.parent().unwrap()).unwrap();
    fs::copy(root().join("plugins/pi/icon.png"), icon).unwrap();
    f.env
        .insert("CITO_PLUGIN_PATH".into(), " ~/first : ~/second ".into());
    let p = start(&f);
    let mut initial = state();
    let mut other_network = peer("work-peer");
    other_network["hub"] = json!("work");
    other_network["scoped_id"] = json!("work:work-peer");
    other_network["display_address"] = json!("work:bob/fixture#1234");
    let mut unscoped = peer("unscoped");
    unscoped.as_object_mut().unwrap().remove("hub");
    initial["my_remote_listeners"] = json!([peer("remote"), other_network, unscoped]);
    initial["available"] = json!([{"harness":"fixture","session_id":"candidate","title":"alice","cwd":"/tmp","endpoint_pid":4242}]);
    send_companion(&f, p, initial);
    let s = inspect(&f, p);
    assert_eq!(s["count"], 1);
    assert!(s.menu("remote:work:work-peer").is_null());
    assert!(!s.menu("remote:personal:unscoped").is_null());
    assert_eq!(s["icon_template"], true);
    assert_eq!(s["icon_width"], 20);
    assert_eq!(s["button_title"], "");
    let candidate = s.menu("candidate_title:fixture:candidate");
    assert_eq!(candidate["text"], "Fixture");
    assert_eq!(candidate["tooltip"], "Fixture — /tmp");
    assert_eq!(
        s.menu("listen:fixture:candidate")["tooltip"],
        "Start listening in Fixture"
    );
    control(&f, p, "listen:fixture:candidate");
    wait(|| f.frames_for(p, "listen_session").next().is_some());
    assert_eq!(
        f.frames_for(p, "listen_session").next().unwrap()["title"],
        "Fixture"
    );
    initial = state();
    initial["source_op"] = json!("listen_session");
    initial["request_id"] = f.frames_for(p, "listen_session").next().unwrap()["request_id"].clone();
    initial["available"] = json!([{"harness":"fixture","session_id":"candidate","title":"alice","custom_title":true,"cwd":"/tmp","endpoint_pid":4242}]);
    send_companion(&f, p, initial);
    let custom = inspect(&f, p);
    assert_eq!(
        custom.menu("candidate_title:fixture:candidate")["text"],
        "alice"
    );
    control(&f, p, "listen:fixture:candidate");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "listen_session" && v["title"] == "alice")
    });
    assert!(s["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["width"] == 380));
    assert_eq!(s.menu("plugin_icon:fixture")["tint"], json!([]));
    for id in ["toggle_my_remote", "toggle_group:fixture"] {
        assert_eq!(s.menu(id)["x"], 12);
        assert_eq!(s.menu(id)["width"], 356);
    }
    assert_eq!(s.menu("header")["height"], 40);
    assert_eq!(s.menu("footer")["height"], 36);
    assert_eq!(s.menu("drawer:personal")["hidden"], true);
    control(&f, p, "hub:personal");
    assert_eq!(
        inspect(&f, p).menu("drawer:personal")["hidden"],
        false,
        "The selected healthy Hub must expose its drawer"
    );
    control(&f, p, "hub:personal");
    assert_eq!(inspect(&f, p).menu("drawer:personal")["hidden"], true);
    control(&f, p, "copy:personal:repo-abcdefghijkl");
    let copied = inspect(&f, p);
    assert_eq!(copied["clipboard"], "alice/fixture#abcd");
    assert_eq!(
        copied.menu("copy:personal:repo-abcdefghijkl")["text"],
        "✓ Copied"
    );
    assert_eq!(copied["truncated_labels"], json!([]));
    for appearance in ["dark", "light"] {
        f.send(p, json!({"op":"test_appearance","appearance":appearance}));
        f.send(p, json!({"op":"test_hover","id":"quit","hovered":true}));
        let s = inspect(&f, p);
        assert_eq!(s.menu("quit")["hovered"], true);
        f.send(p, json!({"op":"test_menu_close"}));
        assert!(nodes(&inspect(&f, p)["items"])
            .iter()
            .all(|v| v["hovered"] != true));
    }
    let mut next = state();
    next["hubs"][1]["status"] = json!("disconnected");
    let mut second = listener();
    second["hub"] = json!("work");
    second["scoped_id"] = json!("work:repo-workabcdefgh");
    second["listener_id"] = json!("c_workabcdefgh");
    second["display_address"] = json!("work:alice/fixture#abcd");
    next["listeners"] = json!([listener(), second]);
    let first_copy = "copy:personal:repo-abcdefghijkl";
    control(&f, p, first_copy);
    let before_rebuild = inspect(&f, p);
    send_companion(&f, p, next.clone());
    let s = inspect(&f, p);
    assert_ne!(
        s.menu(first_copy)["instance"],
        before_rebuild.menu(first_copy)["instance"]
    );
    assert_eq!(s.menu(first_copy)["text"], "✓ Copied");
    f.send(p, json!({"op":"test_expire_copy_feedback","id":first_copy}));
    assert_eq!(
        inspect(&f, p).menu(first_copy)["text"],
        "alice/fixture#abcd"
    );
    assert_eq!(s["count"], 1);
    assert!(s.menu("session:work:repo-workabcdefgh").is_null());
    assert!(!s.menu("session:personal:repo-abcdefghijkl").is_null());
    control(&f, p, "hub:work");
    let switched = inspect(&f, p);
    assert!(!switched.menu("session:work:repo-workabcdefgh").is_null());
    assert!(switched
        .menu("session:personal:repo-abcdefghijkl")
        .is_null());
    control(&f, p, "copy:work:repo-workabcdefgh");
    assert_eq!(inspect(&f, p)["clipboard"], "work:alice/fixture#abcd");
    control(&f, p, "hub:personal");
    assert!(!inspect(&f, p)
        .menu("session:personal:repo-abcdefghijkl")
        .is_null());
    control(&f, p, "copy:personal:repo-abcdefghijkl");
    assert_eq!(inspect(&f, p)["clipboard"], "alice/fixture#abcd");
    next["hubs"][1]["supports_workspace_scope"] = json!(false);
    send_companion(&f, p, next.clone());
    control(&f, p, "hub:work");
    assert!(!inspect(&f, p)
        .menu("session:work:repo-workabcdefgh")
        .is_null());
    assert!(
        inspect(&f, p)
            .menu("scope:work:repo-workabcdefgh")
            .is_null(),
        "Personal networks hide redundant visibility controls"
    );
    let pause = inspect(&f, p).menu("stop:work:repo-workabcdefgh");
    assert_eq!(pause["text"], "Stop");
    assert_eq!(
        pause["tooltip"],
        "Stop only this session and keep it stopped. Auto-Listen on work stays unchanged."
    );
    assert_eq!(pause["border_width"], 1);
    assert!(pause["symbol"].is_null());
    next["hubs"][1]["auth_mode"] = json!("token");
    send_companion(&f, p, next.clone());
    assert!(inspect(&f, p)
        .menu("scope:work:repo-workabcdefgh")
        .is_null());
    let mut other = next["listeners"][1].clone();
    other["user"] = json!("bob");
    other["listener_id"] = json!("c_other");
    other["scoped_id"] = json!("work:other");
    next["listeners"].as_array_mut().unwrap().push(other);
    send_companion(&f, p, next.clone());
    assert!(
        inspect(&f, p)
            .menu("scope:work:repo-workabcdefgh")
            .is_null(),
        "Other users cannot grant a Hub capability"
    );
    next["listeners"].as_array_mut().unwrap().pop();
    next["hubs"][1]["auth_mode"] = json!("oidc");
    next["hubs"][1]["supports_workspace_scope"] = json!(true);
    send_companion(&f, p, next.clone());
    let scope_before = inspect(&f, p).menu("scope:work:repo-workabcdefgh");
    assert_eq!(scope_before["width"], 68);
    assert_eq!(scope_before["tooltip"], "Visibility: Only Me (@alice) — only your own sessions and devices on 'work' can message this node. Click to share with Team.");
    control(&f, p, "scope:work:repo-workabcdefgh");
    wait(|| f.frames_for(p, "set_scope").count() == 1);
    let scope_after = inspect(&f, p).menu("scope:work:repo-workabcdefgh");
    assert_eq!(scope_after["width"], scope_before["width"]);
    assert_eq!(scope_after["x"], scope_before["x"]);
    let scope_team = scope_after;
    assert_eq!(scope_team["tooltip"], "Visibility: Team — anyone connected to 'work' can discover and message this node. Click to restrict to Only Me.");
    control(&f, p, "scope:work:repo-workabcdefgh");
    let scope_self = inspect(&f, p).menu("scope:work:repo-workabcdefgh");
    assert_eq!(scope_self["text"], "Self");
    assert_eq!(scope_self["width"], scope_team["width"]);
    assert_eq!(scope_self["x"], scope_team["x"]);
    let scopes: Vec<_> = f.frames_for(p, "set_scope").collect();
    assert!(!scopes.iter().any(|v| v["hub"] == "personal"));
    assert!(scopes.iter().any(|v| v["hub"] == "work"));
    assert_eq!(scopes[0]["scope"], "workspace");
    assert_eq!(scopes[1]["scope"], "self");
    control(&f, p, "rename:work:repo-workabcdefgh");
    assert!(!inspect(&f, p)
        .menu("rename_field:work:repo-workabcdefgh")
        .is_null());
    f.send(p, json!({"op":"test_settings_input","id":"rename_field:work:repo-workabcdefgh","value":"Backend API"}));
    f.send(p, json!({"op":"test_menu_key","key":"escape"}));
    assert!(inspect(&f, p)
        .menu("rename_field:work:repo-workabcdefgh")
        .is_null());
    control(&f, p, "rename:work:repo-workabcdefgh");
    f.send(p, json!({"op":"test_settings_input","id":"rename_field:work:repo-workabcdefgh","value":"Backend API"}));
    let rename_identity = inspect(&f, p)["rename_field_identity"].clone();
    next["unread_count"] = json!(1);
    send_companion(&f, p, next);
    assert_eq!(
        inspect(&f, p)["rename_field_identity"],
        rename_identity,
        "A background refresh must retain the native rename field and its editor"
    );
    assert_eq!(
        inspect(&f, p).menu("rename_field:work:repo-workabcdefgh")["text"],
        "Backend API"
    );
    f.send(p, json!({"op":"test_menu_key","key":"return"}));
    wait(|| f.frames_for(p, "rename_session").next().is_some());
    let renamed = f.frames_for(p, "rename_session").next().unwrap();
    assert_eq!(renamed["harness"], "fixture");
    assert_eq!(renamed["session_id"], "s1");
    assert_eq!(renamed["title"], "Backend API");
    let after = inspect(&f, p);
    assert!(after.menu("rename_field:work:repo-workabcdefgh").is_null());
    assert_eq!(after["overlapping_siblings"], json!([]));
    control(&f, p, "stop:work:repo-workabcdefgh");
    wait(|| f.frames_for(p, "unlisten_session").next().is_some());
    let stop = f.frames_for(p, "unlisten_session").next().unwrap();
    assert_eq!(stop["session_id"], "s1");
    assert_eq!(stop["hub"], "work");

    let mut human = listener();
    human["hub"] = json!("local");
    human["harness"] = json!("human");
    human["session_id"] = json!("default");
    human["listener_id"] = json!("human:default");
    human["public_id"] = json!("human:default");
    human["scoped_id"] = json!("local:human:default");
    human["title"] = json!("Human");
    human["user"] = json!("a");
    human["display_address"] = json!("@a");
    let mut short_addresses = state();
    short_addresses["hubs"] =
        json!([{"name":"local","status":"connected","auth_mode":"none","user":"a"}]);
    short_addresses["listeners"] = json!([human]);
    short_addresses["settings"] = settings();
    short_addresses["settings"]["hubs"][0]["name"] = json!("local");
    send_companion(&f, p, short_addresses);
    assert!(inspect(&f, p).menu("session:local:human:default").is_null());
    control(&f, p, "open_directory");
    let mut person = peer("human:default");
    person["hub"] = json!("local");
    person["scoped_id"] = json!("local:human:default");
    person["user"] = json!("a");
    person["harness"] = json!("human");
    person["display_address"] = json!("@a");
    send_fixture(&f, p, json!({"op":"directory","peers":[person]}));
    let before = inspect(&f, p);
    let id = "copy_dir:local:human:default";
    assert_eq!(before.directory(id)["text"], "@a");
    for appearance in ["light", "dark"] {
        control(&f, p, id);
        let copied = inspect_as(&f, p, Some(appearance));
        assert_eq!(copied["clipboard"], "@a");
        assert_eq!(copied.directory(id)["text"], "✓ Copied");
        assert_eq!(copied.directory(id)["width"], before.directory(id)["width"]);
        assert_eq!(copied["clipped_views"], json!([]));
        assert_eq!(copied["truncated_labels"], json!([]));
    }
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    send_fixture(&f, p, json!({"op":"directory","peers":[]}));
    inspect(&f, p);
    // Isolate the address projection while keeping unrelated identity captions short.
    let full = format!("work:{}/fixture#abcd", "a".repeat(64));
    let mut long = listener();
    long["hub"] = json!("work");
    long["scoped_id"] = json!("work:long");
    long["display_address"] = json!(full);
    long["title"] = json!("Review");
    let mut remote = peer("long-remote");
    remote["hub"] = json!("work");
    remote["scoped_id"] = json!("work:long-remote");
    remote["display_address"] = json!(full);
    let mut addresses = state_with_settings(settings());
    addresses["listeners"] = json!([long]);
    addresses["my_remote_listeners"] = json!([remote.clone()]);
    send_companion(&f, p, addresses);
    control(&f, p, "target_hub:work");
    if inspect(&f, p)
        .menu("copy_remote:work:long-remote")
        .is_null()
    {
        control(&f, p, "toggle_my_remote");
    }
    render_png(&f, p, "menu", "r2_long_address_menu.png");
    for id in ["copy:work:long", "copy_remote:work:long-remote"] {
        let chip = inspect(&f, p).menu(id);
        let shown = chip["text"].as_str().unwrap();
        assert!(shown.contains('…') && shown.starts_with("work:") && shown.ends_with("#abcd"));
        assert_eq!(chip["tooltip"], full);
        assert!(chip["width"].as_f64().unwrap() <= 220.0);
        control(&f, p, id);
        assert_eq!(inspect(&f, p)["clipboard"], full);
    }
    control(&f, p, "open_directory");
    send_fixture(&f, p, json!({"op":"directory","peers":[remote]}));
    for appearance in ["light", "dark"] {
        let s = inspect_as(&f, p, Some(appearance));
        let chip = s.directory("copy_dir:work:long-remote");
        let shown = chip["text"].as_str().unwrap();
        assert!(shown.contains('…') && shown.starts_with("work:") && shown.ends_with("#abcd"));
        assert_eq!(chip["tooltip"], full);
        assert!(chip["width"].as_f64().unwrap() <= 190.0);
        render_png_as(
            &f,
            p,
            "directory",
            &format!("r2_long_address_directory_{appearance}.png"),
            Some(appearance),
        );
    }
    control(&f, p, "copy_dir:work:long-remote");
    assert_eq!(inspect(&f, p)["clipboard"], full);
    let mut many = state();
    many["listeners"] = json!((0..6)
        .map(|i| {
            let mut row = listener();
            row["session_id"] = json!(format!("session-{i}"));
            row["scoped_id"] = json!(format!("personal:session-{i}"));
            row
        })
        .collect::<Vec<_>>());
    send_companion(&f, p, many);
    control(&f, p, "hub:personal");
    let scrolled = inspect(&f, p);
    assert_eq!(scrolled.menu("active_scroll")["height"], 256);
    for i in 0..6 {
        assert_eq!(
            scrolled.menu(&format!("session:personal:session-{i}"))["height"],
            64
        );
    }
}

#[test]
fn status_only_frame_uses_live_non_first_default_hub_without_refreshed_settings() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut work_listener = listener();
    work_listener["hub"] = json!("work");
    work_listener["scoped_id"] = json!("work:repo-abcdefghijkl");
    work_listener["display_address"] = json!("alice/fixture#abcd");

    let mut frame = state();
    frame["hubs"][0]["is_default"] = json!(false);
    frame["hubs"][1]["is_default"] = json!(true);
    frame["listeners"] = json!([work_listener.clone()]);
    f.send(p, frame.clone());

    let s = inspect(&f, p);
    assert!(!s.menu("session:work:repo-abcdefghijkl").is_null());
    assert_eq!(s.menu("header_subtitle")["text"], "1 session listening");

    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    let hubs_tab = inspect(&f, p);
    assert!(!hubs_tab.settings("default_hub:work").is_null());
    assert!(hubs_tab.settings("set_default_hub:work").is_null());
    assert!(!hubs_tab.settings("set_default_hub:personal").is_null());
    control(&f, p, "close_settings");

    // Stale settings with personal as default must not override live status-only hubs.
    let mut stale = state_with_settings(settings());
    stale["hubs"][0]["is_default"] = json!(true);
    stale["hubs"][1]["is_default"] = json!(false);
    f.send(p, stale);
    assert!(!inspect(&f, p)
        .menu("session:personal:repo-abcdefghijkl")
        .is_null());

    f.send(p, frame.clone());
    let updated = inspect(&f, p);
    assert!(!updated.menu("session:work:repo-abcdefghijkl").is_null());
    assert_eq!(
        updated.menu("header_subtitle")["text"],
        "1 session listening"
    );

    // Explicit user selection must still be preserved across status-only updates.
    control(&f, p, "target_hub:personal");
    f.send(p, frame);
    let selected = inspect(&f, p);
    assert!(selected.menu("session:work:repo-abcdefghijkl").is_null());
    assert_eq!(selected.menu("header_subtitle")["text"], "Not listening");
}

#[test]
fn state_frame_reconciles_default_from_root_hub() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut initial = state();
    initial["hub"] = json!("personal");
    initial["hubs"][0]["is_default"] = json!(true);
    initial["hubs"][1]["is_default"] = json!(false);
    f.send(p, initial);
    assert!(!inspect(&f, p)
        .menu("session:personal:repo-abcdefghijkl")
        .is_null());

    let mut work_listener = listener();
    work_listener["hub"] = json!("work");
    work_listener["scoped_id"] = json!("work:repo-abcdefghijkl");
    work_listener["display_address"] = json!("alice/fixture#abcd");

    let mut update = state();
    update["hub"] = json!("work");
    update["hubs"][0]["is_default"] = json!(true);
    update["hubs"][1]["is_default"] = json!(false);
    update["listeners"] = json!([work_listener]);
    f.send(p, update);

    let updated = inspect(&f, p);
    assert!(!updated.menu("session:work:repo-abcdefghijkl").is_null());
    assert_eq!(
        updated.menu("header_subtitle")["text"],
        "1 session listening"
    );
}
