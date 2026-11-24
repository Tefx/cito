use super::settlement::{input, latest, open_settings};
use super::*;

#[test]
fn deferred_sharing_controls_send_partial_updates_while_stop_is_pending() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state_with_settings(settings());
    frame["settings"]["host_hub"]["enabled"] = json!(true);
    frame["settings"]["host_hub"]["running"] = json!(true);
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"4292"}),
    );
    f.send(p, json!({"op":"test_focus_field","id":"host_hub_port"}));
    control(&f, p, "toggle_host_hub");
    let stale = inspect(&f, p);
    assert_eq!(stale["settings_rebuild_deferred"], true);
    assert_eq!(stale.settings("toggle_host_hub")["text"], "Stop Sharing");
    assert_eq!(stale["pending_controls"].as_array().unwrap().len(), 1);

    control(&f, p, "toggle_host_hub_auth");
    control(&f, p, "apply_host_hub_port");
    inspect(&f, p);
    let updates: Vec<_> = f.frames_for(p, "set_host_hub").collect();
    assert_eq!(updates.len(), 3);
    assert_eq!(updates[0]["enabled"], false);
    assert_eq!(updates[1]["auth_mode"], "token");
    assert_eq!(updates[2]["port"], 4292);
    assert!(updates[1].get("enabled").is_none());
    assert!(updates[2].get("enabled").is_none());
}

#[test]
fn deferred_hub_rows_exclude_duplicate_requests_before_repaint() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state_with_settings(settings());
    frame["settings"]["host_hub"]["enabled"] = json!(true);
    frame["settings"]["hubs"][1]["can_login"] = json!(true);
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    f.send(p, json!({"op":"test_focus_field","id":"host_hub_port"}));
    for (id, op) in [
        ("login_hub:work", "hub_action"),
        ("toggle_hub:work", "setup_toggle_hub"),
        ("remove_hub:work", "setup_remove_hub"),
    ] {
        control(&f, p, id);
        let stale = inspect(&f, p);
        assert_eq!(stale["settings_rebuild_deferred"], true);
        assert_eq!(stale.settings(id)["enabled"], true);
        control(&f, p, id);
        inspect(&f, p);
        let actions: Vec<_> = f.frames_for(p, op).collect();
        assert_eq!(actions.len(), 1, "duplicate {op} from the mounted row");
        assert_eq!(actions[0]["hub"], "work");
    }
    f.send(p, json!({"op":"test_end_editing"}));
    let repainted = inspect(&f, p);
    assert_eq!(repainted["settings_rebuild_deferred"], false);
    for id in ["login_hub:work", "toggle_hub:work", "remove_hub:work"] {
        assert_eq!(repainted.settings(id)["enabled"], false);
    }
}

#[test]
fn host_hub_failed_projection_and_trailing_error_have_one_error_surface() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["host_hub"]["enabled"] = json!(true);
    let frame = state_with_settings(cfg.clone());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"4292"}),
    );
    control(&f, p, "apply_host_hub_port");
    let action = latest(&f, p, "set_host_hub");
    cfg["host_hub"]["error"] = json!("Cannot start sharing: Address already in use");
    cfg["host_hub"]["running"] = json!(false);
    f.send(p, json!({"op":"settings","source_op":"set_host_hub","request_id":action["request_id"],"ok":false,"error":"Cannot start sharing: Address already in use","settings":cfg}));
    f.send(p, json!({"op":"error","source_op":"set_host_hub","request_id":action["request_id"],"error":"Cannot start sharing: Address already in use"}));
    let s = inspect(&f, p);
    assert_eq!(s["host_hub_port_pending"], false);
    assert_eq!(s["pending_controls"], json!([]));
    assert_eq!(
        s.settings("host_hub_error")["text"],
        "Cannot start sharing: Address already in use"
    );
    assert!(s.settings("settings_error_banner").is_null());
    assert_eq!(s.settings("host_hub_port")["text"], "4292");
}

#[test]
fn editing_host_port_retires_saved_feedback_in_place() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["host_hub"]["enabled"] = json!(true);
    let frame = state_with_settings(cfg.clone());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    let input = |value: &str| {
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":value}),
        );
    };
    input("4292");
    control(&f, p, "apply_host_hub_port");
    let request = latest(&f, p, "set_host_hub");
    cfg["host_hub"]["port"] = json!(4292);
    f.send(p, json!({"op":"settings","source_op":"set_host_hub","ok":true,"request_id":request["request_id"],"settings":cfg}));
    let button = |snapshot: &Value| snapshot.settings("apply_host_hub_port");
    let saved = button(&inspect(&f, p));
    assert_eq!(saved["text"], "✓ Saved");
    assert_eq!(saved["green"], true);
    assert_eq!(saved["enabled"], false);
    input("4293");
    let edited = button(&inspect(&f, p));
    assert_eq!(edited["instance"], saved["instance"]);
    assert_eq!(edited["enabled"], true);
    assert_eq!(edited["text"], "Apply");
    assert_eq!(edited["accessibility_label"], "Apply");
    assert_eq!(edited["green"], false);
    input("4292");
    let unchanged = button(&inspect(&f, p));
    assert_eq!(unchanged["enabled"], false);
    assert_eq!(unchanged["text"], "Apply");
}

