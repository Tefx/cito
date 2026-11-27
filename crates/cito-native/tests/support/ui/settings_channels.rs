use super::settlement::{input, latest, open_settings};
use super::*;

#[test]
fn deferred_channel_editor_keeps_its_rendered_hub_across_passive_reselection() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let field = "channel_field:channel:account";
    let mut personal = settings();
    personal["channels"][0]["fields"][1]["value"] = json!("Personal account");
    let mut frame = state();
    frame["settings"] = personal.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "channel_hub_selector:personal");
    let input = |value: &str| {
        f.send(
            p,
            json!({"op":"test_settings_input","id":field,"value":value}),
        )
    };
    input("Personal draft");
    f.send(p, json!({"op":"test_focus_field","id":field}));
    let before = inspect(&f, p);
    assert_eq!(before["field_editors"][0]["id"], field);

    let mut work = personal;
    work["hubs"][0]["enabled"] = json!(false);
    work["channels"][0]["fields"][1]["value"] = json!("Work account");
    f.send(p, json!({"op":"settings","hub":"work","settings":work}));
    let deferred = inspect(&f, p);
    assert_eq!(deferred["settings_rebuild_deferred"], true);
    assert_eq!(deferred["settings_channel_hub"], "personal");
    assert_eq!(
        deferred.settings(field)["instance"],
        before.settings(field)["instance"]
    );
    assert!(f
        .frames(p)
        .iter()
        .any(|a| a["op"] == "open_settings" && a["hub"] == "work"));

    // Both a continued edit and Return belong to the still-mounted field.
    input("Latest personal draft");
    f.send(
        p,
        json!({"op":"test_settings_key","key":"return","via":"field"}),
    );
    inspect(&f, p);
    let saves: Vec<_> = f.frames_for(p, "channel_save").collect();
    assert_eq!(saves.len(), 1);
    assert_eq!(saves[0]["hub"], "personal");
    assert_eq!(
        saves[0]["values"],
        json!({"account":"Latest personal draft"})
    );

    f.send(p, json!({"op":"test_end_editing"}));
    let work = inspect(&f, p);
    assert_eq!(work["settings_rebuild_deferred"], false);
    assert_eq!(work["settings_channel_hub"], "work");
    assert_eq!(work.settings(field)["text"], "Work account");
    assert_eq!(work.settings("save_channel:channel")["enabled"], true);
}

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
    cycle.until(|s| s.menu("header").is_object());
    cycle.control("open_settings");
    cycle.control("settings_tab:channels");
    cycle.until(|s| !s.settings("save_hub_service:channel").is_null());
    cycle.input("channel_field:channel:credential", "valid");
    cycle.control("save_hub_service:channel");
    cycle.until(|s| !s.settings("channel_field:channel:address").is_null());
    cycle.input("channel_field:channel:address", "personal-room");
    cycle.control("save_channel:channel");
    cycle.until(|s| !s.settings("channel_summary:channel").is_null());
    cycle.control("test_channel:channel");
    cycle.until(|s| {
        s.settings("settings_channel_detail:channel")["text"] == "Fixture test succeeded"
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
    cycle.until(|s| s.settings("stratum_hub:channel")["height"] == 36);
    cycle.control("test_channel:channel");
    let failed =
        cycle.until(|s| s.settings("settings_channel_detail:channel")["text"] == "Invalid token");
    assert!(failed.settings("settings_error_banner").is_null());
    assert_eq!(failed["pending_controls"], json!([]));
    let request_id = cycle
        .results
        .iter()
        .rev()
        .find(|(a, _, _)| a["op"] == "channel_test")
        .unwrap()
        .0["request_id"]
        .clone();
    wait(|| {
        cycle.pump();
        cycle
            .frames
            .iter()
            .any(|frame| frame["request_id"] == request_id && frame["op"] == "settings")
    });
    let frames: Vec<_> = cycle
        .frames
        .iter()
        .filter(|frame| frame["request_id"] == request_id)
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
    let baseline = inspect(&f, fresh_ui).settings("settings_channel_detail:channel");
    f.send(fresh_ui, old_result);
    let stale = inspect(&f, fresh_ui);
    assert_eq!(
        stale.settings("settings_channel_detail:channel")["text"],
        baseline["text"]
    );
    assert_ne!(
        stale.settings("settings_channel_detail:channel")["text"],
        "Fixture test succeeded"
    );
    f.action_ok(json!({"op":"setup_save_hub","name":"work","url":format!("http://127.0.0.1:{}",port()),"set_default":false}));
    f.action_ok(json!({"op":"setup_toggle_hub","hub":"local","enabled":false}));
    // Dispatch from the still-mounted old generation before pumping its refresh.
    cycle.control("unpair_channel:channel");
    let removed = cycle.until(|s| {
        s.settings("settings_channel:channel").is_null() && s["pending_controls"] == json!([])
    });
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
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    let frame = state_with_settings(cfg.clone());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "pair_channel:channel");
    let pair = latest(&f, p, "channel_pair");
    f.send(p, json!({"op":"pair_code","hub":"personal","channel_id":"channel","request_id":pair["request_id"],"instruction":"Send the pairing command","pairing_command":"/start example-code","pairing_url":"https://example.com/pair"}));
    let first = inspect(&f, p);
    let copy = first.settings("copy_pair_command:channel");
    assert_eq!(copy["text"], "✓ Copied");
    let mut warning = json!({"op":"settings","source_op":"channel_pair","hub":"work","channel_id":"channel","request_id":pair["request_id"],"ok":true,"committed":true,"warning":"Pairing started. Refresh is temporarily unavailable."});
    f.send(p, warning.clone());
    assert!(inspect(&f, p).settings("settings_error_banner").is_null());
    warning["hub"] = json!("personal");
    f.send(p, warning.clone());
    let settled = inspect(&f, p);
    assert_eq!(
        settled.settings("settings_error_label")["text"],
        warning["warning"]
    );
    assert_eq!(
        settled.settings("pairing_instruction:channel")["text"],
        "Send the pairing command"
    );
    assert_eq!(settled["pending_controls"], json!([]));
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    warning["warning"] = json!("Duplicate pairing receipt");
    f.send(p, warning);
    assert!(inspect(&f, p).menu("menu_error_row").is_null());
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:apps");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "copy_pair_command:channel");
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
            second.settings("pairing_instruction:channel")["text"],
            "Send the pairing command"
        );
        assert_eq!(
            second.settings("copy_pair_command:channel")["text"],
            "✓ Copied"
        );
        control(&f, p, "copy_pair_command:channel");
        assert_eq!(inspect(&f, p)["clipboard"], "/start example-code");
    }
    cfg["channel_inventory_error"] = Value::Null;
    control(&f, p, "test_channel:channel");
    let test = latest(&f, p, "channel_test");
    f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","request_id":test["request_id"],"ok":false,"detail":"Current test failed"}));
    cfg["channels"][0]["warning"] = json!(true);
    cfg["channels"][0]["detail"] = json!("Older health observation");
    cfg["launch_at_login"] = json!(true);
    f.send(p, json!({"op":"settings","source_op":"channel_test","request_id":test["request_id"],"channel_id":"channel","settings":cfg,"ok":false}));
    let second = inspect(&f, p);
    assert_eq!(second["settings_launch_at_login"], true);
    assert_eq!(
        second.settings("settings_channel_detail:channel")["text"],
        "Current test failed"
    );
    assert!(second.settings("settings_error_banner").is_null());
    f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","request_id":"previous-ui-generation","ok":true,"detail":"Stale test succeeded"}));
    let stale = inspect(&f, p);
    assert_eq!(
        stale.settings("settings_channel_detail:channel")["text"],
        "Current test failed"
    );
    assert_eq!(stale["pending_controls"], json!([]));
}

