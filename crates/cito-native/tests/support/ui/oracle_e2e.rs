use super::*;

#[test]
fn failed_state_and_directory_receipts_route_errors_before_projection_guards() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    frame["settings"]["hubs"][0]["human_state_status"] = json!("current");
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    for (operation, id) in [
        ("human_inbox_update", "toggle_inbox"),
        ("unlisten_all", "stop_all"),
    ] {
        control(&f, p, id);
        inspect(&f, p);
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == operation)
            .unwrap();
        f.send(p, json!({"op":"state","ok":false,"source_op":operation,"request_id":request["request_id"],"error":"Mutation failed"}));
        let s = inspect(&f, p);
        assert_eq!(s["pending_controls"], json!([]));
        assert_eq!(
            node(&s, "settings_items", "settings_error_label")["text"],
            "Mutation failed"
        );
    }
    control(&f, p, "open_directory");
    inspect(&f, p);
    let query = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "open_directory")
        .unwrap();
    for (op, source) in [
        ("directory", "open_directory"),
        ("error", "directory"),
        ("directory", "directory"),
        ("directory", ""),
    ] {
        let error = format!("Directory unavailable: {op}/{source}");
        f.send(p, json!({"op":op,"ok":false,"source_op":source,"request_id":query["request_id"],"error":error}));
        let s = inspect(&f, p);
        assert!(s["directory_items"].to_string().contains(&error));
        assert_eq!(
            node(&s, "settings_items", "settings_error_label")["text"],
            "Mutation failed",
            "Directory failures must not replace the Settings banner"
        );
    }
}

#[test]
fn pin_stop_and_auto_listen_settle_through_the_real_daemon_with_semantic_a() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    let mut cycle = ClosedLoop::new(&f);
    f.tool(
        "cito_listen",
        json!({"about":"Lifecycle check"}),
        meta.clone(),
    );
    let s = cycle.until(|s| !node(s, "items", "stop_all").is_null());
    let stop_id = nodes(&s["items"])
        .into_iter()
        .find_map(|n| {
            n["id"]
                .as_str()
                .filter(|id| id.starts_with("stop:"))
                .map(str::to_owned)
        })
        .unwrap();
    let pin_id = stop_id.replacen("stop:", "pin:", 1);
    let session_id = stop_id.replacen("stop:", "session:", 1);
    assert!(
        node(&s, "items", &pin_id).is_null(),
        "Manual listening without Auto-Listen needs no pin control"
    );

    cycle.control("toggle_auto_listen");
    let s = cycle.until(|s| node(s, "items", &pin_id)["symbol"] == "pin.fill");
    assert!(node(&s, "items", "toggle_auto_listen")["tooltip"]
        .as_str()
        .unwrap()
        .contains("keeps pinned sessions listening"));
    cycle.control("toggle_auto_listen");
    let s = cycle.until(|s| node(s, "items", "toggle_auto_listen")["text"] == "Auto-Listen");
    assert!(
        !node(&s, "items", &session_id).is_null(),
        "Turning Auto-Listen off preserves pinned listeners"
    );
    assert!(node(&s, "items", &pin_id).is_null());

    cycle.control("toggle_auto_listen");
    cycle.until(|s| node(s, "items", &pin_id)["symbol"] == "pin.fill");
    cycle.control(&pin_id);
    cycle.until(|s| node(s, "items", &pin_id)["symbol"] == "pin");
    assert!(cycle
        .results
        .iter()
        .any(|(a, status, _)| a["op"] == "set_listen_mode"
            && a["listen_mode"] == "auto"
            && *status == 200));
    cycle.control(&pin_id);
    cycle.until(|s| node(s, "items", &pin_id)["symbol"] == "pin.fill");
    cycle.control(&pin_id);
    cycle.until(|s| node(s, "items", &pin_id)["symbol"] == "pin");
    cycle.control("toggle_auto_listen");
    cycle.until(|s| {
        node(s, "items", &session_id).is_null()
            && node(s, "items", "toggle_auto_listen")["text"] == "Auto-Listen"
    });

    f.tool("cito_listen", json!({"about":"Stop only me"}), meta.clone());
    cycle.until(|s| !node(s, "items", &session_id).is_null());
    cycle.control("toggle_auto_listen");
    cycle.until(|s| node(s, "items", &pin_id)["symbol"] == "pin.fill");
    cycle.control(&stop_id);
    let s = cycle.until(|s| node(s, "items", &session_id).is_null());
    assert_eq!(
        node(&s, "items", "toggle_auto_listen")["text"],
        "Auto-Listen: On"
    );
    assert_eq!(
        f.action(json!({"op":"get_snapshot"})).1["hubs"][0]["auto_listen"],
        true
    );

    f.tool("cito_listen", json!({"about":"Stop all agents"}), meta);
    cycle.until(|s| !node(s, "items", "stop_all").is_null());
    cycle.control("stop_all");
    cycle.until(|s| {
        node(s, "items", &session_id).is_null()
            && node(s, "items", "toggle_auto_listen")["text"] == "Auto-Listen"
    });
    assert_eq!(
        f.action(json!({"op":"get_snapshot"})).1["hubs"][0]["auto_listen"],
        false
    );
    let (status, _) = f.action(json!({"op":"unlisten_all","hub":"local"}));
    assert_eq!(
        status, 200,
        "Bulk stop remains idempotent with no active agents"
    );
}

#[test]
fn add_hub_errors_wrap_below_fields_and_above_actions() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    f.send(p, json!({"op":"test_settings_input","id":"settings_hub_url","value":"https://hub.example.com"}));
    control(&f, p, "submit_add_hub");
    inspect(&f, p);
    let submitted = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "setup_save_hub")
        .unwrap();
    let clean =
        "The Hub is unavailable. Check its address and network connection before trying again. "
            .repeat(3);
    f.send(p, json!({"op":"error","source_op":"setup_save_hub","request_id":submitted["request_id"],"error":format!("10071: {clean}")}));
    for appearance in ["light", "dark"] {
        let s = inspect_as(&f, p, Some(appearance));
        let error = node(&s, "settings_items", "add_hub_error");
        assert_eq!(error["text"], clean.trim());
        assert_eq!(error["tooltip"], clean.trim());
        assert_eq!(error["width"], 452);
        assert!(error["height"].as_f64().unwrap() > 15.0);
        let submit = node(&s, "settings_items", "submit_add_hub");
        assert!(
            submit["y"].as_f64().unwrap() + submit["height"].as_f64().unwrap()
                <= error["y"].as_f64().unwrap()
        );
    }
}

#[test]
fn v40_four_surface_captures_clear_confirmation_and_dynamic_accessibility() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    let mut cfg = settings();
    cfg["apps"][0]["display_name"] = json!("Pi Coding Agent");
    cfg["apps"][2]["id"] = json!("claude");
    cfg["apps"][2]["display_name"] = json!("Claude Code");
    cfg["channels"][0]["id"] = json!("telegram");
    cfg["channels"][0]["display_name"] = json!("Telegram");
    cfg["channels"][0]["pairing_manual_disclosure"] = json!(true);
    cfg["channels"][0]["fields"][0]["label"] = json!("Bot Token");
    cfg["channels"][0]["fields"][1]["label"] = json!("Chat ID");
    cfg["host_hub"] = json!({"enabled":true,"running":true,"port":4291,"endpoint_url":"http://192.0.2.10:4291","lan_url":"http://192.0.2.10:4291","token":"example-token","auth_mode":"token","invite_command":"cito setup --hub work --url http://192.0.2.10:4291 --default"});
    frame["settings"] = cfg.clone();
    frame["harnesses"][0]["id"] = json!("pi");
    frame["harnesses"][0]["display_name"] = json!("Pi");
    frame["listeners"][0]["harness"] = json!("pi");
    frame["listeners"][0]["display_address"] = json!("alice/pi#abcd");
    frame["listeners"][0]["title"] = json!("Release review");
    frame["listeners"][0]["listen_mode"] = json!("manual");
    frame["hubs"][0]["auto_listen"] = json!(true);
    frame["listeners"][0]["cwd"] = json!("~/work/service");
    frame["directory_online_count"] = json!(3);
    frame["hubs"][0]["human_listener_id"] = json!("human-node");
    frame["hubs"][0]["human_control_revision"] = json!(4);
    frame["hubs"][0]["human_state_status"] = json!("current");
    frame["hubs"][0]["human_paused"] = json!(false);
    frame["hubs"][0]["attachments"] = json!([{"channel_id":"hub:telegram","location":"hub","plugin_id":"telegram","display_name":"Telegram","availability":"ACTIVE","is_local":false},{"channel_id":"here:human","location":"device","host_id":"here","display_name":"laptop","availability":"ACTIVE","is_local":true}]);
    frame["unread_count"] = json!(2);
    frame["activity"]=json!([("review","The review is ready. Can you check the final changes?",true,true),("build","Build passed. All checks are green.",false,false),("reply","Approved. Please continue with the next step.",true,true)].into_iter().map(|(id,message,unread,for_me)|json!({"hub":"personal","signal_id":id,"listener_id":"human-node","from_user":"bob","from_host_name":"desktop","from_harness":"pi","from_title":"Release review","from_scoped_id":"personal:sender","from_display_address":"bob/pi#abcd","target_harness":if for_me{"human"}else{"pi"},"target_title":"Build session","target_scoped_id":"personal:receiver","message":message,"created_at":"2030-01-01T00:00:00Z","unread":unread,"for_me":for_me,"can_reply":for_me})).collect::<Vec<_>>());
    f.send(p, frame);
    let s = inspect(&f, p);
    assert!(s["button_tooltip"].as_str().unwrap().contains("unread"));
    let activity = node(&s, "items", "open_activity");
    assert!(!activity["tooltip"].as_str().unwrap().is_empty());
    assert!(nodes(&activity["children"])
        .iter()
        .all(|child| child["tooltip"].as_str().unwrap_or("").is_empty()));
    render_png_as(&f, p, "menu", "menu-dark.png", Some("dark"));
    control(&f, p, "open_directory");
    let mut schema = peer("schema");
    schema["harness"] = json!("pi");
    schema["display_address"] = json!("bob/pi#df0a");
    let mut review = peer("review");
    review["harness"] = json!("pi");
    review["display_address"] = json!("bob/pi#c97a");
    let mut inbox = peer("inbox");
    inbox["harness"] = json!("human");
    inbox["title"] = json!("Human");
    inbox["about"] = json!("Human");
    inbox["repo"] = Value::Null;
    inbox["branch"] = Value::Null;
    inbox["display_address"] = json!("@bob");
    inbox["attachments"] = json!([{"channel_id":"hub:telegram","location":"hub","plugin_id":"telegram","display_name":"Telegram","availability":"ACTIVE","is_local":false}]);
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[schema,review,inbox],"matched_count":3,"online_count":3}),
    );
    let directory = inspect(&f, p);
    assert_eq!(
        node(&directory, "directory_items", "directory_scope_track")["width"],
        129
    );
    assert_eq!(
        node(&directory, "directory_items", "directory_facet_separator")["x"],
        151
    );
    render_png_as(&f, p, "directory", "directory-dark.png", Some("dark"));
    control(&f, p, "copy_dir:personal:schema");
    let copied = inspect(&f, p);
    assert_eq!(
        node(&copied, "directory_items", "copy_dir:personal:schema")["accessibility_label"],
        "✓ Copied"
    );
    control(&f, p, "dir_mode:activity");
    control(&f, p, "act_filter:all");
    control(&f, p, "reply_activity:review");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Approved. Please continue."}),
    );
    let reply = inspect(&f, p);
    assert_eq!(reply["composer_open"], true);
    assert!(f
        .frames(p)
        .iter()
        .any(|v| v["op"] == "ack_signals" && v["signal_ids"] == json!(["review"])));
    render_png_as(&f, p, "directory", "activity-dark.png", Some("dark"));
    control(&f, p, "composer_cancel");
    inspect(&f, p);
    control(&f, p, "activity_clear");
    let confirm = inspect(&f, p);
    assert_eq!(
        node(&confirm, "directory_items", "activity_clear_confirm_label")["text"],
        "Clear 3 All Activity messages?"
    );
    assert!(!node(&confirm, "directory_items", "activity_clear_confirm_commit").is_null());
    control(&f, p, "activity_clear_confirm_cancel");
    inspect(&f, p);
    assert!(!f
        .frames(p)
        .iter()
        .any(|v| v["op"] == "clear_activity_history"));
    f.send(
        p,
        json!({"op":"test_directory_query","query":"Build passed"}),
    );
    inspect(&f, p);
    control(&f, p, "activity_clear");
    assert_eq!(
        node(
            &inspect(&f, p),
            "directory_items",
            "activity_clear_confirm_label"
        )["text"],
        "Clear 1 matching messages?"
    );
    control(&f, p, "activity_clear_confirm_cancel");
    f.send(p, json!({"op":"test_directory_query","query":""}));
    inspect(&f, p);
    control(&f, p, "activity_clear");
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    let cancelled = inspect(&f, p);
    assert_eq!(cancelled["directory_open"], true);
    assert!(node(
        &cancelled,
        "directory_items",
        "activity_clear_confirm_commit"
    )
    .is_null());
    control(&f, p, "activity_clear");
    control(&f, p, "activity_clear_confirm_commit");
    inspect(&f, p);
    let cleared = f
        .frames(p)
        .into_iter()
        .find(|v| v["op"] == "clear_activity_history")
        .unwrap();
    assert_eq!(cleared["filter"], "all");
    assert_eq!(cleared["query"], "");
    control(&f, p, "open_settings");
    for (tab, file) in [
        ("apps", "settings-apps-dark.png"),
        ("channels", "settings-channels-dark.png"),
        ("hubs", "settings-hubs-dark.png"),
    ] {
        control(&f, p, &format!("settings_tab:{tab}"));
        inspect(&f, p);
        render_png_as(&f, p, "settings", file, Some("dark"));
    }
}

