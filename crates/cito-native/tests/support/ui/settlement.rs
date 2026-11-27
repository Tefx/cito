use super::lazy_rendering::render_state;
use super::*;

pub(super) fn open_settings(f: &Fixture, p: usize, cfg: &Value, tab: &str) {
    let frame = state_with_settings(cfg.clone());
    f.send(p, frame);
    control(f, p, "open_settings");
    control(f, p, &format!("settings_tab:{tab}"));
    inspect(f, p);
}

pub(super) fn input(f: &Fixture, p: usize, id: &str, value: &str) {
    f.send(p, json!({"op":"test_settings_input","id":id,"value":value}));
}

pub(super) fn latest(f: &Fixture, p: usize, op: &str) -> Value {
    inspect(f, p);
    f.frames_for(p, op).next_back().unwrap()
}

#[test]
fn ipc_delivers_live_state_while_the_menu_is_tracking() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, state());
    f.send(p, json!({"op":"test_menu_open"}));
    assert_eq!(render_state(&f, p)["menu_open"], true);
    f.send(p, json!({"op":"test_menu_tracking_probe"}));
    wait(|| f.output(p, true).contains("menu-tracking-start"));
    let mut change = state();
    change["unread_count"] = json!(8);
    f.send(p, change);
    wait(|| f.output(p, true).contains("menu-tracking-unread 8"));
    assert_eq!(render_state(&f, p)["unread_count"], 8);
}

#[test]
fn pending_setup_inspection_does_not_settle_first_launch() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["apps"][0]["configured"] = Value::Null;
    cfg["apps"][0]["status"] = json!("error");
    cfg["apps"][0]["detail"] = json!("Setup inspection pending");
    f.send(p, state_with_settings(cfg.clone()));
    assert_eq!(render_state(&f, p)["onboarding_checked"], false);
    cfg["apps"][0]["configured"] = json!(false);
    cfg["apps"][0]["status"] = json!("not_configured");
    cfg["apps"][0]["detail"] = json!("Click Set Up to connect");
    f.send(p, state_with_settings(cfg));
    assert_eq!(render_state(&f, p)["onboarding_checked"], true);
}

#[test]
fn menu_card_failure_stays_on_menu_and_only_its_successful_retry_clears_it() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    open_settings(&f, p, &settings(), "apps");
    control(&f, p, "toggle_auto_listen");
    let action = latest(&f, p, "set_auto_listen");
    f.send(p, json!({"op":"error","source_op":"set_auto_listen","request_id":action["request_id"],"error":"Auto-Listen could not be changed."}));
    let failed = inspect(&f, p);
    assert!(failed.menu("menu_error_row").is_object());
    assert!(failed.settings("settings_error_banner").is_null());
    control(&f, p, "toggle_auto_listen");
    let retry = latest(&f, p, "set_auto_listen");
    assert_ne!(retry["request_id"], action["request_id"]);
    assert!(inspect(&f, p).menu("menu_error_row").is_object());
    let mut settled = state();
    settled["ok"] = json!(true);
    settled["source_op"] = json!("set_auto_listen");
    settled["request_id"] = retry["request_id"].clone();
    f.send(p, settled);
    assert!(inspect(&f, p).menu("menu_error_row").is_null());
}

#[test]
fn control_reopen_reaches_the_companion_and_opens_settings() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let mut l = ClosedLoop::new(&f);
    assert_eq!(
        l.until(|s| s.menu("header").is_object())["settings_open"],
        false
    );
    let event = json!({"op":"reopen_ui"});
    let (status, reply) = http(f.port, "/control", Some(&event));
    assert_eq!((status, reply), (200, json!({"ok":true})));
    l.until(|s| s["settings_open"] == true);
    assert_eq!(
        l.frames
            .iter()
            .filter(|frame| frame["op"] == "reopen_ui")
            .collect::<Vec<_>>(),
        vec![&event]
    );
}

