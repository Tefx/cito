use super::*;

#[test]
fn host_hub_failed_projection_and_trailing_error_have_one_error_surface() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["host_hub"]["enabled"] = json!(true);
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"4292"}),
    );
    control(&f, p, "apply_host_hub_port");
    inspect(&f, p);
    let action = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "set_host_hub")
        .unwrap();
    cfg["host_hub"]["error"] = json!("Cannot start sharing: Address already in use");
    cfg["host_hub"]["running"] = json!(false);
    f.send(p, json!({"op":"settings","source_op":"set_host_hub","request_id":action["request_id"],"ok":false,"error":"Cannot start sharing: Address already in use","settings":cfg}));
    f.send(p, json!({"op":"error","source_op":"set_host_hub","request_id":action["request_id"],"error":"Cannot start sharing: Address already in use"}));
    let s = inspect(&f, p);
    assert_eq!(s["host_hub_port_pending"], false);
    assert_eq!(s["pending_controls"], json!([]));
    assert_eq!(
        node(&s, "settings_items", "host_hub_error")["text"],
        "Cannot start sharing: Address already in use"
    );
    assert!(node(&s, "settings_items", "settings_error_banner").is_null());
    assert_eq!(node(&s, "settings_items", "host_hub_port")["text"], "4292");
}

#[test]
fn hub_row_mutations_disable_duplicate_submission_and_settle_correlated_receipts() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
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
        assert_eq!(node(&pending, "settings_items", id)["enabled"], false);
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
        assert_eq!(
            node(&inspect(&f, p), "settings_items", id)["enabled"],
            false
        );
        f.send(p, json!({"op":"state","source_op":op,"request_id":action["request_id"],"ok":true,"committed":true,"warning":"Saved; refresh unavailable"}));
        let settled = inspect(&f, p);
        assert_eq!(node(&settled, "settings_items", id)["enabled"], true);
        assert_eq!(settled["pending_controls"], json!([]));
        assert_eq!(
            node(&settled, "settings_items", "settings_error_label")["text"],
            "Saved; refresh unavailable"
        );
    }
}

#[test]
fn host_toggle_failures_settle_and_redraw_the_settings_banner() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    frame["settings"]["host_hub"]["enabled"] = json!(true);
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    for id in ["toggle_host_hub", "toggle_host_hub_auth"] {
        control(&f, p, id);
        inspect(&f, p);
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "set_host_hub")
            .unwrap();
        assert_eq!(
            node(&inspect(&f, p), "settings_items", id)["enabled"],
            false
        );
        f.send(p, json!({"op":"error","source_op":"set_host_hub","request_id":request["request_id"],"error":"Sharing failed"}));
        let s = inspect(&f, p);
        assert_eq!(s["pending_controls"], json!([]));
        assert_eq!(node(&s, "settings_items", id)["enabled"], true);
        assert_eq!(
            node(&s, "settings_items", "settings_error_label")["text"],
            "Sharing failed"
        );
        control(&f, p, "dismiss_settings_error");
    }
}

#[test]
fn clipboard_join_errors_stay_outside_an_idle_add_hub_form() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    f.send(
        p,
        json!({"op":"test_clipboard","text":"https://hub.example.com"}),
    );
    control(&f, p, "paste_hub_invite");
    inspect(&f, p);
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "paste_hub_invite")
        .unwrap();
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
        assert!(node(&s, "settings_items", "add_hub_error").is_null());
        assert_eq!(
            node(&s, "settings_items", "settings_error_label")["text"],
            "Join failed"
        );
    }
}