#[test]
fn native_edit_shortcuts_use_real_field_editors_for_secure_text_search_and_composer() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(p, frame);
    f.send(
        p,
        json!({"op":"test_settings_key","key":"command-comma","via":"panel"}),
    );
    assert_eq!(
        inspect(&f, p)["settings_open"],
        false,
        "A missing panel must not fall through to the direct Settings handler"
    );
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "open_directory");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"matched_count":1}),
    );
    f.send(p, json!({"op":"test_tile_scroll_views"}));
    let s = inspect(&f, p);
    assert_eq!(
        s["key_panels"],
        json!([{
            "titled":true,"full_size_content":true,"hidden_title":true,"transparent_titlebar":true,"hidden_window_buttons":true,"automatic_key_view_loop":true
        }, {
            "titled":true,"full_size_content":true,"hidden_title":true,"transparent_titlebar":true,"hidden_window_buttons":true,"automatic_key_view_loop":true
        }])
    );
    assert_eq!(
        node(&s, "directory_items", "directory_scroll")["scroller_style"],
        1
    );
    assert_eq!(
        node(&s, "settings_items", "settings_channels_tab")["scroller_style"],
        1
    );
    assert_eq!(
        json!(&s["edit_menu"].as_array().unwrap()[..6]),
        json!([
            {"title":"Undo","key":"z","action":"undo:","shift":false},
            {"title":"Redo","key":"Z","action":"redo:","shift":true},
            {"title":"Cut","key":"x","action":"cut:","shift":false},
            {"title":"Copy","key":"c","action":"copy:","shift":false},
            {"title":"Paste","key":"v","action":"paste:","shift":false},
            {"title":"Select All","key":"a","action":"selectAll:","shift":false}
        ])
    );
    let focus = |id: &str| {
        f.send(p, json!({"op":"test_focus_field","id":id}));
        assert!(
            inspect(&f, p)["field_editors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|editor| editor["id"] == id),
            "{id} must own a native field editor"
        );
    };
    let key = |op: &str, key: &str, via: &str| {
        f.send(p, json!({"op":op,"key":key,"via":via}));
        let snapshot = inspect(&f, p);
        for editor in snapshot["field_editors"].as_array().unwrap() {
            assert_eq!(editor["attached"], true, "{key}: {editor}");
            assert_eq!(editor["automatic_groups"], true, "{key}: {editor}");
        }
        snapshot
    };
    let save_secret = |expected: &str| {
        let before = f.frames(p).len();
        control(&f, p, "save_hub_service:channel");
        let mut saved = Value::Null;
        wait(|| {
            saved = f
                .frames(p)
                .into_iter()
                .skip(before)
                .find(|a| a["op"] == "save_hub_service")
                .unwrap_or(Value::Null);
            saved.is_object()
        });
        assert_eq!(saved["values"]["secret"], expected);
    };
    focus("channel_field:channel:secret");
    f.send(
        p,
        json!({"op":"test_clipboard","text":"opaque secure value / keep spaces"}),
    );
    key("test_settings_key", "command-v", "panel");
    save_secret("opaque secure value / keep spaces");
    key("test_settings_key", "command-a", "direct");
    f.send(
        p,
        json!({"op":"test_clipboard","text":"replacement secure value + punctuation"}),
    );
    key("test_settings_key", "command-v", "panel");
    save_secret("replacement secure value + punctuation");
    key("test_settings_key", "command-z", "direct");
    save_secret("opaque secure value / keep spaces");
    key("test_settings_key", "shift-command-z", "panel");
    save_secret("replacement secure value + punctuation");
    key("test_settings_key", "command-a", "panel");
    f.send(p, json!({"op":"test_clipboard","text":"public clipboard"}));
    for command in ["command-c", "command-x"] {
        assert_eq!(
            key("test_settings_key", command, "panel")["clipboard"],
            "public clipboard"
        );
    }
    let unchanged = inspect(&f, p);
    assert_eq!(
        node(&unchanged, "settings_items", "save_hub_service:channel")["enabled"],
        false,
        "Clipboard shortcuts cannot resubmit an unchanged pending secret"
    );
    assert_eq!(
        node(&unchanged, "settings_items", "channel_field:channel:secret")["text"],
        "••••"
    );

    for (id, op, surface) in [
        (
            "channel_field:channel:account",
            "test_settings_key",
            "settings_items",
        ),
        ("directory_search", "test_directory_key", "directory_items"),
        ("composer_input", "test_directory_key", "directory_items"),
    ] {
        if id == "composer_input" {
            control(&f, p, "msg_dir:personal:one");
        }
        focus(id);
        let text = "café / ordinary text";
        f.send(p, json!({"op":"test_clipboard","text":"stale cached text"}));
        f.send(
            p,
            json!({"op":"test_clipboard","system_only":true,"text":text}),
        );
        assert_eq!(
            node(&key(op, "command-v", "panel"), surface, id)["text"],
            text
        );
        let selected = key(op, "command-a", "direct");
        let editor = selected["field_editors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .unwrap();
        assert_eq!(
            editor["selection_length"],
            text.encode_utf16().count(),
            "{id}: {editor}"
        );
        assert_eq!(editor["editable"], true, "{id}: {editor}");
        f.send(
            p,
            json!({"op":"test_clipboard","text":"copy must replace this sentinel"}),
        );
        let copied = key(op, "command-c", "panel");
        assert_eq!(copied["clipboard"], text);
        let editor = copied["field_editors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .unwrap();
        assert_eq!(
            editor["selection_length"],
            text.encode_utf16().count(),
            "{id}: {editor}"
        );
        assert_eq!(
            node(&key(op, "command-x", "panel"), surface, id)["text"],
            "",
            "{id}"
        );
        assert_eq!(
            node(&key(op, "command-z", "direct"), surface, id)["text"],
            text,
            "{id}"
        );
        assert_eq!(
            node(&key(op, "shift-command-z", "panel"), surface, id)["text"],
            ""
        );
    }
    f.send(
        p,
        json!({"op":"test_clipboard","text":"Final café message"}),
    );
    key("test_directory_key", "command-v", "panel");
    let before = f.frames(p).len();
    // A panel key-equivalent miss must not fall through to the direct Return handler.
    key("test_directory_key", "return", "panel");
    assert!(!f
        .frames(p)
        .iter()
        .skip(before)
        .any(|a| a["op"] == "send_signal"));
    control(&f, p, "composer_send");
    wait(|| {
        f.frames(p)
            .iter()
            .skip(before)
            .any(|a| a["op"] == "send_signal" && a["message"] == "Final café message")
    });
    assert_eq!(
        key("test_settings_key", "command-w", "panel")["settings_open"],
        false
    );
    assert_eq!(
        key("test_directory_key", "command-comma", "panel")["settings_open"],
        true
    );
}

#[test]
fn r1_r5_real_daemon_ui_closed_loop_gateway_personal_errors_and_retina_states() {
    let _lock = serial();
    let f = Fixture::new(json!({"host_hub":{"bind":"127.0.0.1"}}));
    f.provider("delivered_unconfirmed");
    let plugin = f.state.join("plugins/channel");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"),json!({"id":"channel","kind":"human-channel","scope":"hub","displayName":"Messenger","setupGuide":["Create a gateway credential with your provider.","Save it once, then pair your personal account."],
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],"pairing":{"supported":true},"fields":[
        {"key":"credential","label":"Gateway Token","scope":"hub","type":"secret","required":true},
        {"key":"address","label":"Personal Address","scope":"user","type":"string","required":true}]}).to_string()).unwrap();
    let mut l = ClosedLoop::new(&f);
    l.until(|s| node(s, "items", "header").is_object());
    l.control("open_settings");
    l.until(|s| s["settings_open"] == true);
    l.control("settings_tab:channels");
    let s = l.until(|s| !node(s, "settings_items", "save_hub_service:channel").is_null());
    assert!(node(&s, "settings_items", "stratum_user:channel").is_null());
    for id in [
        "channel_field:channel:address",
        "pair_channel:channel",
        "test_channel:channel",
        "save_channel:channel",
    ] {
        assert!(node(&s, "settings_items", id).is_null(), "{id}");
    }
    let before = f.frames(l.ui).len();
    for id in [
        "pair_channel:channel",
        "test_channel:channel",
        "save_channel:channel",
    ] {
        l.control(id);
    }
    inspect(&f, l.ui);
    assert_eq!(
        f.frames(l.ui).len(),
        before,
        "Disabled controls cannot emit actions"
    );
    l.snapshot("settings", "r1_gateway_unconfigured.png");
    l.input("channel_field:channel:credential", "gateway-one");
    l.control("save_hub_service:channel");
    l.until(|s| node(s, "settings_items", "stratum_hub:channel")["height"] == 36);
    assert!(f
        .db("hub.sqlite")
        .query(
            "SELECT * FROM hub_human_channels WHERE plugin_id='channel'",
            &[]
        )
        .unwrap()
        .is_empty());
    assert_eq!(f.db("hub.sqlite").query("SELECT json_extract(config_json,'$.credential') AS credential FROM hub_plugin_services WHERE plugin_id='channel'",&[]).unwrap()[0]["credential"],"gateway-one");
    l.snapshot("settings", "r1_gateway_active_collapsed.png");
    l.control("edit_hub_service:channel");
    l.until(|s| !node(s, "settings_items", "channel_field:channel:credential").is_null());
    l.snapshot("settings", "r1_gateway_active_expanded.png");
    l.control("save_hub_service:channel");
    l.until(|s| node(s, "settings_items", "stratum_hub:channel")["height"] == 36);
    assert_eq!(
        l.results
            .iter()
            .rev()
            .find(|(a, _, _)| a["op"] == "save_hub_service")
            .unwrap()
            .0["values"],
        json!({}),
        "Acknowledged secrets must not be resubmitted"
    );
    // Submit both scopes before pumping the daemon, then request a background
    // settings snapshot. Both request acknowledgements must reach the real UI.
    let frame_start = l.frames.len();
    l.control("edit_hub_service:channel");
    l.input("channel_field:channel:credential", "gateway-overlap");
    l.control("save_hub_service:channel");
    l.input("channel_field:channel:address", "personal-room");
    l.control("save_channel:channel");
    let mut saves = Vec::new();
    wait(|| {
        saves = f
            .frames(l.ui)
            .into_iter()
            .filter(|a| {
                (a["op"] == "save_hub_service"
                    && a["values"] == json!({"credential":"gateway-overlap"}))
                    || (a["op"] == "channel_save" && a["values"]["address"] == "personal-room")
            })
            .collect();
        saves.len() == 2
    });
    assert_ne!(saves[0]["request_id"], saves[1]["request_id"]);
    l.control("open_settings");
    wait(|| {
        l.pump();
        let frames = &l.frames[frame_start..];
        frames.iter().enumerate().any(|(index, frame)| {
            frame["op"] == "settings"
                && frame["hub"] == "local"
                && frame["request_id"].is_null()
                && frame["source_op"].is_null()
                && saves.iter().all(|save| {
                    frames[..index].iter().any(|ack| {
                        ack["op"] == "settings" && ack["request_id"] == save["request_id"]
                    })
                })
        })
    });
    let s = inspect(&f, l.ui);
    assert_eq!(
        node(&s, "settings_items", "toggle_channel:channel")["text"],
        "Pause Messenger"
    );
    assert_eq!(
        node(&s, "settings_items", "stratum_hub:channel")["height"],
        36
    );
    l.control("toggle_channel:channel");
    l.until(|s| node(s, "settings_items", "toggle_channel:channel")["text"] == "Resume Messenger");
    let paused = f
        .db("hub.sqlite")
        .query(
            "SELECT * FROM hub_human_channels WHERE plugin_id='channel'",
            &[],
        )
        .unwrap();
    l.control("edit_hub_service:channel");
    l.until(|s| !node(s, "settings_items", "channel_field:channel:credential").is_null());
    l.input("channel_field:channel:credential", "gateway-two");
    l.control("save_hub_service:channel");
    l.until(|s| node(s, "settings_items", "stratum_hub:channel")["height"] == 36);
    assert_eq!(
        f.db("hub.sqlite")
            .query(
                "SELECT * FROM hub_human_channels WHERE plugin_id='channel'",
                &[]
            )
            .unwrap(),
        paused,
        "Gateway rotation cannot change/resume the personal binding"
    );
    l.control("pair_channel:channel");
    l.until(|s| !node(s, "settings_items", "pairing_instruction:channel").is_null());
    l.snapshot("settings", "r1_pairing_real_code.png");
    l.control("test_channel:channel");
    l.until(|s| {
        node(s, "settings_items", "settings_channel_detail:channel")["text"]
            == "Fixture test succeeded"
    });

    l.control("settings_tab:hubs");
    l.control("open_add_hub");
    l.control("submit_add_hub");
    l.until(|s| {
        node(s, "settings_items", "add_hub_error")["text"]
            == "Hub URL must start with http:// or https://."
    });
    l.snapshot("settings", "r3_add_hub_validation.png");
    l.input("settings_hub_name", "Office Mac");
    l.input("settings_hub_url", "http://");
    l.control("submit_add_hub");
    l.until(|s| {
        node(s, "settings_items", "add_hub_error")["text"]
            == "Enter a Hub URL using HTTP or HTTPS, without credentials, query, or fragment."
    });
    l.snapshot("settings", "r3_add_hub_daemon_error.png");
    for (name, url, canonical) in [
        ("Office Mac", "http://127.0.0.1:4291/", "office-mac"),
        ("", "  https://studio.example.com/  ", "studio"),
    ] {
        if name.is_empty() {
            l.control("open_add_hub");
        }
        l.input("settings_hub_name", name);
        l.input("settings_hub_url", url);
        l.control("submit_add_hub");
        l.until(|s| {
            node(s, "settings_items", "add_hub_form").is_null()
                && !node(s, "settings_items", &format!("settings_hub:{canonical}")).is_null()
        });
        let (action, status, _) = l
            .results
            .iter()
            .rev()
            .find(|(action, _, _)| action["op"] == "setup_save_hub" && action["name"] == canonical)
            .unwrap();
        assert_eq!(*status, 200);
        assert!(action["request_id"].is_string());
        assert!(l.frames.iter().any(|frame| frame["op"] == "settings"
            && frame["ok"] == true
            && frame["source_op"] == action["op"]
            && frame["request_id"] == action["request_id"]));
        l.control(&format!("remove_hub:{canonical}"));
        l.until(|s| node(s, "settings_items", &format!("settings_hub:{canonical}")).is_null());
    }
    l.control("open_add_hub");
    l.input("settings_hub_name", "scratch");
    l.input("settings_hub_url", &format!("http://127.0.0.1:{}", port()));
    l.control("submit_add_hub");
    l.until(|s| {
        node(s, "settings_items", "add_hub_form").is_null()
            && !node(s, "settings_items", "settings_hub:scratch").is_null()
    });
    // The same form remains open through validation and daemon errors, and closes
    // only on a real settings/state acknowledgement of the saved Hub.
    l.control("toggle_host_hub");
    l.until(|s| node(s, "settings_items", "toggle_host_hub")["text"] == "Stop Sharing");
    l.input("host_hub_port", &port().to_string());
    l.control("apply_host_hub_port");
    l.until(|s| node(s, "settings_items", "apply_host_hub_port")["text"] == "✓ Saved");
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = occupied.local_addr().unwrap().port().to_string();
    l.input("host_hub_port", &busy);
    l.control("apply_host_hub_port");
    let failed_port = l.until(|s| !node(s, "settings_items", "settings_error_banner").is_null());
    assert_eq!(
        node(&failed_port, "settings_items", "host_hub_error")["text"],
        "Cannot start sharing: Address already in use"
    );
    assert!(
        node(&failed_port, "settings_items", "host_hub_error")["tooltip"]
            .as_str()
            .unwrap()
            .contains("Cannot start sharing:")
    );
    l.snapshot("settings", "r3_settings_error_banner.png");
    l.control("settings_tab:apps");
    l.control("settings_tab:hubs");
    let s = l.until(|s| node(s, "settings_items", "host_hub_port")["text"] == busy);
    assert_eq!(node(&s, "settings_items", "host_hub_port")["text"], busy);
    l.control("dismiss_settings_error");
    l.until(|s| node(s, "settings_items", "settings_error_banner").is_null());
    l.control("paste_hub_invite");
    l.until(|s| {
        node(s, "settings_items", "settings_error_label")["text"]
            .as_str()
            .is_some_and(|t| t.contains("Clipboard"))
    });
    l.control("dismiss_settings_error");
    // JSON invites use the daemon parser. CLI/URL formats are also exercised by
    // the invite round-trip and parser tests, without connecting to real services.
    f.send(l.ui,json!({"op":"test_clipboard","text":json!({"name":"pasted","url":format!("http://127.0.0.1:{}",port())}).to_string()}));
    l.control("paste_hub_invite");
    let joined = l.until(|s| !node(s, "settings_items", "settings_hub:pasted").is_null());
    assert_eq!(
        node(&joined, "settings_items", "paste_hub_invite")["text"],
        "✓ Joined"
    );
    l.control("remove_hub:pasted");
    l.until(|s| node(s, "settings_items", "settings_hub:pasted").is_null());
    l.control("remove_hub:scratch");
    l.until(|s| node(s, "settings_items", "settings_hub:scratch").is_null());
    l.control("close_settings");
    l.until(|s| s["settings_open"] == false);

    // A real plugin discovery response travels through SessionActivity, snapshot,
    // and the UI, including mixed 36/46-point rows in the scroll document.
    let options = f.state.join("plugins/fixture/options.json");
    let sessions:Vec<Value>=(0..5).map(|i|json!({"harness":"fixture","session_id":format!("reload-{i}"),"title":"Work session","cwd":"/tmp","needs_reload":i%2==0,"reload_hint":"Run /reload in Fixture to enable 1-click listen"})).collect();
    fs::write(&options,json!({"session_id":"session-1","status":"alive","outcome":"delivered_unconfirmed","sessions":sessions}).to_string()).unwrap();
    assert_eq!(
        f.action(json!({"op":"setup_apply","harness":"fixture"})).0,
        200
    );
    l.until(|s| !node(s, "items", "reload:fixture:reload-0").is_null());
    l.snapshot("menu", "r4_needs_reload_candidates.png");
    l.control("reload:fixture:reload-0");
    l.until(|s| s["clipboard"] == "/reload");
    l.control("listen:fixture:reload-1");
    l.until(|s| !node(s, "items", "menu_error_row").is_null());
    l.snapshot("menu", "r3_menu_error.png");
    l.control("dismiss_menu_error");
    l.until(|s| node(s, "items", "menu_error_row").is_null());

    l.control("open_directory");
    let s = l.until(|s| {
        s["directory_open"] == true && s["directory_matched_count"].as_u64().is_some_and(|n| n > 0)
    });
    l.snapshot("directory", "r5_real_directory.png");
    let msg = nodes(&s["directory_items"])
        .into_iter()
        .find_map(|n| {
            n["id"]
                .as_str()
                .filter(|id| id.starts_with("msg_dir:"))
                .map(str::to_string)
        })
        .unwrap();
    l.control(&msg);
    l.until(|s| s["composer_open"] == true);
    f.send(
        l.ui,
        json!({"op":"test_composer_input","value":"Closed-loop message"}),
    );
    l.control("composer_send");
    let s = l.until(|s| {
        node(s, "directory_items", "composer_error")["text"]
            .as_str()
            .is_some_and(|t| t.contains("Cannot send to your own session"))
    });
    assert_eq!(
        node(&s, "directory_items", "composer_input")["text"],
        "Closed-loop message"
    );
    l.snapshot("directory", "r3_real_composer_error.png");
    l.control("composer_cancel");
    assert_eq!(
        f.action(json!({"op":"listen_session","harness":"fixture","session_id":"session-1"}))
            .0,
        200
    );
    f.send(l.ui, json!({"op":"test_directory_query","query":""}));
    let target = f
        .rows("SELECT public_id FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
        [0]["public_id"]
        .as_str()
        .unwrap()
        .to_string();
    let message = format!("msg_dir:{target}");
    l.until(|s| !node(s, "directory_items", &message).is_null());
    l.control(&message);
    l.until(|s| s["composer_open"] == true);
    f.send(
        l.ui,
        json!({"op":"test_composer_input","value":"Closed-loop delivery"}),
    );
    l.control("composer_send");
    l.until(|s| s["composer_open"] == false);
    l.snapshot("directory", "r5_real_composer_acknowledged.png");
    for op in [
        "state",
        "settings",
        "directory",
        "pair_code",
        "channel_test_result",
        "composer_result",
        "error",
    ] {
        assert!(
            l.frames.iter().any(|frame| frame["op"] == op),
            "Missing real daemon frame: {op}"
        );
    }
    // Both Apply submissions and the sharing toggle own correlated receipts.
    assert_eq!(
        l.results
            .iter()
            .filter(
                |(action, _, _)| action["op"] == "set_host_hub" && action["request_id"].is_string()
            )
            .count(),
        3
    );
    // Command-specific results and generic failures must retain the UI's identity.
    for (action, status, _) in &l.results {
        let result_op = match action["op"].as_str() {
            Some("send_signal" | "send_reply_from_human") => "composer_result",
            Some("set_host_hub") if action["request_id"].is_string() => "settings",
            _ => continue,
        };
        assert!(!action["request_id"].as_str().unwrap().is_empty());
        assert!(l
            .frames
            .iter()
            .any(|frame| frame["op"] == result_op && frame["request_id"] == action["request_id"]));
        if *status >= 400 {
            assert!(l.frames.iter().any(|frame| frame["op"] == "error"
                && frame["source_op"] == action["op"]
                && frame["request_id"] == action["request_id"]));
        }
    }
    for (name, op) in [("office-mac", "error"), ("scratch", "settings")] {
        let (action, _, _) = l
            .results
            .iter()
            .find(|(action, _, _)| action["op"] == "setup_save_hub" && action["name"] == name)
            .unwrap();
        assert!(!action["request_id"].as_str().unwrap().is_empty());
        assert!(l.frames.iter().any(|frame| frame["op"] == op
            && frame["source_op"] == action["op"]
            && frame["request_id"] == action["request_id"]));
    }
}

