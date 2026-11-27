use super::*;

fn channels(f: &Fixture, ui: usize, cfg: &Value) -> Value {
    f.send(ui, json!({"op":"settings","settings":cfg}));
    control(f, ui, "settings_tab:channels");
    inspect(f, ui)
}

#[test]
fn v48_non_admin_channel_authority_and_gateway_health() {
    let _lock = serial();
    let (f, ui) = ui_fixture();
    f.send(ui, state());
    control(&f, ui, "open_settings");
    let mut cfg = settings();
    cfg["channels"][0]["can_configure_hub"] = json!(false);
    cfg["channels"][0]["can_clear_hub"] = json!(false);
    cfg["channels"][0]["hub_ready"] = json!(false);
    cfg["channels"][0]["status"] = json!("not_configured");
    cfg["channels"][0]["fields"][0]["hidden"] = json!(true);
    let hidden = channels(&f, ui, &cfg);
    assert!(hidden.settings("settings_channel:channel").is_null());
    assert_eq!(
        hidden.settings("channels_managed_notice")["text"],
        "External channels are managed by your 'personal' Hub administrator. None are enabled yet."
    );
    cfg["channel_inventory_error"] = json!("Service unavailable");
    assert!(channels(&f, ui, &cfg)
        .settings("channels_managed_notice")
        .is_null());
    cfg["channel_inventory_error"] = Value::Null;
    cfg["channels"][0]["hub_ready"] = json!(true);
    for status in ["hub_ready", "ready", "paused"] {
        cfg["channels"][0]["status"] = json!(status);
        cfg["channels"][0]["configured"] = json!(status != "hub_ready");
        cfg["channels"][0]["fields"][1]["value"] = json!("room");
        let s = channels(&f, ui, &cfg);
        assert!(s.settings("stratum_hub:channel").is_null());
        assert!(s.settings("edit_hub_service:channel").is_null());
        if status == "hub_ready" {
            assert_eq!(
                s.settings("stratum_user_title:channel")["text"],
                "Your Personal Account (@alice)"
            );
        }
    }
    // Gateway health is independent of pairing status and administrator access.
    cfg["channels"][0]["warning"] = json!(true);
    cfg["channels"][0]["error_origin"] = json!("hub");
    cfg["channels"][0]["detail"] = json!("Gateway token needs attention.");
    let readonly = channels(&f, ui, &cfg);
    assert_eq!(
        readonly.settings("settings_channel_detail:channel")["text"],
        "Gateway token needs attention."
    );
    assert_eq!(
        readonly.settings("stratum_hub_status:channel")["text"],
        "Service needs attention"
    );
    assert!(readonly.settings("edit_hub_service:channel").is_null());
    assert!(readonly.settings("clear_channel_service:channel").is_null());
    cfg["channels"][0]["can_configure_hub"] = json!(true);
    cfg["channels"][0]["can_clear_hub"] = json!(true);
    cfg["channels"][0]["fields"][0]["hidden"] = json!(false);
    assert_eq!(
        channels(&f, ui, &cfg).settings("edit_hub_service:channel")["enabled"],
        true
    );
    control(&f, ui, "clear_channel_service:channel");
    inspect(&f, ui);
    let action = f.frames_for(ui, "disconnect_human_channel").next().unwrap();
    assert_eq!(action["reset_hub"], true);
    assert_eq!(action["reset_user"], false);
}