#[test]
fn hub_row_mutations_disable_duplicate_submission_and_settle_correlated_receipts() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state_with_settings(settings());
    frame["settings"]["hubs"][1]["can_login"] = json!(true);
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    for (id, op) in [
        ("login_hub:work", "hub_action"),
        ("toggle_hub:work", "setup_toggle_hub"),
        ("remove_hub:work", "setup_remove_hub"),
    ] {
        let before = f.frames(p).len();
        control(&f, p, id);
        let pending = inspect(&f, p);
        assert_eq!(pending.settings(id)["enabled"], false);
        control(&f, p, id);
        inspect(&f, p);
        let actions: Vec<_> = f
            .frames(p)
            .into_iter()
            .skip(before)
            .filter(|a| a["op"] == op)
            .collect();
        assert_eq!(actions.len(), 1);
        let action = &actions[0];
        assert!(!action["request_id"].as_str().unwrap().is_empty());
        f.send(p, json!({"op":"state","source_op":op,"request_id":"stale","ok":true,"committed":true,"warning":"An older request"}));
        assert_eq!(inspect(&f, p).settings(id)["enabled"], false);
        f.send(p, json!({"op":"state","source_op":op,"request_id":action["request_id"],"ok":true,"committed":true,"warning":"Saved; refresh unavailable"}));
        let settled = inspect(&f, p);
        assert_eq!(settled.settings(id)["enabled"], true);
        assert_eq!(settled["pending_controls"], json!([]));
        assert_eq!(
            settled.settings("settings_error_label")["text"],
            "Saved; refresh unavailable"
        );
    }
}

#[test]
fn host_toggle_failures_settle_and_redraw_the_settings_banner() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state_with_settings(settings());
    frame["settings"]["host_hub"]["enabled"] = json!(true);
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    for id in ["toggle_host_hub", "toggle_host_hub_auth"] {
        control(&f, p, id);
        let request = latest(&f, p, "set_host_hub");
        assert_eq!(inspect(&f, p).settings(id)["enabled"], false);
        f.send(p, json!({"op":"error","source_op":"set_host_hub","request_id":request["request_id"],"error":"Sharing failed"}));
        let s = inspect(&f, p);
        assert_eq!(s["pending_controls"], json!([]));
        assert_eq!(s.settings(id)["enabled"], true);
        assert_eq!(s.settings("settings_error_label")["text"], "Sharing failed");
        control(&f, p, "dismiss_settings_error");
    }
}

#[test]
fn clipboard_join_errors_stay_outside_an_idle_add_hub_form() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let frame = state_with_settings(settings());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    f.send(
        p,
        json!({"op":"test_clipboard","text":"https://hub.example.com"}),
    );
    control(&f, p, "paste_hub_invite");
    let request = latest(&f, p, "paste_hub_invite");
    f.send(
        p,
        json!({"op":"error","request_id":"unrelated","error":"Other request failed"}),
    );
    assert_eq!(inspect(&f, p)["hub_invite_pending"], true);
    for correlated in [true, false] {
        let mut error = json!({"op":"error","source_op":"paste_hub_invite","error":"Join failed"});
        if correlated {
            error["request_id"] = request["request_id"].clone();
        }
        f.send(p, error);
        let s = inspect(&f, p);
        assert_eq!(s["hub_invite_pending"], false);
        assert!(s.settings("add_hub_error").is_null());
        assert_eq!(s.settings("settings_error_label")["text"], "Join failed");
    }
}

#[test]
fn a_late_parse_failure_cannot_consume_the_current_invite() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    open_settings(&f, p, &settings(), "hubs");
    control(&f, p, "open_add_hub");
    let paste = |value| {
        input(&f, p, "settings_hub_url", value);
        latest(&f, p, "paste_hub_invite")["request_id"].clone()
    };
    let old = paste("cito setup --url https://old.example.com");
    let current = paste("cito setup --url https://new.example.com");
    assert_ne!(old, current);
    f.send(p,json!({"op":"error","source_op":"paste_hub_invite","request_id":old,"error":"Delayed invalid invite"}));
    f.send(p,json!({"op":"hub_invite","request_id":current,"config":{"name":"corrected","url":"https://new.example.com"}}));
    let parsed = inspect(&f, p);
    assert_eq!(parsed.settings("settings_hub_name")["text"], "corrected");
    assert!(parsed.settings("settings_error_banner").is_null());
}

#[test]
fn v41_start_sharing_and_auth_validate_ports_without_blocking_stop() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    let frame = state_with_settings(cfg.clone());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    for bad in ["0", "65536", "not-a-port"] {
        cfg["host_hub"]["enabled"] = json!(true);
        f.send(p, json!({"op":"settings","settings":cfg}));
        inspect(&f, p);
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":bad}),
        );
        control(&f, p, "toggle_host_hub_auth");
        assert!(inspect(&f, p).settings("host_hub_error")["text"]
            .as_str()
            .unwrap()
            .contains("1 to 65535"));
        cfg["host_hub"]["enabled"] = json!(false);
        f.send(p, json!({"op":"settings","settings":cfg}));
        inspect(&f, p);
        control(&f, p, "toggle_host_hub");
        inspect(&f, p);
        assert!(f.frames_for(p, "set_host_hub").next().is_none());
    }
    cfg["host_hub"]["enabled"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    inspect(&f, p);
    control(&f, p, "toggle_host_hub");
    let stopped = latest(&f, p, "set_host_hub");
    assert_eq!(stopped["enabled"], false);
    assert_eq!(stopped["port"], 4291);
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"65535"}),
    );
    control(&f, p, "toggle_host_hub_auth");
    inspect(&f, p);
    assert_eq!(
        f.frames_for(p, "set_host_hub").next_back().unwrap()["port"],
        65535
    );
}