#[test]
fn u3_u11_visual_states_strata_navigation_labels_and_retina_snapshots() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("local");
    cfg["hubs"][1]["url"] = json!("https://cito.example.com");
    cfg["hubs"][1]["auth_mode"] = json!("oidc");
    cfg["channels"][0]["status"] = json!("not_configured");
    cfg["channels"][0]["hub_ready"] = json!(false);
    cfg["channels"][0]["fields"][0]["configured"] = json!(false);
    cfg["channels"][0]["fields"][0]["value"] = json!("");
    cfg["channels"][0]["setup_guide"] = json!([
        "1. Create a gateway credential with your provider.",
        "2. Save it once for everyone on this Hub."
    ]);
    cfg["hubs"][0]["name"] = json!("local");
    let mut frame = state();
    frame["hubs"][0]["name"] = json!("local");
    frame["settings"] = cfg.clone();
    frame["listeners"] = json!([]);
    for hub in frame["hubs"].as_array_mut().unwrap() {
        hub["human_paused"] = json!(true);
        hub["human_state_status"] = json!("current");
        hub["human_control_revision"] = json!(7);
        hub["human_availability"] = json!("UNLISTENED");
    }
    frame["available"] =
        json!([{"harness":"fixture","session_id":"s1","title":"Work session","cwd":"/tmp"}]);
    f.send(p, frame.clone());
    control(&f, p, "inbox_paused_notice");
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "inbox_control_bar")["height"],
        48
    );
    assert_eq!(
        node(&s, "settings_items", "toggle_inbox")["text"],
        "Resume Inbox"
    );
    assert_eq!(node(&s, "settings_items", "toggle_inbox")["enabled"], true);
    render_png(&f, p, "menu", "menu_human_paused.png");
    let rendered = inspect(&f, p);
    for item in nodes(&s["items"]) {
        if let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) {
            let restored = node(&rendered, "items", id);
            for coordinate in ["x", "y", "width", "height"] {
                assert_eq!(
                    restored[coordinate], item[coordinate],
                    "Rendering must restore {id}.{coordinate}"
                );
            }
        }
    }
    for (status, expected) in [
        ("auth_required", "Sign in required"),
        ("connecting", "Connecting…"),
        ("unreachable", "Hub unavailable"),
        ("error", "Hub unavailable"),
    ] {
        let mut unavailable = frame.clone();
        unavailable["hubs"][0]["status"] = json!(status);
        f.send(p, unavailable);
        control(&f, p, "target_hub:local");
        let snapshot = inspect(&f, p);
        assert_eq!(
            node(&snapshot, "items", "header_subtitle")["text"],
            expected
        );
        assert_eq!(
            node(&snapshot, "settings_items", "toggle_inbox")["enabled"],
            false
        );
        assert_eq!(
            node(&snapshot, "items", "inbox_paused_notice")["height"],
            24
        );
        render_png(&f, p, "menu", &format!("inbox_{status}.png"));
    }
    // Resume uses shared policy CAS and never sends a replacement description.
    for about in [
        Value::Null,
        json!("Human"),
        json!("You (@alice)"),
        json!("Release approvals"),
        json!("café notes"),
    ] {
        let mut paused = frame.clone();
        paused["hubs"][0]["human_about"] = about;
        f.send(p, paused.clone());
        assert_eq!(
            node(&inspect(&f, p), "items", "inbox_paused_notice")["height"],
            24
        );
        control(&f, p, "toggle_inbox");
        inspect(&f, p);
        let resume = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "human_inbox_update")
            .unwrap();
        assert_eq!(resume["hub"], "local");
        assert_eq!(resume["paused"], false);
        assert_eq!(resume["expected_control_revision"], 7);
        assert!(resume.get("about").is_none());
        paused["hubs"][0]["human_paused"] = json!(false);
        paused["hubs"][0]["human_control_revision"] = json!(8);
        paused["hubs"][0]["human_availability"] = json!("ACTIVE");
        paused["source_op"] = json!("human_inbox_update");
        paused["request_id"] = resume["request_id"].clone();
        f.send(p, paused);
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "inbox_control_bar")["height"],
            48
        );
    }
    f.send(p, frame.clone());
    control(&f, p, "target_hub:work");
    control(&f, p, "inbox_paused_notice");
    control(&f, p, "toggle_inbox");
    inspect(&f, p);
    assert!(f.frames(p).iter().any(|a| a["op"] == "human_inbox_update"
        && a["hub"] == "work"
        && a["paused"] == false
        && a["expected_control_revision"] == 7));
    control(&f, p, "open_settings");
    control(&f, p, "channel_hub_selector:local");
    control(&f, p, "settings_tab:apps");
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "onboarding_banner").is_null());
    assert!(node(&s, "settings_items", "apps_next_step_callout").is_null());
    let tier = node(&s, "settings_items", "settings_app_tier:cursor");
    assert_eq!(tier["text"], "Experimental");
    assert!(tier["width"].as_f64().unwrap() < 88.0);
    render_png(&f, p, "settings", "settings_apps.png");
    for tab in ["apps", "channels", "hubs"] {
        control(&f, p, &format!("settings_tab:{tab}"));
        let s = inspect(&f, p);
        assert!(node(&s, "settings_items", "onboarding_banner").is_null());
        assert_eq!(
            node(&s, "settings_items", &format!("settings_{tab}_tab"))["height"],
            368
        );
    }
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "onboarding_banner").is_null());
    assert_eq!(
        node(&s, "settings_items", "settings_channel_status:channel")["text"],
        "Not Set Up"
    );
    let hub = node(&s, "settings_items", "stratum_hub:channel");
    assert!(node(&s, "settings_items", "stratum_user:channel").is_null());
    assert!(nodes(&hub["children"])
        .iter()
        .any(|n| n["id"] == "channel_field:channel:secret"));
    assert!(!nodes(&hub["children"])
        .iter()
        .any(|n| n["id"] == "channel_field:channel:account"));
    assert_eq!(
        node(&s, "settings_items", "stratum_hub_title:channel")["text"],
        "1. Shared Hub Service"
    );
    assert!(node(&s, "settings_items", "stratum_user_title:channel").is_null());
    render_png(&f, p, "settings", "settings_channels_unconfigured.png");
    f.send(p, json!({"op":"test_settings_scroll","offset":0}));
    render_png(&f, p, "settings", "settings_channels_unconfigured_node.png");
    f.send(p, json!({"op":"test_settings_scroll","offset":100000}));
    let before = f.frames(p).len();
    control(&f, p, "skip_channel:channel");
    assert_eq!(inspect(&f, p)["settings_tab"], "channels");
    assert_eq!(
        f.frames(p).len(),
        before,
        "Hidden prerequisite actions cannot mutate Hub state"
    );
    control(&f, p, "settings_tab:apps");
    assert!(node(&inspect(&f, p), "settings_items", "apps_next_step_callout").is_null());
    cfg["channels"][0]["status"] = json!("pairing_pending");
    cfg["channels"][0]["hub_ready"] = json!(true);
    cfg["channels"][0]["fields"][0]["configured"] = json!(true);
    cfg["channels"][0]["pairing_instruction"] =
        json!("Send /start cito-abc123def4 to your account provider.");
    cfg["channels"][0]["pairing_command"] = json!("/start cito-abc123def4");
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "stratum_hub_status:channel")["text"],
        "Service Ready"
    );
    assert!(!node(&s, "settings_items", "clear_channel_service:channel").is_null());
    assert!(
        nodes(&node(&s, "settings_items", "stratum_user:channel")["children"])
            .iter()
            .any(|n| n["id"] == "pairing_instruction:channel")
    );
    let copy = node(&s, "settings_items", "copy_pair_command:channel");
    assert!(
        copy["label_measured_width"].as_f64().unwrap() <= copy["label_width"].as_f64().unwrap()
    );
    render_png(&f, p, "settings", "settings_channels_pairing.png");
    f.send(p, json!({"op":"test_settings_scroll","offset":0}));
    render_png(&f, p, "settings", "settings_channels_pairing_node.png");
    f.send(p, json!({"op":"test_settings_scroll","offset":100000}));
    control(&f, p, "copy_pair_command:channel");
    assert_eq!(inspect(&f, p)["clipboard"], "/start cito-abc123def4");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["pairing_instruction"] = json!("");
    cfg["channels"][0]["pairing_command"] = json!("");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("account-42");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let paired = inspect(&f, p);
    assert_eq!(
        node(&paired, "settings_items", "stratum_user:channel")["height"],
        38
    );
    assert!(node(&paired, "settings_items", "channel_field:channel:account").is_null());
    assert_eq!(
        node(&paired, "settings_items", "channel_summary:channel")["text"],
        "Paired · account-42"
    );
    control(&f, p, "edit_user_channel:channel");
    assert!(!node(
        &inspect(&f, p),
        "settings_items",
        "channel_field:channel:account"
    )
    .is_null());
    control(&f, p, "save_channel:channel");
    wait(|| f.frames(p).iter().any(|v| v["op"] == "channel_save"));
    let save = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "channel_save")
        .unwrap();
    assert!(!node(&inspect(&f, p), "settings_items", "save_channel:channel").is_null());
    f.send(p,json!({"op":"settings","ok":true,"source_op":"channel_save","channel_id":"channel","hub":save["hub"],"request_id":save["request_id"],"settings":cfg}));
    let saved = inspect(&f, p);
    assert_eq!(
        node(&saved, "settings_items", "stratum_hub_title:channel")["text"],
        "Hub Service"
    );
    assert_eq!(
        node(&saved, "settings_items", "stratum_hub_status:channel")["hidden"],
        true
    );
    for id in [
        "edit_hub_service",
        "clear_channel_service",
        "test_channel",
        "toggle_channel",
        "edit_user_channel",
        "unpair_channel",
    ] {
        let button = node(&saved, "settings_items", &format!("{id}:channel"));
        assert!(
            button["label_width"].as_f64().unwrap()
                >= button["label_measured_width"].as_f64().unwrap(),
            "{id}: {button}"
        );
    }
    assert_eq!(
        node(&saved, "settings_items", "clear_channel_service:channel")["foreground"],
        node(&saved, "settings_items", "edit_hub_service:channel")["foreground"]
    );
    assert_eq!(
        node(&saved, "settings_items", "channel_summary:channel")["text"],
        "✓ Saved"
    );
    assert!(node(&saved, "settings_items", "save_channel:channel").is_null());
    let reserved_toggle = node(&saved, "settings_items", "toggle_channel:channel");
    for enabled in [false, true] {
        cfg["channels"][0]["enabled"] = json!(enabled);
        f.send(p, json!({"op":"settings","settings":cfg}));
        let toggled = node(&inspect(&f, p), "settings_items", "toggle_channel:channel");
        assert_eq!(
            toggled["text"],
            if enabled {
                "Pause Human Channel"
            } else {
                "Resume Human Channel"
            }
        );
        for dimension in ["x", "width"] {
            assert_eq!(
                toggled[dimension], reserved_toggle[dimension],
                "{dimension}"
            );
        }
    }
    render_png(&f, p, "settings", "settings_channels_paired.png");
    // An active pairing command always expands even an already configured account.
    cfg["channels"][0]["pairing_instruction"] = json!("Send /start cito-abc123def4 to @bot");
    cfg["channels"][0]["pairing_command"] = json!("/start cito-abc123def4");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert!(!node(
        &inspect(&f, p),
        "settings_items",
        "pairing_instruction:channel"
    )
    .is_null());
    // Non-admins see their personal account without a shared-service wizard.
    cfg["channels"][0]["hub_provided"] = json!(true);
    cfg["channels"][0]["can_configure_hub"] = json!(false);
    cfg["channels"][0]["can_clear_hub"] = json!(false);
    cfg["channels"][0]["fields"][0]["hidden"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "channel_field:channel:secret").is_null());
    assert!(node(&s, "settings_items", "clear_channel_service:channel").is_null());
    assert!(node(&s, "settings_items", "stratum_hub:channel").is_null());
    render_png(&f, p, "settings", "r1_gateway_non_admin.png");
    let channel = cfg["channels"][0].clone();
    cfg["channels"][0]["status"] = json!("error");
    cfg["channels"][0]["configured"] = json!(false);
    cfg["channels"][0]["fields"] = json!([]);
    cfg["channels"][0]["detail"] = json!("Unable to load channel settings.");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let unavailable = inspect(&f, p);
    assert!(node(&unavailable, "settings_items", "stratum_hub:channel").is_null());
    assert!(node(&unavailable, "settings_items", "save_channel:channel").is_null());
    assert_eq!(
        node(
            &unavailable,
            "settings_items",
            "settings_channel_detail:channel"
        )["text"],
        "Unable to load channel settings."
    );
    cfg["channels"][0] = channel;
    cfg["host_hub"]["auth_mode"] = json!("token");
    cfg["host_hub"]["token"] = json!("12345678abcdef0123456789");
    cfg["host_hub"]["enabled"] = json!(true);
    cfg["host_hub"]["endpoint_url"] = json!("http://mac-studio.tailnet.ts.net:4291");
    cfg["host_hub"]["tailscale_url"] = cfg["host_hub"]["endpoint_url"].clone();
    cfg["host_hub"]["tailscale_invite_command"] =
        json!("cito setup --hub mac-studio --url http://mac-studio.tailnet.ts.net:4291");
    cfg["host_hub"]["invite_command"] = cfg["host_hub"]["tailscale_invite_command"].clone();
    cfg["host_hub"]["lan_url"] = json!("http://192.168.1.10:4291");
    cfg["host_hub"]["lan_invite_command"] =
        json!("cito setup --hub mac-studio --url http://192.168.1.10:4291");
    cfg["hubs"][1]["auth_mode"] = json!("oidc");
    cfg["hubs"][1]["status"] = json!("auth_required");
    cfg["hubs"][1]["can_login"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "settings_tab:hubs");
    let sharing_toggle = node(&inspect(&f, p), "settings_items", "toggle_host_hub");
    for (running, error) in [
        (false, None),
        (false, Some("Address already in use")),
        (false, Some("")),
        (true, Some("")),
        (true, Some("Hub listener failed")),
        (true, None),
    ] {
        let failure = error.filter(|message| !message.is_empty());
        cfg["host_hub"]["running"] = json!(running);
        cfg["host_hub"]["error"] = json!(error);
        f.send(p, json!({"op":"settings","settings":cfg}));
        let status = inspect(&f, p);
        let toggle = node(&status, "settings_items", "toggle_host_hub");
        assert_eq!(toggle["text"], "Stop Sharing");
        let dot = node(&status, "settings_items", "host_hub_status");
        assert_eq!(dot["width"], 6);
        assert_eq!(
            dot["tooltip"],
            failure.unwrap_or(if running {
                "Sharing is running"
            } else {
                "Sharing is starting"
            })
        );
        assert_eq!(toggle["width"], sharing_toggle["width"]);
        assert_eq!(toggle["x"], sharing_toggle["x"]);
        let prominent = node(&status, "settings_items", "settings_tab:hubs")["background"].clone();
        let invite = node(&status, "settings_items", "copy_hub_invite");
        if running && failure.is_none() {
            assert_ne!(toggle["background"], prominent);
            assert_eq!(invite["background"], prominent);
        } else {
            assert_ne!(invite["background"], prominent);
            if failure.is_none() {
                assert_ne!(toggle["background"], prominent);
            }
        }
        let warning = node(&status, "settings_items", "host_hub_error");
        if !running && error.is_none() {
            render_png(&f, p, "settings", "r3_host_hub_starting.png");
        }
        if error == Some("Address already in use") {
            render_png(&f, p, "settings", "r3_host_hub_failed.png");
        }
        if let Some(failure) = failure {
            assert_eq!(warning["text"], failure);
        } else {
            assert!(warning.is_null());
            let title = node(&status, "settings_items", "host_hub_title");
            assert_eq!(
                title["text"].as_str().unwrap().contains("Starting…"),
                !running
            );
        }
        if !running || failure.is_some() {
            assert!(
                node(&status, "settings_items", "copy_hub_invite")["tooltip"]
                    .as_str()
                    .unwrap()
                    .contains("unavailable")
            );
        }
    }
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"4292"}),
    );
    cfg["host_hub"]["enabled"] = json!(false);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let off = inspect(&f, p);
    let toggle = node(&off, "settings_items", "toggle_host_hub");
    assert_eq!(toggle["text"], "Start Sharing");
    for dimension in ["x", "width"] {
        assert_eq!(toggle[dimension], sharing_toggle[dimension]);
    }
    assert_eq!(
        node(&off, "settings_items", "host_hub_error")["text"],
        "Choose Start Sharing to accept connections"
    );
    let off_invite = node(&off, "settings_items", "copy_hub_invite");
    assert_eq!(
        off_invite["tooltip"],
        "Enable sharing and apply the port before inviting other Macs"
    );
    assert_eq!(off_invite["background"], toggle["background"]);
    render_png(&f, p, "settings", "r3_host_hub_off_with_draft.png");
    cfg["host_hub"]["error"] = json!("Port already in use");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let off_error = inspect(&f, p);
    assert_eq!(
        node(&off_error, "settings_items", "toggle_host_hub")["text"],
        "Start Sharing"
    );
    assert_eq!(
        node(&off_error, "settings_items", "host_hub_error")["text"],
        "Port already in use"
    );
    cfg["host_hub"]["error"] = Value::Null;
    cfg["host_hub"]["enabled"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    for (hub, trailing) in [("local", "toggle_hub:local"), ("work", "remove_hub:work")] {
        let card = node(&s, "settings_items", &format!("settings_hub:{hub}"));
        let button = node(&s, "settings_items", trailing);
        let edit = node(&s, "settings_items", &format!("edit_hub:{hub}"));
        assert_eq!(
            button["x"].as_f64().unwrap() + button["width"].as_f64().unwrap(),
            card["width"].as_f64().unwrap() - 12.0
        );
        let toggle = node(&s, "settings_items", &format!("toggle_hub:{hub}"));
        assert_eq!(
            edit["x"].as_f64().unwrap() + edit["width"].as_f64().unwrap() + 6.0,
            toggle["x"].as_f64().unwrap()
        );
        if hub == "work" {
            assert_eq!(
                toggle["x"].as_f64().unwrap() + toggle["width"].as_f64().unwrap() + 6.0,
                button["x"].as_f64().unwrap()
            );
        }
    }
    let host = node(&s, "settings_items", "host_hub_card");
    let toggle = node(&s, "settings_items", "toggle_host_hub");
    assert_eq!(
        host["height"].as_f64().unwrap()
            - toggle["y"].as_f64().unwrap()
            - toggle["height"].as_f64().unwrap(),
        10.0
    );
    let login = node(&s, "settings_items", "login_hub:work");
    let edit = node(&s, "settings_items", "edit_hub:work");
    assert_eq!(
        login["x"].as_f64().unwrap() + login["width"].as_f64().unwrap() + 6.0,
        edit["x"].as_f64().unwrap()
    );
    let token = node(&s, "settings_items", "copy_host_hub_token");
    assert!(!token.is_null());
    let apply = node(&s, "settings_items", "apply_host_hub_port");
    let access = node(&s, "settings_items", "toggle_host_hub_auth");
    assert!(
        apply["x"].as_f64().unwrap() + apply["width"].as_f64().unwrap()
            < token["x"].as_f64().unwrap()
    );
    assert!(
        token["x"].as_f64().unwrap() + token["width"].as_f64().unwrap()
            < access["x"].as_f64().unwrap()
    );
    render_png(&f, p, "settings", "settings_hubs_token.png");
    control(&f, p, "copy_hub_invite");
    assert_eq!(
        inspect(&f, p)["clipboard"],
        cfg["host_hub"]["invite_command"]
    );
    for address in ["lan", "tailscale"] {
        control(&f, p, &format!("host_hub_addr:{address}"));
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "host_hub_endpoint")["text"],
            cfg["host_hub"][format!("{address}_url")]
        );
        control(&f, p, "copy_hub_invite");
        assert_eq!(
            inspect(&f, p)["clipboard"],
            cfg["host_hub"][format!("{address}_invite_command")]
        );
    }
    // A dirty port keeps the sharing editor open while sharing is off, so
    // both access labels participate in the same paired-width assertion.
    f.send(p,json!({"op":"test_settings_input","id":"host_hub_port","value":cfg["host_hub"]["port"].as_u64().unwrap().to_string()}));
    let mut reserved = None;
    for enabled in [false, true] {
        let mut toggled = cfg.clone();
        toggled["host_hub"]["enabled"] = json!(enabled);
        toggled["hubs"][0]["enabled"] = json!(enabled);
        toggled["host_hub"]["auth_mode"] = json!(if enabled { "token" } else { "none" });
        f.send(p, json!({"op":"settings","settings":toggled}));
        let snapshot = inspect(&f, p);
        let frames: Vec<_> = [
            "toggle_host_hub",
            "toggle_host_hub_auth",
            "toggle_hub:local",
            "edit_hub:local",
        ]
        .into_iter()
        .map(|id| {
            let view = node(&snapshot, "settings_items", id);
            assert!(!view.is_null(), "{id}");
            (view["x"].clone(), view["width"].clone())
        })
        .collect();
        if let Some(previous) = &reserved {
            assert_eq!(&frames, previous);
        } else {
            reserved = Some(frames);
        }
    }
    f.send(p, json!({"op":"settings","settings":cfg}));
    let snapshot = inspect(&f, p);
    for id in ["settings_tab_track", "host_hub_addr_track"] {
        assert!(
            (node(&snapshot, "settings_items", id)["background"][3]
                .as_f64()
                .unwrap()
                - 0.06)
                .abs()
                < 0.001
        );
    }
    for invalid in ["0", "65536", "word", ""] {
        let before = f.frames(p).len();
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":invalid}),
        );
        control(&f, p, "apply_host_hub_port");
        let s = inspect(&f, p);
        assert_eq!(
            node(&s, "settings_items", "apply_host_hub_port")["text"],
            "Apply"
        );
        assert!(!node(&s, "settings_items", "host_hub_error").is_null());
        assert_eq!(f.frames(p).len(), before);
    }
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"5301"}),
    );
    control(&f, p, "settings_tab:hubs");
    assert!(
        node(&inspect(&f, p), "settings_items", "host_hub_error").is_null(),
        "Editing the port clears local validation feedback"
    );
    // Only a command acknowledgement may settle Apply, including an unchanged save.
    frame["settings"] = cfg.clone();
    f.send(p, frame.clone());
    for op in ["state", "settings"] {
        let current = cfg["host_hub"]["port"].as_i64().unwrap();
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":current.to_string()}),
        );
        f.send(p, json!({"op":"settings","settings":cfg}));
        let unchanged = inspect(&f, p);
        assert_eq!(
            node(&unchanged, "settings_items", "apply_host_hub_port")["enabled"],
            false
        );
        let before = f.frames(p).len();
        control(&f, p, "apply_host_hub_port");
        inspect(&f, p);
        assert_eq!(
            f.frames(p).len(),
            before,
            "Unchanged Apply must not emit a mutation"
        );
        let draft = current + 1;
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":draft.to_string()}),
        );
        control(&f, p, "apply_host_hub_port");
        wait(|| {
            f.frames(p)[before..]
                .iter()
                .any(|v| v["op"] == "set_host_hub" && v["port"] == draft)
        });
        let submitted = f.frames(p)[before..]
            .iter()
            .find(|v| v["op"] == "set_host_hub")
            .unwrap()
            .clone();
        assert!(!submitted["request_id"].as_str().unwrap().is_empty());
        f.send(
            p,
            if op == "state" {
                frame.clone()
            } else {
                json!({"op":"settings","settings":cfg})
            },
        );
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["text"],
            "Applying…",
            "An uncorrelated {op} snapshot cannot acknowledge the save"
        );
        cfg["host_hub"]["port"] = json!(draft);
        f.send(
            p,
            json!({"op":"settings","request_id":submitted["request_id"],"settings":cfg}),
        );
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["text"],
            "✓ Saved"
        );
        frame["settings"] = cfg.clone();
    }
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":" 5301 "}),
    );
    control(&f, p, "apply_host_hub_port");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "set_host_hub" && v["port"] == 5301)
    });
    let submitted = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "set_host_hub")
        .unwrap();
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"5302"}),
    );
    let before = f.frames(p).len();
    for id in ["toggle_host_hub_auth", "toggle_host_hub"] {
        control(&f, p, id);
    }
    inspect(&f, p);
    let toggles = f.frames(p)[before..].to_vec();
    assert_eq!(toggles.len(), 2);
    for action in toggles {
        assert_eq!(action["op"], "set_host_hub");
        assert_eq!(action["port"], 5302);
        assert!(
            action["request_id"].is_string() && action["request_id"] != submitted["request_id"],
            "Toggles own distinct receipts without replacing the port save"
        );
    }
    // Neither untagged errors nor refreshes may release Apply's pending identity.
    for source in [json!({}), json!({"source_op":"set_host_hub"})] {
        let mut error = json!({"op":"error","error":"Unrelated host error"});
        error
            .as_object_mut()
            .unwrap()
            .extend(source.as_object().unwrap().clone());
        f.send(p, error);
    }
    cfg["host_hub"]["port"] = json!(5301);
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["text"],
        "Applying…"
    );
    f.send(
        p,
        json!({"op":"settings","request_id":submitted["request_id"],"settings":cfg}),
    );
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "apply_host_hub_port")["text"],
        "Apply",
        "An older correlated save must settle without marking the newer draft saved"
    );
    assert_eq!(node(&s, "settings_items", "host_hub_port")["text"], "5302");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"5303"}),
    );
    control(&f, p, "apply_host_hub_port");
    inspect(&f, p);
    let failed = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "set_host_hub")
        .unwrap();
    cfg["host_hub"]["error"] = json!("Port already in use");
    f.send(
        p,
        json!({"op":"settings","request_id":failed["request_id"],"settings":cfg}),
    );
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "apply_host_hub_port")["text"],
        "Apply"
    );
    assert_eq!(
        node(&s, "settings_items", "host_hub_error")["text"],
        "Port already in use"
    );
    control(&f, p, "apply_host_hub_port");
    inspect(&f, p);
    let retry = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "set_host_hub")
        .unwrap();
    assert_ne!(retry["request_id"], failed["request_id"]);
    f.send(p, json!({"op":"error","source_op":"set_host_hub","request_id":failed["request_id"],"error":"Late port error"}));
    f.send(
        p,
        json!({"op":"settings","request_id":failed["request_id"],"settings":cfg}),
    );
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["text"],
        "Applying…"
    );
    cfg["host_hub"]["error"] = Value::Null;
    cfg["host_hub"]["port"] = json!(5303);
    f.send(
        p,
        json!({"op":"settings","request_id":retry["request_id"],"settings":cfg}),
    );
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "apply_host_hub_port")["text"],
        "✓ Saved",
        "Late failure must not settle the retry"
    );
    assert!(!s.to_string().contains("Late port error"));
    assert_eq!(
        node(&s, "settings_items", "settings_error_label")["text"],
        "Unrelated host error"
    );
    control(&f, p, "dismiss_settings_error");
    assert!(node(&inspect(&f, p), "settings_items", "settings_error_banner").is_null());
    control(&f, p, "edit_hub:local");
    let s = inspect(&f, p);
    assert!(
        node(&s, "settings_items", "host_hub_error").is_null(),
        "A cleared daemon error must not linger locally"
    );
    assert_eq!(
        node(&s, "settings_items", "settings_hub_user")["placeholder"],
        "Defaults to your Mac username"
    );
    assert_eq!(node(&s, "settings_items", "settings_hub_name"), Value::Null);
    render_png(&f, p, "settings", "settings_hubs_edit_local.png");
    control(&f, p, "cancel_add_hub");
    control(&f, p, "open_add_hub");
    render_png(&f, p, "settings", "settings_hubs_add_remote.png");
    for mode in ["none", "token", "oidc"] {
        control(&f, p, &format!("settings_hub_auth:{mode}"));
        let s = inspect(&f, p);
        let track = node(&s, "settings_items", "settings_hub_auth_track");
        // Selection styling is checked by the contrast oracle. This assertion
        // owns packing geometry rather than implementation-specific RGBA values.
        let mut right = 0.0;
        for candidate in ["none", "token", "oidc"] {
            let pill = node(
                &s,
                "settings_items",
                &format!("settings_hub_auth:{candidate}"),
            );
            let x = pill["x"].as_f64().unwrap();
            assert!(x >= right);
            right = x + pill["width"].as_f64().unwrap();
            assert!(right <= track["width"].as_f64().unwrap());
            assert!(pill["y"].as_f64().unwrap() >= 0.0);
            assert!(
                pill["y"].as_f64().unwrap() + pill["height"].as_f64().unwrap()
                    <= track["height"].as_f64().unwrap()
            );
        }
        let labels: Vec<_> = nodes(&node(&s, "settings_items", "add_hub_form")["children"])
            .into_iter()
            .filter_map(|n| n["text"].as_str().map(String::from))
            .collect();
        assert!(labels.contains(&"Hub Name (Optional)".to_owned()));
        assert!(labels.contains(&"Hub URL".to_owned()));
        assert!(labels.contains(&"Access".to_owned()));
    }
    control(&f, p, "open_directory");
    for mode in ["directory", "activity"] {
        control(&f, p, &format!("dir_mode:{mode}"));
        inspect(&f, p);
        render_png(&f, p, "directory", &format!("directory_{mode}.png"));
        for appearance in ["light", "dark"] {
            f.send(p, json!({"op":"test_appearance","appearance":appearance}));
            inspect(&f, p);
        }
    }
}