#[test]
fn v48_personal_form_requires_every_required_value_and_explicit_edit_authority() {
    let _lock = serial();
    let (f, ui) = ui_fixture();
    f.send(ui, state());
    control(&f, ui, "open_settings");
    let mut cfg = settings();
    cfg["channels"][0]["has_hub_service"] = json!(false);
    let mut second = cfg["channels"][0]["fields"][1].clone();
    second["key"] = json!("destination");
    second["label"] = json!("Destination");
    cfg["channels"][0]["fields"] = json!([cfg["channels"][0]["fields"][1], second]);
    let s = channels(&f, ui, &cfg);
    assert!(s.settings("stratum_hub:channel").is_null());
    for (account, destination, enabled) in [
        ("42", "", false),
        ("42", "room", true),
        (" ", "room", false),
    ] {
        for (key, value) in [("account", account), ("destination", destination)] {
            f.send(ui, json!({"op":"test_settings_input","id":format!("channel_field:channel:{key}"),"value":value}));
        }
        assert_eq!(
            inspect(&f, ui).settings("test_channel:channel")["enabled"],
            enabled
        );
    }
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["can_edit_personal"] = json!(false);
    let s = channels(&f, ui, &cfg);
    assert!(
        s.settings("edit_user_channel:channel")["hidden"] == true
            || s.settings("edit_user_channel:channel").is_null()
    );
    assert!(s.settings("save_channel:channel").is_null());
}

#[test]
fn v48_team_controls_use_authoritative_capabilities_and_seed_identity() {
    let _lock = serial();
    let (f, ui) = ui_fixture();
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
        s.directory("dir_title:personal:repo-abcdefghijkl")["text"],
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
            let n = s.menu(&format!("{prefix}:personal:repo-abcdefghijkl"));
            assert_eq!(!n.is_null() && n["hidden"] != true, allowed, "{prefix}");
        }
    }
    frame["hubs"][0]["supports_workspace_scope"] = json!(false);
    f.send(ui, frame);
    assert!(inspect(&f, ui)
        .menu("scope:personal:repo-abcdefghijkl")
        .is_null());
}

#[test]
fn v48_host_hub_and_nonconnected_drawers_gate_only_projected_affordances() {
    let _lock = serial();
    let (f, ui) = ui_fixture();
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
        assert!(!s.menu("open_hub_settings:personal").is_null(), "{status}");
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
            s.settings("copy_host_hub_token")["hidden"],
            mode != "token" || token.is_empty()
        );
        assert_eq!(s.settings("copy_hub_invite")["enabled"], enabled && running);
        assert!(!s.settings("login_hub:personal").is_null());
    }
    cfg["hubs"][0]["can_login"] = json!(false);
    f.send(ui, json!({"op":"settings","settings":cfg}));
    assert!(inspect(&f, ui).settings("login_hub:personal").is_null());
}

#[test]
fn v48_candidate_readiness_counts_and_collapse_share_one_predicate() {
    let _lock = serial();
    let (f, ui) = ui_fixture();
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
            s.menu(&format!("toggle_group:{harness}"))["text"],
            format!("Fixture · 1 {}", if open { "open" } else { "recent" })
        );
        assert_eq!(
            !s.menu(&format!("candidate:{harness}:candidate")).is_null(),
            open
        );
    }
}

#[test]
fn v48_failed_destructive_receipts_preserve_drafts_and_newer_intent() {
    let _lock = serial();
    let (f, ui) = ui_fixture();
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
        let request = f.frames_for(ui, "channel_unpair").next_back().unwrap();
        if ok {
            f.send(ui, json!({"op":"test_settings_input","id":"channel_field:channel:account","value":value}));
        }
        f.send(ui, json!({"op":if ok {"settings"} else {"error"},"ok":ok,"committed":ok,"source_op":"channel_unpair","request_id":request["request_id"],"hub":request["hub"],"channel_id":"channel","error":if ok {Value::Null} else {json!("Permission denied")}}));
        assert_eq!(
            inspect(&f, ui).settings("channel_field:channel:account")["text"],
            value
        );
    }
}

