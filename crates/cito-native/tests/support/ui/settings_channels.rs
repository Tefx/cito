use super::*;

#[test]
fn real_daemon_failed_channel_test_projects_settings_without_a_duplicate_banner() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let plugin = f.state.join("plugins/channel");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"), json!({
        "id":"channel", "kind":"human-channel", "scope":"hub", "displayName":"Messenger",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],
        "pairing":{"supported":true},
        "fields":[
            {"key":"credential","label":"Gateway Token","scope":"hub","type":"secret","required":true},
            {"key":"address","label":"Personal Address","scope":"user","type":"string","required":true}
        ]
    }).to_string()).unwrap();
    let mut cycle = ClosedLoop::new(&f);
    cycle.until(|s| node(s, "items", "header").is_object());
    cycle.control("open_settings");
    cycle.control("settings_tab:channels");
    cycle.until(|s| !node(s, "settings_items", "save_hub_service:channel").is_null());
    cycle.input("channel_field:channel:credential", "valid");
    cycle.control("save_hub_service:channel");
    cycle.until(|s| !node(s, "settings_items", "channel_field:channel:address").is_null());
    cycle.input("channel_field:channel:address", "personal-room");
    cycle.control("save_channel:channel");
    cycle.until(|s| !node(s, "settings_items", "channel_summary:channel").is_null());
    cycle.control("test_channel:channel");
    cycle.until(|s| {
        node(s, "settings_items", "settings_channel_detail:channel")["text"]
            == "Fixture test succeeded"
    });
    let old_result = cycle
        .frames
        .iter()
        .rev()
        .find(|frame| frame["op"] == "channel_test_result")
        .unwrap()
        .clone();
    cycle.control("edit_hub_service:channel");
    cycle.input("channel_field:channel:credential", "invalid");
    cycle.control("save_hub_service:channel");
    cycle.until(|s| node(s, "settings_items", "stratum_hub:channel")["height"] == 36);
    cycle.control("test_channel:channel");
    let failed = cycle.until(|s| {
        node(s, "settings_items", "settings_channel_detail:channel")["text"] == "Invalid token"
    });
    assert!(node(&failed, "settings_items", "settings_error_banner").is_null());
    assert_eq!(failed["pending_controls"], json!([]));
    let action = &cycle
        .results
        .iter()
        .rev()
        .find(|(a, _, _)| a["op"] == "channel_test")
        .unwrap()
        .0;
    let frames: Vec<_> = cycle
        .frames
        .iter()
        .filter(|frame| frame["request_id"] == action["request_id"])
        .collect();
    assert_eq!(frames[0]["op"], "channel_test_result");
    assert_eq!(frames[0]["ok"], false);
    assert!(frames.iter().skip(1).any(|frame| frame["op"] == "settings"
        && frame["committed"] == false
        && frame["settings"].is_object()));
    // Replay an actual daemon receipt into a fresh UI process. Its canonical
    // settings remain authoritative because it owns no request from the old UI.
    let fresh_ui = start(&f);
    let mut fresh_state = cycle
        .frames
        .iter()
        .rev()
        .find(|frame| frame["op"] == "state")
        .unwrap()
        .clone();
    fresh_state["settings"] = frames
        .iter()
        .find(|frame| frame["op"] == "settings")
        .unwrap()["settings"]
        .clone();
    f.send(fresh_ui, fresh_state);
    control(&f, fresh_ui, "open_settings");
    control(&f, fresh_ui, "settings_tab:channels");
    let baseline = node(
        &inspect(&f, fresh_ui),
        "settings_items",
        "settings_channel_detail:channel",
    );
    f.send(fresh_ui, old_result);
    let stale = inspect(&f, fresh_ui);
    assert_eq!(
        node(&stale, "settings_items", "settings_channel_detail:channel")["text"],
        baseline["text"]
    );
    assert_ne!(
        node(&stale, "settings_items", "settings_channel_detail:channel")["text"],
        "Fixture test succeeded"
    );
    assert_eq!(f.action(json!({"op":"setup_save_hub","name":"work","url":format!("http://127.0.0.1:{}",port()),"set_default":false})).0, 200);
    assert_eq!(
        f.action(json!({"op":"setup_toggle_hub","hub":"local","enabled":false}))
            .0,
        200
    );
    // Dispatch from the still-mounted old generation before pumping its refresh.
    cycle.control("unpair_channel:channel");
    let removed = cycle.until(|s| node(s, "settings_items", "settings_channel:channel").is_null());
    assert_eq!(removed["pending_controls"], json!([]));
    let action = &cycle
        .results
        .iter()
        .rev()
        .find(|(a, _, _)| a["op"] == "channel_unpair")
        .unwrap()
        .0;
    assert!(cycle
        .frames
        .iter()
        .any(|frame| frame["request_id"] == action["request_id"]
            && frame["op"] == "settings"
            && frame["ok"] == false
            && frame["settings"].is_object()));
}

#[test]
fn multi_frame_channel_receipts_preserve_pairing_and_latest_test_feedback() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "pair_channel:channel");
    inspect(&f, p);
    let pair = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "channel_pair")
        .unwrap();
    f.send(p, json!({"op":"pair_code","hub":"personal","channel_id":"channel","request_id":pair["request_id"],"instruction":"Send the pairing command","pairing_command":"/start example-code","pairing_url":"https://example.com/pair"}));
    let first = inspect(&f, p);
    let copy = node(&first, "settings_items", "copy_pair_command:channel");
    assert_eq!(copy["text"], "✓ Copied");
    let mut warning = json!({"op":"settings","source_op":"channel_pair","hub":"work","channel_id":"channel","request_id":pair["request_id"],"ok":true,"committed":true,"warning":"Pairing started. Refresh is temporarily unavailable."});
    f.send(p, warning.clone());
    assert!(node(&inspect(&f, p), "settings_items", "settings_error_banner").is_null());
    warning["hub"] = json!("personal");
    f.send(p, warning.clone());
    let settled = inspect(&f, p);
    assert_eq!(
        node(&settled, "settings_items", "settings_error_label")["text"],
        warning["warning"]
    );
    assert_eq!(
        node(&settled, "settings_items", "pairing_instruction:channel")["text"],
        "Send the pairing command"
    );
    assert_eq!(settled["pending_controls"], json!([]));
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    warning["warning"] = json!("Duplicate pairing receipt");
    f.send(p, warning);
    assert!(node(&inspect(&f, p), "items", "menu_error_row").is_null());
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:apps");
    control(&f, p, "settings_tab:channels");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["value"] = json!("account");
    for degraded in [false, true] {
        cfg["channel_inventory_error"] = if degraded {
            json!("Refresh temporarily unavailable")
        } else {
            Value::Null
        };
        f.send(p, json!({"op":"settings","source_op":if degraded { "open_settings" } else { "channel_pair" },"request_id":pair["request_id"],"settings":cfg,"ok":true}));
        let second = inspect(&f, p);
        assert_eq!(
            node(&second, "settings_items", "pairing_instruction:channel")["text"],
            "Send the pairing command"
        );
        assert_eq!(
            node(&second, "settings_items", "copy_pair_command:channel")["text"],
            "✓ Copied"
        );
        control(&f, p, "copy_pair_command:channel");
        assert_eq!(inspect(&f, p)["clipboard"], "/start example-code");
    }
    cfg["channel_inventory_error"] = Value::Null;
    control(&f, p, "test_channel:channel");
    inspect(&f, p);
    let test = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "channel_test")
        .unwrap();
    f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","request_id":test["request_id"],"ok":false,"detail":"Current test failed"}));
    cfg["channels"][0]["status"] = json!("error");
    cfg["channels"][0]["detail"] = json!("Older health observation");
    cfg["launch_at_login"] = json!(true);
    f.send(p, json!({"op":"settings","source_op":"channel_test","request_id":test["request_id"],"channel_id":"channel","settings":cfg,"ok":false}));
    let second = inspect(&f, p);
    assert_eq!(second["settings_launch_at_login"], true);
    assert_eq!(
        node(&second, "settings_items", "settings_channel_detail:channel")["text"],
        "Current test failed"
    );
    assert!(node(&second, "settings_items", "settings_error_banner").is_null());
    f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","request_id":"previous-ui-generation","ok":true,"detail":"Stale test succeeded"}));
    let stale = inspect(&f, p);
    assert_eq!(
        node(&stale, "settings_items", "settings_channel_detail:channel")["text"],
        "Current test failed"
    );
    assert_eq!(stale["pending_controls"], json!([]));
}