#[test]
fn v49_hub_save_settles_by_request_even_without_a_matching_projected_name() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    let frame = state_with_settings(cfg.clone());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    for (id, value) in [
        ("settings_hub_name", "shared"),
        ("settings_hub_url", "https://EXAMPLE.COM:443/"),
    ] {
        f.send(p, json!({"op":"test_settings_input","id":id,"value":value}));
    }
    control(&f, p, "submit_add_hub");
    let request = latest(&f, p, "setup_save_hub");
    f.send(p,json!({"op":"settings","ok":true,"source_op":"setup_save_hub","request_id":request["request_id"],"settings":cfg}));
    assert!(
        inspect(&f, p).settings("add_hub_form").is_null(),
        "A matching explicit success settles even when its projection omits the saved Hub"
    );
    cfg["hubs"].as_array_mut().unwrap().push(json!({"name":"shared","url":"https://example.com/","auth_mode":"none","enabled":true,"is_default":false,"status":"connected"}));
    f.send(p,json!({"op":"settings","ok":true,"source_op":"setup_save_hub","request_id":request["request_id"],"settings":cfg}));
    assert!(inspect(&f, p).settings("add_hub_form").is_null());
}

#[test]
fn v39_invite_parse_receipts_stay_in_the_form_and_modal_return_cannot_submit() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let frame = state_with_settings(settings());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    let latest = |op: &str| f.frames_for(p, op).next_back().unwrap();
    let paste = || {
        f.send(p,json!({"op":"test_settings_input","id":"settings_hub_url","value":"cito setup --hub remote --url https://hub.example.com"}));
        inspect(&f, p);
        let request = latest("paste_hub_invite");
        assert_eq!(request["parse_only"], true);
        request
    };
    let request = paste();
    f.send(p,json!({"op":"error","source_op":"paste_hub_invite","request_id":request["request_id"],"error":"Invalid invite draft"}));
    let s = inspect(&f, p);
    assert!(s.settings("settings_error_banner").is_null());
    assert!(s.settings("add_hub_error").is_null());
    let stale = paste();
    control(&f, p, "cancel_add_hub");
    control(&f, p, "open_add_hub");
    f.send(p,json!({"op":"hub_invite","request_id":stale["request_id"],"config":{"name":"stale","url":"https://stale.example.com"}}));
    assert_eq!(inspect(&f, p).settings("settings_hub_url")["text"], "");
    f.send(p, json!({"op":"test_settings_key","key":"return"}));
    assert!(!inspect(&f, p).settings("add_hub_error").is_null());
    let request = paste();
    f.send(p,json!({"op":"hub_invite","request_id":request["request_id"],"config":{"name":"remote","url":"https://hub.example.com"}}));
    let s = inspect(&f, p);
    assert!(s.settings("add_hub_error").is_null());
    assert_eq!(
        s.settings("settings_hub_url")["text"],
        "https://hub.example.com"
    );
    f.send(p,json!({"op":"hub_invite","request_id":"","config":{"name":"stale","url":"https://stale.example.com"}}));
    assert_eq!(
        inspect(&f, p).settings("settings_hub_url")["text"],
        "https://hub.example.com"
    );
    control(&f, p, "open_uninstall_confirm");
    let before = f.frames_for(p, "setup_save_hub").count();
    f.send(p, json!({"op":"test_settings_key","key":"return"}));
    inspect(&f, p);
    assert_eq!(f.frames_for(p, "setup_save_hub").count(), before);
    control(&f, p, "cancel_uninstall_all");
    f.send(p, json!({"op":"test_focus_field","id":"settings_hub_url"}));
    f.send(p, json!({"op":"test_settings_key","key":"return"}));
    inspect(&f, p);
    assert_eq!(latest("setup_save_hub")["url"], "https://hub.example.com");
    assert_eq!(f.frames_for(p, "setup_save_hub").count(), before + 1);
}

#[test]
fn new_automatic_hub_name_stays_in_create_mode_after_previous_draft_commits() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    let frame = state_with_settings(cfg.clone());
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    let enter_url = |url| {
        f.send(
            p,
            json!({"op":"test_settings_input","id":"settings_hub_url","value":url}),
        )
    };
    let latest = || f.frames_for(p, "setup_save_hub").next_back().unwrap();
    enter_url("https://relay.example.com");
    control(&f, p, "submit_add_hub");
    inspect(&f, p);
    let submitted = latest();
    assert_eq!(submitted["name"], "relay");

    enter_url("https://relay.example.org");
    control(&f, p, "settings_tab:apps");
    control(&f, p, "settings_tab:hubs");
    assert_eq!(
        inspect(&f, p).settings("settings_hub_url")["text"],
        "https://relay.example.org"
    );
    cfg["hubs"].as_array_mut().unwrap().push(json!({"name":"relay","url":"https://relay.example.com","auth_mode":"none","enabled":true,"is_default":false,"status":"connected"}));
    f.send(p, json!({"op":"settings","settings":cfg,"ok":true,"source_op":"setup_save_hub","request_id":submitted["request_id"]}));
    let draft = inspect(&f, p);
    assert_eq!(draft.settings("submit_add_hub")["text"], "Join Hub");
    control(&f, p, "submit_add_hub");
    inspect(&f, p);
    let next = latest();
    assert_ne!(next["request_id"], submitted["request_id"]);
    assert_eq!(next["name"], "relay-2");
    assert_eq!(next["url"], "https://relay.example.org");
    assert!(next["edit"].is_null());
}