#[test]
fn v29_oracle_detects_real_appkit_geometry_contrast_jargon_and_address_faults() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    f.send(p, state());
    for appearance in ["light", "dark"] {
        for kind in [
            "truncated_labels",
            "overlapping_siblings",
            "clipped_views",
            "low_contrast_labels",
            "jargon_leaks",
            "address_mismatches",
        ] {
            f.send(p, json!({"op":"test_oracle_calibration","kind":kind}));
            let s = inspect_unchecked(&f, p, Some(appearance));
            assert!(
                !s[kind].as_array().unwrap().is_empty(),
                "{appearance}: {kind}"
            );
            f.send(p, json!({"op":"test_oracle_calibration","kind":"reset"}));
            inspect_as(&f, p, Some(appearance));
        }
        for (kind, oracle) in [
            ("bar_anchor", "clipped_views"),
            ("fractional_truncation", "truncated_labels"),
            ("card_padding", "clipped_views"),
            ("strip_anchor", "clipped_views"),
            ("status_emoji", "jargon_leaks"),
            ("hit_test", "overlapping_siblings"),
        ] {
            f.send(p, json!({"op":"test_oracle_calibration","kind":kind}));
            assert!(
                !inspect_unchecked(&f, p, Some(appearance))[oracle]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "{kind}"
            );
            f.send(p, json!({"op":"test_oracle_calibration","kind":"reset"}));
            inspect_as(&f, p, Some(appearance));
        }
        for text in [
            "Handle",
            "every human",
            "each human",
            "receive wakes",
            "Wake banners",
            "cito_listen",
            "Hub Service:",
            "Channel inventory",
            "UI action exceeds",
            "Passive",
            "[Passive]",
            "Refresh failed: PLUGIN_EXEC_FAILED: retry",
            "Discovery failed; 10071: retry",
            "An embedded 10037: failure",
            "Prefix EXPIRED: retry",
            r#"Response {"error_code":"FAILED"}"#,
            "Storage SQLITE_CANTOPEN",
            "Call sqlite3 failed",
            "Storage (SQLite 14)",
            "unable to open database file",
        ] {
            f.send(
                p,
                json!({"op":"test_oracle_calibration","kind":"jargon_leaks","text":text}),
            );
            assert!(
                !inspect_unchecked(&f, p, Some(appearance))["jargon_leaks"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "{text}"
            );
            f.send(p, json!({"op":"test_oracle_calibration","kind":"reset"}));
        }
        let mut configured = state();
        configured["settings"] = settings();
        f.send(p, configured);
        control(&f, p, "open_settings");
        control(&f, p, "settings_tab:channels");
        f.send(
            p,
            json!({"op":"test_oracle_calibration","kind":"bar_anchor","id":"save_channel:channel"}),
        );
        assert!(inspect_unchecked(&f, p, Some(appearance))["clipped_views"]
            .as_array()
            .unwrap()
            .contains(&json!(
                "stratum_user:channel: bottom row is not edge anchored"
            )));
        f.send(p, json!({"op":"test_oracle_calibration","kind":"reset"}));
        control(&f, p, "edit_hub_service:channel");
        f.send(p,json!({"op":"test_oracle_calibration","kind":"bar_anchor","id":"save_hub_service:channel"}));
        assert!(inspect_unchecked(&f, p, Some(appearance))["clipped_views"]
            .as_array()
            .unwrap()
            .contains(&json!(
                "stratum_hub:channel: bottom row is not edge anchored"
            )));
        f.send(p, json!({"op":"test_oracle_calibration","kind":"reset"}));
        control(&f, p, "cancel_edit_hub_service:channel");
        control(&f, p, "close_settings");
        control(&f, p, "open_activity");
        f.send(
            p,
            json!({"op":"test_oracle_calibration","kind":"bar_anchor","id":"activity_clear"}),
        );
        assert!(inspect_unchecked(&f, p, Some(appearance))["clipped_views"]
            .as_array()
            .unwrap()
            .contains(&json!("directory_facet_bar: top row is not edge anchored")));
        f.send(p, json!({"op":"test_oracle_calibration","kind":"reset"}));
        f.send(p, json!({"op":"test_directory_key","key":"escape"}));
        inspect_as(&f, p, Some(appearance));
        for human in [false, true] {
            let mut invalid = state();
            invalid["listeners"][0]["hub"] = json!(if human { "personal" } else { "work" });
            invalid["listeners"][0]["harness"] = json!(if human { "human" } else { "fixture" });
            // DTO and chip agree, but the address violates the ontology/routing rule.
            invalid["listeners"][0]["display_address"] = json!("alice/fixture#abcd");
            f.send(p, invalid);
            control(&f, p, if human { "hub:personal" } else { "hub:work" });
            if human {
                control(&f, p, "open_directory");
                let mut person = peer("invalid-human");
                person["harness"] = json!("human");
                // Human address validation lives in the unified Directory, not an agent card.
                inspect_unchecked(&f, p, Some(appearance));
                let request = f
                    .frames(p)
                    .into_iter()
                    .rev()
                    .find(|a| {
                        matches!(a["op"].as_str(), Some("query_directory" | "open_directory"))
                    })
                    .unwrap();
                f.send(p,json!({"op":"directory","request_id":request["request_id"],"query":request["query"],"scope":request["scope"],"harness":request["harness"],"peers":[person]}));
            }
            assert!(
                !inspect_unchecked(&f, p, Some(appearance))["address_mismatches"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        let mut invalid = state();
        invalid["activity"] = json!([{"hub":"personal","signal_id":"jargon","listener_id":"target","from_user":"alice","from_host_name":"host","from_title":"Human Node","target_harness":"human","target_title":"Human Node","target_scoped_id":"personal:target","message":"Message","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true}]);
        f.send(p, invalid);
        control(&f, p, "open_activity");
        control(&f, p, "act_filter:all");
        assert_eq!(
            inspect_as(&f, p, Some(appearance))["jargon_leaks"],
            json!([])
        );
        f.send(
            p,
            json!({"op":"test_oracle_calibration","kind":"activity_jargon"}),
        );
        let s = inspect_unchecked(&f, p, Some(appearance));
        assert!(s["jargon_leaks"]
            .as_array()
            .unwrap()
            .contains(&json!("act_title:jargon")));
        assert_eq!(
            node(&s, "directory_items", "directory_shortcuts")["text"],
            "⎋ Close"
        );
        let mut clean = state();
        clean["activity"] = json!([]);
        f.send(p, clean);
        let mut user_titles = state();
        user_titles["listeners"][0]["title"] = json!("Human Node");
        user_titles["listeners"][0]["about"] = json!("[Passive] build");
        user_titles["activity"] = json!([{"hub":"personal","signal_id":"user-title","listener_id":"target","from_user":"alice","from_host_name":"host","from_title":"[Passive] Worker","target_harness":"fixture","target_title":"Human Node","target_scoped_id":"personal:target","message":"[Passive] build","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true}]);
        f.send(p, user_titles);
        control(&f, p, "act_filter:all");
        let s = inspect_as(&f, p, Some(appearance));
        assert!(node(&s, "directory_items", "act_title:user-title")["text"]
            .as_str()
            .unwrap()
            .contains("Human Node"));
        assert_eq!(s["jargon_leaks"], json!([]));
        for title in ["Human Node", "Human listening"] {
            let mut shared = state();
            shared["activity"] = json!([
                {"hub":"personal","signal_id":"shared","listener_id":"first","from_user":"Session","from_host_name":"host","from_title":"[Passive] Worker","target_harness":"fixture","target_title":"Review","target_scoped_id":"personal:first","message":"Message","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true},
                {"hub":"work","signal_id":"shared","listener_id":"second","from_user":"alice","from_host_name":"host","from_title":"Sender","target_harness":"fixture","target_title":title,"target_scoped_id":"work:second","message":"Message","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true}
            ]);
            f.send(p, shared.clone());
            let s = inspect_as(&f, p, Some(appearance));
            assert_eq!(
                nodes(&s["directory_items"])
                    .iter()
                    .filter(|n| n["id"] == "act_title:shared")
                    .count(),
                2
            );
            assert_eq!(
                s["jargon_leaks"],
                json!([]),
                "User title on the second Hub: {title}"
            );
            // The excluded title cannot hide a generated suffix with the same token.
            f.send(p, json!({"op":"test_oracle_calibration","kind":"activity_jargon","row":1,"text":format!("@alice → {title} · Human receiver")}));
            assert!(inspect_unchecked(&f, p, Some(appearance))["jargon_leaks"]
                .as_array()
                .unwrap()
                .contains(&json!("act_title:shared")));
            shared["activity"][1]["target_harness"] = json!("human");
            f.send(p, shared);
            assert_eq!(
                inspect_as(&f, p, Some(appearance))["jargon_leaks"],
                json!([])
            );
            for text in [
                "@alice → Human Node",
                "@alice → Human listening",
                "@alice → Session",
                "ERROR: invalid",
            ] {
                f.send(p, json!({"op":"test_oracle_calibration","kind":"activity_jargon","row":1,"text":text}));
                assert!(
                    inspect_unchecked(&f, p, Some(appearance))["jargon_leaks"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("act_title:shared")),
                    "Generated chrome must be checked: {text}"
                );
                f.send(p, json!({"op":"test_oracle_calibration","kind":"activity_jargon","row":1,"text":"@alice → You"}));
                inspect_as(&f, p, Some(appearance));
            }
        }
    }
}

#[test]
fn v29_canonical_four_surface_retina_states_in_both_appearances() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let capture = |surface: &str, state: &str| {
        let snapshot = inspect(&f, p);
        let strip = match surface {
            "menu" => {
                let subtitle_width = node(&snapshot, "items", "header_subtitle")["width"]
                    .as_f64()
                    .unwrap();
                Some((
                    "items",
                    "header_hub_strip",
                    4.0,
                    (368.0 - (54.0 + subtitle_width).max(138.0)).max(44.0),
                ))
            }
            "directory" => Some(("directory_items", "directory_harness_scroll", 4.0, 387.0)),
            "settings" => Some(("settings_items", "channel_hub_strip", 6.0, 398.0)),
            _ => None,
        };
        if let Some((tree, id, spacing, allocated)) = strip {
            let view = node(&snapshot, tree, id);
            if !view.is_null() && view["hidden"] == false {
                let pills: Vec<_> = view["children"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|p| p["hidden"] == false)
                    .collect();
                let intrinsic = pills
                    .iter()
                    .map(|p| p["width"].as_f64().unwrap())
                    .sum::<f64>()
                    + spacing * pills.len().saturating_sub(1) as f64
                    + if pills.is_empty() { 0.0 } else { 4.0 };
                let width = view["width"].as_f64().unwrap();
                assert!(
                    width <= allocated + 0.5,
                    "{id} exceeds its allocation: {view}"
                );
                assert!(
                    (width - intrinsic.min(allocated)).abs() < 0.5,
                    "{id} must hug its pills without an empty tail: {view}"
                );
                if let Some(first) = pills.first() {
                    assert!((first["x"].as_f64().unwrap() - 2.0).abs() < 0.5);
                    let end = pills
                        .iter()
                        .map(|pill| pill["x"].as_f64().unwrap() + pill["width"].as_f64().unwrap())
                        .fold(0.0, f64::max);
                    assert!(
                        (intrinsic - end - 2.0).abs() < 0.5,
                        "{id} must have exactly 2pt trailing padding: {view}"
                    );
                }
                let x = view["x"].as_f64().unwrap();
                match surface {
                    "menu" => assert!((x + width - 368.0).abs() < 0.5),
                    "directory" => assert_eq!(x, 157.0),
                    "settings" => assert_eq!(x, 102.0),
                    _ => unreachable!(),
                }
            }
        }
        if surface == "directory" {
            for id in ["dir_mode:directory", "dir_mode:activity"] {
                assert_eq!(node(&snapshot, "directory_items", id)["radius"], 7);
            }
        }
        for appearance in ["light", "dark"] {
            render_png_as(
                &f,
                p,
                surface,
                &format!("v29_{state}_{appearance}.png"),
                Some(appearance),
            );
        }
    };
    let mut cfg = settings();
    cfg["apps"][2]["id"] = json!("claude");
    cfg["apps"][2]["display_name"] = json!("Claude Code");
    cfg["apps"][2]["installed"] = json!(true);
    cfg["apps"][0]["display_name"] = json!("Pi Coding Agent");
    cfg["channels"][0]["id"] = json!("telegram");
    cfg["channels"][0]["display_name"] = json!("Telegram");
    cfg["channels"][0]["pairing_manual_disclosure"] = json!(true);
    cfg["channels"][0]["pairing_open_url_label"] = json!("Open in Telegram ↗");
    cfg["channels"][0]["fields"][1]["label"] = json!("Chat ID");
    cfg["channels"][0]["fields"][1]["placeholder"] = json!("Paired via /start or enter Chat ID");
    cfg["host_hub"]["enabled"] = json!(true);
    cfg["host_hub"]["running"] = json!(true);
    cfg["host_hub"]["endpoint_url"] = json!("http://mac-studio.tailnet.ts.net:4291");
    cfg["host_hub"]["invite_command"] =
        json!("cito setup --hub mac-studio --url http://mac-studio.tailnet.ts.net:4291 --default");
    cfg["host_hub"]["tailscale_url"] = cfg["host_hub"]["endpoint_url"].clone();
    cfg["host_hub"]["tailscale_invite_command"] = cfg["host_hub"]["invite_command"].clone();
    cfg["host_hub"]["lan_url"] = json!("http://192.168.1.10:4291");
    cfg["host_hub"]["lan_invite_command"] =
        json!("cito setup --hub lan --url http://192.168.1.10:4291 --default");
    cfg["hubs"][0]["name"] = json!("local");
    cfg["hubs"][1]["url"] = json!("https://hub.example.com");
    cfg["hubs"][1]["auth_mode"] = json!("oidc");
    cfg["hubs"][1]["issuer"] = json!("https://id.example.com");
    cfg["hubs"][1]["client_id"] = json!("cito");
    let mut frame = state();
    frame["hubs"][0]["name"] = json!("local");
    frame["hubs"][0]["auth_mode"] = json!("none");
    frame["hubs"][1]["auth_mode"] = json!("oidc");
    frame["settings"] = cfg;
    frame["harnesses"] = json!([
        {"id":"pi","display_name":"Pi","kind":"agent","sf_symbol":"terminal","tint":"purple","tier":"stable","order":0},
        {"id":"codex","display_name":"Codex","kind":"agent","sf_symbol":"terminal","tint":"blue","tier":"stable","order":1},
        {"id":"claude","display_name":"Claude Code","kind":"agent","sf_symbol":"terminal","tint":"orange","tier":"experimental","order":2}
    ]);
    let mut agent = listener();
    agent["hub"] = json!("local");
    agent["harness"] = json!("pi");
    agent["display_address"] = json!("alice/pi#abcd");
    agent["title"] = json!("Release orchestration");
    agent["host_name"] = json!("mac-studio.local");
    agent["cwd"] = json!("~/Projects/acme/service");
    let mut human = listener();
    human["hub"] = json!("local");
    human["harness"] = json!("human");
    human["session_id"] = json!("default");
    human["title"] = json!("Human");
    human["user"] = json!("alice");
    human["listener_id"] = json!("c_76e846d115ce642f");
    human["public_id"] = json!("alice-76e846d115ce642f");
    human["scoped_id"] = json!("local:alice-76e846d115ce642f");
    human["display_address"] = json!("@alice");
    human["host_name"] = json!("mac-studio.local");
    human["channels"] = json!([]);
    human["attachments"] = json!([{"channel_id":"here:human","location":"device","host_id":"here","display_name":"mac-studio.local","availability":"ACTIVE","is_local":true}]);
    frame["hubs"][0]["attachments"] = human["attachments"].clone();
    frame["hubs"][0]["human_state_status"] = json!("current");
    frame["hubs"][0]["human_paused"] = json!(false);
    frame["hubs"][0]["human_control_revision"] = json!(1);
    frame["listeners"] = json!([agent, human]);
    frame["available"] = json!([{"harness":"codex","session_id":"review","title":"api-server","cwd":"~/Projects/acme/service","branch":"main"}]);
    frame["unread_count"] = json!(2);
    f.send(p, frame);
    let s = inspect(&f, p);
    assert_eq!(s["count"], 1);
    assert_eq!(node(&s, "items", "plugin_icon:pi")["tint"], json!([]));
    assert_eq!(node(&s, "items", "plugin_icon:pi")["x"], 0);
    assert_eq!(node(&s, "items", "plugin_icon:pi")["width"], 28);
    let copy = node(&s, "items", "copy:personal:repo-abcdefghijkl");
    assert!(nodes(&copy["children"])
        .iter()
        .any(|n| n["x"] == 0 && n["width"] == 12));
    assert!(node(&s, "items", "human_inbox_card").is_null());
    for appearance in ["light", "dark"] {
        let snapshot = inspect_as(&f, p, Some(appearance));
        let title = nodes(&snapshot["items"])
            .into_iter()
            .find(|n| n["text"] == "Release orchestration")
            .unwrap()
            .clone();
        assert!(
            title["label_measured_width"].as_f64().unwrap()
                <= title["label_width"].as_f64().unwrap(),
            "Hero title must fit beside the rename pencil in {appearance}: {title}"
        );
        let pauses: Vec<_> = nodes(&snapshot["items"])
            .into_iter()
            .filter(|n| n["id"].as_str().is_some_and(|id| id.starts_with("stop:")))
            .collect();
        assert_eq!(pauses.len(), 1);
        for pause in &pauses {
            assert_eq!(pause["text"], "Stop");
            assert!(pause["symbol"].is_null());
            assert_eq!(pause["border_width"], 1);
            assert_eq!(pause["width"], pauses[0]["width"]);
            assert_eq!(pause["height"], 22);
        }
    }
    capture("menu", "menu_active");

    let mut remote_agent = peer("review");
    remote_agent["hub"] = json!("local");
    remote_agent["scoped_id"] = json!("local:review");
    remote_agent["harness"] = json!("pi");
    remote_agent["display_address"] = json!("bob/pi#8a4f");
    remote_agent["title"] = json!("Review session");
    let mut remote_human = peer("human");
    remote_human["hub"] = json!("local");
    remote_human["scoped_id"] = json!("local:human");
    remote_human["harness"] = json!("human");
    remote_human["title"] = json!("Human");
    remote_human["about"] = json!("Human receiver");
    remote_human["display_address"] = json!("@bob");
    remote_human["repo"] = Value::Null;
    remote_human["branch"] = Value::Null;
    let peers = json!([remote_agent, remote_human]);
    let short = json!({"hub":"local","signal_id":"local:short","listener_id":"c_76e846d115ce642f","from_user":"bob","from_host_name":"laptop","from_harness":"pi","from_title":"Review","from_scoped_id":"local:review","from_display_address":"bob/pi#8a4f","target_harness":"human","target_title":"Human","target_scoped_id":"local:alice-76e846d115ce642f","target_display_address":"@alice","message":"All checks passed on release branch. Ship v2.4 now or hold for docs?","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true,"can_reply":true});
    let mut expanded = short.clone();
    expanded["signal_id"] = json!("local:expanded");
    expanded["message"] =
        json!("First line for review.\nSecond line with details.\nThird line: ready to continue.");
    expanded["held_reason"] = json!("stale_generation");
    let activity = json!([short, expanded]);
    control(&f, p, "open_directory");
    let seeded = inspect(&f, p);
    assert_eq!(
        node(
            &seeded,
            "directory_items",
            "dir_meta:local:alice-76e846d115ce642f"
        )["text"],
        "Inbox · mac-studio"
    );
    assert!(!node(
        &seeded,
        "directory_items",
        "dir_peer:local:alice-76e846d115ce642f"
    )
    .is_null());
    assert_eq!(
        node(&seeded, "directory_items", "directory_empty_state")["hidden"],
        true
    );
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":peers,"activity":activity,"matched_count":2,"online_count":2,"unread_count":2}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["directory_mode"], "directory");
    assert_eq!(
        node(&s, "directory_items", "plugin_icon:pi")["tint"],
        json!([])
    );
    assert_eq!(node(&s, "directory_items", "plugin_icon:pi")["width"], 28);
    capture("directory", "directory");
    control(&f, p, "msg_dir:local:review");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Ready to review"}),
    );
    assert_eq!(inspect(&f, p)["composer_target"], "bob/pi#8a4f");
    capture("directory", "directory_composer");
    control(&f, p, "composer_cancel");
    control(&f, p, "dir_mode:activity");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":peers,"activity":activity,"matched_count":2,"online_count":2,"unread_count":2}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["directory_selected_id"], "local:short");
    assert_eq!(
        node(&s, "directory_items", "activity_row:local:short")["height"],
        60
    );
    assert_eq!(
        node(&s, "directory_items", "plugin_icon:pi")["tint"],
        json!([])
    );
    assert_eq!(node(&s, "directory_items", "plugin_icon:pi")["x"], 0);
    let mut clean = activity.clone();
    clean[1].as_object_mut().unwrap().remove("held_reason");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":peers,"activity":clean,"matched_count":2,"online_count":2,"unread_count":2}),
    );
    control(&f, p, "reply_activity:local:short");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Ship v2.4 now — docs can follow."}),
    );
    let reply = inspect(&f, p);
    assert_eq!(reply["composer_open"], true);
    assert_eq!(reply["composer_target"], "bob/pi#8a4f");
    assert_eq!(
        node(&reply, "directory_items", "composer_target")["text"],
        "To: bob/pi#8a4f"
    );
    assert_eq!(
        node(&reply, "directory_items", "composer_input")["text"],
        "Ship v2.4 now — docs can follow."
    );
    capture("directory", "activity");
    control(&f, p, "composer_cancel");
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    let s = inspect(&f, p);
    assert_eq!(s["directory_selected_id"], "local:expanded");
    let height = node(&s, "directory_items", "activity_row:local:expanded")["height"]
        .as_f64()
        .unwrap();
    assert!(height > 60.0 && height < 108.0, "{height}");
    assert_eq!(
        node(&s, "directory_items", "act_msg:local:expanded")["height"],
        height - 42.0
    );
    let message = node(&s, "directory_items", "act_msg:local:expanded");
    assert_eq!(
        message["y"].as_f64().unwrap() + message["height"].as_f64().unwrap(),
        height - 34.0
    );
    for id in [
        "reply_activity:local:expanded",
        "copy_activity_msg:local:expanded",
        "delete_activity:local:expanded",
    ] {
        assert!(
            node(&s, "directory_items", id)["y"].as_f64().unwrap()
                - (message["y"].as_f64().unwrap() + message["height"].as_f64().unwrap())
                >= 4.0
        );
    }
    capture("directory", "activity_expanded");
    control(&f, p, "reply_activity:local:expanded");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Thanks for the review"}),
    );
    assert_eq!(inspect(&f, p)["composer_reply_signal_id"], "local:expanded");
    capture("directory", "activity_composer");
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["directory_open"], false);

    control(&f, p, "open_settings");
    for tab in ["apps", "channels", "hubs"] {
        control(&f, p, &format!("settings_tab:{tab}"));
        let s = inspect(&f, p);
        assert_eq!(s["settings_tab"], tab);
        if tab == "apps" {
            for id in ["pi", "codex", "claude"] {
                let icon = node(&s, "settings_items", &format!("plugin_icon:{id}"));
                assert_eq!(icon["tint"], json!([]));
                assert_eq!(icon["x"], 0);
                assert_eq!(icon["y"], 0);
                assert_eq!(icon["width"], 28);
            }
        } else if tab == "channels" {
            for appearance in ["light", "dark"] {
                let snapshot = inspect_as(&f, p, Some(appearance));
                let track = node(&snapshot, "settings_items", "channel_hub_strip");
                assert!(track["width"].as_f64().unwrap() <= 398.0);
                let mut right = 0.0;
                for hub in ["local", "work"] {
                    let pill = node(
                        &snapshot,
                        "settings_items",
                        &format!("channel_hub_selector:{hub}"),
                    );
                    let x = pill["x"].as_f64().unwrap();
                    assert!(x >= right);
                    right = x + pill["width"].as_f64().unwrap();
                    assert!(right <= track["width"].as_f64().unwrap());
                    assert!(pill["height"].as_f64().unwrap() <= track["height"].as_f64().unwrap());
                }
            }
            assert_eq!(
                node(&s, "settings_items", "plugin_icon:telegram")["tint"],
                json!([])
            );
            assert_eq!(
                node(&s, "settings_items", "plugin_icon:telegram")["width"],
                24
            );
            assert!(
                node(&s, "settings_items", "settings_channel:telegram")["height"]
                    .as_f64()
                    .unwrap()
                    <= 254.0
            );
        } else {
            assert_eq!(
                node(&s, "settings_items", "copy_hub_invite")["text"],
                "Copy Invite Code"
            );
            assert!(node(&s, "settings_items", "host_hub_guidance").is_null());
            assert_eq!(node(&s, "settings_items", "host_hub_card")["height"], 160);
            assert_eq!(
                node(&s, "settings_items", "paste_hub_invite")["text"],
                "Join from Clipboard"
            );
        }
        capture("settings", &format!("settings_{tab}"));
    }
    control(&f, p, "open_add_hub");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"settings_hub_name","value":"studio"}),
    );
    f.send(p,json!({"op":"test_settings_input","id":"settings_hub_url","value":"https://hub.example.com"}));
    let s = inspect(&f, p);
    assert_eq!(node(&s, "settings_items", "host_hub_card")["height"], 160);
    assert_eq!(node(&s, "settings_items", "add_hub_form")["y"], 8);
    assert_eq!(node(&s, "settings_items", "add_hub_form")["height"], 252);
    assert_eq!(node(&s, "settings_items", "hub_visibility_caption")["text"], "Personal Hubs connect your Macs automatically; Team Sign-In (OIDC) Hubs start private to your account (@user) until shared with Team.");
    assert_eq!(s["overlapping_siblings"], json!([]));
    assert_eq!(s["clipped_views"], json!([]));
    assert!(node(&s, "settings_items", "host_hub_guidance").is_null());
    capture("settings", "settings_add_hub");
    control(&f, p, "cancel_add_hub");
    control(&f, p, "edit_hub:local");
    assert!(!node(&inspect(&f, p), "settings_items", "settings_hub_user").is_null());
    capture("settings", "settings_edit_local");
    f.send(p, json!({"op":"error","source_op":"setup_save_hub","error":"INVALID_INPUT: Username is invalid"}));
    let local_error = inspect(&f, p);
    let error = node(&local_error, "settings_items", "add_hub_error");
    let user = node(&local_error, "settings_items", "settings_hub_user");
    assert_eq!(
        node(&local_error, "settings_items", "add_hub_form")["height"]
            .as_f64()
            .unwrap(),
        124.0 + error["height"].as_f64().unwrap() + 6.0
    );
    assert!(
        user["y"].as_f64().unwrap()
            >= error["y"].as_f64().unwrap() + error["height"].as_f64().unwrap() + 6.0
    );
    assert_eq!(local_error["overlapping_siblings"], json!([]));
    assert_eq!(local_error["clipped_views"], json!([]));
    control(&f, p, "cancel_add_hub");
    control(&f, p, "edit_hub:work");
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "settings_hub_issuer")["text"],
        "https://id.example.com"
    );
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "add_hub_form")["y"],
        8
    );
    capture("settings", "settings_edit_remote");
    control(&f, p, "cancel_add_hub");
    control(&f, p, "open_uninstall_confirm");
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "uninstall_modal_overlay")["width"],
        520
    );
    assert_eq!(
        node(&s, "settings_items", "settings_footer")["hidden"],
        false
    );
    capture("settings", "settings_uninstall_modal");
    control(&f, p, "cancel_uninstall_all");
    control(&f, p, "close_settings");
    assert_eq!(inspect(&f, p)["settings_open"], false);
}