#[test]
fn oversized_channel_submission_preserves_drafts_without_pending_or_transport_effects() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    let mut second = cfg["channels"][0]["fields"][1].clone();
    second["key"] = json!("second");
    second["label"] = json!("Second address");
    cfg["channels"][0]["fields"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let mut frame = state();
    frame["settings"] = cfg;
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    let draft = "a".repeat(530_000);
    for key in ["account", "second"] {
        f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{key}"),"value":draft}));
    }
    let before = f.frames(p).len();
    control(&f, p, "save_channel:channel");
    let s = inspect(&f, p);
    assert!(!f
        .frames(p)
        .iter()
        .skip(before)
        .any(|a| a["op"] == "channel_save"));
    assert_eq!(s["pending_controls"], json!([]));
    assert_eq!(s["pending_channel_saves"], json!([]));
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:account")["text"],
        draft
    );
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:second")["text"],
        draft
    );
    assert!(node(&s, "settings_items", "settings_error_label")["text"]
        .as_str()
        .unwrap()
        .contains("too large"));
}

#[test]
fn v41_inbox_bar_deduplicates_attachments_and_fits_paused_remote_defaults() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    frame["hubs"][0]["human_state_status"] = json!("current");
    frame["hubs"][0]["human_listener_id"] = json!("inbox");
    frame["hubs"][0]["human_control_revision"] = json!(4);
    let mut human = listener();
    human["harness"] = json!("human");
    human["listener_id"] = json!("inbox");
    human["scoped_id"] = json!("personal:inbox");
    human["public_id"] = json!("inbox");
    human["display_address"] = json!("@alice");
    frame["listeners"].as_array_mut().unwrap().push(human);
    frame["hubs"][0]["auth_mode"] = json!("oidc");
    cfg["hubs"][0]["human_state_status"] = json!("current");
    cfg["hubs"][0]["human_listener_id"] = json!("inbox");
    cfg["hubs"][0]["human_can_set_scope"] = json!(true);
    cfg["hubs"][0]["supports_workspace_scope"] = json!(true);
    cfg["hubs"][0]["human_control_revision"] = json!(4);
    let device = json!({"channel_id":"here:human","location":"device","display_name":"MacBook Pro.local","availability":"ACTIVE","is_local":true});
    let external = json!({"channel_id":"hub:telegram","location":"hub","display_name":"Telegram","availability":"ACTIVE","is_local":false});
    for remote_only in [false, true] {
        for paused in [false, true] {
            let mut retained = device.clone();
            retained["availability"] = json!("DISCONNECTED_RETAINED");
            retained["is_local"] = json!(false);
            let mut expired = device.clone();
            expired["display_name"] = json!("Old Mac");
            expired["availability"] = json!("EXPIRED");
            let mut detached = external.clone();
            detached["display_name"] = json!("Retired Channel");
            detached["availability"] = json!("UNLISTENED");
            frame["hubs"][0]["attachments"] = if remote_only {
                json!([external.clone(), expired, detached])
            } else {
                json!([
                    device.clone(),
                    retained,
                    external.clone(),
                    expired,
                    detached
                ])
            };
            frame["hubs"][0]["human_paused"] = json!(paused);
            cfg["hubs"][0]["human_paused"] = json!(paused);
            cfg["hubs"][0]["attachments"] = frame["hubs"][0]["attachments"].clone();
            cfg["channels"][0]["inbox_paused"] = json!(paused);
            frame["settings"] = cfg.clone();
            f.send(p, frame.clone());
            control(&f, p, "open_settings");
            control(&f, p, "settings_tab:channels");
            for appearance in ["light", "dark"] {
                let s = inspect_as(&f, p, Some(appearance));
                assert!(node(&s, "items", "human_inbox_card").is_null());
                assert!(node(&s, "items", "inbox_header").is_null());
                assert_eq!(node(&s, "items", "inbox_paused_notice").is_null(), !paused);
                assert_eq!(
                    node(&s, "settings_items", "inbox_control_bar")["height"],
                    48
                );
                let scroll = node(&s, "settings_items", "settings_channels_tab");
                assert_eq!(scroll["height"], 368);
                assert_eq!(
                    s["clipped_views"],
                    json!([]),
                    "Every resting active/paused default must fit without scrolling"
                );
                assert_eq!(s["overlapping_siblings"], json!([]));
                let summary = node(&s, "settings_items", "inbox_control_summary");
                assert_eq!(
                    summary["text"],
                    if paused {
                        "Inbox paused · Activity & Telegram paused"
                    } else if remote_only {
                        "Telegram · Activity"
                    } else {
                        "Telegram · Activity · MacBook Pro"
                    }
                );
                let tip = summary["tooltip"].as_str().unwrap();
                assert!(
                    tip.contains("External channels deliver alerts even when this Mac is offline.")
                );
                for excluded in [
                    "ACTIVE",
                    "UNLISTENED",
                    "EXPIRED",
                    "Old Mac",
                    "Retired Channel",
                ] {
                    assert!(!tip.contains(excluded));
                }
                assert_eq!(
                    node(&s, "settings_items", "toggle_inbox")["text"],
                    if paused {
                        "Resume Inbox"
                    } else {
                        "Pause Inbox"
                    }
                );
                assert_eq!(node(&s, "settings_items", "scope:inbox")["text"], "Self");
                render_png_as(
                    &f,
                    p,
                    "settings",
                    &format!("v41_inbox_{}_{}_{appearance}.png", remote_only, paused),
                    Some(appearance),
                );
            }
        }
    }
    control(&f, p, "toggle_inbox");
    let pending = inspect(&f, p);
    assert_eq!(
        node(&pending, "settings_items", "toggle_inbox")["enabled"],
        false
    );
    let action = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "human_inbox_update")
        .unwrap();
    frame["source_op"] = json!("human_inbox_update");
    frame["request_id"] = json!("older-request");
    f.send(p, frame.clone());
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "toggle_inbox")["enabled"],
        false
    );
    frame["request_id"] = action["request_id"].clone();
    f.send(p, frame);
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "toggle_inbox")["enabled"],
        true
    );
}