#[test]
fn companion_decodes_complete_passive_listener_and_attachment_collections() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    frame["hubs"][0]["attachments"] = json!((0..129).map(|i| json!({"channel_id":format!("device-{i}"),"location":"device","display_name":format!("Device {i}"),"availability":"ACTIVE","is_local":false})).collect::<Vec<_>>());
    frame["listeners"] = json!((0..65)
        .map(|i| {
            let mut row = listener();
            for key in ["listener_id", "public_id", "scoped_id", "session_id"] {
                row[key] = json!(format!("node-{i}"));
            }
            row
        })
        .collect::<Vec<_>>());
    f.send(p, frame);
    assert_eq!(render_state(&f, p)["listeners"], 65);
    assert_eq!(inspect(&f, p)["hub_attachment_counts"]["personal"], 129);
}

#[test]
fn truncated_settings_refresh_scalars_without_replacing_channels_and_state_banners_redraw() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    let mut base = state_with_settings(cfg.clone());
    f.send(p, base.clone());
    f.send(p, json!({"op":"test_menu_open"}));
    let menu = render_state(&f, p);
    let mut warning = base.clone();
    warning["warning"] = json!("Connection is degraded.");
    f.send(p, warning);
    assert!(
        render_state(&f, p)["menu_rebuilds"].as_u64().unwrap()
            > menu["menu_rebuilds"].as_u64().unwrap()
    );
    assert!(!inspect(&f, p).menu("menu_error_row").is_null());

    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    assert!(!inspect(&f, p)
        .settings("settings_channel:channel")
        .is_null());
    let settings_before = render_state(&f, p);
    base["warning"] = json!("Refresh will retry shortly.");
    f.send(p, base.clone());
    assert!(
        render_state(&f, p)["settings_rebuilds"].as_u64().unwrap()
            > settings_before["settings_rebuilds"].as_u64().unwrap()
    );
    assert_eq!(
        inspect(&f, p).settings("settings_error_label")["text"],
        "Refresh will retry shortly."
    );
    control(&f, p, "dismiss_settings_error");

    cfg["channels"] = json!([]);
    cfg["apps"] = json!([]);
    cfg["hubs"][0]["human_paused"] = json!(true);
    cfg["hubs"][0]["status"] = json!("disconnected");
    cfg["channel_inventory_error"] = json!("Service unavailable.");
    cfg["launch_at_login"] = json!(true);
    f.send(p, json!({"op":"settings","truncated":true,"truncated_collections":["channels","apps"],"settings":cfg}));
    let stale = inspect(&f, p);
    assert_eq!(
        stale.settings("channel_inventory_error")["text"],
        "Service unavailable."
    );
    assert_eq!(stale.settings("toggle_inbox")["text"], "Resume Inbox");
    assert!(!stale.settings("settings_channel:channel").is_null());
    assert_eq!(stale["settings_launch_at_login"], true);

    cfg["channel_inventory_error"] = Value::Null;
    cfg["hubs"][0]["human_paused"] = json!(false);
    base["settings"] = cfg;
    base["truncated"] = json!(true);
    base["truncated_collections"] = json!(["channels", "apps"]);
    base.as_object_mut().unwrap().remove("warning");
    f.send(p, base);
    let fresh = inspect(&f, p);
    assert!(fresh.settings("channel_inventory_error").is_null());
    assert_eq!(fresh.settings("toggle_inbox")["text"], "Pause Inbox");
    assert!(!fresh.settings("settings_channel:channel").is_null());
}

