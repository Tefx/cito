use super::lazy_rendering::render_state;
use super::*;

fn open_settings(f: &Fixture, p: usize, cfg: &Value, tab: &str) {
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(f, p, "open_settings");
    control(f, p, &format!("settings_tab:{tab}"));
    inspect(f, p);
}

#[test]
fn truncated_settings_refresh_scalars_without_replacing_channels_and_state_banners_redraw() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    let mut base = state();
    base["settings"] = cfg.clone();
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
    assert!(!node(&inspect(&f, p), "items", "menu_error_row").is_null());

    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    assert!(!node(
        &inspect(&f, p),
        "settings_items",
        "settings_channel:channel"
    )
    .is_null());
    let settings_before = render_state(&f, p);
    base["warning"] = json!("Refresh will retry shortly.");
    f.send(p, base.clone());
    assert!(
        render_state(&f, p)["settings_rebuilds"].as_u64().unwrap()
            > settings_before["settings_rebuilds"].as_u64().unwrap()
    );
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "settings_error_label")["text"],
        "Refresh will retry shortly."
    );
    control(&f, p, "dismiss_settings_error");

    cfg["channels"] = json!([]);
    cfg["apps"] = json!([]);
    cfg["hubs"][0]["user"] = json!("bob");
    cfg["hubs"][0]["status"] = json!("disconnected");
    cfg["channel_inventory_error"] = json!("Service unavailable.");
    cfg["launch_at_login"] = json!(true);
    f.send(p, json!({"op":"settings","truncated":true,"settings":cfg}));
    let stale = inspect(&f, p);
    assert_eq!(
        node(&stale, "settings_items", "channel_inventory_error")["text"],
        "Service unavailable."
    );
    assert_eq!(
        node(&stale, "settings_items", "inbox_control_title")["text"],
        "You (@bob)"
    );
    assert!(!node(&stale, "settings_items", "settings_channel:channel").is_null());
    assert_eq!(stale["settings_launch_at_login"], true);

    cfg["channel_inventory_error"] = Value::Null;
    cfg["hubs"][0]["user"] = json!("alice");
    base["settings"] = cfg;
    base["truncated"] = json!(true);
    base.as_object_mut().unwrap().remove("warning");
    f.send(p, base);
    let fresh = inspect(&f, p);
    assert!(node(&fresh, "settings_items", "channel_inventory_error").is_null());
    assert_eq!(
        node(&fresh, "settings_items", "inbox_control_title")["text"],
        "You (@alice)"
    );
    assert!(!node(&fresh, "settings_items", "settings_channel:channel").is_null());
}

#[test]
fn settings_shedding_respects_flat_and_dotted_keys_without_freezing_other_collections() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
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
        f.send(p, json!({"op":"settings","hub":"personal","settings":update,"truncated":true,"truncated_collections":[key]}));
        let projection = render_state(&f, p);
        for collection in ["apps", "channels", "hubs"] {
            let count = if collection == preserved {
                original[collection].as_array().unwrap().len()
            } else {
                0
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
    let f = Fixture::new(json!({}));
    let p = start(&f);
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
        node(&before, "settings_items", "channel_field:channel:account")["text"],
        "Unsaved account"
    );
    control(&f, p, "clear_channel_service:channel");
    confirm_channel_action(&f, p, "disconnect_human_channel");
    let after = inspect(&f, p);
    assert_eq!(after["pending_channel_resets"], json!([]));
    assert_eq!(
        node(&after, "settings_items", "channel_field:channel:account")["text"],
        "Unsaved account"
    );
    assert!(node(&after, "settings_items", "channel_field:channel:secret").is_null());
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    let action = f
        .frames(p)
        .into_iter()
        .rfind(|a| a["op"] == "save_hub_service")
        .unwrap();
    assert!(
        action["values"]["secret"].is_null(),
        "Cleared gateway draft must not be resubmitted"
    );
}

#[test]
fn channel_receipts_without_channel_ids_settle_resets_and_retire_only_committed_secrets() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][0]["value"] = Value::Null;
    cfg["channels"][0]["fields"][1]["value"] = json!("saved");
    open_settings(&f, p, &cfg, "channels");
    let latest = |op: &str| f.frames(p).into_iter().rfind(|a| a["op"] == op).unwrap();
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
    assert!(!node(&settled, "settings_items", "channel_field:channel:secret").is_null());
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
        let field = node(&settled, "settings_items", "channel_field:channel:account");
        if fenced {
            assert_eq!(field["text"], "new draft");
        } else {
            assert!(field.is_null());
        }
    }
    control(&f, p, "edit_user_channel:channel");
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:account"
        )["text"],
        "saved"
    );
}

