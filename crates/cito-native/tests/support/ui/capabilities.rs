use super::*;

fn channels(f: &Fixture, ui: usize, cfg: &Value) -> Value {
    f.send(ui, json!({"op":"settings","settings":cfg}));
    control(f, ui, "settings_tab:channels");
    inspect(f, ui)
}

#[test]
fn v48_non_admin_channel_authority_and_error_matrix() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let ui = start(&f);
    f.send(ui, state());
    control(&f, ui, "open_settings");
    let mut cfg = settings();
    cfg["channels"][0]["can_configure_hub"] = json!(false);
    cfg["channels"][0]["can_clear_hub"] = json!(false);
    cfg["channels"][0]["hub_ready"] = json!(false);
    cfg["channels"][0]["status"] = json!("not_configured");
    cfg["channels"][0]["fields"][0]["hidden"] = json!(true);
    let hidden = channels(&f, ui, &cfg);
    assert!(node(&hidden, "settings_items", "settings_channel:channel").is_null());
    assert_eq!(
        node(&hidden, "settings_items", "channels_managed_notice")["text"],
        "External channels are managed by your 'personal' Hub administrator. None are enabled yet."
    );
    cfg["channel_inventory_error"] = json!("Service unavailable");
    assert!(node(
        &channels(&f, ui, &cfg),
        "settings_items",
        "channels_managed_notice"
    )
    .is_null());
    cfg["channel_inventory_error"] = Value::Null;
    cfg["channels"][0]["hub_ready"] = json!(true);
    for status in ["hub_ready", "ready", "paused"] {
        cfg["channels"][0]["status"] = json!(status);
        cfg["channels"][0]["configured"] = json!(status != "hub_ready");
        cfg["channels"][0]["fields"][1]["value"] = json!("room");
        let s = channels(&f, ui, &cfg);
        assert!(node(&s, "settings_items", "stratum_hub:channel").is_null());
        assert!(node(&s, "settings_items", "edit_hub_service:channel").is_null());
        if status == "hub_ready" {
            assert_eq!(
                node(&s, "settings_items", "stratum_user_title:channel")["text"],
                "Your Personal Account (@alice)"
            );
        }
    }
    cfg["channels"][0]["status"] = json!("error");
    cfg["channels"][0]["error_origin"] = json!("hub");
    let readonly = channels(&f, ui, &cfg);
    assert!(!node(&readonly, "settings_items", "stratum_hub:channel").is_null());
    assert!(node(&readonly, "settings_items", "edit_hub_service:channel").is_null());
    assert!(node(&readonly, "settings_items", "clear_channel_service:channel").is_null());
    cfg["channels"][0]["fields"] = json!([]);
    for paired in [false, true] {
        cfg["channels"][0]["configured"] = json!(paired);
        cfg["channels"][0]["can_test"] = json!(paired);
        cfg["channels"][0]["can_unpair"] = json!(paired);
        let s = channels(&f, ui, &cfg);
        let ids: Vec<_> = nodes(&s["settings_items"])
            .into_iter()
            .filter_map(|n| {
                n["id"]
                    .as_str()
                    .filter(|id| id.starts_with("recover_channel:"))
                    .map(str::to_owned)
            })
            .collect();
        assert_eq!(ids.len(), if paired { 3 } else { 1 });
        assert!(ids.contains(&"recover_channel:channel:retry".into()));
        assert_eq!(ids.contains(&"recover_channel:channel:test".into()), paired);
        assert_eq!(
            ids.contains(&"recover_channel:channel:unpair".into()),
            paired
        );
    }
    cfg["channels"][0]["can_configure_hub"] = json!(true);
    cfg["channels"][0]["can_clear_hub"] = json!(true);
    let admin = channels(&f, ui, &cfg);
    assert!(!node(&admin, "settings_items", "recover_channel:channel:change").is_null());
    control(&f, ui, "recover_channel:channel:clear");
    inspect(&f, ui);
    let action = f
        .frames(ui)
        .into_iter()
        .find(|a| a["op"] == "disconnect_human_channel")
        .unwrap();
    assert_eq!(action["reset_hub"], true);
    assert_eq!(action["reset_user"], false);
}