#[test]
fn settings_shedding_respects_flat_and_dotted_keys_without_freezing_other_collections() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let original = settings();
    for (key, preserved) in [
        ("apps", "apps"),
        ("settings.apps", "apps"),
        ("channels", "channels"),
        ("settings.channels", "channels"),
        ("hubs", "hubs"),
        ("settings.hubs", "hubs"),
    ] {
        f.send(
            p,
            json!({"op":"settings","hub":"personal","settings":original}),
        );
        let mut update = original.clone();
        for collection in ["apps", "channels", "hubs"] {
            update[collection] = json!([]);
        }
        // A cached channel inventory belongs to a Hub that still exists.
        if preserved == "channels" {
            update["hubs"] = json!([original["hubs"][0]]);
        }
        f.send(p, json!({"op":"settings","hub":"personal","settings":update,"truncated":true,"truncated_collections":[key]}));
        let projection = render_state(&f, p);
        for collection in ["apps", "channels", "hubs"] {
            let count = if collection == preserved {
                original[collection].as_array().unwrap().len()
            } else {
                update[collection].as_array().unwrap().len()
            };
            assert_eq!(
                projection["collection_counts"][format!("settings.{collection}")],
                count,
                "{key}: {collection}"
            );
        }
    }
}

#[test]
fn hub_only_disconnect_discards_only_its_submitted_gateway_draft() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    open_settings(&f, p, &cfg, "channels");
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "edit_user_channel:channel");
    for (key, value) in [
        ("secret", "Old gateway draft"),
        ("account", "Unsaved account"),
    ] {
        f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{key}"),"value":value}));
    }
    let before = inspect(&f, p);
    assert_eq!(
        before.settings("channel_field:channel:account")["text"],
        "Unsaved account"
    );
    control(&f, p, "clear_channel_service:channel");
    confirm_channel_action(&f, p, "disconnect_human_channel");
    let after = inspect(&f, p);
    assert_eq!(after["pending_channel_resets"], json!([]));
    assert_eq!(
        after.settings("channel_field:channel:account")["text"],
        "Unsaved account"
    );
    assert!(after.settings("channel_field:channel:secret").is_null());
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    let action = f.frames_for(p, "save_hub_service").next_back().unwrap();
    assert!(
        action["values"]["secret"].is_null(),
        "Cleared gateway draft must not be resubmitted"
    );
}

#[test]
fn channel_receipts_without_channel_ids_settle_resets_and_retire_only_committed_secrets() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][0]["value"] = Value::Null;
    cfg["channels"][0]["fields"][1]["value"] = json!("saved");
    open_settings(&f, p, &cfg, "channels");
    let latest = |op: &str| f.frames_for(p, op).next_back().unwrap();
    let input = |key: &str, text: &str| {
        f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{key}"),"value":text}))
    };
    control(&f, p, "edit_hub_service:channel");
    input("secret", "draft-token");
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    let request = latest("save_hub_service");
    f.send(p, json!({"op":"settings","action":"save_hub_service","request_id":request["request_id"],"committed":true}));
    let settled = inspect(&f, p);
    assert_eq!(settled["pending_channel_saves"], json!([]));
    assert!(!settled.settings("channel_field:channel:secret").is_null());
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    assert_eq!(latest("save_hub_service")["values"], json!({}));

    control(&f, p, "edit_user_channel:channel");
    input("account", "new draft");
    for fenced in [true, false] {
        control(&f, p, "unpair_channel:channel");
        inspect(&f, p);
        let reset = latest("channel_unpair");
        f.send(p, json!({"op":"settings","action":"channel_unpair","request_id":reset["request_id"],"committed":true,"authority_changed":fenced}));
        let settled = inspect(&f, p);
        assert_eq!(settled["pending_channel_resets"], json!([]));
        let field = settled.settings("channel_field:channel:account");
        if fenced {
            assert_eq!(field["text"], "new draft");
        } else {
            assert!(field.is_null());
        }
    }
    control(&f, p, "edit_user_channel:channel");
    assert_eq!(
        inspect(&f, p).settings("channel_field:channel:account")["text"],
        "saved"
    );
}