#[test]
fn authority_fenced_pair_receipts_retire_pending_without_replacement_instructions() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    open_settings(&f, p, &cfg, "channels");
    // Exercise both the daemon's receipt-only settlement and defensive handling
    // of a stale pair payload carrying the authority fence.
    for op in ["settings", "pair_code"] {
        control(&f, p, "pair_channel:channel");
        let pending = inspect(&f, p);
        assert_eq!(
            node(&pending, "settings_items", "pair_channel:channel")["enabled"],
            false
        );
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "channel_pair")
            .unwrap();
        cfg["hubs"][0]["url"] = json!("https://hub.example.com");
        cfg["channels"][0]["detail"] = json!("Replacement ready");
        f.send(p, json!({"op":"settings","settings":cfg}));
        let receipt = json!({"op":op,"request_id":request["request_id"],"hub":"personal","channel":"channel","action":"channel_pair","committed":true,"authority_changed":true,"warning":"Hub changed; refresh failed.","instruction":"Obsolete pairing instruction","code":"obsolete-code","pairing_command":"obsolete-command"});
        let mut unrelated = receipt.clone();
        unrelated["hub"] = json!("work");
        f.send(p, unrelated);
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "pair_channel:channel")["enabled"],
            false
        );
        let mut receipt = receipt;
        receipt.as_object_mut().unwrap().remove("channel");
        f.send(p, receipt);
        let settled = inspect(&f, p);
        assert_eq!(
            node(&settled, "settings_items", "pair_channel:channel")["enabled"],
            true
        );
        assert!(node(&settled, "settings_items", "pairing_instruction:channel").is_null());
        assert!(!settled["pending_controls"]
            .as_array()
            .unwrap()
            .contains(&request["request_id"]));
        assert_eq!(
            node(&settled, "settings_items", "settings_error_label")["text"],
            "Hub changed; refresh failed."
        );
        assert_ne!(settled["clipboard"], "obsolete-command");
    }
}

#[test]
fn payload_free_committed_save_settles_exact_request_and_preserves_newer_drafts() {
    let _lock = serial();
    for authority_changed in [false, true] {
        for newer_draft in [false, true] {
            let f = Fixture::new(json!({}));
            let p = start(&f);
            let mut cfg = settings();
            cfg["settings_hub"] = json!("personal");
            open_settings(&f, p, &cfg, "channels");
            f.send(p, json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"submitted"}));
            control(&f, p, "save_channel:channel");
            inspect(&f, p);
            let request = f
                .frames(p)
                .into_iter()
                .rev()
                .find(|a| a["op"] == "channel_save")
                .unwrap();
            if newer_draft {
                f.send(p, json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"newer draft"}));
            }
            let receipt = json!({"op":"settings","request_id":request["request_id"],"hub":"personal","channel":"channel","action":"channel_save","committed":true,"authority_changed":authority_changed,"warning":"Saved; settings refresh failed."});
            for (key, wrong) in [
                ("request_id", "unrelated"),
                ("hub", "work"),
                ("channel", "other-channel"),
                ("action", "channel_pair"),
            ] {
                let mut unrelated = receipt.clone();
                unrelated[key] = json!(wrong);
                f.send(p, unrelated);
                let pending = inspect(&f, p);
                assert!(pending["pending_channel_saves"]
                    .as_array()
                    .unwrap()
                    .contains(&request["request_id"]));
                assert!(node(&pending, "settings_items", "settings_error_banner").is_null());
            }
            f.send(p, receipt);
            let settled = inspect(&f, p);
            assert!(!settled["pending_channel_saves"]
                .as_array()
                .unwrap()
                .contains(&request["request_id"]));
            assert!(!settled["pending_controls"]
                .as_array()
                .unwrap()
                .contains(&request["request_id"]));
            assert_eq!(
                node(&settled, "settings_items", "settings_error_label")["text"],
                "Saved; settings refresh failed."
            );
            let expected = if newer_draft {
                "newer draft"
            } else {
                "submitted"
            };
            assert_eq!(
                node(&settled, "settings_items", "channel_field:channel:account")["text"],
                expected
            );
            control(&f, p, "save_channel:channel");
            inspect(&f, p);
            assert_eq!(
                f.frames(p)
                    .into_iter()
                    .rev()
                    .find(|a| a["op"] == "channel_save")
                    .unwrap()["values"]["account"],
                expected
            );
        }
    }
}