#[test]
fn same_name_channel_authority_change_discards_edits_pairing_and_test_receipts() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    open_settings(&f, p, &cfg, "channels");
    let field = "channel_field:channel:account";
    for (key, next) in [("url", "https://hub.example.com"), ("user", "bob")] {
        input(&f, p, field, "previous account");
        control(&f, p, "pair_channel:channel");
        let pair = latest(&f, p, "channel_pair");
        f.send(p, json!({"op":"pair_code","hub":"personal","channel_id":"channel","request_id":pair["request_id"],"instruction":"Previous account pairing code"}));
        control(&f, p, "test_channel:channel");
        let request = latest(&f, p, "channel_test");
        let result = json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","request_id":request["request_id"],"ok":true,"detail":"Previous account test"});
        f.send(p, result.clone());
        control(&f, p, "test_channel:channel");
        let pending = latest(&f, p, "channel_test");
        f.send(p, json!({"op":"test_focus_field","id":field}));
        let before = inspect(&f, p);
        assert_eq!(before.settings(field)["text"], "previous account");
        assert_eq!(
            before.settings("settings_channel_detail:channel")["text"],
            result["detail"]
        );
        assert_eq!(
            before.settings("pairing_instruction:channel")["text"],
            "Previous account pairing code"
        );
        cfg["hubs"][0][key] = json!(next);
        if key == "user" {
            let mut partial = cfg.clone();
            partial["channels"] = json!([]);
            f.send(
                p,
                json!({"op":"settings","settings":partial,"truncated_collections":["channels"]}),
            );
            let switched = inspect(&f, p);
            assert_eq!(
                switched.settings("inbox_control_title")["text"],
                "You (@bob)"
            );
            assert!(switched.settings("settings_channel:channel").is_null());
        }
        f.send(p, json!({"op":"settings","settings":cfg}));
        let mut late = result;
        late["request_id"] = pending["request_id"].clone();
        f.send(p, late);
        let fresh = inspect(&f, p);
        assert_eq!(fresh.settings(field)["text"], "");
        assert!(fresh.settings("pairing_instruction:channel").is_null());
        assert!(fresh.settings("settings_channel_detail:channel").is_null());
        assert_eq!(fresh["pending_controls"], json!([]));
    }
}