#[test]
fn v48_centered_channel_bars_bound_long_summaries_and_expanded_controls() {
    let _lock = serial();
    let (f, ui) = ui_fixture();
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
    cfg["hubs"][0]["human_state_status"] = json!("current");
    cfg["hubs"][0]["human_listener_id"] = json!("inbox");
    cfg["hubs"][0]["human_can_set_scope"] = json!(true);
    cfg["hubs"][0]["supports_workspace_scope"] = json!(true);
    cfg["hubs"][0]["attachments"] = frame["hubs"][0]["attachments"].clone();
    frame["settings"] = cfg.clone();
    for paused in [false, true] {
        frame["hubs"][0]["human_paused"] = json!(paused);
        frame["settings"]["hubs"][0]["human_paused"] = json!(paused);
        f.send(ui, frame.clone());
        control(&f, ui, "open_settings");
        control(&f, ui, "settings_tab:channels");
        let s = inspect(&f, ui);
        let bar = s.settings("inbox_control_bar");
        let height = bar["height"].as_f64().unwrap();
        assert!(height >= 48.0);
        assert_eq!(bar["border_width"], 0);
        assert_eq!(bar["radius"], 10);
        let mid_y = |n: &Value| n["y"].as_f64().unwrap() + n["height"].as_f64().unwrap() / 2.0;
        let trailing: Vec<_> = ["toggle_inbox", "scope:inbox"]
            .into_iter()
            .map(|id| s.settings(id))
            .collect();
        let min_x = trailing
            .iter()
            .map(|n| n["x"].as_f64().unwrap())
            .fold(f64::INFINITY, f64::min);
        for action in trailing {
            assert!((mid_y(&action) - height / 2.0).abs() <= 1.5);
        }
        let inbox_summary = s.settings("inbox_control_summary");
        assert_eq!(inbox_summary["wraps"], true);
        assert_eq!(inbox_summary["maximum_lines"], 0);
        for id in ["inbox_control_title", "inbox_control_summary"] {
            let label = s.settings(id);
            assert!(
                label["x"].as_f64().unwrap() + label["width"].as_f64().unwrap() <= min_x - 10.0
            );
        }
        let badge = s.settings("settings_channel_badge:channel");
        let status = s.settings("settings_channel_status:channel");
        assert!((mid_y(&badge) - mid_y(&status)).abs() <= 1.0);
        let summary = s.settings("channel_summary:channel");
        let trailing_top = [
            "test_channel:channel",
            "toggle_channel:channel",
            "edit_user_channel:channel",
            "unpair_channel:channel",
        ]
        .into_iter()
        .map(|id| s.settings(id))
        .filter(|n| !n.is_null() && n["hidden"] != true)
        .map(|n| n["y"].as_f64().unwrap() + n["height"].as_f64().unwrap())
        .fold(f64::NEG_INFINITY, f64::max);
        assert!(trailing_top.is_finite());
        assert!(summary["y"].as_f64().unwrap() >= trailing_top + 6.0);
        assert_eq!(summary["x"], 12);
        let stratum = s.settings("stratum_user:channel");
        assert_eq!(
            summary["width"].as_f64().unwrap(),
            stratum["width"].as_f64().unwrap() - 24.0
        );
        assert!(stratum["height"].as_f64().unwrap() < 368.0);
    }
    // An open-settings receipt can observe a different inbox than the last state.
    cfg["hubs"][0]["user"] = json!("bob");
    cfg["hubs"][0]["human_listener_id"] = json!("new-inbox");
    cfg["hubs"][0]["human_scope"] = json!("workspace");
    cfg["hubs"][0]["human_control_revision"] = json!(9);
    cfg["hubs"][0]["human_availability"] = json!("ACTIVE");
    cfg["hubs"][0]["attachments"] = json!([{"channel_id":"hub:alerts","location":"hub","display_name":"Alerts","availability":"ACTIVE","is_local":false}]);
    f.send(
        ui,
        json!({"op":"settings","source_op":"open_settings","settings":cfg}),
    );
    let refreshed = inspect(&f, ui);
    assert_eq!(
        refreshed.settings("inbox_control_title")["text"],
        "You (@bob)"
    );
    assert_eq!(
        refreshed.settings("inbox_control_summary")["text"],
        "Alerts · Activity"
    );
    assert_eq!(refreshed.settings("scope:new-inbox")["text"], "Team");
    assert!(refreshed.settings("scope:inbox").is_null());
    control(&f, ui, "scope:new-inbox");
    let pending = inspect(&f, ui);
    let scope = pending.settings("scope:new-inbox");
    assert_eq!(scope["text"], "Updating…");
    assert_eq!(scope["enabled"], false);
    assert_eq!(pending.settings("toggle_inbox")["text"], "Pause Inbox");
    let action = f.frames_for(ui, "human_inbox_update").next_back().unwrap();
    assert_eq!(action["hub"], "personal");
    assert_eq!(action["scope"], "self");
    assert_eq!(action["expected_control_revision"], 9);
    assert!(action.get("paused").is_none());
    cfg["hubs"][0]["human_scope"] = json!("self");
    f.send(ui, json!({"op":"settings","source_op":"human_inbox_update","request_id":action["request_id"],"ok":true,"settings":cfg}));
    let settled = inspect(&f, ui);
    assert_eq!(settled.settings("scope:new-inbox")["text"], "Self");
    assert_eq!(settled.settings("scope:new-inbox")["enabled"], true);
    control(&f, ui, "edit_user_channel:channel");
    let s = inspect(&f, ui);
    let band = s.settings("stratum_user:channel");
    for id in ["toggle_channel:channel", "unpair_channel:channel"] {
        let button = s.settings(id);
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
    let (f, ui) = ui_fixture();
    let mut frame = state();
    let mut item = json!({"hub":"personal","signal_id":"reply","listener_id":"inbox","from_user":"bob","from_host_name":"laptop","from_title":"Review","from_display_address":"@bob","target_harness":"human","target_title":"Human","target_scoped_id":"personal:inbox","message":"Review ready","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true,"can_reply":false});
    for allowed in [false, true] {
        item["can_reply"] = json!(allowed);
        frame["activity"] = json!([item]);
        f.send(ui, frame.clone());
        control(&f, ui, "open_activity");
        let s = inspect(&f, ui);
        assert_eq!(s.directory("reply_activity:reply")["hidden"], !allowed);
        f.send(ui, json!({"op":"test_directory_key","key":"return"}));
        let after = inspect(&f, ui);
        assert_eq!(after["composer_open"], allowed);
    }
    f.send(ui, json!({"op":"test_directory_key","key":"escape"}));
    let mut blocked = item.clone();
    blocked["signal_id"] = json!("blocked");
    blocked["can_reply"] = json!(false);
    blocked["created_at"] = json!("2029-01-01T00:00:00Z");
    frame["activity"] = json!([item, blocked]);
    f.send(ui, frame);
    let shortcuts = || inspect(&f, ui).directory("directory_shortcuts")["text"].clone();
    assert_eq!(shortcuts(), "↵ Reply    ⎋ Close");
    f.send(ui, json!({"op":"test_directory_key","key":"down"}));
    assert_eq!(shortcuts(), "⎋ Close");
    f.send(ui, json!({"op":"test_directory_key","key":"up"}));
    assert_eq!(shortcuts(), "↵ Reply    ⎋ Close");
}

#[test]
fn v48_hub_form_errors_require_a_hub_operation_or_matching_submission() {
    let _lock = serial();
    let (f, ui) = ui_fixture();
    let frame = state_with_settings(settings());
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
        assert!(s.settings("add_hub_error").is_null());
        assert!(!s.settings("settings_error_banner").is_null());
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
    let request = f.frames_for(ui, "setup_save_hub").next_back().unwrap();
    f.send(
        ui,
        json!({"op":"error","request_id":request["request_id"],"error":"Correlated save failure"}),
    );
    let s = inspect(&f, ui);
    assert_eq!(
        s.settings("add_hub_error")["text"],
        "Correlated save failure"
    );
    assert_eq!(s.settings("submit_add_hub")["enabled"], true);
}