#[test]
fn authority_fenced_test_receipts_retire_pending_without_replacement_results() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["warning"] = json!(true);
    cfg["channels"][0]["detail"] = json!("Replacement ready");
    open_settings(&f, p, &cfg, "channels");
    for op in ["settings", "channel_test_result"] {
        control(&f, p, "test_channel:channel");
        inspect(&f, p);
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "channel_test")
            .unwrap();
        f.send(p, json!({"op":op,"request_id":request["request_id"],"hub":"personal","channel_id":"channel","source_op":"channel_test","committed":true,"authority_changed":true,"warning":"Hub changed; refresh failed.","ok":true,"detail":"Obsolete test result"}));
        let settled = inspect(&f, p);
        assert!(!settled["pending_controls"]
            .as_array()
            .unwrap()
            .contains(&request["request_id"]));
        assert_eq!(
            node(
                &settled,
                "settings_items",
                "settings_channel_detail:channel"
            )["text"],
            "Replacement ready"
        );
    }
}

#[test]
fn read_only_pairing_editor_can_select_and_copy_and_feedback_restores_full_width() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["can_edit_personal"] = json!(false);
    open_settings(&f, p, &cfg, "channels");
    let ready = inspect(&f, p);
    assert_eq!(
        node(&ready, "settings_items", "edit_user_channel:channel")["hidden"],
        true
    );
    assert_eq!(
        node(&ready, "settings_items", "pair_channel:channel")["text"],
        "Re-pair…"
    );
    control(&f, p, "pair_channel:channel");
    inspect(&f, p);
    let pair = f
        .frames(p)
        .into_iter()
        .rfind(|v| v["op"] == "channel_pair")
        .unwrap();
    let instruction = "Open your account, then enter this code.\nCode: 123456";
    f.send(p, json!({"op":"pair_code","hub":"personal","channel_id":"channel","request_id":pair["request_id"],"instruction":instruction,"pairing_command":"pair 123456"}));
    let copied = inspect(&f, p);
    let width = node(&copied, "settings_items", "copy_pair_command:channel")["width"].clone();
    assert_eq!(
        node(&copied, "settings_items", "copy_pair_command:channel")["text"],
        "✓ Copied"
    );
    f.send(
        p,
        json!({"op":"test_expire_copy_feedback","id":"copy_pair_command:channel"}),
    );
    let restored = inspect(&f, p);
    let copy = node(&restored, "settings_items", "copy_pair_command:channel");
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
        node(
            &inspect(&f, p),
            "settings_items",
            "pairing_instruction:channel"
        )["text"],
        instruction
    );

    f.send(p, json!({"op":"test_oracle_calibration","kind":"bar_anchor","id":"cancel_pair_channel:channel"}));
    assert_eq!(
        inspect_unchecked(&f, p, None)["non_anchored_action_bars"],
        json!(["stratum_user:channel: pairing row is not edge anchored"])
    );
    f.send(p, json!({"op":"test_end_editing"}));
    f.send(p, json!({"op":"test_oracle_calibration","kind":"reset"}));
    inspect(&f, p);
}

#[test]
fn gateway_clear_only_bar_fits_and_new_channel_edits_remove_saved_feedback() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["channels"][0]["hub_ready"] = json!(false);
    cfg["channels"][0]["fields"][0]["hidden"] = json!(true);
    open_settings(&f, p, &cfg, "channels");
    let snapshot = inspect(&f, p);
    let clear = node(&snapshot, "settings_items", "clear_channel_service:channel");
    let stratum = node(&snapshot, "settings_items", "stratum_hub:channel");
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
    let saved = f
        .frames(p)
        .into_iter()
        .rfind(|v| v["op"] == "channel_save")
        .unwrap();
    cfg["channels"][0]["fields"][1]["value"] = json!("first draft");
    f.send(p, json!({"op":"settings","settings":cfg,"ok":true,"source_op":"channel_save","request_id":saved["request_id"]}));
    let confirmed = inspect(&f, p);
    let button = |s: &Value| node(s, "settings_items", "save_channel:channel");
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
        node(
            &remounted,
            "settings_items",
            "channel_field:channel:account"
        )["text"],
        "newer edit"
    );
}