#[test]
fn superseded_invite_parse_errors_preserve_the_current_draft_and_pending_requests() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    for replacement in [
        "https://corrected.example.com",
        "cito setup --hub corrected --url https://corrected.example.com",
    ] {
        f.send(p,json!({"op":"test_settings_input","id":"settings_hub_url","value":"cito setup --hub stale --url https://stale.example.com"}));
        inspect(&f, p);
        let stale = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "paste_hub_invite")
            .unwrap();
        assert_eq!(stale["parse_only"], true);
        f.send(
            p,
            json!({"op":"test_settings_input","id":"settings_hub_url","value":replacement}),
        );
        let before = inspect(&f, p);
        let current = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "paste_hub_invite")
            .unwrap();
        for request in [stale["request_id"].clone(), json!("")] {
            f.send(p,json!({"op":"error","source_op":"paste_hub_invite","request_id":request,"error":"Delayed invalid invite"}));
            let after = inspect(&f, p);
            for key in ["pending_hub_save", "pending_controls", "hub_invite_pending"] {
                assert_eq!(after[key], before[key], "{key}");
            }
            for id in ["settings_hub_name", "settings_hub_url"] {
                assert_eq!(
                    node(&after, "settings_items", id)["text"],
                    node(&before, "settings_items", id)["text"]
                );
            }
            assert!(node(&after, "settings_items", "settings_error_banner").is_null());
            assert!(node(&after, "settings_items", "add_hub_error").is_null());
        }
        if replacement.starts_with("cito setup ") {
            assert_ne!(current["request_id"], stale["request_id"]);
            // Acceptance of B proves A's error did not clear the newer parse.
            f.send(p,json!({"op":"hub_invite","request_id":current["request_id"],"config":{"name":"corrected","url":"https://corrected.example.com"}}));
            let parsed = inspect(&f, p);
            assert_eq!(
                node(&parsed, "settings_items", "settings_hub_name")["text"],
                "corrected"
            );
            assert_eq!(
                node(&parsed, "settings_items", "settings_hub_url")["text"],
                "https://corrected.example.com"
            );
        }
    }
}

#[test]
fn v41_start_sharing_and_auth_validate_ports_without_blocking_stop() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    let mut frame = state();
    frame["settings"] = cfg.clone();
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
        assert!(
            node(&inspect(&f, p), "settings_items", "host_hub_error")["text"]
                .as_str()
                .unwrap()
                .contains("1 to 65535")
        );
        cfg["host_hub"]["enabled"] = json!(false);
        f.send(p, json!({"op":"settings","settings":cfg}));
        inspect(&f, p);
        control(&f, p, "toggle_host_hub");
        inspect(&f, p);
        assert!(!f.frames(p).iter().any(|a| a["op"] == "set_host_hub"));
    }
    cfg["host_hub"]["enabled"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    inspect(&f, p);
    control(&f, p, "toggle_host_hub");
    inspect(&f, p);
    let stopped = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "set_host_hub")
        .unwrap();
    assert_eq!(stopped["enabled"], false);
    assert_eq!(stopped["port"], 4291);
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"65535"}),
    );
    control(&f, p, "toggle_host_hub_auth");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "set_host_hub")
            .unwrap()["port"],
        65535
    );
}

#[test]
fn v49_hub_save_settles_by_request_even_without_a_matching_projected_name() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    let mut frame = state();
    frame["settings"] = cfg.clone();
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
    inspect(&f, p);
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "setup_save_hub")
        .unwrap();
    f.send(p,json!({"op":"settings","ok":true,"source_op":"setup_save_hub","request_id":request["request_id"],"settings":cfg}));
    assert!(
        node(&inspect(&f, p), "settings_items", "add_hub_form").is_null(),
        "A matching explicit success settles even when its projection omits the saved Hub"
    );
    cfg["hubs"].as_array_mut().unwrap().push(json!({"name":"shared","url":"https://example.com/","auth_mode":"none","enabled":true,"is_default":false,"status":"connected"}));
    f.send(p,json!({"op":"settings","ok":true,"source_op":"setup_save_hub","request_id":request["request_id"],"settings":cfg}));
    assert!(node(&inspect(&f, p), "settings_items", "add_hub_form").is_null());
}