#[test]
fn a_fenced_pair_receipt_settles_only_the_matching_control_without_copying() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    open_settings(&f, p, &settings(), "channels");
    control(&f, p, "pair_channel:channel");
    inspect(&f, p);
    let action = f.frames_for(p, "channel_pair").next_back().unwrap();
    let id = action["request_id"].clone();
    let mut receipt = json!({"op":"settings","source_op":"channel_pair","channel_id":"channel","request_id":id,"hub":"work","authority_changed":true,"committed":true,"warning":"Hub changed."});
    f.send(p, receipt.clone());
    assert_eq!(inspect(&f, p)["pending_controls"], json!([id]));
    receipt["hub"] = action["hub"].clone();
    f.send(p, receipt);
    let settled = inspect(&f, p);
    assert_eq!(settled["pending_controls"], json!([]));
    assert_eq!(
        settled.settings("settings_error_label")["text"],
        "Hub changed."
    );
    f.send(p, json!({"op":"pair_code","hub":action["hub"],"channel_id":"channel","request_id":id,"pairing_command":"obsolete-command"}));
    let late = inspect(&f, p);
    assert_eq!(late["clipboard"], settled["clipboard"]);
    assert!(late.settings("pairing_instruction:channel").is_null());
}

#[test]
fn read_only_pairing_editor_can_select_and_copy_and_feedback_restores_full_width() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["can_edit_personal"] = json!(false);
    open_settings(&f, p, &cfg, "channels");
    let ready = inspect(&f, p);
    assert_eq!(ready.settings("edit_user_channel:channel")["hidden"], true);
    assert_eq!(ready.settings("pair_channel:channel")["text"], "Re-pair…");
    control(&f, p, "pair_channel:channel");
    inspect(&f, p);
    let pair = f.frames_for(p, "channel_pair").next_back().unwrap();
    let instruction = "Open your account, then enter this code.\nCode: 123456";
    let command = format!("pair {}", "é".repeat(3000));
    let url = format!("https://id.example.com/pair?code={}", "a".repeat(6000));
    f.send(p, json!({"op":"pair_code","hub":"personal","channel_id":"channel","request_id":pair["request_id"],"instruction":instruction,"pairing_command":command,"pairing_url":url}));
    let copied = inspect(&f, p);
    assert_eq!(copied["clipboard"], command);
    assert!(copied.settings("open_pair_url:channel").is_object());
    let width = copied.settings("copy_pair_command:channel")["width"].clone();
    assert_eq!(
        copied.settings("copy_pair_command:channel")["text"],
        "✓ Copied"
    );
    f.send(
        p,
        json!({"op":"test_expire_copy_feedback","id":"copy_pair_command:channel"}),
    );
    let restored = inspect(&f, p);
    let copy = restored.settings("copy_pair_command:channel");
    assert_eq!(copy["width"], width);
    assert_eq!(copy["text"], "Copy Command");
    assert_eq!(copy["accessibility_label"], "Copy Command");

    f.send(
        p,
        json!({"op":"test_focus_field","id":"pairing_instruction:channel","caret_only":true}),
    );
    let editor = inspect(&f, p)["field_editors"][0].clone();
    assert_eq!(editor["id"], "pairing_instruction:channel");
    assert_eq!(editor["editable"], false);
    assert_eq!(editor["selection_length"], 0);
    for key in ["command-a", "command-c"] {
        f.send(p, json!({"op":"test_settings_key","key":key,"via":"panel"}));
    }
    let selected = inspect(&f, p);
    assert_eq!(
        selected["field_editors"][0]["selection_length"],
        instruction.len()
    );
    assert_eq!(selected["clipboard"], instruction);
    f.send(
        p,
        json!({"op":"test_settings_key","key":"command-x","via":"panel"}),
    );
    assert_eq!(
        inspect(&f, p).settings("pairing_instruction:channel")["text"],
        instruction
    );
}