#[test]
fn settings_oracle_checks_native_wrap_insets_and_added_action_rows() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    open_settings(&f, p, &settings(), "channels");
    f.send(
        p,
        json!({"op":"test_oracle_calibration","kind":"wrapped_cell_inset"}),
    );
    assert!(inspect_unchecked(&f, p, None)["truncated_labels"]
        .as_array()
        .unwrap()
        .contains(&json!("wrapped_cell_probe")));
    f.send(p, json!({"op":"test_oracle_calibration","kind":"reset"}));
    inspect(&f, p);

    control(&f, p, "settings_tab:hubs");
    for (open, button, container, row) in [
        (None, "toggle_host_hub", "host_hub_card", "sharing"),
        (
            Some("open_add_hub"),
            "submit_add_hub",
            "add_hub_form",
            "submit",
        ),
        (
            Some("open_uninstall_confirm"),
            "confirm_disconnect_all",
            "uninstall_confirm_box",
            "confirm",
        ),
    ] {
        if let Some(id) = open {
            control(&f, p, id);
        }
        inspect(&f, p);
        for (kind, message) in [
            ("bar_anchor", "is not edge anchored"),
            ("bar_midY", "controls are not vertically centered"),
        ] {
            f.send(
                p,
                json!({"op":"test_oracle_calibration","kind":kind,"id":button}),
            );
            assert_eq!(
                inspect_unchecked(&f, p, None)["non_anchored_action_bars"],
                json!([format!("{container}: {row} row {message}")])
            );
            f.send(p, json!({"op":"test_oracle_calibration","kind":"reset"}));
            inspect(&f, p);
        }
    }
}

#[test]
fn channel_pair_without_prior_test_result_disables_immediately() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    open_settings(&f, p, &settings(), "channels");
    let before = render_state(&f, p);
    control(&f, p, "pair_channel:channel");
    let pending = render_state(&f, p);
    assert!(
        pending["settings_rebuilds"].as_u64().unwrap()
            > before["settings_rebuilds"].as_u64().unwrap()
    );
    let s = inspect(&f, p);
    let button = node(&s, "settings_items", "pair_channel:channel");
    assert_eq!(button["text"], "Pairing…");
    assert_eq!(button["enabled"], false);
    let count = f
        .frames(p)
        .iter()
        .filter(|a| a["op"] == "channel_pair")
        .count();
    assert_eq!(count, 1);
    control(&f, p, "pair_channel:channel");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|a| a["op"] == "channel_pair")
            .count(),
        count
    );
}

#[test]
fn menu_close_removes_inline_rename_without_a_background_frame() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
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
    assert!(node(
        &reopened,
        "items",
        "rename_field:personal:repo-abcdefghijkl"
    )
    .is_null());
    assert!(!node(&reopened, "items", "rename:personal:repo-abcdefghijkl").is_null());
    assert!(!f.frames(p).iter().any(|a| a["op"] == "rename_session"));
}

