use super::*;

#[test]
fn v45_directory_rebuilds_menu_only_for_visible_counters_or_scope_changes() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut state_frame = state();
    state_frame["hubs"][0]["auth_mode"] = json!("none");
    state_frame["hubs"][0]["supports_workspace_scope"] = json!(false);
    state_frame["listeners"][0]["user"] = json!("alice");
    f.send(p, state_frame);
    control(&f, p, "open_directory");
    let mut remote = peer("stable");
    remote["user"] = json!("alice");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[remote.clone()],"online_count":2,"matched_count":1}),
    );
    let before = inspect(&f, p);
    assert!(node(&before, "items", "scope:personal:repo-abcdefghijkl").is_null());
    remote["user"] = json!("bob");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[remote.clone()],"online_count":2,"matched_count":1}),
    );
    let scope = inspect(&f, p);
    assert!(node(&scope, "items", "scope:personal:repo-abcdefghijkl").is_null());
    assert_eq!(
        scope["rebuilds"], before["rebuilds"],
        "Directory identities cannot change Hub affordances"
    );
    remote["title"] = json!("Updated title");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[remote.clone()],"online_count":2,"matched_count":1,"warnings":["work: Temporarily unavailable"]}),
    );
    let updated = inspect(&f, p);
    assert_eq!(updated["rebuilds"], scope["rebuilds"]);
    assert_eq!(
        node(&updated, "directory_items", "directory_warning_label")["hidden"],
        false
    );
    assert_eq!(
        node(&updated, "directory_items", "dir_title:personal:stable")["text"],
        "Updated title"
    );
    remote["user"] = json!("carol");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[remote],"online_count":2,"matched_count":1,"warnings":[]}),
    );
    let same_scope = inspect(&f, p);
    assert_eq!(same_scope["rebuilds"], scope["rebuilds"]);
    assert_eq!(
        node(&same_scope, "directory_items", "directory_warning_label")["hidden"],
        true
    );
}

#[test]
fn pushed_remote_and_online_changes_requery_only_the_open_directory() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(p, frame.clone());
    control(&f, p, "open_settings");
    control(&f, p, "open_directory");
    let before = super::lazy_rendering::render_state(&f, p);
    let queries = || {
        f.frames(p)
            .into_iter()
            .filter(|action| action["op"] == "query_directory")
            .collect::<Vec<_>>()
    };
    let initial = queries().len();
    frame["my_remote_listeners"] = json!([peer("remote")]);
    f.send(p, frame.clone());
    let after_remote = super::lazy_rendering::render_state(&f, p);
    let remote_queries = queries();
    assert_eq!(remote_queries.len(), initial + 1);
    assert_eq!(
        after_remote["settings_rebuilds"],
        before["settings_rebuilds"]
    );

    frame["directory_online_count"] = json!(3);
    f.send(p, frame.clone());
    f.send(p, frame.clone());
    let after_count = super::lazy_rendering::render_state(&f, p);
    let count_queries = queries();
    assert_eq!(count_queries.len(), initial + 2);
    assert_ne!(
        count_queries.last().unwrap()["request_id"],
        remote_queries.last().unwrap()["request_id"]
    );
    assert_eq!(
        after_count["settings_rebuilds"],
        before["settings_rebuilds"]
    );

    control(&f, p, "close_directory");
    frame["directory_online_count"] = json!(1);
    f.send(p, frame);
    let after_close = super::lazy_rendering::render_state(&f, p);
    assert_eq!(after_close["directory_open"], false);
    assert_eq!(queries().len(), initial + 2);
}