#[test]
fn v29_correlation_editor_keys_and_four_surface_oracle() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    frame["harnesses"] = json!((0..6).map(|i| json!({"id":format!("agent-{i}"),"display_name":format!("Agent {i}"),"kind":"agent","sf_symbol":"terminal","tint":"blue","tier":"stable","order":i})).collect::<Vec<_>>());
    frame["my_remote_listeners"] = json!([peer("remote")]);
    f.send(p, frame);
    control(&f, p, "copy_remote:personal:remote");
    assert_eq!(
        inspect(&f, p)["clipboard"],
        peer("remote")["display_address"]
    );
    control(&f, p, "open_directory");
    control(&f, p, "dir_harness:agent-5");
    let overflow = inspect(&f, p);
    assert_eq!(overflow["directory_harness"], "agent-5");
    assert!(!node(&overflow, "directory_items", "directory_harness_scroll").is_null());
    control(&f, p, "dir_harness:all");
    let mut canonical = peer("one");
    canonical["listener_id"] = json!("c_abcd123456789012");
    canonical["public_id"] = json!("bob-abcd123456789012");
    canonical["display_address"] = json!("bob/fixture#abcd");
    send_fixture(&f, p, json!({"op":"directory","peers":[canonical]}));
    for prefix in ["abcd1234", "abcd12345678", "abcd123456789012"] {
        f.send(
            p,
            json!({"op":"test_directory_query","query":format!("bob/fixture#{prefix}")}),
        );
        assert_eq!(inspect(&f, p)["directory_selected_id"], "personal:one");
    }
    f.send(p, json!({"op":"test_directory_query","query":""}));
    let before = inspect(&f, p);
    f.send(p,json!({"op":"directory","request_id":"stale","query":"","scope":"all","harness":"all","peers":[peer("two")],"online_count":0}));
    assert_eq!(
        inspect(&f, p)["directory_selected_id"],
        before["directory_selected_id"]
    );
    control(&f, p, "msg_dir:personal:one");
    f.send(p, json!({"op":"test_directory_key","key":"return"}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "composer_error")["text"],
        "Enter a message to send."
    );
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["composer_open"], false);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["settings_open"], true);
    control(&f, p, "open_uninstall_confirm");
    let modal = inspect(&f, p);
    assert_eq!(
        node(&modal, "settings_items", "uninstall_modal_overlay")["width"],
        520
    );
    assert_eq!(
        node(&modal, "settings_items", "settings_footer")["hidden"],
        false
    );
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["settings_open"], true);
    for appearance in ["light", "dark"] {
        let s = inspect_as(&f, p, Some(appearance));
        for invariant in [
            "truncated_labels",
            "overlapping_siblings",
            "clipped_views",
            "low_contrast_labels",
            "jargon_leaks",
            "address_mismatches",
        ] {
            assert_eq!(s[invariant], json!([]), "{appearance}: {invariant}");
        }
        assert!(!s["settings_items"].as_array().unwrap().is_empty());
        render_png_as(
            &f,
            p,
            "settings",
            &format!("v40-settings-{appearance}.png"),
            Some(appearance),
        );
    }
}