#[test]
fn receipt_without_inventory_keeps_an_automatic_name_under_daemon_ownership() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let p = start(&f);
    open_settings(&f, p, &settings(), "hubs");
    control(&f, p, "open_add_hub");
    control(&f, p, "settings_hub_auth:none");
    let submit = || {
        control(&f, p, "submit_add_hub");
        latest(&f, p, "setup_save_hub")
    };
    input(&f, p, "settings_hub_url", "http://127.0.0.1:1");
    let first = submit();
    assert_eq!(f.action(first.clone()).0, 200);
    input(&f, p, "settings_hub_url", "http://127.0.0.1:2");
    f.send(p, json!({"op":"settings","source_op":"setup_save_hub","request_id":first["request_id"],"ok":true,"committed":true}));
    let form = inspect(&f, p);
    assert_eq!(form.settings("settings_hub_name")["text"], "");
    let second = submit();
    assert!(second.get("edit").is_none());
    assert!(second.get("name").is_none());
    assert_eq!(f.action(second.clone()).0, 200);
    let saved: Value = read_json(&f.config);
    assert_eq!(saved["hubs"]["remote"]["url"], first["url"]);
    assert_eq!(saved["hubs"]["remote-2"]["url"], second["url"]);
}

#[test]
fn hub_save_preserves_newer_drafts_and_rejects_a_late_error_on_retry() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    open_settings(&f, p, &cfg, "hubs");
    control(&f, p, "open_add_hub");
    control(&f, p, "settings_hub_auth:token");
    input(&f, p, "settings_hub_url", "https://studio.example.com");
    input(&f, p, "settings_hub_token", "submitted-token");
    let submit = || {
        control(&f, p, "submit_add_hub");
        latest(&f, p, "setup_save_hub")
    };
    let first = submit();
    input(&f, p, "settings_hub_token", "new-token");
    cfg["hubs"].as_array_mut().unwrap().push(json!({"name":"studio","url":first["url"],"auth_mode":"token","enabled":true,"is_default":false,"status":"connected"}));
    f.send(
        p,
        json!({"op":"settings","source_op":"setup_save_hub","ok":true,"settings":cfg}),
    );
    assert_eq!(inspect(&f, p)["pending_hub_save"], first["request_id"]);
    let ack = |id: &Value| {
        f.send(p,json!({"op":"settings","source_op":"setup_save_hub","ok":true,"request_id":id,"settings":cfg}))
    };
    ack(&first["request_id"]);
    let second = submit();
    assert_eq!(second["edit"], true);
    assert_eq!(second["original_name"], "studio");
    assert_eq!(second["token"], "new-token");
    assert_ne!(second["request_id"], first["request_id"]);
    f.send(p,json!({"op":"error","source_op":"setup_save_hub","request_id":first["request_id"],"error":"Late error"}));
    let pending = inspect(&f, p);
    assert_eq!(pending["pending_hub_save"], second["request_id"]);
    assert!(pending.settings("add_hub_error").is_null());
    ack(&second["request_id"]);
    assert!(inspect(&f, p).settings("add_hub_form").is_null());
}

#[test]
fn d12_onboarding_uses_inline_guidance_without_redundant_controls() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    for app in cfg["apps"].as_array_mut().unwrap() {
        app["configured"] = json!(false);
    }
    let frame = state_with_settings(cfg);
    f.send(p, frame);
    control(&f, p, "open_settings");
    let apps = inspect(&f, p);
    assert!(!nodes(&apps["settings_items"])
        .iter()
        .any(|n| n["text"] == "Click Set Up below, then + Listen in the Cito menu bar."));
    for tab in ["apps", "channels", "hubs"] {
        control(&f, p, &format!("settings_tab:{tab}"));
        let s = inspect(&f, p);
        for id in [
            "toggle_onboarding_guide",
            "onboarding_banner",
            "apps_next_step_callout",
        ] {
            assert!(s.settings(id).is_null());
        }
    }
    control(&f, p, "settings_tab:channels");
    control(&f, p, "channel_hub_selector:work");
    let mut disabled = settings();
    disabled["hubs"][1]["enabled"] = json!(false);
    disabled["settings_hub"] = json!("personal");
    f.send(p, json!({"op":"settings","settings":disabled}));
    let channels = inspect(&f, p);
    assert!(channels.settings("channel_hub_selector:work").is_null());
    assert!(!channels.settings("stratum_user:channel").is_null());
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "open_settings" && v["hub"] == "personal")
    });
}