#[test]
fn v45_settings_feedback_expiry_fits_without_a_background_rebuild() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    let cfg = settings();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    f.send(p, json!({"op":"test_clipboard","text":"{\"name\":\"shared\",\"url\":\"https://hub.example.com\"}"}));
    control(&f, p, "paste_hub_invite");
    inspect(&f, p);
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "paste_hub_invite")
        .unwrap();
    f.send(p, json!({"op":"settings","settings":cfg,"ok":true,"source_op":"setup_save_hub","request_id":request["request_id"]}));
    let feedback = inspect(&f, p);
    let button = node(&feedback, "settings_items", "paste_hub_invite");
    assert_eq!(button["text"], "✓ Joined");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "paste_hub_invite")["instance"],
        button["instance"]
    );
    f.send(
        p,
        json!({"op":"test_expire_copy_feedback","id":"paste_hub_invite"}),
    );
    let expired = inspect(&f, p);
    assert_eq!(
        node(&expired, "settings_items", "paste_hub_invite")["text"],
        "Join from Clipboard"
    );
    assert_eq!(
        node(&expired, "settings_items", "paste_hub_invite")["width"],
        button["width"]
    );
}

#[test]
fn v45_settings_dedup_scroll_editor_and_inventory_diagnostics() {
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
    f.send(p, json!({"op":"test_settings_scroll","offset":80}));
    let before = inspect(&f, p);
    let scroll = node(&before, "settings_items", "settings_hubs_tab");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let same = inspect(&f, p);
    assert_eq!(
        node(&same, "settings_items", "settings_hubs_tab")["instance"],
        scroll["instance"]
    );
    cfg["launch_at_login"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let changed = inspect(&f, p);
    assert_eq!(
        node(&changed, "settings_items", "settings_hubs_tab")["content_y"],
        scroll["content_y"]
    );
    f.send(p, json!({"op":"test_focus_field","id":"settings_hub_name"}));
    f.send(p, json!({"op":"test_clipboard","text":"Draft"}));
    f.send(p, json!({"op":"test_settings_key","key":"cmd-v"}));
    f.send(p, json!({"op":"test_editor_marked","value":"compose"}));
    let editing = inspect(&f, p);
    let editor = editing["field_editors"][0].clone();
    assert_eq!(editor["marked"], true);
    assert_eq!(editor["attached"], true);
    assert_eq!(editor["can_undo"], true);
    cfg["host_hub"]["token"] = json!("changed");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let refreshed = inspect(&f, p);
    for key in [
        "instance",
        "id",
        "marked",
        "attached",
        "can_undo",
        "selection_location",
        "selection_length",
    ] {
        assert_eq!(refreshed["field_editors"][0][key], editor[key], "{key}");
    }
    assert_eq!(
        node(&refreshed, "settings_items", "settings_hub_name")["instance"],
        node(&editing, "settings_items", "settings_hub_name")["instance"]
    );
    f.send(p, json!({"op":"test_end_editing"}));
    control(&f, p, "settings_tab:channels");
    cfg["channel_inventory_error"] = json!("Channel inventory temporarily unavailable. Try again.");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let diagnostic = inspect(&f, p);
    assert!(!node(&diagnostic, "settings_items", "channel_inventory_error").is_null());
    assert!(!node(&diagnostic, "settings_items", "settings_channel:channel").is_null());
    assert!(node(&diagnostic, "settings_items", "settings_channel:inventory").is_null());
    assert_eq!(
        nodes(&diagnostic["settings_items"])
            .iter()
            .filter(|n| n["id"] == "settings_channel:channel")
            .count(),
        1
    );
    cfg["channel_inventory_error"] = Value::Null;
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert!(node(&inspect(&f, p), "settings_items", "channel_inventory_error").is_null());
}

#[test]
fn v45_pairing_instruction_survives_pending_health_updates_and_receipts_settle() {
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
    control(&f, p, "edit_user_channel:channel");
    control(&f, p, "save_channel:channel");
    inspect(&f, p);
    let save = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "channel_save")
        .unwrap();
    control(&f, p, "pair_channel:channel");
    inspect(&f, p);
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "channel_pair")
        .unwrap();
    f.send(p, json!({"op":"pair_code","hub":request["hub"],"channel_id":"channel","request_id":request["request_id"],"instruction":"Send the pairing command","pairing_command":"/start sample"}));
    cfg["channels"][0]["status"] = json!("pairing_pending");
    cfg["channels"][0]["detail"] = json!("Waiting for account pairing");
    f.send(p, json!({"op":"settings","settings":cfg,"ok":true,"source_op":"channel_save","channel_id":"channel","request_id":save["request_id"]}));
    let pairing = inspect(&f, p);
    assert!(!node(&pairing, "settings_items", "copy_pair_command:channel").is_null());
    cfg["launch_at_login"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert!(!node(
        &inspect(&f, p),
        "settings_items",
        "copy_pair_command:channel"
    )
    .is_null());
    control(&f, p, "cancel_pair_channel:channel");
    confirm_channel_action(&f, p, "channel_cancel_pair");
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "copy_pair_command:channel").is_null());
    control(&f, p, "settings_tab:apps");
    control(&f, p, "setup_app:codex");
    inspect(&f, p);
    let action = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "setup_apply")
        .unwrap();
    f.send(p, json!({"op":"settings","settings":cfg,"ok":true,"source_op":"setup_apply","request_id":action["request_id"]}));
    let settled = inspect(&f, p);
    assert!(!node(&settled, "settings_items", "setup_app:codex")["text"]
        .as_str()
        .unwrap()
        .contains("Setting"));
}