#[test]
fn v39_invite_parse_receipts_stay_in_the_form_and_modal_return_cannot_submit() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    let latest = |op: &str| {
        f.frames(p)
            .into_iter()
            .rev()
            .find(|v| v["op"] == op)
            .unwrap()
    };
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
    assert!(node(&s, "settings_items", "settings_error_banner").is_null());
    assert!(node(&s, "settings_items", "add_hub_error").is_null());
    let stale = paste();
    control(&f, p, "cancel_add_hub");
    control(&f, p, "open_add_hub");
    f.send(p,json!({"op":"hub_invite","request_id":stale["request_id"],"config":{"name":"stale","url":"https://stale.example.com"}}));
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "settings_hub_url")["text"],
        ""
    );
    f.send(p, json!({"op":"test_settings_key","key":"return"}));
    assert!(!node(&inspect(&f, p), "settings_items", "add_hub_error").is_null());
    let request = paste();
    f.send(p,json!({"op":"hub_invite","request_id":request["request_id"],"config":{"name":"remote","url":"https://hub.example.com"}}));
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "add_hub_error").is_null());
    assert_eq!(
        node(&s, "settings_items", "settings_hub_url")["text"],
        "https://hub.example.com"
    );
    f.send(p,json!({"op":"hub_invite","request_id":"","config":{"name":"stale","url":"https://stale.example.com"}}));
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "settings_hub_url")["text"],
        "https://hub.example.com"
    );
    control(&f, p, "open_uninstall_confirm");
    let before = f
        .frames(p)
        .iter()
        .filter(|v| v["op"] == "setup_save_hub")
        .count();
    f.send(p, json!({"op":"test_settings_key","key":"return"}));
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "setup_save_hub")
            .count(),
        before
    );
    control(&f, p, "cancel_uninstall_all");
    f.send(p, json!({"op":"test_focus_field","id":"settings_hub_url"}));
    f.send(p, json!({"op":"test_settings_key","key":"return"}));
    inspect(&f, p);
    assert_eq!(latest("setup_save_hub")["url"], "https://hub.example.com");
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "setup_save_hub")
            .count(),
        before + 1
    );
}