#[test]
fn personal_save_that_configures_channel_preserves_a_newer_draft() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    open_settings(&f, p, &cfg, "channels");
    let field = "channel_field:channel:account";
    input(&f, p, field, "submitted");
    control(&f, p, "save_channel:channel");
    let request = latest(&f, p, "channel_save");
    input(&f, p, field, "newer");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("submitted");
    f.send(p, json!({"op":"settings","settings":cfg,"source_op":"channel_save","request_id":request["request_id"],"ok":true}));
    assert_eq!(inspect(&f, p).settings(field)["text"], "newer");
    control(&f, p, "save_channel:channel");
    let request = latest(&f, p, "channel_save");
    assert_eq!(request["values"]["account"], "newer");
    cfg["channels"][0]["fields"][1]["value"] = json!("newer");
    f.send(p, json!({"op":"settings","settings":cfg,"source_op":"channel_save","request_id":request["request_id"],"ok":true}));
    assert_eq!(
        inspect(&f, p).settings("channel_summary:channel")["text"],
        "✓ Saved"
    );
}

#[test]
fn oversized_channel_submission_preserves_drafts_without_pending_or_transport_effects() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    let mut second = cfg["channels"][0]["fields"][1].clone();
    second["key"] = json!("second");
    second["label"] = json!("Second address");
    cfg["channels"][0]["fields"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let frame = state_with_settings(cfg);
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
    assert_eq!(s.settings("channel_field:channel:account")["text"], draft);
    assert_eq!(s.settings("channel_field:channel:second")["text"], draft);
    assert!(s.settings("settings_error_label")["text"]
        .as_str()
        .unwrap()
        .contains("too large"));
}

#[test]
fn v41_inbox_bar_deduplicates_attachments_and_fits_paused_remote_defaults() {
    let _lock = serial();
    let (f, p) = ui_fixture();
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
                assert!(s.menu("human_inbox_card").is_null());
                assert!(s.menu("inbox_header").is_null());
                assert_eq!(s.menu("inbox_paused_notice").is_null(), !paused);
                let bar = s.settings("inbox_control_bar");
                let bar_height = bar["height"].as_f64().unwrap();
                assert!(bar_height >= 48.0);
                let toggle = s.settings("toggle_inbox");
                assert_eq!(
                    toggle["y"].as_f64().unwrap() + toggle["height"].as_f64().unwrap() / 2.0,
                    bar_height / 2.0
                );
                let scroll = s.settings("settings_channels_tab");
                assert_eq!(scroll["height"], 368);
                let summary = s.settings("inbox_control_summary");
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
                    s.settings("toggle_inbox")["text"],
                    if paused {
                        "Resume Inbox"
                    } else {
                        "Pause Inbox"
                    }
                );
                assert_eq!(s.settings("scope:inbox")["text"], "Self");
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
    assert_eq!(pending.settings("toggle_inbox")["enabled"], false);
    let action = f.frames_for(p, "human_inbox_update").next_back().unwrap();
    frame["source_op"] = json!("human_inbox_update");
    frame["request_id"] = json!("older-request");
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p).settings("toggle_inbox")["enabled"], false);
    frame["request_id"] = action["request_id"].clone();
    f.send(p, frame);
    assert_eq!(inspect(&f, p).settings("toggle_inbox")["enabled"], true);
}