#[test]
fn v45_directory_timestamp_dedup_hover_copy_and_error_clear() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    f.send(p, state());
    control(&f, p, "open_directory");
    let mut remote = peer("stable");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[remote.clone()],"online_count":2,"matched_count":1,"warnings":[]}),
    );
    control(&f, p, "copy_dir:personal:stable");
    f.send(
        p,
        json!({"op":"test_hover","id":"copy_dir:personal:stable","hovered":true}),
    );
    let before = inspect(&f, p);
    remote["last_seen_at"] = Value::Null;
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[remote.clone()],"online_count":2,"matched_count":1,"warnings":[]}),
    );
    let after = inspect(&f, p);
    assert_eq!(after["rebuilds"], before["rebuilds"]);
    let copy = node(&after, "directory_items", "copy_dir:personal:stable");
    assert_eq!(
        copy["instance"],
        node(&before, "directory_items", "copy_dir:personal:stable")["instance"]
    );
    assert_eq!(copy["text"], "✓ Copied");
    assert_eq!(copy["hovered"], true);
    f.send(p, json!({"op":"test_reconfigure_rows"}));
    let reconfigured = inspect(&f, p);
    let copy = node(&reconfigured, "directory_items", "copy_dir:personal:stable");
    assert_eq!(copy["hovered"], true);
    assert_eq!(copy["text"], "✓ Copied");
    f.send(p, json!({"op":"error","source_op":"query_directory","error":"Directory temporarily unavailable"}));
    let failed = inspect(&f, p);
    assert!(
        !node(&failed, "directory_items", "directory_warning_label")["hidden"]
            .as_bool()
            .unwrap()
    );
    remote["title"] = json!("Peer without a timestamp");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[remote.clone()],"online_count":2,"matched_count":1,"warnings":[]}),
    );
    let cleared = inspect(&f, p);
    assert_eq!(
        node(&cleared, "directory_items", "dir_title:personal:stable")["text"],
        "Peer without a timestamp"
    );
    assert_eq!(
        node(&cleared, "directory_items", "directory_warning_label")["hidden"],
        true
    );
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("replacement")],"online_count":2}),
    );
    assert!(node(
        &inspect(&f, p),
        "directory_items",
        "copy_dir:personal:stable"
    )
    .is_null());
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[remote],"online_count":2}),
    );
    let restored = node(
        &inspect(&f, p),
        "directory_items",
        "copy_dir:personal:stable",
    );
    assert_eq!(restored["text"], "✓ Copied");
    assert_eq!(restored["accessibility_label"], "✓ Copied");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"online_count":0,"matched_count":0,"warnings":[],"unread_count":3,"activity_has_any":true}),
    );
    let counters = inspect(&f, p);
    assert_eq!(counters["unread_count"], 3);
    assert_eq!(counters["directory_online_count"], 0);
    assert!(counters["rebuilds"].as_u64().unwrap() > cleared["rebuilds"].as_u64().unwrap());
}