#[test]
fn v48_personal_form_requires_every_required_value_and_explicit_edit_authority() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let ui = start(&f);
    f.send(ui, state());
    control(&f, ui, "open_settings");
    let mut cfg = settings();
    cfg["channels"][0]["has_hub_service"] = json!(false);
    let mut second = cfg["channels"][0]["fields"][1].clone();
    second["key"] = json!("destination");
    second["label"] = json!("Destination");
    cfg["channels"][0]["fields"] = json!([cfg["channels"][0]["fields"][1], second]);
    let s = channels(&f, ui, &cfg);
    assert!(node(&s, "settings_items", "stratum_hub:channel").is_null());
    for (account, destination, enabled) in [
        ("42", "", false),
        ("42", "room", true),
        (" ", "room", false),
    ] {
        for (key, value) in [("account", account), ("destination", destination)] {
            f.send(ui, json!({"op":"test_settings_input","id":format!("channel_field:channel:{key}"),"value":value}));
        }
        assert_eq!(
            node(&inspect(&f, ui), "settings_items", "test_channel:channel")["enabled"],
            enabled
        );
    }
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["can_edit_personal"] = json!(false);
    let s = channels(&f, ui, &cfg);
    assert!(
        node(&s, "settings_items", "edit_user_channel:channel")["hidden"] == true
            || node(&s, "settings_items", "edit_user_channel:channel").is_null()
    );
    assert!(node(&s, "settings_items", "save_channel:channel").is_null());
}

#[test]
fn v48_team_controls_use_authoritative_capabilities_and_seed_identity() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let ui = start(&f);
    let mut frame = state();
    frame["hubs"][0]["auto_listen"] = json!(true);
    frame["hubs"][0]["capabilities"] = json!({"status":"degraded","auth_valid":true,"rpc_status":"ready","sse_live":false,"observation_current":false});
    // Unknown status/auth strings remain usable projections, not dropped frames.
    frame["hubs"][0]["status"] = json!("recovering");
    frame["hubs"][0]["auth_mode"] = json!("custom");
    frame["listeners"][0]["is_self_principal"] = json!(false);
    frame["listeners"][0]["harness"] = json!("human");
    frame["listeners"][0]["display_address"] = json!("@bob");
    frame["listeners"][0]["user"] = json!("bob");
    f.send(ui, frame.clone());
    control(&f, ui, "open_directory");
    let s = inspect(&f, ui);
    assert_eq!(
        node(
            &s,
            "directory_items",
            "dir_title:personal:repo-abcdefghijkl"
        )["text"],
        "@bob"
    );
    frame["hubs"][0]["status"] = json!("connected");
    frame["listeners"][0] = listener();
    for allowed in [false, true] {
        for key in ["can_set_scope", "can_pin", "can_rename", "can_stop"] {
            frame["listeners"][0][key] = json!(allowed);
        }
        f.send(ui, frame.clone());
        let s = inspect(&f, ui);
        for prefix in ["scope", "pin", "rename", "stop"] {
            let n = node(&s, "items", &format!("{prefix}:personal:repo-abcdefghijkl"));
            assert_eq!(!n.is_null() && n["hidden"] != true, allowed, "{prefix}");
        }
    }
    frame["hubs"][0]["supports_workspace_scope"] = json!(false);
    f.send(ui, frame);
    assert!(node(
        &inspect(&f, ui),
        "items",
        "scope:personal:repo-abcdefghijkl"
    )
    .is_null());
}

#[test]
fn v48_host_hub_and_nonconnected_drawers_gate_only_projected_affordances() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let ui = start(&f);
    let mut frame = state();
    let mut cfg = settings();
    for status in [
        "connecting",
        "auth_required",
        "disconnected",
        "unreachable",
        "error",
    ] {
        frame["hubs"][0]["status"] = json!(status);
        frame["hubs"][0]["can_login"] = json!(true);
        cfg["hubs"][0]["can_login"] = json!(true);
        frame["settings"] = cfg.clone();
        f.send(ui, frame.clone());
        let s = inspect(&f, ui);
        assert!(
            !node(&s, "items", "open_hub_settings:personal").is_null(),
            "{status}"
        );
    }
    control(&f, ui, "open_settings");
    control(&f, ui, "settings_tab:hubs");
    for (mode, token, enabled, running) in [
        ("none", "retained", true, true),
        ("token", "", true, true),
        ("token", "secret", true, false),
        ("token", "secret", false, false),
        ("token", "secret", true, true),
    ] {
        cfg["host_hub"]["auth_mode"] = json!(mode);
        cfg["host_hub"]["token"] = json!(token);
        cfg["host_hub"]["enabled"] = json!(enabled);
        cfg["host_hub"]["running"] = json!(running);
        cfg["host_hub"]["error"] = json!("Sharing stopped");
        f.send(ui, json!({"op":"settings","settings":cfg}));
        let s = inspect(&f, ui);
        assert_eq!(
            node(&s, "settings_items", "copy_host_hub_token")["hidden"],
            mode != "token" || token.is_empty()
        );
        assert_eq!(
            node(&s, "settings_items", "copy_hub_invite")["enabled"],
            enabled && running
        );
        assert!(!node(&s, "settings_items", "login_hub:personal").is_null());
    }
    cfg["hubs"][0]["can_login"] = json!(false);
    f.send(ui, json!({"op":"settings","settings":cfg}));
    assert!(node(&inspect(&f, ui), "settings_items", "login_hub:personal").is_null());
}