#[test]
fn v41_new_test_receipts_override_health_until_the_next_changed_health_frame() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["channels"][0]["status"] = json!("error");
    cfg["channels"][0]["detail"] = json!("Credentials rejected. Update your token.");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    f.send(p,json!({"op":"channel_test_result","channel_id":"channel","ok":true,"detail":"Test accepted"}));
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "settings_channel_detail:channel"
        )["text"],
        "Test accepted"
    );
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "settings_channel_detail:channel"
        )["text"],
        "Test accepted",
        "An unchanged health tick cannot displace the newer test receipt"
    );
    cfg["launch_at_login"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "settings_channel_detail:channel"
        )["text"],
        "Test accepted",
        "An unrelated settings change cannot displace the newer test receipt"
    );
    cfg["channels"][0]["warning"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "settings_channel_detail:channel"
        )["text"],
        "Credentials rejected. Update your token."
    );
    cfg["channels"][0]["warning"] = json!(false);
    cfg["channels"][0]["status"] = json!("ready");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "settings_channel_detail:channel"
        )["text"],
        "Test accepted"
    );
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    control(&f, p, "settings_tab:channels");
    f.send(p, json!({"op":"test_settings_key","key":"return"}));
    inspect(&f, p);
    assert!(!f.frames(p).iter().any(|a| a["op"] == "setup_save_hub"));
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["settings_open"], false);
}

#[test]
fn v39_manifest_pairing_first_correlates_copy_and_preserves_feedback_on_refresh() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["channels"][0]["pairing_manual_disclosure"] = json!(true);
    cfg["channels"][0]["pairing_open_url_label"] = json!("Open Pairing Link ↗");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "channel_field:channel:account").is_null());
    assert!(node(&s, "settings_items", "save_channel:channel").is_null());
    assert_eq!(
        node(&s, "settings_items", "test_channel:channel")["hidden"],
        true
    );
    control(&f, p, "manual_channel:channel");
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "test_channel:channel")["enabled"],
        false
    );
    f.send(
        p,
        json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"42"}),
    );
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "test_channel:channel")["enabled"],
        true
    );
    f.send(
        p,
        json!({"op":"test_settings_input","id":"channel_field:channel:account","value":""}),
    );
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "test_channel:channel")["enabled"],
        false
    );
    control(&f, p, "manual_channel:channel");
    control(&f, p, "pair_channel:channel");
    inspect(&f, p);
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "channel_pair")
        .unwrap();
    let mut pair = json!({"op":"pair_code","hub":request["hub"],"channel_id":"channel","request_id":"unrelated","code":"cito-example","instruction":"Send /start cito-example","pairing_command":"/start cito-example","pairing_url":"https://example.com/pair"});
    f.send(p, pair.clone());
    assert_ne!(inspect(&f, p)["clipboard"], "/start cito-example");
    pair["request_id"] = request["request_id"].clone();
    f.send(p, pair);
    let s = inspect(&f, p);
    assert_eq!(s["clipboard"], "/start cito-example");
    assert_eq!(
        node(&s, "settings_items", "copy_pair_command:channel")["text"],
        "✓ Copied"
    );
    assert_eq!(
        node(&s, "settings_items", "open_pair_url:channel")["text"],
        "Open Pairing Link ↗"
    );
    assert!(node(&s, "settings_items", "pair_channel:channel").is_null());
    cfg["channels"][0]["pairing_instruction"] = json!("Send /start cito-example");
    cfg["channels"][0]["pairing_command"] = json!("/start cito-example");
    cfg["channels"][0]["pairing_url"] = json!("https://example.com/pair");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "copy_pair_command:channel"
        )["text"],
        "✓ Copied"
    );
}
#[test]
fn inbox_attachments_pause_agents_and_unavailable_details_follow_live_state() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    f.send(p, state());
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    let mut frame = state();
    let mut human = listener();
    human["harness"] = json!("human");
    human["title"] = json!("Human");
    human["session_id"] = json!("default");
    human["scoped_id"] = json!("personal:human");
    human["display_address"] = json!("@alice");
    human["host_name"] = json!("arbitrary-desktop.local");
    frame["hubs"][0]["human_listener_id"] = json!("human-node");
    frame["hubs"][0]["human_control_revision"] = json!(7);
    frame["hubs"][0]["human_state_status"] = json!("current");
    frame["hubs"][0]["human_paused"] = json!(false);
    cfg["hubs"][0]["human_control_revision"] = json!(7);
    cfg["hubs"][0]["human_state_status"] = json!("current");
    let local = json!({"channel_id":"here:human","location":"device","host_id":"here","display_name":"laptop.local","availability":"ACTIVE","is_local":true});
    let other = json!({"channel_id":"other:human","location":"device","host_id":"other","display_name":"desktop.local","availability":"ACTIVE","is_local":false});
    let external = json!({"channel_id":"hub:channel","location":"hub","plugin_id":"channel","display_name":"Messenger","availability":"ACTIVE","is_local":false});
    for (attachments, summary) in [
        (json!([]), "Activity"),
        (json!([local.clone()]), "Activity · laptop"),
        (
            json!([local.clone(), other.clone()]),
            "Activity · desktop · laptop",
        ),
        (
            json!([external.clone(), other.clone()]),
            "Messenger · Activity · desktop",
        ),
        (json!([external, local]), "Messenger · Activity · laptop"),
    ] {
        frame["hubs"][0]["attachments"] = attachments.clone();
        cfg["hubs"][0]["attachments"] = attachments;
        frame["settings"] = cfg.clone();
        f.send(p, frame.clone());
        let snapshot = inspect(&f, p);
        assert!(
            nodes(&snapshot["settings_items"])
                .iter()
                .any(|n| n["text"] == summary),
            "{summary}"
        );
        assert_eq!(node(&snapshot, "items", "stop_all")["text"], "Stop All");
        assert_eq!(
            node(&snapshot, "settings_items", "toggle_inbox")["enabled"],
            true
        );
    }
    control(&f, p, "toggle_inbox");
    inspect(&f, p);
    let update = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "human_inbox_update")
        .unwrap();
    assert_eq!(update["expected_control_revision"], 7);
    assert_eq!(update["paused"], true);
    control(&f, p, "stop_all");
    inspect(&f, p);
    let stop = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "unlisten_all")
        .unwrap();
    assert_eq!(stop["hub"], "personal");
    cfg["channels"][0]["status"] = json!("error");
    cfg["channels"][0]["fields"] = json!([]);
    cfg["channels"][0]["warning"] = json!(true);
    cfg["channels"][0]["detail"]=json!("Connection unavailable.\nThe remote gateway rejected the request.\nCheck credentials and try again.");
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    let detail = node(&s, "settings_items", "settings_channel_detail:channel");
    let card = node(&s, "settings_items", "settings_channel:channel");
    assert!(detail["height"].as_f64().unwrap() > 28.0);
    assert!(card["height"].as_f64().unwrap() >= detail["height"].as_f64().unwrap() + 46.0);
    assert!(detail["y"].as_f64().unwrap() >= 8.0);
    for appearance in ["light", "dark"] {
        inspect_as(&f, p, Some(appearance));
    }
    let recovery = node(&s, "settings_items", "channel_recovery_bar:channel");
    let rightmost = nodes(&recovery["children"])
        .into_iter()
        .filter(|node| {
            node["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("recover_channel:"))
        })
        .max_by(|a, b| {
            a["x"]
                .as_f64()
                .unwrap()
                .total_cmp(&b["x"].as_f64().unwrap())
        })
        .unwrap();
    for (kind, issue) in [
        (
            "bar_anchor",
            "settings_channel:channel: recovery row is not edge anchored",
        ),
        (
            "bar_midY",
            "settings_channel:channel: recovery row controls are not vertically centered",
        ),
    ] {
        f.send(
            p,
            json!({"op":"test_oracle_calibration","kind":kind,"id":rightmost["id"]}),
        );
        let snapshot = inspect_unchecked(&f, p, None);
        assert_eq!(
            snapshot["non_anchored_action_bars"],
            json!([issue]),
            "{kind}"
        );
        f.send(p, json!({"op":"test_oracle_calibration","kind":"reset"}));
        inspect(&f, p);
    }
}