#[test]
fn v45_activity_copy_survives_cell_replacement_and_confirmation_stays_mounted() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    f.send(p, state());
    control(&f, p, "open_activity");
    let item = json!({"hub":"personal","signal_id":"stable","listener_id":"agent","from_user":"bob","from_host_name":"laptop","from_title":"Build","target_harness":"fixture","target_title":"Session","target_scoped_id":"personal:agent","message":"Build ready","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true});
    let mut frame = json!({"op":"directory","peers":[],"activity":[item],"activity_total":1,"activity_filter":"me","activity_query":"","warnings":[]});
    send_fixture(&f, p, frame.clone());
    control(&f, p, "copy_activity_msg:stable");
    control(&f, p, "activity_clear");
    f.send(
        p,
        json!({"op":"test_hover","id":"activity_clear_confirm_cancel","hovered":true}),
    );
    let before = inspect(&f, p);
    let confirm = node(&before, "directory_items", "activity_clear_confirm_cancel");
    frame["activity"][0]["message"] = json!("Build ready with details");
    send_fixture(&f, p, frame.clone());
    let after = inspect(&f, p);
    assert_eq!(
        node(&after, "directory_items", "activity_clear_confirm_cancel")["instance"],
        confirm["instance"]
    );
    assert_eq!(
        node(&after, "directory_items", "activity_clear_confirm_cancel")["hovered"],
        true
    );
    assert_eq!(
        node(&after, "directory_items", "copy_activity_msg:stable")["text"],
        "✓ Copied"
    );
    assert!(
        after["activity_copy_remaining"]["personal|stable"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    // Switching away forces a peer cell to replace the activity row before it returns.
    control(&f, p, "dir_mode:directory");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("replacement")]}),
    );
    control(&f, p, "dir_mode:activity");
    send_fixture(&f, p, frame);
    assert_eq!(
        node(
            &inspect(&f, p),
            "directory_items",
            "copy_activity_msg:stable"
        )["text"],
        "✓ Copied"
    );
}

#[test]
fn invalid_rename_shows_banner_without_replacing_the_focused_editor() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    f.send(p, state());
    control(&f, p, "rename:personal:repo-abcdefghijkl");
    f.send(p, json!({"op":"test_settings_input","id":"rename_field:personal:repo-abcdefghijkl","value":"   "}));
    f.send(
        p,
        json!({"op":"test_focus_field","id":"rename_field:personal:repo-abcdefghijkl"}),
    );
    let before = inspect(&f, p);
    assert_eq!(before["field_editors"][0]["attached"], true);
    f.send(p, json!({"op":"test_menu_key","key":"return"}));
    let rejected = inspect(&f, p);
    assert!(!node(&rejected, "items", "menu_error_row").is_null());
    assert_eq!(
        rejected["rename_field_identity"],
        before["rename_field_identity"]
    );
    for key in [
        "instance",
        "id",
        "attached",
        "selection_location",
        "selection_length",
    ] {
        assert_eq!(
            rejected["field_editors"][0][key], before["field_editors"][0][key],
            "{key}"
        );
    }
    assert!(!f
        .frames(p)
        .iter()
        .any(|frame| frame["op"] == "rename_session"));
    control(&f, p, "dismiss_menu_error");
    let dismissed = inspect(&f, p);
    assert!(node(&dismissed, "items", "menu_error_row").is_null());
    assert_eq!(
        dismissed["field_editors"][0]["instance"],
        before["field_editors"][0]["instance"]
    );
}