#[test]
fn v48_candidate_readiness_counts_and_collapse_share_one_predicate() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let ui = start(&f);
    for (index, pid_bound, endpoint, reload, open) in [
        (0, true, None, false, false),
        (1, true, Some(42), false, true),
        (2, false, None, false, true),
        (3, true, None, true, true),
    ] {
        let mut frame = state();
        let harness = format!("provider{index}");
        frame["hubs"][0]["auto_listen"] = json!(true);
        frame["harnesses"][0]["id"] = json!(harness);
        frame["harnesses"][0]["pid_bound"] = json!(pid_bound);
        frame["available"] = json!([{"harness":harness,"session_id":"candidate","title":"Review","cwd":"/tmp","endpoint_pid":endpoint,"needs_reload":reload}]);
        f.send(ui, frame);
        let s = inspect(&f, ui);
        assert_eq!(
            node(&s, "items", &format!("toggle_group:{harness}"))["text"],
            format!("Fixture · 1 {}", if open { "open" } else { "recent" })
        );
        assert_eq!(
            !node(&s, "items", &format!("candidate:{harness}:candidate")).is_null(),
            open
        );
    }
}

#[test]
fn v48_oracle_detects_borderless_padding_mid_y_nested_overlap_and_data_labels() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let ui = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(ui, frame);
    control(&f, ui, "open_settings");
    control(&f, ui, "settings_tab:channels");
    for (kind, key) in [
        ("borderless_padding", "tight_padding"),
        ("bar_midY", "non_anchored_action_bars"),
        ("footer_midY", "non_anchored_action_bars"),
        ("nested_overlap", "overlapping_views"),
        ("data_row_truncation", "truncated_labels"),
    ] {
        f.send(ui, json!({"op":"test_oracle_calibration","kind":kind}));
        let s = inspect_unchecked(&f, ui, None);
        assert!(!s[key].as_array().unwrap().is_empty(), "{kind}");
        if kind == "footer_midY" {
            assert_eq!(node(&s, "settings_items", "settings_footer")["height"], 44);
            assert!(s[key].as_array().unwrap().iter().any(|issue| issue
                .as_str()
                .unwrap()
                .contains("settings_footer: top row controls are not vertically centered")));
        }
        if kind == "nested_overlap" {
            assert!(s[key]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str().unwrap().contains("(nested)")));
        }
        f.send(ui, json!({"op":"test_oracle_calibration","kind":"reset"}));
        inspect(&f, ui);
    }
}

#[test]
fn v48_failed_destructive_receipts_preserve_drafts_and_newer_intent() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let ui = start(&f);
    let mut frame = state();
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["value"] = json!("saved");
    frame["settings"] = cfg;
    f.send(ui, frame);
    control(&f, ui, "open_settings");
    control(&f, ui, "settings_tab:channels");
    control(&f, ui, "edit_user_channel:channel");
    f.send(
        ui,
        json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"draft"}),
    );
    for (ok, value) in [(false, "draft"), (true, "newer")] {
        control(&f, ui, "unpair_channel:channel");
        inspect(&f, ui);
        let request = f
            .frames(ui)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "channel_unpair")
            .unwrap();
        if ok {
            f.send(ui, json!({"op":"test_settings_input","id":"channel_field:channel:account","value":value}));
        }
        f.send(ui, json!({"op":if ok {"settings"} else {"error"},"ok":ok,"committed":ok,"source_op":"channel_unpair","request_id":request["request_id"],"hub":request["hub"],"channel_id":"channel","error":if ok {Value::Null} else {json!("Permission denied")}}));
        assert_eq!(
            node(
                &inspect(&f, ui),
                "settings_items",
                "channel_field:channel:account"
            )["text"],
            value
        );
    }
}