#[test]
fn shared_service_mode_and_reuse_emit_scoped_actions_and_follow_projection() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    let reuse =
        json!({"source_hub":"work","display_label":"@alice","values":{"account":"subscriber-42"}});
    let channel = &mut cfg["channels"][0];
    channel["supports_outbound_only"] = json!(true);
    channel["inbound_mode"] = json!("primary");
    channel["service_identity"] = json!("service-42");
    channel["service_label"] = json!("@studio_bot");
    channel["pairing_manual_disclosure"] = json!(true);
    channel["reusable_binding"] = reuse.clone();
    open_settings(&f, p, &cfg, "channels");
    let s = inspect(&f, p);
    assert_eq!(s.settings("stratum_hub:channel")["height"], 64);
    assert_eq!(
        s.settings("stratum_hub_title:channel")["text"],
        "1. Shared Hub Service · @studio_bot"
    );
    assert_eq!(
        s.settings("stratum_hub_status:channel")["text"],
        "Service Ready"
    );
    let toggle = "toggle_channel_inbound_mode:channel";
    let link = "reuse_channel_binding:channel";
    assert_eq!(s.settings(toggle)["text"], "Use Outbound-Only");
    assert_eq!(s.settings(link)["text"], "Link @alice (from 'work')");
    control(&f, p, "manual_channel:channel");
    assert!(inspect(&f, p).settings(link).is_null());
    control(&f, p, "manual_channel:channel");
    control(&f, p, link);
    assert_eq!(inspect(&f, p).settings(link)["enabled"], false);
    let save = latest(&f, p, "channel_save");
    assert_eq!(save["hub"], "personal");
    assert_eq!(save["channel_id"], "channel");
    assert_eq!(
        save["expected_service_identity"],
        cfg["channels"][0]["service_identity"]
    );
    assert_eq!(save["values"], reuse["values"]);
    assert_eq!(save["display_label"], reuse["display_label"]);
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["display_label"] = reuse["display_label"].clone();
    cfg["channels"][0]["reusable_binding"] = Value::Null;
    f.send(p, json!({"op":"settings","hub":"personal","request_id":save["request_id"],"source_op":"channel_save","ok":true,"settings":cfg}));
    let s = inspect(&f, p);
    assert!(s.settings(link).is_null());
    assert_eq!(
        s.settings("settings_channel_status:channel")["text"],
        "Connected"
    );
    control(&f, p, toggle);
    assert_eq!(inspect(&f, p).settings(toggle)["enabled"], false);
    let change = latest(&f, p, "save_hub_service");
    assert_eq!(change["hub"], "personal");
    assert_eq!(change["channel_id"], "channel");
    assert_eq!(change["inbound_mode"], "outbound_only");
    assert!(change.get("values").is_none());
    cfg["channels"][0]["inbound_mode"] = json!("outbound_only");
    cfg["channels"][0]["can_pair"] = json!(false);
    f.send(p, json!({"op":"settings","hub":"personal","request_id":change["request_id"],"source_op":"save_hub_service","ok":true,"settings":cfg}));
    let s = inspect(&f, p);
    assert_eq!(
        s.settings("settings_channel_status:channel")["text"],
        "Outbound Only"
    );
    assert_eq!(
        s.settings("stratum_hub_status:channel")["text"],
        "Outbound Only (Shared Bot)"
    );
    assert_eq!(s.settings(toggle)["text"], "Enable Replies");
    control(&f, p, toggle);
    inspect(&f, p);
    assert_eq!(latest(&f, p, "save_hub_service")["inbound_mode"], "primary");
}

#[test]
fn long_reusable_binding_shares_the_visible_action_bar_with_manual_controls() {
    let _lock = serial();
    let source = "w".repeat(63);
    let account = format!("@{}", "W".repeat(31));
    let service = format!("@{}", "m".repeat(31));
    for (mode, can_pair) in [("outbound_only", false), ("primary", true)] {
        let (f, p) = ui_fixture();
        let mut cfg = settings();
        cfg["hubs"][1]["name"] = json!(source);
        let ch = &mut cfg["channels"][0];
        ch["hub_provided"] = json!(true);
        ch["supports_outbound_only"] = json!(true);
        ch["service_identity"] = json!("service-42");
        ch["inbound_mode"] = json!(mode);
        ch["can_pair"] = json!(can_pair);
        ch["can_unpair"] = json!(false);
        ch["pairing_manual_disclosure"] = json!(!can_pair);
        ch["service_label"] = json!(service);
        ch["reusable_binding"] = json!({"source_hub":source,"display_label":account,"values":{"account":"subscriber-42"}});
        open_settings(&f, p, &cfg, "channels");
        input(&f, p, "channel_field:channel:account", "typed-account");
        let s = inspect(&f, p); // Includes AppKit hit-testing of enabled controls in the viewport.
        let link = "reuse_channel_binding:channel";
        assert_eq!(
            s.settings(link)["tooltip"],
            format!("Link {account} (from '{source}')")
        );
        assert_eq!(
            s.settings("stratum_hub_title:channel")["text"],
            format!("1. Shared Hub Service · {service}")
        );
        assert_eq!(s.settings("pair_channel:channel").is_null(), !can_pair);
        let viewport = s.settings("settings_channels_tab");
        let card = s.settings("settings_channel:channel");
        let bar = s.settings("stratum_user:channel");
        for id in [link, "test_channel:channel", "save_channel:channel"] {
            let control = s.settings(id);
            assert_eq!(control["hidden"], false, "{mode}: {id}");
            assert_eq!(control["enabled"], true, "{mode}: {id}");
            for (axis, extent, origin) in
                [("x", "width", "content_x"), ("y", "height", "content_y")]
            {
                let start = card[axis].as_f64().unwrap()
                    + bar[axis].as_f64().unwrap()
                    + control[axis].as_f64().unwrap()
                    - viewport[origin].as_f64().unwrap();
                assert!(
                    start >= 0.
                        && start + control[extent].as_f64().unwrap()
                            <= viewport[extent].as_f64().unwrap(),
                    "{mode}: {id} outside {axis} viewport"
                );
            }
        }
    }
}