#[test]
fn channel_save_expiry_preserves_the_active_marked_text_editor() {
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
    control(&f, p, "edit_user_channel:channel");
    control(&f, p, "save_channel:channel");
    inspect(&f, p);
    let save = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "channel_save")
        .unwrap();
    f.send(p, json!({"op":"settings","settings":cfg,"ok":true,"source_op":"channel_save","channel_id":"channel","request_id":save["request_id"]}));
    let saved = inspect(&f, p);
    assert!(nodes(&saved["settings_items"])
        .iter()
        .any(|n| n["text"] == "✓ Saved"));
    control(&f, p, "edit_user_channel:channel");
    f.send(
        p,
        json!({"op":"test_focus_field","id":"channel_field:channel:account"}),
    );
    f.send(p, json!({"op":"test_editor_marked","value":"composing"}));
    let editing = inspect(&f, p);
    assert_eq!(editing["field_editors"][0]["marked"], true);
    // Observe the real scheduled expiry rather than synthesizing its rebuild.
    wait(|| {
        let refreshed = inspect(&f, p);
        for key in [
            "instance",
            "id",
            "marked",
            "attached",
            "can_undo",
            "selection_location",
            "selection_length",
        ] {
            assert_eq!(
                refreshed["field_editors"][0][key], editing["field_editors"][0][key],
                "{key}"
            );
        }
        assert_eq!(
            node(
                &refreshed,
                "settings_items",
                "channel_field:channel:account"
            )["instance"],
            node(&editing, "settings_items", "channel_field:channel:account")["instance"]
        );
        refreshed["settings_rebuild_deferred"] == true
    });
}

#[test]
fn v45_menu_candidate_timestamp_empty_drawer_and_active_rename_stability() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["hubs"] = json!([hubs()[0].clone()]);
    frame["listeners"] = json!([]);
    frame["my_remote_listeners"] = json!([peer("other-host")]);
    frame["available"] = json!([{"harness":"fixture","session_id":"s1","title":"Available","cwd":"/tmp","endpoint_pid":123,"updated_at_ms":1}]);
    f.send(p, frame.clone());
    let before = inspect(&f, p);
    let height = node(&before, "items", "empty")["height"].clone();
    frame["available"][0]["updated_at_ms"] = json!(2);
    frame["my_remote_listeners"][0]["last_seen_at"] = json!("2030-01-01T00:00:30Z");
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p)["rebuilds"], before["rebuilds"]);
    frame["available"] = json!([]);
    frame["hubs"][0]["status"] = json!("connecting");
    f.send(p, frame.clone());
    let reconnect = inspect(&f, p);
    assert_eq!(node(&reconnect, "items", "empty")["height"], height);
    assert_eq!(node(&reconnect, "items", "drawer:personal")["hidden"], true);
    frame["hubs"][0]["status"] = json!("connected");
    frame["listeners"] = json!([listener()]);
    f.send(p, frame.clone());
    control(&f, p, "rename:personal:repo-abcdefghijkl");
    f.send(
        p,
        json!({"op":"test_focus_field","id":"rename_field:personal:repo-abcdefghijkl"}),
    );
    f.send(
        p,
        json!({"op":"test_editor_marked","surface":"menu","value":"draft"}),
    );
    let editing = inspect(&f, p);
    assert_eq!(editing["field_editors"][0]["marked"], true);
    frame["unread_count"] = json!(1);
    f.send(p, frame);
    let refreshed = inspect(&f, p);
    assert_eq!(refreshed["rebuilds"], editing["rebuilds"]);
    for key in [
        "instance",
        "id",
        "marked",
        "attached",
        "can_undo",
        "selection_location",
        "selection_length",
    ] {
        assert_eq!(
            refreshed["field_editors"][0][key], editing["field_editors"][0][key],
            "{key}"
        );
    }
    assert_eq!(
        refreshed["rename_field_identity"],
        editing["rename_field_identity"]
    );
    f.send(p, json!({"op":"test_end_editing"}));
    f.send(p, json!({"op":"test_menu_key","key":"escape"}));
    assert!(node(
        &inspect(&f, p),
        "items",
        "rename_field:personal:repo-abcdefghijkl"
    )
    .is_null());
}