#[test]
fn appkit_fragmented_utf8_invalid_frames_activity_and_eof() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let bytes = format!("{}\n", state()).into_bytes();
    let split = bytes.iter().position(|b| *b == 0xc3).unwrap() + 1;
    f.send_raw(p, &bytes[..split]);
    f.send_raw(p, &bytes[split..]);
    wait(|| f.output(p, true).contains("state hubs=2 listeners=1"));
    f.send_raw(p, b"bad json\n");
    f.send_raw(p, &vec![b'x'; 1_048_577]);
    f.send_raw(p, b"\n");
    f.send(p, state());
    inspect(&f, p);
    assert!(!f.exited(p));
    assert!(f.output(p, true).contains("invalid frame"));
    assert!(f.output(p, true).contains("oversized frame"));
    control(&f, p, "open_activity");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "open_directory" && v["mode"] == "activity")
    });
    assert_eq!(inspect(&f, p)["directory_mode"], "activity");
    f.eof(p);
    wait(|| f.exited(p));
}
#[test]
fn daemon_ui_action_stream_accepts_full_messages_and_recovers_after_oversized_lines() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    let ui = f.dir.join("action-ui.py");
    script(
        &ui,
        r#"#!/usr/bin/env python3
import json, os, sys
sent = False
with open(os.environ['HOME'] + '/ui-frames', 'a') as out:
    for line in sys.stdin:
        out.write(line)
        out.flush()
        frame = json.loads(line)
        target = next((row['listener_id'] for row in frame.get('listeners', [])
                       if row.get('harness') == 'fixture'), None)
        if sent or not target:
            continue
        sent = True
        def action(message):
            return json.dumps({'op': 'send_signal', 'to': target, 'message': message},
                              ensure_ascii=False).encode('utf-8')
        output = sys.stdout.buffer
        output.write(action('😀' * 65_536) + b'\n')
        output.write(action('After maximum-size message') + b'\n')
        exact = action('Exact action limit')
        output.write(exact + b' ' * (1_048_576 - len(exact)) + b'\n')
        oversized = action('😀' * 262_144)
        # Force the bounded read to end inside a UTF-8 sequence.
        padding = (2 - oversized.index('😀'.encode('utf-8'))) % 4
        output.write(b' ' * padding + oversized + b'\n')
        output.write(action('After oversized action') + b'\n')
        output.flush()
"#,
    );
    let daemon = f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    f.tool(
        "cito_listen",
        json!({"about":"UI action receiver","scope":"workspace"}),
        meta,
    );
    let frames = || -> Vec<Value> {
        fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    };
    wait(|| {
        frames()
            .iter()
            .filter(|frame| frame["op"] == "composer_result")
            .count()
            == 4
    });
    let frames = frames();
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["op"] == "composer_result")
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            json!({"op":"composer_result","ok":true,"error":null,"request_id":null}),
            json!({"op":"composer_result","ok":true,"error":null,"request_id":null}),
            json!({"op":"composer_result","ok":true,"error":null,"request_id":null}),
            json!({"op":"composer_result","ok":true,"error":null,"request_id":null}),
        ]
    );
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["op"] == "error")
            .cloned()
            .collect::<Vec<_>>(),
        vec![json!({"op":"error","error":"INVALID_INPUT: Message is too large to send"})]
    );
    assert_eq!(
        f.db("hub.sqlite")
            .query(
                "SELECT json_extract(CAST(data AS TEXT),'$.message') AS message FROM hub_signals ORDER BY seq",
                &[],
            )
            .unwrap(),
        vec![
            json!({"message":"😀".repeat(65_536)}),
            json!({"message":"After maximum-size message"}),
            json!({"message":"Exact action limit"}),
            json!({"message":"After oversized action"}),
        ]
    );
    assert!(!f.exited(daemon));
}
#[test]
fn daemon_owns_headless_ui_child_and_reaps_it_on_shutdown() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"local":{"url":"local://","autoListen":false}}}));
    let fake = f.recording_ui();
    let daemon = f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", fake.to_str().unwrap()),
    ]);
    wait(|| f.home.join("ui-pid").exists());
    f.action(json!({"op":"open_settings"}));
    wait(|| {
        fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .contains("settings")
    });
    let pid = fs::read_to_string(f.home.join("ui-pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    f.action(json!({"op":"open_menu"}));
    f.action(json!({"op":"open_directory"}));
    // A crash closes the connection and replacement starts with every surface
    // closed. An external mutation then emits status, never a full inventory.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    let mut replacement = pid;
    wait(|| {
        replacement = fs::read_to_string(f.home.join("ui-pid"))
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(pid);
        replacement != pid
    });
    wait(|| {
        !fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .is_empty()
    });
    f.db("state.sqlite").execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,hub_url,principal_id) SELECT hub,'crash-state',listener_id,1,1,'bob','host','Sender','Work','now',?,'RECEIVED',hub_url,principal_id FROM local_listeners WHERE hub='local' AND harness='human' AND status='ACTIVE'", &[json!(cito_native::plugins::now_ms()+60_000)]).unwrap();
    let mut frame = Value::Null;
    wait(|| {
        frame = fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|s| serde_json::from_str::<Value>(s).ok())
            .rfind(|v| v["op"] == "state" && v["unread_count"] == 1)
            .unwrap_or(Value::Null);
        !frame.is_null()
    });
    assert!(frame.get("settings").is_none(), "{frame}");
    assert!(frame.get("activity").is_none(), "{frame}");
    f.stop(daemon);
    wait(|| unsafe { libc::kill(replacement, 0) } != 0);
}