#[test]
fn v41_new_test_receipts_override_health_until_the_next_changed_health_frame() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["channels"][0]["warning"] = json!(true);
    cfg["channels"][0]["detail"] = json!("Credentials rejected. Update your token.");
    let frame = state_with_settings(cfg.clone());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    f.send(p,json!({"op":"channel_test_result","channel_id":"channel","ok":true,"detail":"Test accepted"}));
    assert_eq!(
        inspect(&f, p).settings("settings_channel_detail:channel")["text"],
        "Test accepted"
    );
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        inspect(&f, p).settings("settings_channel_detail:channel")["text"],
        "Test accepted",
        "An unchanged health tick cannot displace the newer test receipt"
    );
    cfg["launch_at_login"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        inspect(&f, p).settings("settings_channel_detail:channel")["text"],
        "Test accepted",
        "An unrelated settings change cannot displace the newer test receipt"
    );
    cfg["channels"][0]["detail"] = json!("New gateway failure. Update your token.");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        inspect(&f, p).settings("settings_channel_detail:channel")["text"],
        "New gateway failure. Update your token."
    );
    cfg["channels"][0]["warning"] = json!(false);
    cfg["channels"][0]["status"] = json!("ready");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        inspect(&f, p).settings("settings_channel_detail:channel")["text"],
        "Test accepted"
    );
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    control(&f, p, "settings_tab:channels");
    f.send(p, json!({"op":"test_settings_key","key":"return"}));
    inspect(&f, p);
    assert!(f.frames_for(p, "setup_save_hub").next().is_none());
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["settings_open"], false);
}

#[test]
fn v39_manifest_pairing_first_correlates_copy_and_preserves_feedback_on_refresh() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["channels"][0]["pairing_manual_disclosure"] = json!(true);
    cfg["channels"][0]["pairing_open_url_label"] = json!("Open Pairing Link ↗");
    let frame = state_with_settings(cfg.clone());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert!(s.settings("channel_field:channel:account").is_null());
    assert!(s.settings("save_channel:channel").is_null());
    assert_eq!(s.settings("test_channel:channel")["hidden"], true);
    control(&f, p, "manual_channel:channel");
    let s = inspect(&f, p);
    assert_eq!(s.settings("test_channel:channel")["enabled"], false);
    f.send(
        p,
        json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"42"}),
    );
    assert_eq!(
        inspect(&f, p).settings("test_channel:channel")["enabled"],
        true
    );
    f.send(
        p,
        json!({"op":"test_settings_input","id":"channel_field:channel:account","value":""}),
    );
    assert_eq!(
        inspect(&f, p).settings("test_channel:channel")["enabled"],
        false
    );
    control(&f, p, "manual_channel:channel");
    control(&f, p, "pair_channel:channel");
    let request = latest(&f, p, "channel_pair");
    let mut pair = json!({"op":"pair_code","hub":request["hub"],"channel_id":"channel","request_id":"unrelated","code":"cito-example","instruction":"Send /start cito-example","pairing_command":"/start cito-example","pairing_url":"https://example.com/pair"});
    f.send(p, pair.clone());
    assert_ne!(inspect(&f, p)["clipboard"], "/start cito-example");
    pair["request_id"] = request["request_id"].clone();
    f.send(p, pair);
    let s = inspect(&f, p);
    assert_eq!(s["clipboard"], "/start cito-example");
    assert_eq!(s.settings("copy_pair_command:channel")["text"], "✓ Copied");
    assert_eq!(
        s.settings("open_pair_url:channel")["text"],
        "Open Pairing Link ↗"
    );
    assert!(s.settings("pair_channel:channel").is_null());
    cfg["channels"][0]["pairing_instruction"] = json!("Send /start cito-example");
    cfg["channels"][0]["pairing_command"] = json!("/start cito-example");
    cfg["channels"][0]["pairing_url"] = json!("https://example.com/pair");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        inspect(&f, p).settings("copy_pair_command:channel")["text"],
        "✓ Copied"
    );
}
#[test]
fn inbox_attachments_pause_agents_and_unavailable_details_follow_live_state() {
    let _lock = serial();
    let (f, p) = ui_fixture();
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
        assert_eq!(snapshot.menu("stop_all")["text"], "Stop All");
        assert_eq!(snapshot.settings("toggle_inbox")["enabled"], true);
    }
    control(&f, p, "toggle_inbox");
    let update = latest(&f, p, "human_inbox_update");
    assert_eq!(update["expected_control_revision"], 7);
    assert_eq!(update["paused"], true);
    control(&f, p, "stop_all");
    let stop = latest(&f, p, "unlisten_all");
    assert_eq!(stop["hub"], "personal");
    cfg["channels"][0]["warning"] = json!(true);
    cfg["channels"][0]["error_origin"] = json!("user");
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["detail"]=json!("Connection unavailable.\nThe remote gateway rejected the request.\nCheck credentials and try again.");
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    let detail = s.settings("settings_channel_detail:channel");
    let card = s.settings("settings_channel:channel");
    assert!(detail["height"].as_f64().unwrap() > 28.0);
    assert!(card["height"].as_f64().unwrap() >= detail["height"].as_f64().unwrap() + 46.0);
    assert!(detail["y"].as_f64().unwrap() >= 8.0);
    for appearance in ["light", "dark"] {
        inspect_as(&f, p, Some(appearance));
    }
}