#[test]
fn v39_personal_edit_unpair_repair_and_cancel_reset_transient_ui() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("42");
    cfg["channels"][0]["pairing_manual_disclosure"] = json!(true);
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    for cancel in ["cancel_edit_user_channel:channel", "escape"] {
        control(&f, p, "edit_user_channel:channel");
        f.send(p,json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"discard"}));
        if cancel == "escape" {
            f.send(p, json!({"op":"test_settings_key","key":"escape"}));
        } else {
            control(&f, p, cancel);
        }
        let s = inspect(&f, p);
        assert!(!node(&s, "settings_items", "channel_summary:channel").is_null());
        control(&f, p, "edit_user_channel:channel");
        assert_eq!(
            node(
                &inspect(&f, p),
                "settings_items",
                "channel_field:channel:account"
            )["text"],
            "42"
        );
        control(&f, p, "cancel_edit_user_channel:channel");
    }
    control(&f, p, "edit_user_channel:channel");
    control(&f, p, "unpair_channel:channel");
    confirm_channel_action(&f, p, "channel_unpair");
    cfg["channels"][0]["configured"] = json!(false);
    cfg["channels"][0]["status"] = json!("hub_ready");
    cfg["channels"][0]["fields"][1]["configured"] = json!(false);
    cfg["channels"][0]["fields"][1]["value"] = json!("");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "channel_field:channel:account").is_null());
    let manual = node(&s, "settings_items", "manual_channel:channel");
    assert_eq!(manual["x"], 12);
    assert_eq!(manual["children"][0]["x"], 0);
    let width = manual["width"].as_f64().unwrap();
    assert!(width >= manual["label_measured_width"].as_f64().unwrap());
    assert!(
        width < 190.0,
        "short disclosure text should not reserve a fixed width"
    );
    for round in 0..2 {
        control(&f, p, "pair_channel:channel");
        inspect(&f, p);
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|v| v["op"] == "channel_pair")
            .unwrap();
        f.send(p,json!({"op":"pair_code","hub":request["hub"],"channel_id":"channel","request_id":request["request_id"],"instruction":"Send /start cito-example","pairing_command":"/start cito-example"}));
        assert_eq!(
            node(
                &inspect(&f, p),
                "settings_items",
                "copy_pair_command:channel"
            )["text"],
            "✓ Copied"
        );
        if round == 0 {
            control(&f, p, "cancel_pair_channel:channel");
            confirm_channel_action(&f, p, "channel_cancel_pair");
            f.send(p, json!({"op":"settings","settings":cfg}));
            let s = inspect(&f, p);
            assert!(node(&s, "settings_items", "copy_pair_command:channel").is_null());
        }
    }
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("42");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert!(!node(&s, "settings_items", "channel_summary:channel").is_null());
    assert!(node(&s, "settings_items", "channel_field:channel:account").is_null());
    control(&f, p, "edit_user_channel:channel");
    control(&f, p, "pair_channel:channel");
    inspect(&f, p);
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "channel_pair")
        .unwrap();
    f.send(p,json!({"op":"pair_code","hub":request["hub"],"channel_id":"channel","request_id":request["request_id"],"instruction":"Send /start cito-again","pairing_command":"/start cito-again"}));
    assert!(!node(
        &inspect(&f, p),
        "settings_items",
        "copy_pair_command:channel"
    )
    .is_null());
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert!(!node(&s, "settings_items", "channel_summary:channel").is_null());
    assert!(node(&s, "settings_items", "channel_field:channel:account").is_null());
}

#[test]
fn v39_settings_resets_preserve_newer_pairing_request_and_auto_copy() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    for transition in ["save_ack", "configured", "instruction_removed"] {
        let mut cfg = settings();
        let paired = transition != "configured";
        cfg["channels"][0]["configured"] = json!(paired);
        cfg["channels"][0]["status"] = json!(if paired { "ready" } else { "hub_ready" });
        cfg["channels"][0]["fields"][1]["configured"] = json!(paired);
        cfg["channels"][0]["fields"][1]["value"] = json!(if paired { "42" } else { "" });
        f.send(p, json!({"op":"settings","settings":cfg}));
        if paired {
            control(&f, p, "edit_user_channel:channel");
        }
        // Save S is unchanged and remains pending when Re-pair P is submitted.
        let save = if transition == "save_ack" {
            control(&f, p, "save_channel:channel");
            inspect(&f, p);
            Some(
                f.frames(p)
                    .into_iter()
                    .rev()
                    .find(|v| v["op"] == "channel_save")
                    .unwrap(),
            )
        } else {
            None
        };
        control(&f, p, "pair_channel:channel");
        inspect(&f, p);
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|v| v["op"] == "channel_pair")
            .unwrap();
        f.send(p, json!({"op":"test_clipboard","text":"keep clipboard"}));
        if let Some(save) = save {
            assert_eq!(save["values"]["account"], "42");
            assert_ne!(save["request_id"], request["request_id"]);
            f.send(p, json!({"op":"settings","settings":cfg,"ok":true,"source_op":"channel_save","channel_id":"channel","request_id":save["request_id"]}));
            assert_eq!(
                node(&inspect(&f, p), "settings_items", "channel_summary:channel")["text"],
                "✓ Saved"
            );
        } else {
            // Both a configured transition and disappearing instructions can
            // reset the view before P's correlated receipt reaches the UI.
            if transition == "instruction_removed" {
                cfg["channels"][0]["pairing_instruction"] = json!("Send /start cito-pending");
                cfg["channels"][0]["pairing_command"] = json!("/start cito-pending");
                f.send(p, json!({"op":"settings","settings":cfg}));
                inspect(&f, p);
            }
            cfg["channels"][0]["configured"] = json!(true);
            cfg["channels"][0]["status"] = json!("ready");
            cfg["channels"][0]["fields"][1]["configured"] = json!(true);
            cfg["channels"][0]["fields"][1]["value"] = json!("42");
            cfg["channels"][0]["pairing_instruction"] = Value::Null;
            cfg["channels"][0]["pairing_command"] = Value::Null;
            f.send(p, json!({"op":"settings","settings":cfg}));
        }
        assert_eq!(
            inspect(&f, p)["clipboard"],
            "keep clipboard",
            "{transition}"
        );
        f.send(p, json!({"op":"pair_code","hub":request["hub"],"channel_id":"channel","request_id":request["request_id"],"instruction":"Send /start cito-new","pairing_command":"/start cito-new"}));
        let s = inspect(&f, p);
        assert_eq!(s["clipboard"], "/start cito-new", "{transition}");
        assert_eq!(
            node(&s, "settings_items", "copy_pair_command:channel")["text"],
            "✓ Copied",
            "{transition}"
        );
    }
}

