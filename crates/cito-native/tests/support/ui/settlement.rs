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
            if newer_draft || authority_changed {
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