#[test]
fn v39_personal_edit_unpair_repair_and_cancel_reset_transient_ui() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("42");
    cfg["channels"][0]["pairing_manual_disclosure"] = json!(true);
    let frame = state_with_settings(cfg.clone());
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
        assert!(!s.settings("channel_summary:channel").is_null());
        control(&f, p, "edit_user_channel:channel");
        assert_eq!(
            inspect(&f, p).settings("channel_field:channel:account")["text"],
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
    assert!(s.settings("channel_field:channel:account").is_null());
    let manual = s.settings("manual_channel:channel");
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
        let request = latest(&f, p, "channel_pair");
        f.send(p,json!({"op":"pair_code","hub":request["hub"],"channel_id":"channel","request_id":request["request_id"],"instruction":"Send /start cito-example","pairing_command":"/start cito-example"}));
        assert_eq!(
            inspect(&f, p).settings("copy_pair_command:channel")["text"],
            "✓ Copied"
        );
        if round == 0 {
            control(&f, p, "cancel_pair_channel:channel");
            confirm_channel_action(&f, p, "channel_cancel_pair");
            f.send(p, json!({"op":"settings","settings":cfg}));
            let s = inspect(&f, p);
            assert!(s.settings("copy_pair_command:channel").is_null());
        }
    }
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("42");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert!(!s.settings("channel_summary:channel").is_null());
    assert!(s.settings("channel_field:channel:account").is_null());
    control(&f, p, "edit_user_channel:channel");
    control(&f, p, "pair_channel:channel");
    let request = latest(&f, p, "channel_pair");
    f.send(p,json!({"op":"pair_code","hub":request["hub"],"channel_id":"channel","request_id":request["request_id"],"instruction":"Send /start cito-again","pairing_command":"/start cito-again"}));
    assert!(!inspect(&f, p)
        .settings("copy_pair_command:channel")
        .is_null());
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert!(!s.settings("channel_summary:channel").is_null());
    assert!(s.settings("channel_field:channel:account").is_null());
}

#[test]
fn a_pending_pair_survives_a_refresh_and_a_committed_cancel_rejects_late_codes() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    open_settings(&f, p, &cfg, "channels");
    control(&f, p, "pair_channel:channel");
    let pair = latest(&f, p, "channel_pair");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let code = json!({"op":"pair_code","hub":pair["hub"],"channel_id":"channel","request_id":pair["request_id"],"instruction":"Link your account","pairing_command":"/link example"});
    f.send(p, code.clone());
    assert_eq!(inspect(&f, p)["clipboard"], "/link example");
    control(&f, p, "cancel_pair_channel:channel");
    confirm_channel_action(&f, p, "channel_cancel_pair");
    f.send(p, json!({"op":"test_clipboard","text":"Keep this"}));
    f.send(p, code);
    let settled = inspect(&f, p);
    assert_eq!(settled["clipboard"], "Keep this");
    assert!(settled.settings("pairing_instruction:channel").is_null());
}