#[test]
fn v39_pairing_invalidation_rejects_late_receipts_without_clipboard_or_instruction_effects() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    for transition in [
        "cancel_pair_channel",
        "unpair_channel",
        "clear_channel_service",
    ] {
        let mut cfg = settings();
        cfg["channels"][0]["configured"] = json!(true);
        cfg["channels"][0]["status"] = json!("ready");
        cfg["channels"][0]["fields"][1]["configured"] = json!(true);
        cfg["channels"][0]["fields"][1]["value"] = json!("42");
        f.send(p, json!({"op":"settings","settings":cfg}));
        control(&f, p, "edit_user_channel:channel");
        control(&f, p, "save_channel:channel");
        inspect(&f, p);
        let save = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|v| v["op"] == "channel_save")
            .unwrap();
        f.send(p,json!({"op":"settings","settings":cfg,"ok":true,"source_op":"channel_save","channel_id":"channel","request_id":save["request_id"]}));
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "channel_summary:channel")["text"],
            "✓ Saved"
        );
        control(&f, p, "edit_user_channel:channel");
        control(&f, p, "pair_channel:channel");
        inspect(&f, p);
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|v| v["op"] == "channel_pair")
            .unwrap();
        // A live inventory can expose the pending code before the action receipt arrives.
        cfg["channels"][0]["pairing_instruction"] = json!("Send /start cito-pending");
        cfg["channels"][0]["pairing_command"] = json!("/start cito-pending");
        f.send(p, json!({"op":"settings","settings":cfg}));
        assert!(!node(
            &inspect(&f, p),
            "settings_items",
            "pairing_instruction:channel"
        )
        .is_null());
        f.send(p, json!({"op":"test_clipboard","text":"keep clipboard"}));
        control(&f, p, &format!("{transition}:channel"));
        // Destructive UI retirement belongs to the correlated committed receipt.
        let pending = inspect(&f, p);
        assert!(!node(&pending, "settings_items", "pairing_instruction:channel").is_null());
        confirm_channel_action(
            &f,
            p,
            match transition {
                "cancel_pair_channel" => "channel_cancel_pair",
                "unpair_channel" => "channel_unpair",
                _ => "disconnect_human_channel",
            },
        );
        let s = inspect(&f, p);
        assert!(
            node(&s, "settings_items", "pairing_instruction:channel").is_null(),
            "{transition}"
        );
        assert!(
            node(&s, "settings_items", "copy_pair_command:channel").is_null(),
            "{transition}"
        );
        assert_ne!(
            node(&s, "settings_items", "channel_summary:channel")["text"],
            "✓ Saved",
            "{transition}"
        );
        let late = json!({"op":"pair_code","hub":request["hub"],"channel_id":"channel","request_id":request["request_id"],"instruction":"Send /start cito-late","pairing_command":"/start cito-late","pairing_url":"https://example.com/late"});
        f.send(p, late.clone());
        let s = inspect(&f, p);
        assert_eq!(s["clipboard"], "keep clipboard", "{transition}");
        assert!(
            node(&s, "settings_items", "pairing_instruction:channel").is_null(),
            "{transition}"
        );
        assert!(
            node(&s, "settings_items", "copy_pair_command:channel").is_null(),
            "{transition}"
        );
        cfg["channels"][0]["pairing_instruction"] = Value::Null;
        cfg["channels"][0]["pairing_command"] = Value::Null;
        f.send(p, json!({"op":"settings","settings":cfg}));
        f.send(p, late);
        assert_eq!(
            inspect(&f, p)["clipboard"],
            "keep clipboard",
            "{transition}"
        );
    }
}

#[test]
fn v39_live_channel_health_overrides_stored_test_success_and_recovers() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("42");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "test_channel:channel");
    inspect(&f, p);
    assert!(f.frames(p).iter().any(|v| v["op"] == "channel_test"));
    let tested = "Test message accepted by the channel";
    f.send(
        p,
        json!({"op":"channel_test_result","channel_id":"channel","ok":true,"detail":tested}),
    );
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "settings_channel_detail:channel"
        )["text"],
        tested
    );
    for (status, warning, detail, presented) in [
        (
            "ready",
            true,
            "HTTP 409: another polling request is active. Stop the other poller.",
            "HTTP another polling request is active. Stop the other poller.",
        ),
        (
            "ready",
            true,
            "Delivery outcome unknown. Check the destination before retrying.",
            "Delivery outcome unknown. Check the destination before retrying.",
        ),
        (
            "error",
            false,
            "HTTP 401: credentials rejected. Update the gateway token.",
            "HTTP credentials rejected. Update the gateway token.",
        ),
    ] {
        cfg["channels"][0]["status"] = json!(status);
        cfg["channels"][0]["warning"] = json!(warning);
        cfg["channels"][0]["detail"] = json!(detail);
        f.send(p, json!({"op":"settings","settings":cfg}));
        let s = inspect(&f, p);
        assert_eq!(
            node(&s, "settings_items", "settings_channel_detail:channel")["text"],
            presented
        );
        assert_eq!(
            node(&s, "settings_items", "settings_channel_detail:channel")["tooltip"],
            presented
        );
        assert_eq!(
            node(&s, "settings_items", "settings_channel_status:channel")["text"],
            if status == "error" {
                "⚠ Error"
            } else {
                "Connected"
            }
        );
        cfg["channels"][0]["status"] = json!("ready");
        cfg["channels"][0]["warning"] = json!(false);
        cfg["channels"][0]["detail"] = json!("Connected");
        f.send(p, json!({"op":"settings","settings":cfg}));
        let s = inspect(&f, p);
        assert_eq!(
            node(&s, "settings_items", "settings_channel_status:channel")["text"],
            "Connected"
        );
        // The last test receipt remains useful after health recovers, until the next mutation.
        assert_eq!(
            node(&s, "settings_items", "settings_channel_detail:channel")["text"],
            tested
        );
    }
}