#[test]
fn r1_add_hub_acknowledgement_settles_submission_without_discarding_newer_drafts() {
    let _lock = serial();
    for (field, newer) in [
        ("settings_hub_name", "newer"),
        ("settings_hub_url", "http://127.0.0.1:4291"),
        ("settings_hub_token", "newer-token"),
        ("settings_hub_user", "bob"),
        ("settings_hub_auth:none", "none"),
    ] {
        let f = Fixture::new(json!({}));
        let p = start(&f);
        let mut cfg = settings();
        let mut frame = state();
        frame["settings"] = cfg.clone();
        f.send(p, frame);
        control(&f, p, "open_settings");
        control(&f, p, "settings_tab:hubs");
        control(&f, p, "open_add_hub");
        control(&f, p, "settings_hub_auth:token");
        for (id, value) in [
            ("settings_hub_name", "submitted"),
            ("settings_hub_url", "https://submitted.example.com"),
            ("settings_hub_token", "submitted-token"),
        ] {
            f.send(p, json!({"op":"test_settings_input","id":id,"value":value}));
        }
        control(&f, p, "submit_add_hub");
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "submit_add_hub")["enabled"],
            false
        );
        let submitted = f
            .frames(p)
            .into_iter()
            .find(|a| a["op"] == "setup_save_hub")
            .unwrap();
        assert_eq!(submitted["name"], "submitted");
        assert_eq!(submitted["url"], "https://submitted.example.com");
        assert!(!submitted["request_id"].as_str().unwrap().is_empty());
        cfg["hubs"].as_array_mut().unwrap().push(json!({"name":"submitted","url":"https://submitted.example.com","auth_mode":"token","is_builtin":false,"enabled":true,"is_default":false,"status":"connected","user":"alice","error":null}));
        f.send(p, json!({"op":"settings","ok":true,"source_op":"setup_save_hub","request_id":null,"settings":cfg}));
        let s = inspect(&f, p);
        assert!(
            !node(&s, "settings_items", "add_hub_form").is_null(),
            "An untagged acknowledgement must not close the active submission"
        );
        assert_eq!(
            node(&s, "settings_items", "submit_add_hub")["enabled"],
            false,
            "An untagged acknowledgement must not settle the active submission"
        );
        if field.starts_with("settings_hub_auth:") {
            control(&f, p, field);
        } else {
            f.send(
                p,
                json!({"op":"test_settings_input","id":field,"value":newer}),
            );
        }
        f.send(p, json!({"op":"settings","ok":true,"source_op":"setup_save_hub","request_id":submitted["request_id"],"settings":cfg}));
        let s = inspect(&f, p);
        assert!(!node(&s, "settings_items", "add_hub_form").is_null());
        assert_eq!(
            node(&s, "settings_items", "submit_add_hub")["enabled"],
            true
        );
        if field != "settings_hub_token" && !field.starts_with("settings_hub_auth:") {
            assert_eq!(node(&s, "settings_items", field)["text"], newer);
        }
        control(&f, p, "submit_add_hub");
        inspect(&f, p);
        let resubmitted = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "setup_save_hub")
            .unwrap();
        let key = if field.starts_with("settings_hub_auth:") {
            "auth_mode"
        } else {
            field.strip_prefix("settings_hub_").unwrap()
        };
        assert_eq!(resubmitted[key], newer, "Preserve the newer {field} draft");
        if field != "settings_hub_name" {
            assert_eq!(resubmitted["edit"], true);
            assert_eq!(resubmitted["original_name"], "submitted");
        }
    }

    for retry_after_error in [false, true] {
        let f = Fixture::new(json!({}));
        let p = start(&f);
        let mut cfg = settings();
        let mut frame = state();
        frame["settings"] = cfg.clone();
        f.send(p, frame);
        control(&f, p, "open_settings");
        control(&f, p, "settings_tab:hubs");
        let mut submissions = Vec::new();
        for token in ["cancelled-token", "current-token"] {
            control(&f, p, "open_add_hub");
            control(&f, p, "settings_hub_auth:token");
            for (id, value) in [
                ("settings_hub_name", "submitted"),
                ("settings_hub_url", "https://submitted.example.com"),
                ("settings_hub_token", token),
            ] {
                f.send(p, json!({"op":"test_settings_input","id":id,"value":value}));
            }
            control(&f, p, "submit_add_hub");
            inspect(&f, p);
            let action = f
                .frames(p)
                .into_iter()
                .rev()
                .find(|a| a["op"] == "setup_save_hub")
                .unwrap();
            assert!(!action["request_id"].as_str().unwrap().is_empty());
            assert_eq!(action["token"], token);
            submissions.push(action);
            if token == "cancelled-token" {
                control(&f, p, "cancel_add_hub");
            }
        }
        let cancelled = &submissions[0];
        let mut current = submissions[1].clone();
        assert_ne!(cancelled["request_id"], current["request_id"]);
        cfg["hubs"].as_array_mut().unwrap().push(json!({"name":"submitted","url":"https://submitted.example.com","auth_mode":"token","is_builtin":false,"enabled":true,"is_default":false,"status":"connected","user":"alice","error":null}));
        f.send(p, json!({"op":"settings","ok":true,"source_op":"setup_save_hub","request_id":cancelled["request_id"],"settings":cfg}));
        let s = inspect(&f, p);
        assert!(!node(&s, "settings_items", "add_hub_form").is_null());
        assert_eq!(
            node(&s, "settings_items", "submit_add_hub")["enabled"],
            false
        );
        for op in ["setup_save_hub", "save_hub"] {
            f.send(p, json!({"op":"error","source_op":op,"request_id":cancelled["request_id"],"error":"Cancelled submission failed"}));
            let s = inspect(&f, p);
            assert!(!node(&s, "settings_items", "add_hub_form").is_null());
            assert_eq!(
                node(&s, "settings_items", "submit_add_hub")["enabled"],
                false
            );
            assert!(node(&s, "settings_items", "add_hub_error").is_null());
        }
        for frame in [
            json!({"op":"error","source_op":"hub_action","error":"OIDC cancelled"}),
            json!({"op":"error","source_op":"setup_save_hub","error":"Invite failed"}),
            json!({"op":"error","source_op":"paste_hub_invite","error":"Invalid invite"}),
            json!({"op":"error","error":"INVALID_INPUT: Message is too large to send"}),
            json!({"op":"error","source_op":"","request_id":"unrelated-request","error":"Source-less uncorrelated transport failure"}),
        ] {
            let error = frame["error"].clone();
            let source_less = frame.get("source_op").is_none();
            if source_less {
                control(&f, p, "close_settings");
                assert_eq!(inspect(&f, p)["settings_open"], false);
            }
            f.send(p, frame);
            let s = inspect(&f, p);
            assert!(!node(&s, "settings_items", "add_hub_form").is_null());
            assert_eq!(
                node(&s, "settings_items", "submit_add_hub")["enabled"],
                false,
                "An unrelated error must not settle the active submission: {error}"
            );
            assert!(node(&s, "settings_items", "add_hub_error").is_null());
            assert!(!node(&s, "settings_items", "settings_error_banner").is_null());
            assert_eq!(
                node(&s, "settings_items", "settings_error_label")["text"],
                error
                    .as_str()
                    .unwrap()
                    .strip_prefix("INVALID_INPUT: ")
                    .unwrap_or(error.as_str().unwrap())
            );
            if source_less {
                assert!(node(&s, "items", "menu_error_row").is_null());
                control(&f, p, "open_settings");
            }
        }
        if retry_after_error {
            f.send(p, json!({"op":"error","source_op":"setup_save_hub","request_id":current["request_id"],"error":"Current submission failed"}));
            let s = inspect(&f, p);
            assert_eq!(
                node(&s, "settings_items", "submit_add_hub")["enabled"],
                true
            );
            assert_eq!(
                node(&s, "settings_items", "add_hub_error")["text"],
                "Current submission failed"
            );
            control(&f, p, "submit_add_hub");
            inspect(&f, p);
            let retry = f
                .frames(p)
                .into_iter()
                .rev()
                .find(|a| a["op"] == "setup_save_hub")
                .unwrap();
            assert_eq!(
                retry["token"], "current-token",
                "Stale responses must preserve the newer secret draft"
            );
            assert_ne!(retry["request_id"], current["request_id"]);
            current = retry;
        }
        f.send(p, json!({"op":"settings","ok":true,"source_op":"setup_save_hub","request_id":current["request_id"],"settings":cfg}));
        let s = inspect(&f, p);
        assert!(node(&s, "settings_items", "add_hub_form").is_null());
        assert!(!node(&s, "settings_items", "settings_hub:submitted").is_null());
    }
}