#[test]
fn gateway_clear_only_bar_fits_and_new_channel_edits_remove_saved_feedback() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["channels"][0]["hub_ready"] = json!(false);
    cfg["channels"][0]["fields"][0]["hidden"] = json!(true);
    open_settings(&f, p, &cfg, "channels");
    let snapshot = inspect(&f, p);
    let clear = snapshot.settings("clear_channel_service:channel");
    let stratum = snapshot.settings("stratum_hub:channel");
    assert_eq!(clear["x"], 12);
    assert!(stratum["height"].as_f64().unwrap() >= 68.0);
    cfg["channels"][0]["hub_ready"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let input = |value| {
        f.send(
            p,
            json!({"op":"test_settings_input","id":"channel_field:channel:account","value":value}),
        )
    };
    input("first draft");
    control(&f, p, "save_channel:channel");
    inspect(&f, p);
    let saved = f.frames_for(p, "channel_save").next_back().unwrap();
    cfg["channels"][0]["fields"][1]["value"] = json!("first draft");
    f.send(p, json!({"op":"settings","settings":cfg,"ok":true,"source_op":"channel_save","request_id":saved["request_id"]}));
    let confirmed = inspect(&f, p);
    let button = |s: &Value| s.settings("save_channel:channel");
    assert_eq!(button(&confirmed)["text"], "✓ Saved");
    input("newer edit");
    let editing = inspect(&f, p);
    assert_eq!(button(&editing)["instance"], button(&confirmed)["instance"]);
    assert_eq!(button(&editing)["green"], false);
    assert_eq!(button(&editing)["text"], "Save");
    assert_eq!(button(&editing)["accessibility_label"], "Save");
    control(&f, p, "settings_tab:apps");
    control(&f, p, "settings_tab:channels");
    let remounted = inspect(&f, p);
    assert_eq!(button(&remounted)["text"], "Save");
    assert_eq!(
        remounted.settings("channel_field:channel:account")["text"],
        "newer edit"
    );
}

#[test]
fn channel_pair_without_prior_test_result_disables_immediately() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    open_settings(&f, p, &settings(), "channels");
    let before = render_state(&f, p);
    control(&f, p, "pair_channel:channel");
    let pending = render_state(&f, p);
    assert!(
        pending["settings_rebuilds"].as_u64().unwrap()
            > before["settings_rebuilds"].as_u64().unwrap()
    );
    let s = inspect(&f, p);
    let button = s.settings("pair_channel:channel");
    assert_eq!(button["text"], "Pairing…");
    assert_eq!(button["enabled"], false);
    let count = f.frames_for(p, "channel_pair").count();
    assert_eq!(count, 1);
    control(&f, p, "pair_channel:channel");
    inspect(&f, p);
    assert_eq!(f.frames_for(p, "channel_pair").count(), count);
}

#[test]
fn menu_close_removes_inline_rename_without_a_background_frame() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, state());
    f.send(p, json!({"op":"test_menu_open"}));
    control(&f, p, "rename:personal:repo-abcdefghijkl");
    f.send(
        p,
        json!({"op":"test_focus_field","id":"rename_field:personal:repo-abcdefghijkl"}),
    );
    assert_eq!(render_state(&f, p)["mounted_rename"], true);
    f.send(p, json!({"op":"test_menu_close"}));
    let closed = render_state(&f, p);
    assert_eq!(
        closed["mounted_rename"], false,
        "Close must remove the actual native field before any inspection flush"
    );
    f.send(p, json!({"op":"test_menu_open"}));
    let reopened = inspect(&f, p);
    assert!(reopened
        .menu("rename_field:personal:repo-abcdefghijkl")
        .is_null());
    assert!(!reopened.menu("rename:personal:repo-abcdefghijkl").is_null());
    assert!(f.frames_for(p, "rename_session").next().is_none());
}