#[test]
fn r1_save_acknowledgements_clear_only_matching_secret_intent_not_tests_or_newer_drafts() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    cfg["channels"][0]["fields"][0]["value"] = json!("");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "edit_hub_service:channel");
    let save = |value: &str| {
        f.send(
            p,
            json!({"op":"test_settings_input","id":"channel_field:channel:secret","value":value}),
        );
        control(&f, p, "save_hub_service:channel");
        inspect(&f, p);
        f.frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "save_hub_service")
            .unwrap()
    };
    let first = save("one");
    let second = save("two");
    assert_ne!(first["request_id"], second["request_id"]);
    f.send(p,json!({"op":"settings","ok":true,"source_op":"save_hub_service","channel_id":"channel","request_id":first["request_id"],"settings":cfg}));
    assert!(
        !node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:secret"
        )
        .is_null(),
        "An older save cannot close the current edit"
    );
    f.send(p,json!({"op":"settings","ok":true,"source_op":"channel_test","channel_id":"channel","settings":cfg}));
    assert!(
        !node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:secret"
        )
        .is_null(),
        "A test is not a save acknowledgement"
    );
    f.send(p,json!({"op":"test_settings_input","id":"channel_field:channel:secret","value":"newer-draft"}));
    f.send(p,json!({"op":"settings","ok":true,"source_op":"save_hub_service","channel_id":"channel","request_id":second["request_id"],"settings":cfg}));
    let newer = inspect(&f, p);
    assert!(
        !node(&newer, "settings_items", "channel_field:channel:secret").is_null(),
        "A matching acknowledgement must keep a newer draft editable"
    );
    assert_ne!(
        node(&newer, "settings_items", "channel_summary:channel")["text"],
        "✓ Saved"
    );
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    let third = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "save_hub_service")
        .unwrap();
    assert_eq!(third["values"], json!({"secret":"newer-draft"}));
    f.send(p,json!({"op":"settings","ok":false,"source_op":"save_hub_service","channel_id":"channel","request_id":third["request_id"],"settings":cfg}));
    assert!(!node(
        &inspect(&f, p),
        "settings_items",
        "channel_field:channel:secret"
    )
    .is_null());
    f.send(p,json!({"op":"settings","ok":true,"source_op":"save_hub_service","channel_id":"channel","request_id":third["request_id"],"settings":cfg}));
    assert!(
        !node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:secret"
        )
        .is_null(),
        "A late success cannot acknowledge an already failed submission"
    );
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    let retry = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "save_hub_service")
        .unwrap();
    assert_ne!(retry["request_id"], third["request_id"]);
    assert_eq!(retry["values"], json!({"secret":"newer-draft"}));
    f.send(p,json!({"op":"settings","ok":true,"source_op":"save_hub_service","channel_id":"channel","request_id":retry["request_id"],"settings":cfg}));
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "stratum_hub:channel")["height"],
        36
    );
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "save_hub_service")
            .unwrap()["values"],
        json!({})
    );
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["paired"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "edit_user_channel:channel");
    f.send(p,json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"submitted"}));
    control(&f, p, "save_channel:channel");
    inspect(&f, p);
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "channel_save")
        .unwrap();
    f.send(p,json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"newer-account"}));
    cfg["channels"][0]["fields"][1]["value"] = json!("submitted");
    f.send(p,json!({"op":"settings","ok":true,"source_op":"channel_save","channel_id":"channel","request_id":request["request_id"],"settings":cfg}));
    let newer = inspect(&f, p);
    assert_eq!(
        node(&newer, "settings_items", "channel_field:channel:account")["text"],
        "newer-account"
    );
    assert_eq!(
        node(&newer, "settings_items", "save_channel:channel")["text"],
        "Save"
    );
    control(&f, p, "save_channel:channel");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "channel_save")
            .unwrap()["values"]["account"],
        "newer-account"
    );
}

#[test]
fn r1_overlapping_channel_scopes_acknowledge_secrets_and_gateway_edits_independently() {
    let _lock = serial();
    for hub_first in [false, true] {
        let f = Fixture::new(json!({}));
        let p = start(&f);
        let mut cfg = settings();
        cfg["settings_hub"] = json!("personal");
        cfg["channels"][0]["fields"][0]["value"] = json!("");
        cfg["channels"][0]["fields"][1]["type"] = json!("secret");
        cfg["channels"][0]["fields"][1]["configured"] = json!(true);
        let mut frame = state();
        frame["settings"] = cfg.clone();
        f.send(p, frame);
        control(&f, p, "open_settings");
        control(&f, p, "settings_tab:channels");
        control(&f, p, "edit_hub_service:channel");
        for (field, value, control_id) in [
            ("secret", "gateway-token", "save_hub_service:channel"),
            ("account", "personal-token", "save_channel:channel"),
        ] {
            f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{field}"),"value":value}));
            control(&f, p, control_id);
        }
        inspect(&f, p);
        let latest = |op: &str| {
            f.frames(p)
                .into_iter()
                .rev()
                .find(|a| a["op"] == op)
                .unwrap()
        };
        let gateway = latest("save_hub_service");
        let personal = latest("channel_save");
        assert_eq!(gateway["values"], json!({"secret":"gateway-token"}));
        assert_eq!(personal["values"], json!({"account":"personal-token"}));
        assert_ne!(gateway["request_id"], personal["request_id"]);
        let ordered = if hub_first {
            [&gateway, &personal]
        } else {
            [&personal, &gateway]
        };
        for (index, action) in ordered.into_iter().enumerate() {
            f.send(p, json!({"op":"settings","ok":true,"source_op":action["op"],"channel_id":"channel","request_id":action["request_id"],"settings":cfg}));
            let s = inspect(&f, p);
            assert_eq!(
                node(&s, "settings_items", "stratum_hub:channel")["height"] == 36,
                hub_first || index == 1,
                "Only a gateway acknowledgement may collapse the gateway editor"
            );
            if !hub_first && index == 0 {
                control(&f, p, "save_channel:channel");
                inspect(&f, p);
                assert_eq!(latest("channel_save")["values"], json!({}));
                assert!(!node(&s, "settings_items", "channel_field:channel:secret").is_null());
            }
        }
        control(&f, p, "edit_hub_service:channel");
        control(&f, p, "save_hub_service:channel");
        control(&f, p, "save_channel:channel");
        inspect(&f, p);
        for op in ["save_hub_service", "channel_save"] {
            assert_eq!(
                latest(op)["values"],
                json!({}),
                "Acknowledged secrets must not be resubmitted: {op}"
            );
        }
    }
}