#[test]
fn d12_onboarding_uses_inline_guidance_without_redundant_controls() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    for app in cfg["apps"].as_array_mut().unwrap() {
        app["configured"] = json!(false);
    }
    let mut frame = state();
    frame["settings"] = cfg;
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
            assert!(node(&s, "settings_items", id).is_null());
        }
    }
    control(&f, p, "settings_tab:channels");
    control(&f, p, "channel_hub_selector:work");
    let mut disabled = settings();
    disabled["hubs"][1]["enabled"] = json!(false);
    disabled["settings_hub"] = json!("personal");
    f.send(p, json!({"op":"settings","settings":disabled}));
    let channels = inspect(&f, p);
    assert!(node(&channels, "settings_items", "channel_hub_selector:work").is_null());
    assert!(!node(&channels, "settings_items", "stratum_user:channel").is_null());
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "open_settings" && v["hub"] == "personal")
    });
}

#[test]
fn lifecycle_onboarding_hub_edit_share_controls_disconnect_and_empty_actions() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
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
    assert_eq!(
        node(&s, "settings_items", "setup_app:codex")["text"],
        "Repair"
    );
    control(&f, p, "settings_tab:channels");
    assert_eq!(inspect(&f, p)["settings_tab"], "channels");
    control(&f, p, "settings_tab:hubs");
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "host_hub_error")["text"],
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
        node(&s, "settings_items", "settings_hub_issuer")["text"],
        "https://id.example.com"
    );
    assert_eq!(
        node(&s, "settings_items", "submit_add_hub")["text"],
        "Save Hub"
    );
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
        let before = f
            .frames(p)
            .iter()
            .filter(|v| v["op"] == "setup_save_hub")
            .count();
        control(&f, p, "submit_add_hub");
        wait(|| {
            f.frames(p)
                .iter()
                .filter(|v| v["op"] == "setup_save_hub")
                .count()
                > before
        });
        let action = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|v| v["op"] == "setup_save_hub")
            .unwrap();
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
        assert!(node(&inspect(&f, p), "settings_items", "add_hub_form").is_null());
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
        node(&rejected, "settings_items", "add_hub_error")["text"],
        "Hub name must contain a letter or number."
    );
    control(&f, p, "cancel_add_hub");
    control(&f, p, "open_uninstall_confirm");
    let modal = inspect(&f, p);
    assert!(node(&modal, "settings_items", "confirm_uninstall_all").is_null());
    assert_eq!(
        node(&modal, "settings_items", "confirm_disconnect_all")["text"],
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
    assert!(!f.frames(p).iter().any(|v| v["op"] == "setup_uninstall_all"));
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
    assert_eq!(
        node(&s, "directory_items", "directory_empty_state")["hidden"],
        false
    );
    assert!(!node(&s, "directory_items", "dir_empty_open_apps").is_null());
    f.send(p, json!({"op":"test_directory_query","query":"missing"}));
    let s = inspect(&f, p);
    assert!(!node(&s, "directory_items", "directory_reset_filters").is_null());
    control(&f, p, "directory_reset_filters");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "query_directory" && v["query"] == "")
    });
    control(&f, p, "dir_mode:activity");
    assert!(!node(
        &inspect(&f, p),
        "directory_items",
        "act_empty_open_channels"
    )
    .is_null());
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"activity":[{"hub":"personal","signal_id":"read","listener_id":"me","from_user":"bob","from_host_name":"laptop","from_title":"Sender","target_harness":"human","target_title":"Me","target_scoped_id":"personal:me","message":"Done","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true}]}),
    );
    control(&f, p, "act_filter:unread");
    assert!(!node(&inspect(&f, p), "directory_items", "act_empty_show_all").is_null());
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
    assert!(!node(&inspect(&f, p), "directory_items", "act_empty_show_all").is_null());
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
        node(&inspect(&f, p), "directory_items", "directory_empty_state")["hidden"],
        true
    );
    control(&f, p, "dir_mode:directory");
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_empty_state")["hidden"],
        true
    );
}
#[test]
fn appkit_settings_three_tabs_secure_fields_dirty_only_drafts_and_uninstall_overlay() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut initial = state();
    initial["settings"] = settings();
    f.send(p, initial);
    f.send(p, json!({"op":"test_settings_key","key":"command-comma"}));
    let s = inspect(&f, p);
    assert_eq!(s["settings_open"], true);
    assert_eq!(s["settings_tab"], "apps");
    for (id, configured) in [("pi", true), ("codex", false), ("cursor", false)] {
        assert!(!node(&s, "settings_items", &format!("settings_app:{id}")).is_null());
        let status = node(&s, "settings_items", &format!("settings_app_status:{id}"));
        assert_eq!(status["hidden"], false);
        if configured {
            assert_eq!(status["text"], "Connected");
        }
        assert_eq!(
            node(&s, "settings_items", &format!("setup_app:{id}"))["text"],
            if configured {
                "Check & Repair"
            } else {
                "Set Up"
            }
        );
        assert_eq!(
            node(&s, "settings_items", &format!("remove_app:{id}")).is_null(),
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
        node(&inspect(&f, p), "settings_items", "setup_app:pi")["text"],
        "Checking…",
        "A click remains pending until its correlated receipt"
    );
    let checked = f
        .frames(p)
        .into_iter()
        .find(|v| v["op"] == "setup_apply" && v["harness"] == "pi")
        .unwrap();
    f.send(p,json!({"op":"settings","ok":true,"source_op":"setup_apply","request_id":checked["request_id"],"settings":settings()}));
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "setup_app:pi")["text"],
        "✓ Checked"
    );
    assert!(!node(&s, "settings_items", "settings_app_tier:cursor").is_null());
    assert!(node(&s, "settings_items", "settings_app_tier:pi").is_null());
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
    assert!(node(&s, "settings_items", "channel_field:channel:secret").is_null());
    control(&f, p, "edit_hub_service:channel");
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:secret")["secure"],
        true
    );
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:secret")["text"],
        "••••"
    );
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
        node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:account"
        )["text"],
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
    assert_eq!(
        node(&s, "settings_items", "uninstall_confirm_box")["hidden"],
        false
    );
    assert_eq!(
        node(&s, "settings_items", "settings_channels_tab")["hidden"],
        false
    );
    assert!(!f.frames(p).iter().any(|v| v["op"] == "setup_uninstall_all"));
    control(&f, p, "settings_tab:hubs");
    let modal = inspect(&f, p);
    assert_eq!(modal["settings_tab"], "channels");
    assert_eq!(
        node(&modal, "settings_items", "settings_tab:hubs")["enabled"],
        false
    );
    assert!(!node(&modal, "settings_items", "uninstall_confirm_box").is_null());
    control(&f, p, "cancel_uninstall_all");
    control(&f, p, "settings_tab:hubs");
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "uninstall_confirm_box").is_null());
    assert_eq!(
        node(&s, "settings_items", "settings_hubs_tab")["hidden"],
        false
    );
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
    assert_eq!(
        host.action(json!({"op":"set_host_hub","enabled":true,"port":0,"bind":"127.0.0.1"}))
            .0,
        200
    );
    let (status, snapshot) = host.action(json!({"op":"get_snapshot"}));
    assert_eq!(status, 200);
    let hosted = &snapshot["settings"]["host_hub"];
    assert_eq!(hosted["running"], true);
    let port = u16::try_from(hosted["port"].as_u64().unwrap()).unwrap();
    assert_ne!(port, 0);
    let configured: Value =
        serde_json::from_str(&fs::read_to_string(&host.config).unwrap()).unwrap();
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
    let mut initial = state();
    initial["settings"] = settings();
    client.send(ui, initial);
    client.send(ui, json!({"op":"test_settings_key","key":"command-comma"}));
    control(&client, ui, "settings_tab:hubs");
    client.send(
        ui,
        json!({"op":"test_clipboard","text":hosted["invite_command"]}),
    );
    control(&client, ui, "paste_hub_invite");
    wait(|| {
        client
            .frames(ui)
            .iter()
            .any(|v| v["op"] == "paste_hub_invite")
    });
    let action = client
        .frames(ui)
        .into_iter()
        .find(|v| v["op"] == "paste_hub_invite")
        .unwrap();
    assert_eq!(action["invite"], hosted["invite_command"]);
    let (status, result) = client.action(action.clone());
    assert_eq!(status, 200, "{result}");
    let snapshot = client.action(json!({"op":"get_snapshot"})).1;
    client.send(ui, snapshot.clone());
    client.send(ui,json!({"op":"settings","ok":true,"source_op":"setup_save_hub","request_id":action["request_id"],"settings":snapshot["settings"]}));
    inspect(&client, ui);
    let (status, result) = client.action(json!({"op":"test_hub","hub":"remote"}));
    assert_eq!(status, 200, "{result}");
    assert!(result["streams"].is_number());
    let configured: Value =
        serde_json::from_str(&fs::read_to_string(&client.config).unwrap()).unwrap();
    assert_eq!(configured["default_hub"], "remote");
    assert_eq!(configured["hubs"]["remote"]["url"], endpoint);
    assert_eq!(configured["hubs"]["remote"]["authMode"], "token");
    assert_eq!(configured["hubs"]["remote"]["token"], "lan-test-token");
    // The host already enrolled this principal on its local Hub with workspace
    // scope. Joining through token auth preserves that existing Node scope.
    wait(|| {
        let snapshot = client.action(json!({"op":"get_snapshot"})).1;
        ["local", "remote"].iter().all(|hub| {
            snapshot["listeners"].as_array().unwrap().iter().any(|row| {
                row["hub"] == *hub && row["harness"] == "human" && row["scope"] == "workspace"
            })
        })
    });
    let (status, exported) = client.action(json!({"op":"generate_hub_invite","hub":"remote"}));
    assert_eq!(status, 200);
    assert_eq!(exported["invite"]["url"], endpoint);
    assert_eq!(exported["invite"]["authMode"], "token");
    assert_eq!(exported["invite"]["token"], "lan-test-token");
    assert_eq!(exported["invite"]["name"], "remote");
    let prior = client
        .frames(ui)
        .iter()
        .filter(|v| v["op"] == "paste_hub_invite")
        .count();
    client.send(
        ui,
        json!({"op":"test_clipboard","text":exported["invite"].to_string()}),
    );
    control(&client, ui, "paste_hub_invite");
    wait(|| {
        client
            .frames(ui)
            .iter()
            .filter(|v| v["op"] == "paste_hub_invite")
            .count()
            > prior
    });
    let action = client
        .frames(ui)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "paste_hub_invite")
        .unwrap();
    let (status, result) = client.action(action);
    assert_eq!(status, 200, "{result}");
    let mut unnamed = exported["invite"].clone();
    unnamed.as_object_mut().unwrap().remove("name");
    let (status, result) =
        client.action(json!({"op":"paste_hub_invite","invite":unnamed.to_string()}));
    assert_eq!(status, 200, "{result}");

    // Inspect documentation-range binds without opening non-loopback test sockets.
    // Explicit no-auth must override a retained token.
    for (bind, authority) in [
        ("192.0.2.35", "192.0.2.35"),
        ("2001:db8::35", "[2001:db8::35]"),
    ] {
        assert_eq!(host.action(json!({"op":"set_host_hub","enabled":false,"port":port,"bind":bind,"auth_mode":"none"})).0, 200);
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
        assert_eq!(
            host.action(json!({"op":"set_host_hub","enabled":true,"bind":bind}))
                .0,
            200
        );
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