#[test]
fn host_port_matching_receipt_settles_normalized_values_and_keeps_new_drafts() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["host_hub"]["enabled"] = json!(true);
    open_settings(&f, p, &cfg, "hubs");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":" \n4291\t "}),
    );
    assert_eq!(
        inspect(&f, p).settings("apply_host_hub_port")["enabled"],
        false,
        "Whitespace around the existing port must remain unchanged"
    );
    for value in [" 4501 \n", "\t4502 "] {
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":value}),
        );
        control(&f, p, "apply_host_hub_port");
        assert_eq!(
            inspect(&f, p).settings("apply_host_hub_port")["enabled"],
            false
        );
        let request = f.frames_for(p, "set_host_hub").next_back().unwrap();
        assert_eq!(request["port"], value.trim().parse::<u16>().unwrap());
        f.send(
            p,
            json!({"op":"settings","request_id":"unrelated","settings":cfg}),
        );
        assert_eq!(
            inspect(&f, p).settings("apply_host_hub_port")["enabled"],
            false
        );
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":" 4600 "}),
        );
        // The daemon retains/normalizes to its old port; a matching receipt still settles.
        f.send(p, json!({"op":"settings","source_op":"set_host_hub","ok":true,"request_id":request["request_id"],"settings":cfg}));
        let s = inspect(&f, p);
        let apply = s.settings("apply_host_hub_port");
        assert_eq!(apply["text"], "Apply");
        assert_eq!(apply["enabled"], true);
        assert_eq!(s.settings("host_hub_port")["text"], " 4600 ");
    }
    // Both sharing and auth changes use the same trimmed validation path.
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":" \t4700 "}),
    );
    f.send(p, json!({"op":"test_focus_field","id":"host_hub_port"}));
    for id in ["toggle_host_hub_auth", "toggle_host_hub"] {
        let before = f.frames_for(p, "set_host_hub").count();
        control(&f, p, id);
        control(&f, p, id);
        let focused = inspect(&f, p);
        assert_eq!(focused["settings_rebuild_deferred"], true);
        assert_eq!(f.frames_for(p, "set_host_hub").count(), before + 1);
        assert_eq!(
            f.frames_for(p, "set_host_hub").next_back().unwrap()["port"],
            4700
        );
    }
}

#[test]
fn every_settings_menu_entry_cancels_tracking_and_footer_advertises_shortcut() {
    let _lock = serial();
    for (id, tab) in [
        ("open_hub_settings:personal", "hubs"),
        ("empty_open_setup", "apps"),
        ("empty_open_channels", "channels"),
        ("open_settings", "apps"),
    ] {
        let (f, p) = ui_fixture();
        let mut frame = state();
        frame["listeners"] = json!([]);
        frame["settings"] = settings();
        f.send(p, frame);
        f.send(p, json!({"op":"test_menu_open"}));
        let before = inspect(&f, p);
        assert_eq!(before.menu("open_settings")["tooltip"], "Settings… (⌘,)");
        assert!(!before.menu(id).is_null(), "{id}");
        control(&f, p, id);
        let after = inspect(&f, p);
        assert_eq!(
            after["tracking_cancellations"].as_u64().unwrap(),
            before["tracking_cancellations"].as_u64().unwrap() + 1,
            "{id}"
        );
        assert_eq!(render_state(&f, p)["settings_open"], true);
        for candidate in ["apps", "channels", "hubs"] {
            assert_eq!(
                after.settings(&format!("settings_{candidate}_tab"))["hidden"],
                candidate != tab,
                "{id}: {candidate}"
            );
        }
    }
}

#[test]
fn local_key_monitor_accepts_only_exact_command_shortcuts() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let frame = state_with_settings(settings());
    f.send(p, frame);
    for (character, op) in [(",", "open_settings"), ("k", "open_directory")] {
        let count = || f.frames_for(p, op).count();
        let baseline = count();
        for modifiers in [
            json!([]),
            json!(["command", "shift"]),
            json!(["command", "option"]),
            json!(["command", "control"]),
        ] {
            f.send(
                p,
                json!({"op":"test_key_event","character":character,"modifiers":modifiers}),
            );
            render_state(&f, p);
            assert_eq!(count(), baseline, "extra modifiers must not invoke {op}");
        }
        for modifiers in [json!(["command"]), json!(["command", "capsLock"])] {
            let before = inspect(&f, p)["tracking_cancellations"].as_u64().unwrap();
            f.send(
                p,
                json!({"op":"test_key_event","character":character,"modifiers":modifiers}),
            );
            let after = inspect(&f, p);
            assert_eq!(
                after["tracking_cancellations"].as_u64().unwrap(),
                before + 1
            );
        }
        assert_eq!(count(), baseline + 2);
    }
}