#[test]
fn shared_human_inbox_survives_local_detach_and_disables_mutations_while_offline() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["listeners"] = json!([]);
    frame["settings"] = settings();
    for hub in frame["hubs"].as_array_mut().unwrap() {
        hub["human_state_status"] = json!("current");
        hub["human_control_revision"] = json!(3);
    }
    frame["hubs"][0]["human_paused"] = json!(false);
    frame["hubs"][0]["human_availability"] = json!("ACTIVE");
    frame["hubs"][1]["human_paused"] = json!(true);
    frame["settings"]["hubs"][1]["human_paused"] = json!(true);
    frame["settings"]["hubs"][1]["human_control_revision"] = json!(3);
    frame["settings"]["hubs"][1]["human_state_status"] = json!("current");
    frame["available"] =
        json!([{"harness":"fixture","session_id":"s2","title":"Session","cwd":"/tmp"}]);
    f.send(p, frame.clone());
    let active = inspect(&f, p);
    assert!(node(&active, "items", "human_inbox_card").is_null());
    assert!(node(&active, "items", "inbox_paused_notice").is_null());
    control(&f, p, "target_hub:work");
    let remote = inspect(&f, p);
    assert_eq!(
        node(&remote, "items", "header_subtitle")["text"],
        "Inbox paused"
    );
    assert_eq!(
        node(&remote, "items", "inbox_paused_notice")["text"],
        "Inbox paused · Resume in Settings…"
    );
    assert_eq!(remote["count"], 0);
    assert!(node(&remote, "items", "stop_all").is_null());
    control(&f, p, "inbox_paused_notice");
    let selected = inspect(&f, p);
    assert_eq!(selected["settings_tab"], "channels");
    assert!(f
        .frames(p)
        .iter()
        .any(|a| a["op"] == "open_settings" && a["hub"] == "work"));
    assert_eq!(
        node(&selected, "settings_items", "toggle_inbox")["text"],
        "Resume Inbox"
    );
    frame["hubs"][1]["status"] = json!("disconnected");
    frame["settings"]["hubs"][1]["status"] = json!("disconnected");
    f.send(p, frame.clone());
    let offline = inspect(&f, p);
    assert_eq!(
        node(&offline, "settings_items", "toggle_inbox")["enabled"],
        false
    );
    assert_eq!(
        node(&offline, "items", "header_subtitle")["text"],
        "Hub unavailable"
    );
    let before = f
        .frames(p)
        .iter()
        .filter(|a| a["op"] == "human_inbox_update")
        .count();
    control(&f, p, "toggle_inbox");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|a| a["op"] == "human_inbox_update")
            .count(),
        before
    );
    frame["hubs"][1]["status"] = json!("connected");
    frame["settings"]["hubs"][1]["status"] = json!("connected");
    f.send(p, frame);
    control(&f, p, "toggle_inbox");
    let pending = inspect(&f, p);
    assert_eq!(
        node(&pending, "settings_items", "toggle_inbox")["text"],
        "Resuming…"
    );
    assert_eq!(
        node(&pending, "settings_items", "toggle_inbox")["enabled"],
        false
    );
    let update = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "human_inbox_update")
        .unwrap();
    assert_eq!(update["hub"], "work");
    assert_eq!(update["paused"], false);
    assert_eq!(update["expected_control_revision"], 3);
    assert!(update.get("about").is_none());
}

#[test]
fn lifecycle_channel_test_results_survive_refresh_until_mutation() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");

    for action in [
        "save_channel",
        "pair_channel",
        "toggle_channel",
        "unpair_channel",
        "clear_channel_service",
    ] {
        let configured = action != "clear_channel_service";
        cfg["channels"][0]["configured"] = json!(configured);
        cfg["channels"][0]["status"] = json!(if configured { "ready" } else { "hub_ready" });
        f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","ok":false,"error":"Invalid token"}));
        f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
        let mut frame = state();
        frame["settings"] = cfg.clone();
        f.send(p, frame);
        control(&f, p, "test_channel:channel");
        let s = inspect(&f, p);
        assert_eq!(
            node(&s, "settings_items", "settings_channel_detail:channel")["text"],
            "Invalid token",
            "{action}"
        );
        assert_eq!(
            node(&s, "settings_items", "settings_channel_status:channel")["text"],
            "⚠ Error",
            "{action}"
        );
        if ["save_channel", "pair_channel"].contains(&action) {
            control(&f, p, "edit_user_channel:channel");
        }
        control(&f, p, &format!("{action}:channel"));
        let s = inspect(&f, p);
        assert!(
            node(&s, "settings_items", "settings_channel_detail:channel").is_null(),
            "{action} must clear the previous result"
        );
        assert_eq!(
            node(&s, "settings_items", "settings_channel_status:channel")["text"],
            if configured {
                "Connected"
            } else {
                "Account Not Paired"
            }
        );
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| a["request_id"].is_string() && a["channel_id"] == "channel")
            .unwrap();
        f.send(p, json!({"op":"settings","source_op":request["op"],"request_id":request["request_id"],"hub":"personal","channel_id":"channel","committed":true}));
        inspect(&f, p);
    }

    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","ok":true,"detail":"Test sent"}));
    f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "settings_channel_detail:channel"
        )["text"],
        "Test sent"
    );
    for action in ["save_hub_service", "clear_channel_service"] {
        for ok in [false, true] {
            cfg["channels"][0]["configured"] = json!(false);
            cfg["channels"][0]["status"] = json!("not_configured");
            cfg["channels"][0]["hub_ready"] = json!(false);
            cfg["channels"][0]["fields"][0]["configured"] = json!(false);
            cfg["channels"][0]["fields"][0]["value"] = json!("");
            f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
            control(&f, p, "test_channel:channel");
            let detail = if ok { "Test sent" } else { "Invalid token" };
            f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","ok":ok,"detail":detail}));
            f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
            let mut frame = state();
            frame["settings"] = cfg.clone();
            f.send(p, frame);
            let s = inspect(&f, p);
            assert_eq!(
                node(&s, "settings_items", "settings_channel_detail:channel")["text"],
                detail,
                "{action}, ok={ok}"
            );
            assert_eq!(
                node(&s, "settings_items", "settings_channel_status:channel")["text"],
                if ok { "Not Set Up" } else { "⚠ Error" }
            );
            if action == "clear_channel_service" {
                // Clear Token becomes available once the hub service is configured.
                cfg["channels"][0]["status"] = json!("hub_ready");
                cfg["channels"][0]["hub_ready"] = json!(true);
                cfg["channels"][0]["fields"][0]["configured"] = json!(true);
                f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
                assert_eq!(
                    node(
                        &inspect(&f, p),
                        "settings_items",
                        "settings_channel_detail:channel"
                    )["text"],
                    detail
                );
            }
            control(&f, p, &format!("{action}:channel"));
            let s = inspect(&f, p);
            assert!(
                node(&s, "settings_items", "settings_channel_detail:channel").is_null(),
                "{action} must clear the candidate test result, ok={ok}"
            );
            assert_eq!(
                node(&s, "settings_items", "settings_channel_status:channel")["text"],
                if action == "clear_channel_service" {
                    "Account Not Paired"
                } else {
                    "Not Set Up"
                }
            );
            // Finish this mutation before the next loop iteration submits the
            // same values; pending controls intentionally reject duplicate clicks.
            let request = f
                .frames(p)
                .into_iter()
                .rev()
                .find(|a| a["op"] == "save_hub_service" || a["op"] == "disconnect_human_channel")
                .unwrap();
            f.send(p, json!({"op":"settings","source_op":request["op"],"request_id":request["request_id"],"hub":"personal","channel_id":"channel","committed":true}));
            inspect(&f, p);
        }
    }
}