#[test]
fn v39_live_channel_health_overrides_stored_test_success_and_recovers() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("42");
    let frame = state_with_settings(cfg.clone());
    send_companion(&f, p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "test_channel:channel");
    inspect(&f, p);
    assert!(f.frames_for(p, "channel_test").next().is_some());
    let tested = "Test message accepted by the channel";
    send_companion(
        &f,
        p,
        json!({"op":"channel_test_result","channel_id":"channel","ok":true,"detail":tested}),
    );
    assert_eq!(
        inspect(&f, p).settings("settings_channel_detail:channel")["text"],
        tested
    );
    for (detail, presented) in [
        (
            "HTTP 409: another polling request is active. Stop the other poller.",
            "HTTP another polling request is active. Stop the other poller.",
        ),
        (
            "Delivery outcome unknown. Check the destination before retrying.",
            "Delivery outcome unknown. Check the destination before retrying.",
        ),
        (
            "HTTP 401: credentials rejected. Update the gateway token.",
            "HTTP credentials rejected. Update the gateway token.",
        ),
    ] {
        cfg["channels"][0]["warning"] = json!(true);
        cfg["channels"][0]["detail"] = json!(detail);
        send_companion(&f, p, json!({"op":"settings","settings":cfg}));
        let s = inspect(&f, p);
        assert_eq!(
            s.settings("settings_channel_detail:channel")["text"],
            presented
        );
        assert_eq!(
            s.settings("settings_channel_detail:channel")["tooltip"],
            presented
        );
        assert_eq!(
            s.settings("settings_channel_status:channel")["text"],
            "Connected"
        );
        cfg["channels"][0]["status"] = json!("ready");
        cfg["channels"][0]["warning"] = json!(false);
        cfg["channels"][0]["detail"] = json!("Connected");
        send_companion(&f, p, json!({"op":"settings","settings":cfg}));
        let s = inspect(&f, p);
        assert_eq!(
            s.settings("settings_channel_status:channel")["text"],
            "Connected"
        );
        // The last test receipt remains useful after health recovers, until the next mutation.
        assert_eq!(
            s.settings("settings_channel_detail:channel")["text"],
            tested
        );
    }
}

#[test]
fn overlapping_channel_receipts_acknowledge_only_the_matching_scope_and_draft() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["channels"][0]["fields"][0]["value"] = json!("");
    cfg["channels"][0]["fields"][1]["type"] = json!("secret");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    open_settings(&f, p, &cfg, "channels");
    control(&f, p, "edit_hub_service:channel");
    let save = |field: &str, value: &str, button: &str, op: &str| {
        input(&f, p, &format!("channel_field:channel:{field}"), value);
        control(&f, p, button);
        latest(&f, p, op)
    };
    let gateway = |v| save("secret", v, "save_hub_service:channel", "save_hub_service");
    let old = gateway("first");
    let newer = gateway("second");
    let personal = save(
        "account",
        "personal-token",
        "save_channel:channel",
        "channel_save",
    );
    let ack = |action: &Value| {
        f.send(p, json!({"op":"settings","ok":true,"source_op":action["op"],"channel_id":"channel","request_id":action["request_id"],"settings":cfg}))
    };
    ack(&personal);
    ack(&old);
    let partial = inspect(&f, p);
    assert!(!partial.settings("channel_field:channel:secret").is_null());
    assert_eq!(
        partial["pending_channel_saves"],
        json!([newer["request_id"]])
    );
    control(&f, p, "save_channel:channel");
    assert_eq!(latest(&f, p, "channel_save")["values"], json!({}));
    input(&f, p, "channel_field:channel:secret", "edited-after-submit");
    ack(&newer);
    control(&f, p, "save_hub_service:channel");
    let retry = latest(&f, p, "save_hub_service");
    assert_eq!(retry["values"], json!({"secret":"edited-after-submit"}));
    ack(&retry);
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "save_hub_service:channel");
    assert_eq!(latest(&f, p, "save_hub_service")["values"], json!({}));
}

#[test]
fn shared_human_inbox_survives_local_detach_and_disables_mutations_while_offline() {
    let _lock = serial();
    let (f, p) = ui_fixture();
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
    send_companion(&f, p, frame.clone());
    let active = inspect(&f, p);
    assert!(active.menu("human_inbox_card").is_null());
    assert!(active.menu("inbox_paused_notice").is_null());
    control(&f, p, "target_hub:work");
    let remote = inspect(&f, p);
    assert_eq!(remote.menu("header_subtitle")["text"], "Inbox paused");
    assert_eq!(
        remote.menu("inbox_paused_notice")["text"],
        "Inbox paused · Resume in Settings…"
    );
    assert_eq!(remote["count"], 0);
    assert!(remote.menu("stop_all").is_null());
    control(&f, p, "inbox_paused_notice");
    let selected = inspect(&f, p);
    assert_eq!(selected["settings_tab"], "channels");
    assert!(f
        .frames(p)
        .iter()
        .any(|a| a["op"] == "open_settings" && a["hub"] == "work"));
    assert_eq!(selected.settings("toggle_inbox")["text"], "Resume Inbox");
    frame["hubs"][1]["status"] = json!("disconnected");
    frame["settings"]["hubs"][1]["status"] = json!("disconnected");
    send_companion(&f, p, frame.clone());
    let offline = inspect(&f, p);
    assert_eq!(offline.settings("toggle_inbox")["enabled"], false);
    assert_eq!(offline.menu("header_subtitle")["text"], "Hub unavailable");
    let before = f.frames_for(p, "human_inbox_update").count();
    control(&f, p, "toggle_inbox");
    inspect(&f, p);
    assert_eq!(f.frames_for(p, "human_inbox_update").count(), before);
    frame["hubs"][1]["status"] = json!("connected");
    frame["settings"]["hubs"][1]["status"] = json!("connected");
    send_companion(&f, p, frame);
    control(&f, p, "toggle_inbox");
    let pending = inspect(&f, p);
    assert_eq!(pending.settings("toggle_inbox")["text"], "Resuming…");
    assert_eq!(pending.settings("toggle_inbox")["enabled"], false);
    let update = f.frames_for(p, "human_inbox_update").next_back().unwrap();
    assert_eq!(update["hub"], "work");
    assert_eq!(update["paused"], false);
    assert_eq!(update["expected_control_revision"], 3);
    assert!(update.get("about").is_none());
}