#[test]
fn v48_centered_channel_bars_bound_long_summaries_and_expanded_controls() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let ui = start(&f);
    let mut frame = state();
    // The inbox is hosted remotely; this Mac has no Human listener.
    frame["hubs"][0]["human_listener_id"] = json!("inbox");
    frame["hubs"][0]["human_can_set_scope"] = json!(true);
    frame["hubs"][0]["attachments"] = json!([{"channel_id":"hub:channel","location":"hub","display_name":"Shared Messenger Channel","availability":"ACTIVE","is_local":false}]);
    frame["hubs"][0]["human_state_status"] = json!("current");
    let mut cfg = settings();
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["value"] = json!("room");
    frame["settings"] = cfg.clone();
    for paused in [false, true] {
        frame["hubs"][0]["human_paused"] = json!(paused);
        frame["settings"]["hubs"][0]["human_paused"] = json!(paused);
        f.send(ui, frame.clone());
        control(&f, ui, "open_settings");
        control(&f, ui, "settings_tab:channels");
        let s = inspect(&f, ui);
        let bar = node(&s, "settings_items", "inbox_control_bar");
        assert_eq!(bar["height"], 48);
        assert_eq!(bar["border_width"], 0);
        assert_eq!(bar["radius"], 10);
        let mid_y = |n: &Value| n["y"].as_f64().unwrap() + n["height"].as_f64().unwrap() / 2.0;
        let trailing: Vec<_> = ["toggle_inbox", "scope:inbox"]
            .into_iter()
            .map(|id| node(&s, "settings_items", id))
            .collect();
        let min_x = trailing
            .iter()
            .map(|n| n["x"].as_f64().unwrap())
            .fold(f64::INFINITY, f64::min);
        for action in trailing {
            assert!((mid_y(&action) - 24.0).abs() <= 1.5);
        }
        for id in ["inbox_control_title", "inbox_control_summary"] {
            let label = node(&s, "settings_items", id);
            assert!(
                label["x"].as_f64().unwrap() + label["width"].as_f64().unwrap() <= min_x - 10.0
            );
        }
        let badge = node(&s, "settings_items", "settings_channel_badge:channel");
        let status = node(&s, "settings_items", "settings_channel_status:channel");
        assert!((mid_y(&badge) - mid_y(&status)).abs() <= 1.0);
    }
    control(&f, ui, "scope:inbox");
    inspect(&f, ui);
    assert!(f
        .frames(ui)
        .iter()
        .any(|action| action["op"] == "human_inbox_update"
            && action["hub"] == "personal"
            && action["scope"] == "workspace"));
    control(&f, ui, "edit_user_channel:channel");
    let s = inspect(&f, ui);
    let band = node(&s, "settings_items", "stratum_user:channel");
    for id in ["toggle_channel:channel", "unpair_channel:channel"] {
        let button = node(&s, "settings_items", id);
        assert!(
            band["height"].as_f64().unwrap()
                - button["y"].as_f64().unwrap()
                - button["height"].as_f64().unwrap()
                >= 7.0
        );
    }
}

#[test]
fn v48_reply_authority_separates_activity_ownership_from_send_permission() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let ui = start(&f);
    let mut frame = state();
    let mut item = json!({"hub":"personal","signal_id":"reply","listener_id":"inbox","from_user":"bob","from_host_name":"laptop","from_title":"Review","from_display_address":"@bob","target_harness":"human","target_title":"Human","target_scoped_id":"personal:inbox","message":"Review ready","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true,"can_reply":false});
    for allowed in [false, true] {
        item["can_reply"] = json!(allowed);
        frame["activity"] = json!([item]);
        f.send(ui, frame.clone());
        control(&f, ui, "open_activity");
        let s = inspect(&f, ui);
        assert_eq!(
            node(&s, "directory_items", "reply_activity:reply")["hidden"],
            !allowed
        );
        f.send(ui, json!({"op":"test_directory_key","key":"return"}));
        let after = inspect(&f, ui);
        assert_eq!(after["composer_open"], allowed);
    }
}

#[test]
fn v48_hub_form_errors_require_a_hub_operation_or_matching_submission() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let ui = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(ui, frame);
    control(&f, ui, "open_settings");
    control(&f, ui, "settings_tab:hubs");
    control(&f, ui, "open_add_hub");
    for source in [Value::Null, json!(""), json!("channel_test")] {
        f.send(
            ui,
            json!({"op":"error","source_op":source,"error":"Unrelated failure"}),
        );
        let s = inspect(&f, ui);
        assert!(node(&s, "settings_items", "add_hub_error").is_null());
        assert!(!node(&s, "settings_items", "settings_error_banner").is_null());
    }
    for (id, value) in [
        ("settings_hub_name", "shared"),
        ("settings_hub_url", "https://shared.example.com"),
    ] {
        f.send(
            ui,
            json!({"op":"test_settings_input","id":id,"value":value}),
        );
    }
    control(&f, ui, "submit_add_hub");
    inspect(&f, ui);
    let request = f
        .frames(ui)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "setup_save_hub")
        .unwrap();
    f.send(
        ui,
        json!({"op":"error","request_id":request["request_id"],"error":"Correlated save failure"}),
    );
    let s = inspect(&f, ui);
    assert_eq!(
        node(&s, "settings_items", "add_hub_error")["text"],
        "Correlated save failure"
    );
    assert_eq!(
        node(&s, "settings_items", "submit_add_hub")["enabled"],
        true
    );
}