#[test]
fn host_port_matching_receipt_settles_normalized_values_and_keeps_new_drafts() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["host_hub"]["enabled"] = json!(true);
    open_settings(&f, p, &cfg, "hubs");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":" \n4291\t "}),
    );
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["enabled"],
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
            node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["enabled"],
            false
        );
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "set_host_hub")
            .unwrap();
        assert_eq!(request["port"], value.trim().parse::<u16>().unwrap());
        f.send(
            p,
            json!({"op":"settings","request_id":"unrelated","settings":cfg}),
        );
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["enabled"],
            false
        );
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":" 4600 "}),
        );
        // The daemon retains/normalizes to its old port; a matching receipt still settles.
        f.send(p, json!({"op":"settings","source_op":"set_host_hub","ok":true,"request_id":request["request_id"],"settings":cfg}));
        let s = inspect(&f, p);
        let apply = node(&s, "settings_items", "apply_host_hub_port");
        assert_eq!(apply["text"], "Apply");
        assert_eq!(apply["enabled"], true);
        assert_eq!(
            node(&s, "settings_items", "host_hub_port")["text"],
            " 4600 "
        );
    }
    // Both sharing and auth changes use the same trimmed validation path.
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":" \t4700 "}),
    );
    for id in ["toggle_host_hub_auth", "toggle_host_hub"] {
        control(&f, p, id);
        inspect(&f, p);
        assert_eq!(
            f.frames(p)
                .into_iter()
                .rev()
                .find(|a| a["op"] == "set_host_hub")
                .unwrap()["port"],
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
        let f = Fixture::new(json!({}));
        let p = start(&f);
        let mut frame = state();
        frame["listeners"] = json!([]);
        frame["settings"] = settings();
        f.send(p, frame);
        f.send(p, json!({"op":"test_menu_open"}));
        let before = inspect(&f, p);
        assert_eq!(
            node(&before, "items", "open_settings")["tooltip"],
            "Settings… (⌘,)"
        );
        assert!(!node(&before, "items", id).is_null(), "{id}");
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
                node(
                    &after,
                    "settings_items",
                    &format!("settings_{candidate}_tab")
                )["hidden"],
                candidate != tab,
                "{id}: {candidate}"
            );
        }
    }
}

#[test]
fn local_key_monitor_accepts_only_exact_command_shortcuts() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(p, frame);
    for (character, op) in [(",", "open_settings"), ("k", "open_directory")] {
        let count = || f.frames(p).iter().filter(|a| a["op"] == op).count();
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
fn native_form_accessibility_checkbox_values_and_tab_navigation() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
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
        assert_eq!(
            node(&s, "settings_items", id)["accessibility_label"],
            label,
            "{id}"
        );
    }
    f.send(p, json!({"op":"test_focus_field","id":"settings_hub_url"}));
    f.send(p, json!({"op":"test_tab"}));
    let s = inspect(&f, p);
    assert_eq!(
        s["field_editors"][0]["id"],
        "settings_hub_name",
        "url={}; name={}; editors={}",
        node(&s, "settings_items", "settings_hub_url"),
        node(&s, "settings_items", "settings_hub_name"),
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
            ],
        ),
    ] {
        control(&f, p, &format!("settings_hub_auth:{mode}"));
        let s = inspect(&f, p);
        for (id, label) in fields {
            assert_eq!(node(&s, "settings_items", id)["accessibility_label"], label);
        }
    }
    for enabled in [false, true] {
        cfg["launch_at_login"] = json!(enabled);
        f.send(p, json!({"op":"settings","settings":cfg}));
        let s = inspect(&f, p);
        let checkbox = node(&s, "settings_items", "settings_launch_at_login");
        assert_eq!(checkbox["accessibility_role"], "AXCheckBox");
        assert_eq!(checkbox["accessibility_value"], u8::from(enabled));
        assert_eq!(checkbox["accessibility_label"], "Start Cito at Login");
    }
    control(&f, p, "open_uninstall_confirm");
    for enabled in [false, true] {
        let checkbox = node(&inspect(&f, p), "settings_items", "uninstall_purge_data");
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
        node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:account"
        )["accessibility_label"],
        "Account"
    );
    f.send(p, json!({"op":"test_end_editing"}));
    control(&f, p, "edit_hub_service:channel");
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:secret"
        )["accessibility_label"],
        "Secret"
    );
    f.send(p, json!({"op":"test_open_panel","surface":"directory"}));
    let s = inspect(&f, p);
    for (id, label) in [
        ("directory_search", "Search directory and activity"),
        ("composer_input", "Message"),
    ] {
        assert_eq!(
            node(&s, "directory_items", id)["accessibility_label"],
            label
        );
    }
    for panel in s["key_panels"].as_array().unwrap() {
        assert_eq!(panel["automatic_key_view_loop"], true);
    }
}

#[test]
fn app_check_pending_uses_checking_label_and_accessibility() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["apps"] = json!([{"id":"fixture","display_name":"Fixture","tier":"stable","installed":true,"configured":true,"status":"configured","detail":"Connected","paths":[]}]);
    open_settings(&f, p, &cfg, "apps");
    control(&f, p, "setup_app:fixture");
    let button = node(&inspect(&f, p), "settings_items", "setup_app:fixture");
    assert_eq!(button["text"], "Checking…");
    assert_eq!(button["accessibility_label"], "Checking…");
    assert_eq!(button["enabled"], false);
}