#[test]
fn lifecycle_channel_ui_retains_redacted_secret_and_preserves_pairing_on_refresh() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    let ch = &mut cfg["channels"][0];
    ch["enabled"] = json!(false);
    ch["status"] = json!("hub_ready");
    ch["setup_guide"] = json!(["Create a service credential.", "Pair your private account."]);
    ch["fields"][0]["value"] = json!("");
    ch["fields"][0]["hidden"] = json!(false);
    ch["fields"][0]["redacted"] = json!(true);
    ch["fields"][0]["configured_placeholder"] = json!("Saved — enter replacement");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "stratum_hub:channel")["height"],
        36
    );
    assert!(node(&s, "settings_items", "channel_field:channel:secret").is_null());
    control(&f, p, "edit_hub_service:channel");
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:secret")["secure"],
        true
    );
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:secret")["placeholder"],
        "Saved — enter replacement"
    );
    assert!(node(&s, "settings_items", "channel_setup_guide:channel").is_null());
    f.send(
        p,
        json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"42"}),
    );
    for (button, op) in [
        ("save_channel:channel", "channel_save"),
        ("test_channel:channel", "channel_test"),
        ("pair_channel:channel", "channel_pair"),
    ] {
        control(&f, p, button);
        wait(|| f.frames(p).iter().any(|v| v["op"] == op));
        let action = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|v| v["op"] == op)
            .unwrap();
        assert!(action["values"].get("secret").is_none(), "{action}");
    }
    cfg["channels"][0]["pairing_code"] = json!("cito-ABCDEFGHIJ");
    cfg["channels"][0]["pairing_command"] = json!("/start cito-ABCDEFGHIJ");
    cfg["channels"][0]["pairing_instruction"] =
        json!("Send /start cito-ABCDEFGHIJ to your account");
    cfg["channels"][0]["status"] = json!("pairing_pending");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "settings_channel_status:channel")["text"],
        "◌ Pairing…"
    );
    assert!(!node(&s, "settings_items", "pairing_instruction:channel").is_null());
    control(&f, p, "copy_pair_command:channel");
    assert_eq!(inspect(&f, p)["clipboard"], "/start cito-ABCDEFGHIJ");
    control(&f, p, "cancel_pair_channel:channel");
    wait(|| f.frames(p).iter().any(|v| v["op"] == "channel_cancel_pair"));
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("paused");
    cfg["channels"][0]["pairing_instruction"] = Value::Null;
    cfg["channels"][0]["pairing_code"] = Value::Null;
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "settings_channel_status:channel")["text"],
        "Paused"
    );
    assert_eq!(
        node(&s, "settings_items", "toggle_channel:channel")["text"],
        "Resume Human Channel"
    );
    control(&f, p, "toggle_channel:channel");
    control(&f, p, "unpair_channel:channel");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "channel_toggle" && v["enabled"] == true)
    });
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "channel_unpair" && v["reset_user"] == true)
    });
}
#[test]
fn channel_cancel_unpair_and_clear_discard_only_their_scope_drafts() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("saved-account");
    cfg["channels"][0]["fields"][0]["value"] = json!("");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "edit_hub_service:channel");
    for (field, value) in [("secret", "discard-gateway"), ("account", "keep-personal")] {
        f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{field}"),"value":value}));
    }
    control(&f, p, "cancel_edit_hub_service:channel");
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    let latest = |op: &str| {
        f.frames(p)
            .into_iter()
            .rev()
            .find(|v| v["op"] == op)
            .unwrap()
    };
    assert_eq!(latest("save_hub_service")["values"], json!({}));
    f.send(p, json!({"op":"test_settings_input","id":"channel_field:channel:secret","value":"escape-discard"}));
    let mut remote = cfg.clone();
    remote["settings_hub"] = json!("work");
    f.send(p, json!({"op":"settings","hub":"work","settings":remote}));
    control(&f, p, "channel_hub_selector:work");
    control(&f, p, "edit_hub_service:channel");
    for (field, value) in [("secret", "escape-work"), ("account", "keep-work")] {
        f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{field}"),"value":value}));
    }
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    for hub in ["work", "personal"] {
        control(&f, p, &format!("channel_hub_selector:{hub}"));
        control(&f, p, "edit_hub_service:channel");
        control(&f, p, "save_hub_service:channel");
        inspect(&f, p);
        assert_eq!(
            latest("save_hub_service")["values"],
            if hub == "work" {
                json!({})
            } else {
                json!({"secret":"escape-discard"})
            },
            "Escape only discards gateway drafts on the selected Hub: {hub}"
        );
        control(&f, p, "save_channel:channel");
        inspect(&f, p);
        assert_eq!(
            latest("channel_save")["values"],
            json!({"account":if hub == "work" { "keep-work" } else { "keep-personal" }})
        );
    }
    control(&f, p, "save_channel:channel");
    inspect(&f, p);
    assert_eq!(
        latest("channel_save")["values"],
        json!({"account":"keep-personal"})
    );
    control(&f, p, "unpair_channel:channel");
    confirm_channel_action(&f, p, "channel_unpair");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert!(node(
        &inspect(&f, p),
        "settings_items",
        "channel_field:channel:account"
    )
    .is_null());
    control(&f, p, "edit_user_channel:channel");
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:account"
        )["text"],
        cfg["channels"][0]["fields"][1]["value"]
    );
    for (field, value) in [("secret", "discard-token"), ("account", "discard-account")] {
        f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{field}"),"value":value}));
    }
    control(&f, p, "clear_channel_service:channel");
    confirm_channel_action(&f, p, "disconnect_human_channel");
    cfg["channels"][0]["hub_ready"] = json!(false);
    cfg["channels"][0]["configured"] = json!(false);
    cfg["channels"][0]["status"] = json!("not_configured");
    for field in cfg["channels"][0]["fields"].as_array_mut().unwrap() {
        field["configured"] = json!(false);
        field["value"] = json!("");
    }
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:account"),
        Value::Null
    );
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    assert_eq!(
        latest("save_hub_service")["values"],
        json!({"secret":""}),
        "An unconfigured gateway submits the empty field, never the discarded secret draft"
    );
    cfg["channels"][0]["fields"][0]["configured"] = json!(true);
    cfg["channels"][0]["hub_ready"] = json!(true);
    cfg["channels"][0]["status"] = json!("hub_ready");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "stratum_hub:channel")["height"],
        36,
        "Clearing a gateway also leaves its old edit mode"
    );
    control(&f, p, "pair_channel:channel");
    inspect(&f, p);
    let request = latest("channel_pair");
    f.send(p, json!({"op":"pair_code","hub":"personal","channel_id":"channel","request_id":request["request_id"],"instruction":"Send the link command below","pairing_command":"/link 123456"}));
    control(&f, p, "copy_pair_command:channel");
    assert_eq!(inspect(&f, p)["clipboard"], "/link 123456");
}