#[test]
fn lifecycle_onboarding_hub_edit_share_controls_disconnect_and_empty_actions() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["apps"][1]["status"] = json!("repair_needed");
    cfg["hubs"][1]["auth_mode"] = json!("oidc");
    cfg["hubs"][1]["issuer"] = json!("https://id.example.com");
    cfg["hubs"][1]["client_id"] = json!("client");
    cfg["host_hub"]["error"] = json!("Port already in use");
    cfg["host_hub"]["auth_mode"] = json!("none");
    let mut frame = state();
    frame["listeners"] = json!([]);
    frame["available"] = json!([]);
    frame["settings"] = cfg.clone();
    frame["hubs"][1]["status"] = json!("disconnected");
    f.send(p, frame);
    control(&f, p, "empty_open_setup");
    let s = inspect(&f, p);
    assert_eq!(s["settings_open"], true);
    assert_eq!(s.settings("setup_app:codex")["text"], "Repair");
    control(&f, p, "settings_tab:channels");
    assert_eq!(inspect(&f, p)["settings_tab"], "channels");
    control(&f, p, "settings_tab:hubs");
    assert_eq!(
        inspect(&f, p).settings("host_hub_error")["text"],
        "Port already in use"
    );
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"5300"}),
    );
    control(&f, p, "apply_host_hub_port");
    control(&f, p, "toggle_host_hub_auth");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "set_host_hub" && v["port"] == 5300 && v["auth_mode"] == "token")
    });
    control(&f, p, "edit_hub:work");
    let s = inspect(&f, p);
    assert_eq!(
        s.settings("settings_hub_issuer")["text"],
        "https://id.example.com"
    );
    assert_eq!(s.settings("submit_add_hub")["text"], "Save Hub");
    control(&f, p, "submit_add_hub");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "setup_save_hub"
                && v["edit"] == true
                && v.get("set_default").is_none()
                && v["original_name"] == "work"
        })
    });
    control(&f, p, "cancel_add_hub");
    for (requested, url, expected) in [
        ("Office Mac", "https://office.example.com", "office-mac"),
        ("Work", "http://127.0.0.1:4291", "work"),
        ("", "https://work.example.com", "work-2"),
        ("", "https://personal.example.com", "personal-2"),
        ("", "http://192.0.2.10:4291", "remote"),
        ("", "http://192.168.1.10:4291", "lan"),
        ("", "http://10.1.2.3:4291", "lan-2"),
        ("", "http://172.31.0.1:4291", "lan-3"),
        ("", "http://100.100.0.1:4291", "tailscale"),
        ("", "  https://studio.example.com/  ", "studio"),
    ] {
        control(&f, p, "open_add_hub");
        f.send(
            p,
            json!({"op":"test_settings_input","id":"settings_hub_name","value":requested}),
        );
        f.send(
            p,
            json!({"op":"test_settings_input","id":"settings_hub_url","value":url}),
        );
        let before = f.frames_for(p, "setup_save_hub").count();
        control(&f, p, "submit_add_hub");
        wait(|| f.frames_for(p, "setup_save_hub").count() > before);
        let action = f.frames_for(p, "setup_save_hub").next_back().unwrap();
        assert_eq!(action["name"], expected);
        let canonical_url = url.trim().trim_end_matches('/');
        assert_eq!(action["url"], canonical_url);
        let saved = json!({"name":expected,"url":canonical_url,"auth_mode":action.get("auth_mode").cloned().unwrap_or(json!("none")),"enabled":true,"is_default":false,"status":"connected","user":"alice","error":null});
        let hubs = cfg["hubs"].as_array_mut().unwrap();
        if let Some(hub) = hubs.iter_mut().find(|hub| hub["name"] == expected) {
            *hub = saved;
        } else {
            hubs.push(saved);
        }
        f.send(p,json!({"op":"settings","ok":true,"source_op":"setup_save_hub","request_id":action["request_id"],"settings":cfg}));
        assert!(inspect(&f, p).settings("add_hub_form").is_null());
    }
    control(&f, p, "open_add_hub");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"settings_hub_name","value":"!!!"}),
    );
    f.send(p,json!({"op":"test_settings_input","id":"settings_hub_url","value":"https://hub.example.com"}));
    inspect(&f, p);
    let before = f.frames(p).len();
    control(&f, p, "submit_add_hub");
    let rejected = inspect(&f, p);
    assert_eq!(f.frames(p).len(), before);
    assert_eq!(
        rejected.settings("add_hub_error")["text"],
        "Hub name must contain a letter or number."
    );
    control(&f, p, "cancel_add_hub");
    control(&f, p, "open_uninstall_confirm");
    let modal = inspect(&f, p);
    assert!(modal.settings("confirm_uninstall_all").is_null());
    assert_eq!(
        modal.settings("confirm_disconnect_all")["text"],
        "Disconnect Apps"
    );
    control(&f, p, "confirm_disconnect_all");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "setup_disconnect_all" && v["purge_data"] == false)
    });
    control(&f, p, "open_uninstall_confirm");
    control(&f, p, "uninstall_purge_data");
    control(&f, p, "confirm_disconnect_all");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "setup_disconnect_all" && v["purge_data"] == true)
    });
    assert!(f.frames_for(p, "setup_uninstall_all").next().is_none());
    assert_eq!(inspect(&f, p)["settings_open"], true);
    control(&f, p, "close_settings");
    control(&f, p, "login:personal");
    control(&f, p, "reconnect:work");
    wait(|| f.frames(p).iter().any(|v| v["action"] == "switch_account"));
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["action"] == "reconnect" && v["hub"] == "work")
    });
    control(&f, p, "empty_open_channels");
    assert_eq!(inspect(&f, p)["settings_tab"], "channels");
    control(&f, p, "close_settings");
    control(&f, p, "open_directory");
    control(&f, p, "dir_mode:directory");
    let s = inspect(&f, p);
    assert_eq!(s.directory("directory_empty_state")["hidden"], false);
    assert!(!s.directory("dir_empty_open_apps").is_null());
    f.send(p, json!({"op":"test_directory_query","query":"missing"}));
    let s = inspect(&f, p);
    assert!(!s.directory("directory_reset_filters").is_null());
    control(&f, p, "directory_reset_filters");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "query_directory" && v["query"] == "")
    });
    control(&f, p, "dir_mode:activity");
    assert!(!inspect(&f, p)
        .directory("act_empty_open_channels")
        .is_null());
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"activity":[{"hub":"personal","signal_id":"read","listener_id":"me","from_user":"bob","from_host_name":"laptop","from_title":"Sender","target_harness":"human","target_title":"Me","target_scoped_id":"personal:me","message":"Done","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true}]}),
    );
    control(&f, p, "act_filter:unread");
    assert!(!inspect(&f, p).directory("act_empty_show_all").is_null());
    // An unread-only total counts only unread rows and can be zero despite read history.
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"activity":[],"activity_total":0,"activity_unread_count":0,"activity_filter":"unread"}),
    );
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"activity_filter":"unread"}),
    );
    assert!(!inspect(&f, p).directory("act_empty_show_all").is_null());
    control(&f, p, "act_empty_show_all");
    assert_eq!(inspect(&f, p)["activity_filter"], "all");
    wait(|| {
        f.frames(p).iter().rev().any(|v| {
            v["op"] == "query_directory" && v["mode"] == "activity" && v["filter"] == "all"
        })
    });
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"activity":[{"hub":"personal","signal_id":"read","listener_id":"me","from_user":"bob","from_host_name":"laptop","from_title":"Sender","target_harness":"human","target_title":"Me","target_scoped_id":"personal:me","message":"Done","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true}],"activity_filter":"all"}),
    );
    let summary = inspect(&f, p);
    assert!(nodes(&summary["directory_items"])
        .iter()
        .any(|n| n["text"] == "1 message · 0 unread"));
    assert_eq!(
        inspect(&f, p).directory("directory_empty_state")["hidden"],
        true
    );
    control(&f, p, "dir_mode:directory");
    assert_eq!(
        inspect(&f, p).directory("directory_empty_state")["hidden"],
        true
    );
}
#[test]
fn appkit_settings_three_tabs_secure_fields_dirty_only_drafts_and_uninstall_overlay() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let initial = state_with_settings(settings());
    f.send(p, initial);
    f.send(p, json!({"op":"test_settings_key","key":"command-comma"}));
    let s = inspect(&f, p);
    assert_eq!(s["settings_open"], true);
    assert_eq!(s["settings_tab"], "apps");
    for (id, configured) in [("pi", true), ("codex", false), ("cursor", false)] {
        assert!(!s.settings(&format!("settings_app:{id}")).is_null());
        let status = s.settings(&format!("settings_app_status:{id}"));
        assert_eq!(status["hidden"], false);
        if configured {
            assert_eq!(status["text"], "Connected");
        }
        assert_eq!(
            s.settings(&format!("setup_app:{id}"))["text"],
            if configured {
                "Check & Repair"
            } else {
                "Set Up"
            }
        );
        assert_eq!(
            s.settings(&format!("remove_app:{id}")).is_null(),
            !configured
        );
        control(&f, p, &format!("setup_app:{id}"));
        wait(|| {
            f.frames(p)
                .iter()
                .any(|v| v["op"] == "setup_apply" && v["harness"] == id)
        });
    }
    assert_eq!(
        inspect(&f, p).settings("setup_app:pi")["text"],
        "Checking…",
        "A click remains pending until its correlated receipt"
    );
    let checked = f
        .frames(p)
        .into_iter()
        .find(|v| v["op"] == "setup_apply" && v["harness"] == "pi")
        .unwrap();
    f.send(p,json!({"op":"settings","ok":true,"source_op":"setup_apply","request_id":checked["request_id"],"settings":settings()}));
    assert_eq!(inspect(&f, p).settings("setup_app:pi")["text"], "✓ Checked");
    assert!(!s.settings("settings_app_tier:cursor").is_null());
    assert!(s.settings("settings_app_tier:pi").is_null());
    control(&f, p, "remove_app:pi");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "setup_remove_app" && v["harness"] == "pi")
    });
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert_eq!(s["settings_open"], true);
    assert_eq!(s["settings_tab"], "channels");
    assert!(s.settings("channel_field:channel:secret").is_null());
    control(&f, p, "edit_hub_service:channel");
    let s = inspect(&f, p);
    assert_eq!(s.settings("channel_field:channel:secret")["secure"], true);
    assert_eq!(s.settings("channel_field:channel:secret")["text"], "••••");
    control(&f, p, "channel_hub_selector:work");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "open_settings" && v["hub"] == "work")
    });
    f.send(
        p,
        json!({"op":"settings","hub":"work","settings":settings()}),
    );
    control(&f, p, "edit_hub_service:channel");
    f.send(p,json!({"op":"test_settings_input","id":"channel_field:channel:secret","value":"new-secret"}));
    control(&f, p, "save_hub_service:channel");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "save_hub_service" && v["values"] == json!({"secret":"new-secret"}))
    });
    let mut paired = settings();
    paired["channels"][0]["fields"][1]["value"] = json!("server-account");
    f.send(p, json!({"op":"settings","hub":"work","settings":paired}));
    assert_eq!(
        inspect(&f, p).settings("channel_field:channel:account")["text"],
        "server-account"
    );
    control(&f, p, "save_channel:channel");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "channel_save" && v["values"] == json!({"account":"server-account"})
        })
    });
    control(&f, p, "open_uninstall_confirm");
    let s = inspect(&f, p);
    assert_eq!(s.settings("uninstall_confirm_box")["hidden"], false);
    assert_eq!(s.settings("settings_channels_tab")["hidden"], false);
    assert!(f.frames_for(p, "setup_uninstall_all").next().is_none());
    control(&f, p, "settings_tab:hubs");
    let modal = inspect(&f, p);
    assert_eq!(modal["settings_tab"], "channels");
    assert_eq!(modal.settings("settings_tab:hubs")["enabled"], false);
    assert!(!modal.settings("uninstall_confirm_box").is_null());
    control(&f, p, "cancel_uninstall_all");
    control(&f, p, "settings_tab:hubs");
    let s = inspect(&f, p);
    assert!(s.settings("uninstall_confirm_box").is_null());
    assert_eq!(s.settings("settings_hubs_tab")["hidden"], false);
    f.send(
        p,
        json!({"op":"test_clipboard","text":"stale cached invite"}),
    );
    f.send(p,json!({"op":"test_clipboard","system_only":true,"text":"cito setup --hub shared --url http://127.0.0.1:4291 --auth token --token test-token --default"}));
    control(&f, p, "paste_hub_invite");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "paste_hub_invite" && v["invite"] == "cito setup --hub shared --url http://127.0.0.1:4291 --auth token --token test-token --default"
        })
    });
    assert!(!f.state.join("onboarding-shown").exists());
}
#[test]
fn hosted_lan_invite_round_trips_through_desktop_paste_to_authenticated_hub() {
    let _lock = serial();
    let host = Fixture::new(json!({"host_hub":{"token":"lan-test-token"}}));
    host.daemon(&[]);
    host.action_ok(json!({"op":"set_host_hub","enabled":true,"port":0,"bind":"127.0.0.1"}));
    let (status, snapshot) = host.action(json!({"op":"get_snapshot"}));
    assert_eq!(status, 200);
    let hosted = &snapshot["settings"]["host_hub"];
    assert_eq!(hosted["running"], true);
    let port = u16::try_from(hosted["port"].as_u64().unwrap()).unwrap();
    assert_ne!(port, 0);
    let configured: Value = read_json(&host.config);
    assert_eq!(configured["host_hub"]["port"], 0);
    let endpoint = hosted["endpoint_url"].as_str().unwrap();
    assert_eq!(endpoint, format!("http://127.0.0.1:{port}"));
    assert_eq!(
        hosted["invite_command"],
        format!(
            "cito setup --hub remote --url {endpoint} --auth token --token lan-test-token --default"
        )
    );
    let page = ureq::get(endpoint)
        .timeout(std::time::Duration::from_secs(2))
        .call()
        .unwrap();
    assert_eq!(page.status(), 200);
    assert_eq!(
        page.header("Content-Type"),
        Some("text/html; charset=utf-8")
    );
    assert!(page.into_string().unwrap().contains("This Hub is running."));
    let anonymous = ureq::get(&format!("{endpoint}/v1/rpc"))
        .timeout(std::time::Duration::from_secs(2))
        .call();
    assert!(
        matches!(anonymous, Err(ureq::Error::Status(401, _))),
        "{endpoint}: {anonymous:?}"
    );

    let committed = fs::read(&host.config).unwrap();
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let occupied_port = occupied.local_addr().unwrap().port();
    for patch in [
        json!({"auth_mode":"invalid"}),
        json!({"auth_mode":"oidc"}),
        json!({"auth_mode":null}),
        json!({"auth_mode":1}),
        json!({"port":70000}),
        json!({"port":-1}),
        json!({"port":1.5}),
        json!({"port":"4291"}),
        json!({"port":null}),
        json!({"enabled":"true"}),
        json!({"enabled":null}),
        json!({"bind":""}),
        json!({"bind":" "}),
        json!({"bind":false}),
        json!({"bind":null}),
        json!({"bind":"127.0.0.1\u{0000}"}),
        json!({"bind":"invalid-bind-host"}),
        json!({"bind":"not:an:ip:address"}),
        json!({"port":occupied_port,"token":"rejected-token"}),
        json!({"token":42}),
        json!({"token":null}),
        json!({"token":" "}),
        json!({"enabled":false,"port":70000}),
        json!({"enabled":false,"auth_mode":"invalid"}),
    ] {
        let mut action = json!({"op":"set_host_hub","enabled":true});
        action
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let (status, error) = host.action(action.clone());
        assert_eq!(status, 400, "{action}: {error}");
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .starts_with("INVALID_INPUT:"),
            "{action}: {error}"
        );
        assert_eq!(fs::read(&host.config).unwrap(), committed, "{action}");
        let (status, health) = http(port, "/health", None);
        assert_eq!(status, 200, "{action}: {health}");
        assert_eq!(health["ok"], true);
        assert_eq!(health["auth_mode"], "token");
        let entries: Value = ureq::post(&format!("{endpoint}/v1/rpc"))
            .set("Authorization", "Bearer lan-test-token")
            .timeout(std::time::Duration::from_secs(2))
            .send_json(json!({"op":"kv_snapshot"}))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(entries["ok"], true, "{action}: {entries}");
        assert!(
            entries["result"]["entries"].is_array(),
            "{action}: {entries}"
        );
    }

    let client = Fixture::new(json!({}));
    client.daemon(&[]);
    let ui = start(&client);
    let initial = state_with_settings(settings());
    client.send(ui, initial);
    client.send(ui, json!({"op":"test_settings_key","key":"command-comma"}));
    control(&client, ui, "settings_tab:hubs");
    client.send(
        ui,
        json!({"op":"test_clipboard","text":hosted["invite_command"]}),
    );
    control(&client, ui, "paste_hub_invite");
    wait(|| client.frames_for(ui, "paste_hub_invite").next().is_some());
    let action = client.frames_for(ui, "paste_hub_invite").next().unwrap();
    assert_eq!(action["invite"], hosted["invite_command"]);
    let (status, result) = client.action(action.clone());
    assert_eq!(status, 200, "{result}");
    let snapshot = client.snapshot();
    client.send(ui, snapshot.clone());
    client.send(ui,json!({"op":"settings","ok":true,"source_op":"setup_save_hub","request_id":action["request_id"],"settings":snapshot["settings"]}));
    inspect(&client, ui);
    let found = client.tool(
        "cito_find",
        json!({"hub":"remote","user":"alice"}),
        json!({}),
    );
    assert_eq!(found["ok"], true, "{found}");
    let configured: Value = read_json(&client.config);
    assert_eq!(configured["default_hub"], "remote");
    assert_eq!(configured["hubs"]["remote"]["url"], endpoint);
    assert_eq!(configured["hubs"]["remote"]["authMode"], "token");
    assert_eq!(configured["hubs"]["remote"]["token"], "lan-test-token");
    // The host already enrolled this principal on its local Hub with workspace
    // scope. Joining through token auth preserves that existing Node scope.
    wait(|| {
        let snapshot = client.snapshot();
        ["local", "remote"].iter().all(|hub| {
            snapshot["listeners"].as_array().unwrap().iter().any(|row| {
                row["hub"] == *hub && row["harness"] == "human" && row["scope"] == "workspace"
            })
        })
    });
    let mut invite = configured["hubs"]["remote"].clone();
    invite["name"] = json!("remote");
    let prior = client.frames_for(ui, "paste_hub_invite").count();
    client.send(ui, json!({"op":"test_clipboard","text":invite.to_string()}));
    control(&client, ui, "paste_hub_invite");
    wait(|| client.frames_for(ui, "paste_hub_invite").count() > prior);
    let action = client
        .frames_for(ui, "paste_hub_invite")
        .next_back()
        .unwrap();
    let (status, result) = client.action(action);
    assert_eq!(status, 200, "{result}");
    invite.as_object_mut().unwrap().remove("name");
    let (status, result) =
        client.action(json!({"op":"paste_hub_invite","invite":invite.to_string()}));
    assert_eq!(status, 200, "{result}");

    // Inspect documentation-range binds without opening non-loopback test sockets.
    // Explicit no-auth must override a retained token.
    for (bind, authority) in [
        ("192.0.2.35", "192.0.2.35"),
        ("2001:db8::35", "[2001:db8::35]"),
    ] {
        host.action_ok(
            json!({"op":"set_host_hub","enabled":false,"port":port,"bind":bind,"auth_mode":"none"}),
        );
        let (_, snapshot) = host.action(json!({"op":"get_snapshot"}));
        assert_eq!(
            snapshot["settings"]["host_hub"]["endpoint_url"],
            format!("http://{authority}:{port}")
        );
        assert_eq!(
            snapshot["settings"]["host_hub"]["invite_command"],
            format!("cito setup --hub remote --url http://{authority}:{port} --default")
        );
    }
    for bind in ["0.0.0.0", "::"] {
        host.action_ok(json!({"op":"set_host_hub","enabled":true,"bind":bind}));
        let (_, snapshot) = host.action(json!({"op":"get_snapshot"}));
        assert_eq!(snapshot["settings"]["host_hub"]["running"], true);
        let url = url::Url::parse(
            snapshot["settings"]["host_hub"]["endpoint_url"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert!(url
            .host_str()
            .is_some_and(|host| !host.is_empty() && host != "0.0.0.0" && host != "[::]"));
        assert_eq!(url.port(), Some(port));
    }
}