#[test]
fn settings_checkbox_first_mouse_activates_without_changing_launch_preference() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    open_settings(&f, p, &settings(), "apps");
    let id = "settings_launch_at_login";
    assert_eq!(inspect(&f, p).settings(id)["accepts_first_mouse"], false);
    f.send(p, json!({"op":"test_window_button_events","id":id}));
    wait(|| {
        f.output(p, true)
            .contains("window-button-event key-activation")
    });
    let output = f.output(p, true);
    let gestures: Vec<_> = output
        .lines()
        .filter_map(|line| line.strip_prefix("cito-ui: window-button-event "))
        .collect();
    assert_eq!(gestures, ["first-click false true 0", "key-activation 1"]);
    wait(|| f.frames_for(p, "set_launch_at_login").count() == 1);
    assert_eq!(latest(&f, p, "set_launch_at_login")["enabled"], true);
    assert_eq!(inspect(&f, p).settings(id)["accessibility_value"], 0);
}

#[test]
fn native_form_accessibility_checkbox_values_and_tab_navigation() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["host_hub"]["enabled"] = json!(true);
    open_settings(&f, p, &cfg, "hubs");
    control(&f, p, "open_add_hub");
    let s = inspect(&f, p);
    for (id, label) in [
        ("host_hub_port", "Host Hub port"),
        ("settings_hub_url", "Hub URL"),
        ("settings_hub_name", "Hub name"),
        ("settings_hub_user", "Hub username"),
    ] {
        assert_eq!(s.settings(id)["accessibility_label"], label, "{id}");
    }
    f.send(p, json!({"op":"test_focus_field","id":"settings_hub_url"}));
    f.send(p, json!({"op":"test_tab"}));
    let s = inspect(&f, p);
    assert_eq!(
        s["field_editors"][0]["id"],
        "settings_hub_name",
        "url={}; name={}; editors={}",
        s.settings("settings_hub_url"),
        s.settings("settings_hub_name"),
        s["field_editors"]
    );
    f.send(p, json!({"op":"test_tab","backward":true}));
    assert_eq!(inspect(&f, p)["field_editors"][0]["id"], "settings_hub_url");
    f.send(p, json!({"op":"test_end_editing"}));
    for (mode, fields) in [
        (
            "token",
            vec![
                ("settings_hub_token", "Hub token"),
                ("settings_hub_user", "Hub username"),
            ],
        ),
        (
            "oidc",
            vec![
                ("settings_hub_issuer", "OIDC issuer URL"),
                ("settings_hub_client_id", "OIDC client ID"),
                ("settings_hub_redirect_port", "OIDC redirect port"),
            ],
        ),
    ] {
        control(&f, p, &format!("settings_hub_auth:{mode}"));
        let collapsed = inspect(&f, p);
        assert_eq!(collapsed.settings("add_hub_form")["height"], 252);
        if mode == "oidc" {
            assert_eq!(
                collapsed.settings("manual_hub_oidc")["accessibility_label"],
                "▸ Configure OIDC settings manually…"
            );
            assert!(collapsed.settings("settings_hub_client_id").is_null());
            f.send(
                p,
                json!({"op":"test_focus_field","id":"settings_hub_client_id"}),
            );
            let expanded = inspect(&f, p);
            assert_eq!(expanded.settings("add_hub_form")["height"], 298);
            assert_eq!(expanded["field_editors"][0]["id"], "settings_hub_client_id");
            f.send(p, json!({"op":"test_end_editing"}));
        }
        let s = inspect(&f, p);
        for (id, label) in fields {
            assert_eq!(s.settings(id)["accessibility_label"], label);
        }
    }
    for enabled in [false, true] {
        cfg["launch_at_login"] = json!(enabled);
        f.send(p, json!({"op":"settings","settings":cfg}));
        let s = inspect(&f, p);
        let checkbox = s.settings("settings_launch_at_login");
        assert_eq!(checkbox["native_button"], true);
        assert_eq!(checkbox["can_become_key_view"], true);
        assert_eq!(checkbox["accessibility_role"], "AXCheckBox");
        assert_eq!(checkbox["accessibility_value"], u8::from(enabled));
        assert_eq!(checkbox["accessibility_label"], "Start Cito at Login");
    }
    control(&f, p, "settings_launch_at_login");
    inspect(&f, p);
    assert_eq!(latest(&f, p, "set_launch_at_login")["enabled"], false);

    let trigger = "open_uninstall_confirm";
    f.send(p, json!({"op":"test_control","id":trigger,"drag":[false]}));
    let canceled = inspect(&f, p);
    assert!(canceled.settings("uninstall_alert_sheet").is_null());
    assert_eq!(canceled.settings(trigger)["accepts_first_mouse"], false);
    assert_eq!(canceled.settings(trigger)["can_become_key_view"], true);
    assert_eq!(canceled.settings(trigger)["focused"], true);
    f.send(p, json!({"op":"test_tab","backward":true}));
    assert_eq!(inspect(&f, p).settings(trigger)["focused"], false);
    f.send(p, json!({"op":"test_tab"}));
    assert_eq!(inspect(&f, p).settings(trigger)["focused"], true);
    f.send(
        p,
        json!({"op":"test_settings_key","key":"space","via":"responder"}),
    );
    assert!(inspect(&f, p).settings("uninstall_alert_sheet").is_object());
    control(&f, p, "cancel_uninstall_all");
    f.send(
        p,
        json!({"op":"test_control","id":trigger,"drag":[false,true]}),
    );
    for enabled in [false, true] {
        let checkbox = inspect(&f, p).settings("uninstall_purge_data");
        assert_eq!(checkbox["native_button"], true);
        assert_eq!(checkbox["accessibility_role"], "AXCheckBox");
        assert_eq!(checkbox["accessibility_value"], u8::from(enabled));
        assert_eq!(
            checkbox["accessibility_label"],
            "Also delete local data and Hub settings"
        );
        if !enabled {
            control(&f, p, "uninstall_purge_data");
        }
    }
    control(&f, p, "cancel_uninstall_all");
    control(&f, p, "settings_tab:channels");
    f.send(
        p,
        json!({"op":"test_focus_field","id":"channel_field:channel:account"}),
    );
    assert_eq!(
        inspect(&f, p).settings("channel_field:channel:account")["accessibility_label"],
        "Account"
    );
    f.send(p, json!({"op":"test_end_editing"}));
    control(&f, p, "edit_hub_service:channel");
    assert_eq!(
        inspect(&f, p).settings("channel_field:channel:secret")["accessibility_label"],
        "Secret"
    );
    f.send(p, json!({"op":"test_open_panel","surface":"directory"}));
    let s = inspect(&f, p);
    for (id, label) in [
        ("directory_search", "Search directory and activity"),
        ("composer_input", "Message"),
    ] {
        assert_eq!(s.directory(id)["accessibility_label"], label);
    }
    for panel in s["key_panels"].as_array().unwrap() {
        assert_eq!(panel["automatic_key_view_loop"], true);
    }
}

#[test]
fn app_check_pending_uses_checking_label_and_accessibility() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["apps"] = json!([{"id":"fixture","display_name":"Fixture","tier":"stable","installed":true,"configured":true,"status":"configured","detail":"Connected","paths":[]}]);
    open_settings(&f, p, &cfg, "apps");
    control(&f, p, "setup_app:fixture");
    let button = inspect(&f, p).settings("setup_app:fixture");
    assert_eq!(button["text"], "Checking…");
    assert_eq!(button["accessibility_label"], "Checking…");
    assert_eq!(button["enabled"], false);
    let request = latest(&f, p, "setup_apply");
    f.send(p, json!({"op":"state","source_op":"setup_apply","request_id":request["request_id"],"committed":true,"ok":true}));
    let checked = inspect(&f, p).settings("setup_app:fixture");
    assert_eq!(checked["text"], "✓ Checked");
    assert_eq!(checked["enabled"], true);
}