#[test]
fn a_channel_mutation_retires_test_feedback_that_survived_a_passive_refresh() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    open_settings(&f, p, &cfg, "channels");
    control(&f, p, "edit_user_channel:channel");
    control(&f, p, "test_channel:channel");
    let test = latest(&f, p, "channel_test");
    f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","request_id":test["request_id"],"ok":false,"detail":"Invalid token"}));
    f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
    assert_eq!(
        inspect(&f, p).settings("settings_channel_detail:channel")["text"],
        "Invalid token"
    );
    control(&f, p, "save_channel:channel");
    assert!(inspect(&f, p)
        .settings("settings_channel_detail:channel")
        .is_null());
}

#[test]
fn lifecycle_channel_ui_retains_redacted_secret_and_preserves_pairing_on_refresh() {
    let _lock = serial();
    let (f, p) = ui_fixture();
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
    let frame = state_with_settings(cfg.clone());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert_eq!(s.settings("stratum_hub:channel")["height"], 36);
    assert!(s.settings("channel_field:channel:secret").is_null());
    control(&f, p, "edit_hub_service:channel");
    let s = inspect(&f, p);
    assert_eq!(s.settings("channel_field:channel:secret")["secure"], true);
    assert_eq!(
        s.settings("channel_field:channel:secret")["placeholder"],
        "Saved — enter replacement"
    );
    assert!(s.settings("channel_setup_guide:channel").is_null());
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
        wait(|| f.frames_for(p, op).next().is_some());
        let action = f.frames_for(p, op).next_back().unwrap();
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
        s.settings("settings_channel_status:channel")["text"],
        "◌ Pairing…"
    );
    assert!(!s.settings("pairing_instruction:channel").is_null());
    control(&f, p, "copy_pair_command:channel");
    assert_eq!(inspect(&f, p)["clipboard"], "/start cito-ABCDEFGHIJ");
    control(&f, p, "cancel_pair_channel:channel");
    wait(|| f.frames_for(p, "channel_cancel_pair").next().is_some());
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("paused");
    cfg["channels"][0]["pairing_instruction"] = Value::Null;
    cfg["channels"][0]["pairing_code"] = Value::Null;
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert_eq!(
        s.settings("settings_channel_status:channel")["text"],
        "Paused"
    );
    assert_eq!(
        s.settings("toggle_channel:channel")["text"],
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
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("saved-account");
    cfg["channels"][0]["fields"][0]["value"] = json!("");
    let frame = state_with_settings(cfg.clone());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "edit_user_channel:channel");
    for (field, value) in [("secret", "discard-gateway"), ("account", "keep-personal")] {
        f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{field}"),"value":value}));
    }
    control(&f, p, "cancel_edit_hub_service:channel");
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    let latest = |op: &str| f.frames_for(p, op).next_back().unwrap();
    assert_eq!(latest("save_hub_service")["values"], json!({}));
    f.send(p, json!({"op":"test_settings_input","id":"channel_field:channel:secret","value":"escape-discard"}));
    let mut remote = cfg.clone();
    remote["settings_hub"] = json!("work");
    f.send(p, json!({"op":"settings","hub":"work","settings":remote}));
    control(&f, p, "channel_hub_selector:work");
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "edit_user_channel:channel");
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
    assert!(inspect(&f, p)
        .settings("channel_field:channel:account")
        .is_null());
    control(&f, p, "edit_user_channel:channel");
    assert_eq!(
        inspect(&f, p).settings("channel_field:channel:account")["text"],
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
    assert_eq!(s.settings("channel_field:channel:account"), Value::Null);
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
        inspect(&f, p).settings("stratum_hub:channel")["height"],
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
