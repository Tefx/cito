use super::support::*;
use serde_json::{json, Value};
use std::fs;

#[test]
fn lifecycle_channel_test_results_survive_refresh_until_mutation() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");

    for action in [
        "save_channel",
        "pair_channel",
        "toggle_channel",
        "unpair_channel",
        "clear_channel_service",
    ] {
        let configured = action != "clear_channel_service";
        cfg["channels"][0]["configured"] = json!(configured);
        cfg["channels"][0]["status"] = json!(if configured { "ready" } else { "hub_ready" });
        f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","ok":false,"error":"Invalid token"}));
        f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
        let mut frame = state();
        frame["settings"] = cfg.clone();
        frame["settings"]["desktop_banner_enabled"] = json!(false);
        f.send(p, frame);
        control(&f, p, "test_channel:channel");
        let s = inspect(&f, p);
        assert_eq!(
            node(&s, "settings_items", "settings_channel_detail:channel")["text"],
            "Invalid token",
            "{action}"
        );
        assert_eq!(
            node(&s, "settings_items", "settings_channel_status:channel")["text"],
            "⚠ Error",
            "{action}"
        );
        control(&f, p, &format!("{action}:channel"));
        let s = inspect(&f, p);
        assert_eq!(
            node(&s, "settings_items", "settings_channel_detail:channel")["text"],
            "Ready",
            "{action} must clear the previous result"
        );
        assert_eq!(
            node(&s, "settings_items", "settings_channel_status:channel")["text"],
            if configured {
                "● Connected"
            } else {
                "Hub Service: ● Ready"
            }
        );
    }

    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","ok":true,"detail":"Test sent"}));
    f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "settings_channel_detail:channel"
        )["text"],
        "Test sent"
    );
    f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","ok":false,"error":"Invalid token"}));
    cfg["channels"][0]["configured"] = json!(false);
    cfg["channels"][0]["status"] = json!("not_configured");
    f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "settings_channel_detail:channel")["text"],
        "Ready"
    );
    assert_eq!(
        node(&s, "settings_items", "settings_channel_status:channel")["text"],
        "○ Not Configured"
    );
}

#[test]
fn lifecycle_channel_ui_retains_redacted_secret_and_preserves_pairing_on_refresh() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
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
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:secret")["secure"],
        true
    );
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:secret")["placeholder"],
        "Saved — enter replacement"
    );
    assert!(!node(&s, "settings_items", "channel_setup_guide:channel").is_null());
    for (button, op) in [
        ("save_channel:channel", "channel_save"),
        ("test_channel:channel", "channel_test"),
        ("pair_channel:channel", "channel_pair"),
    ] {
        control(&f, p, button);
        wait(|| f.frames(p).iter().any(|v| v["op"] == op));
        let action = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|v| v["op"] == op)
            .unwrap();
        assert!(action["values"].get("secret").is_none(), "{action}");
    }
    cfg["channels"][0]["pairing_code"] = json!("cito-ABCDEFGHIJ");
    cfg["channels"][0]["pairing_instruction"] =
        json!("Send /start cito-ABCDEFGHIJ to your account");
    cfg["channels"][0]["status"] = json!("pairing_pending");
    for _ in 0..2 {
        f.send(p, json!({"op":"settings","settings":cfg}));
    }
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "settings_channel_status:channel")["text"],
        "◌ Pairing…"
    );
    assert!(!node(&s, "settings_items", "pairing_instruction:channel").is_null());
    control(&f, p, "copy_pair_command:channel");
    assert_eq!(inspect(&f, p)["clipboard"], "/start cito-ABCDEFGHIJ");
    control(&f, p, "cancel_pair_channel:channel");
    wait(|| f.frames(p).iter().any(|v| v["op"] == "channel_cancel_pair"));
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("paused");
    cfg["channels"][0]["pairing_instruction"] = Value::Null;
    cfg["channels"][0]["pairing_code"] = Value::Null;
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "settings_channel_status:channel")["text"],
        "⏸ Paused"
    );
    assert_eq!(
        node(&s, "settings_items", "toggle_channel:channel")["text"],
        "Resume"
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
    control(&f, p, "toggle_onboarding_guide");
    let s = inspect(&f, p);
    assert!(!node(&s, "settings_items", "onboarding_banner").is_null());
    control(&f, p, "onboarding_step:channels");
    assert_eq!(inspect(&f, p)["settings_tab"], "channels");
    control(&f, p, "settings_tab:apps");
    control(&f, p, "goto_channels_tab");
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
    control(&f, p, "open_uninstall_confirm");
    control(&f, p, "confirm_disconnect_all");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "setup_disconnect_all")
    });
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
    f.send(p,json!({"op":"directory","peers":[peer("one")],"activity":[{"hub":"personal","signal_id":"read","listener_id":"me","from_user":"bob","from_host_name":"laptop","from_title":"Sender","target_harness":"human","target_title":"Me","target_scoped_id":"personal:me","message":"Done","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true}]}));
    control(&f, p, "act_filter:unread");
    assert!(!node(&inspect(&f, p), "directory_items", "act_empty_show_all").is_null());
    control(&f, p, "act_empty_show_all");
    assert_eq!(inspect(&f, p)["activity_filter"], "all");
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
fn appkit_activity_reply_filters_navigation_and_read_all() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    let activity = json!([{"hub":"personal","signal_id":"personal:sig_unread","listener_id":"c_human","from_user":"bob","from_host_name":"laptop","from_harness":"fixture","from_title":"Schema work","from_scoped_id":"personal:one","target_harness":"human","target_title":"Me","target_scoped_id":"personal:me","message":"Approve?","held_reason":null,"in_reply_to":null,"created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true},{"hub":"personal","signal_id":"personal:sig_read","listener_id":"c_other","from_user":"alice","from_host_name":"desktop","from_harness":"fixture","from_title":"Other","from_scoped_id":"personal:two","target_harness":"fixture","target_title":"Other","target_scoped_id":"personal:other","message":"Done","held_reason":null,"in_reply_to":null,"created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":false}]);
    frame["activity"] = activity.clone();
    frame["unread_count"] = json!(1);
    f.send(p, frame);
    f.send(p,json!({"op":"directory","peers":[peer("one")],"matched_count":1,"online_count":2,"activity":activity,"unread_count":1}));
    control(&f, p, "open_directory");
    let s = inspect(&f, p);
    assert_eq!(s["directory_mode"], "activity");
    assert_eq!(
        node(&s, "directory_items", "directory_shortcuts")["text"],
        "↵ Reply    ⎋ Close"
    );
    control(&f, p, "act_filter:all");
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    assert_eq!(inspect(&f, p)["directory_selected_id"], "personal:sig_read");
    f.send(p, json!({"op":"test_directory_key","key":"up"}));
    assert_eq!(
        inspect(&f, p)["directory_selected_id"],
        "personal:sig_unread"
    );
    control(&f, p, "act_filter:unread");
    assert!(node(
        &inspect(&f, p),
        "directory_items",
        "activity_row:personal:sig_read"
    )
    .is_null());
    control(&f, p, "reply_activity:personal:sig_unread");
    let s = inspect(&f, p);
    assert_eq!(s["composer_target"], "personal:one");
    assert_eq!(s["composer_reply_signal_id"], "personal:sig_unread");
    assert_eq!(
        node(&s, "directory_items", "directory_shortcuts")["text"],
        "↵ Send    ⎋ Cancel Reply"
    );
    f.send(p, json!({"op":"test_composer_input","value":"Approved"}));
    control(&f, p, "composer_send");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "send_signal"
                && v["in_reply_to"] == "personal:sig_unread"
                && v["message"] == "Approved"
        })
    });
    control(&f, p, "composer_cancel");
    control(&f, p, "activity_mark_all_read");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "ack_signals" && v["ack_all"] == true)
    });
}
#[test]
fn appkit_available_scroll_hub_selector_and_noop_state_diff() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["available"]=json!((0..5).map(|i|json!({"harness":"fixture","session_id":format!("candidate-{i}"),"title":format!("Candidate {i}"),"cwd":"/tmp/work","endpoint_pid":4242,"listening_hubs":["personal"]})).collect::<Vec<_>>());
    f.send(p, frame.clone());
    let s = inspect(&f, p);
    assert_eq!(node(&s, "items", "available_scroll")["height"], 144);
    assert_eq!(s["available_count"], 5);
    assert_eq!(node(&s, "items", "target_hub:work")["text"], "work");
    control(&f, p, "listen:fixture:candidate-0");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "listen_session" && v["hub"] == "work" && v["session_id"] == "candidate-0"
        })
    });
    control(&f, p, "target_hub:personal");
    assert!(!node(&inspect(&f, p), "items", "available_empty_target").is_null());
    // First response settles the pending Listen mutation; the next identical state is idle.
    f.send(p, frame.clone());
    let before = inspect(&f, p)["rebuilds"].clone();
    f.send(p, frame);
    assert_eq!(inspect(&f, p)["rebuilds"], before);
    control(&f, p, "switch_hub:personal:repo-abcdefghijkl");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "switch_hub" && v["to_hub"] == "work")
    });
}
fn hubs() -> Value {
    json!([{"name":"personal","status":"connected","user":"alice","auth_mode":"oidc","error":null},{"name":"work","status":"connected","user":"alice","auth_mode":"none","error":null}])
}
fn listener() -> Value {
    json!({"hub":"personal","listener_id":"c_abcdefghijkl","public_id":"repo-abcdefghijkl","scoped_id":"personal:repo-abcdefghijkl","harness":"fixture","session_id":"s1","title":"café conversation","cwd":"/tmp","repo":null,"scope":"self"})
}
fn state() -> Value {
    json!({"op":"state","hubs":hubs(),"listeners":[listener()],"harnesses":[{"id":"fixture","display_name":"Fixture","sf_symbol":"terminal","tint":"purple","tier":"stable","order":0}],"unread_count":0,"directory_online_count":2})
}
fn start(f: &Fixture) -> usize {
    let ui = root().join("bin/cito-ui-darwin-arm64");
    f.spawn(
        ui.to_str().unwrap(),
        &[],
        &[("CITO_BANNER_DISMISS_MS", "80")],
    )
}
fn inspect(f: &Fixture, p: usize) -> Value {
    let count = f
        .output(p, true)
        .lines()
        .filter(|l| l.starts_with("cito-ui: menu "))
        .count();
    f.send(p, json!({"op":"test_inspect"}));
    wait(|| {
        f.output(p, true)
            .lines()
            .filter(|l| l.starts_with("cito-ui: menu "))
            .count()
            > count
    });
    let output = f.output(p, true);
    serde_json::from_str(
        output
            .lines()
            .rev()
            .find_map(|l| l.strip_prefix("cito-ui: menu "))
            .unwrap(),
    )
    .unwrap()
}
fn nodes(tree: &Value) -> Vec<Value> {
    let mut result = Vec::new();
    if let Some(rows) = tree.as_array() {
        for node in rows {
            result.push(node.clone());
            result.extend(nodes(&node["children"]));
        }
    }
    result
}
fn node(snapshot: &Value, surface: &str, id: &str) -> Value {
    nodes(&snapshot[surface])
        .into_iter()
        .find(|v| v["id"] == id)
        .unwrap_or(Value::Null)
}
fn control(f: &Fixture, p: usize, id: &str) {
    f.send(p, json!({"op":"test_control","id":id}));
}
fn peer(id: &str) -> Value {
    json!({"hub":"personal","listener_id":format!("c_{id}"),"public_id":id,"scoped_id":format!("personal:{id}"),"user":"bob","host_id":"remote","host_name":"laptop","harness":"fixture","title":id,"about":"Schema work","repo":"acme/service","branch":"main","scope":"workspace","last_seen_at":"2030-01-01T00:00:00Z","is_self_principal":false,"channels":["laptop"]})
}
fn banner() -> Value {
    json!({"op":"banner","payload":{"target_harness":"fixture","target_session_id":"s1","target_title":"café conversation","from_user":"alice","from_host":"host","from_title":"sender","message":"hello café","effective_wake":true,"signal_id":"personal:sig_1","hub":"personal","from_scoped_id":"personal:sender-abcdefgh2345"}})
}
#[test]
fn appkit_card_tree_geometry_icon_hover_copy_and_multi_hub_identity() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    f.send(p, state());
    let s = inspect(&f, p);
    assert_eq!(s["count"], 1);
    assert_eq!(s["icon_template"], true);
    assert_eq!(s["icon_width"], 20);
    assert_eq!(s["button_title"], " 1");
    assert!(s["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["width"] == 380));
    assert_eq!(node(&s, "items", "header")["height"], 40);
    assert_eq!(node(&s, "items", "footer")["height"], 36);
    assert_eq!(node(&s, "items", "drawer:personal")["hidden"], true);
    control(&f, p, "hub:personal");
    assert_eq!(
        node(&inspect(&f, p), "items", "drawer:personal")["hidden"],
        false
    );
    control(&f, p, "copy:personal:repo-abcdefghijkl");
    assert_eq!(inspect(&f, p)["clipboard"], "personal:repo-abcdefghijkl");
    for appearance in ["dark", "light"] {
        f.send(p, json!({"op":"test_appearance","appearance":appearance}));
        f.send(p, json!({"op":"test_hover","id":"quit","hovered":true}));
        let s = inspect(&f, p);
        assert_eq!(node(&s, "items", "quit")["hovered"], true);
        f.send(p, json!({"op":"test_menu_close"}));
        assert!(nodes(&inspect(&f, p)["items"])
            .iter()
            .all(|v| v["hovered"] != true));
    }
    let mut next = state();
    let mut second = listener();
    second["hub"] = json!("work");
    second["scoped_id"] = json!("work:repo-workabcdefgh");
    second["listener_id"] = json!("c_workabcdefgh");
    next["listeners"] = json!([listener(), second]);
    f.send(p, next);
    let s = inspect(&f, p);
    assert_eq!(s["count"], 1);
    assert!(!node(&s, "items", "session:work:repo-workabcdefgh").is_null());
    assert!(!node(&s, "items", "session:personal:repo-abcdefghijkl").is_null());
}
#[test]
fn appkit_directory_keyboard_navigation_shortcuts_and_preserved_peers() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    f.send(p, state());
    control(&f, p, "open_directory");
    f.send(p,json!({"op":"directory","peers":[peer("one"),peer("two")],"matched_count":2,"online_count":5,"unread_count":0,"activity":[]}));
    let s = inspect(&f, p);
    assert_eq!(s["directory_open"], true);
    assert_eq!(s["directory_selected_id"], "personal:one");
    assert_eq!(node(&s, "directory_items", "directory_panel")["width"], 560);
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    assert_eq!(inspect(&f, p)["directory_selected_id"], "personal:two");
    f.send(p, json!({"op":"test_directory_key","key":"return"}));
    assert_eq!(inspect(&f, p)["clipboard"], "personal:two");
    f.send(p, json!({"op":"test_directory_key","key":"up"}));
    assert_eq!(inspect(&f, p)["directory_selected_id"], "personal:one");
    control(&f, p, "msg_dir:personal:one");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Draft preserved"}),
    );
    control(&f, p, "composer_send");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "send_signal" && v["message"] == "Draft preserved")
    });
    f.send(
        p,
        json!({"op":"composer_result","ok":false,"error":"Hub unavailable. Retry."}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(
        node(&s, "directory_items", "composer_input")["text"],
        "Draft preserved"
    );
    assert_eq!(
        node(&s, "directory_items", "composer_error")["text"],
        "Hub unavailable. Retry."
    );
    control(&f, p, "composer_cancel");
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_shortcuts")["text"],
        "↵ Copy Selected ID    ⎋ Close"
    );
    control(&f, p, "dir_mode:activity");
    f.send(p, state());
    control(&f, p, "dir_mode:directory");
    assert!(!node(&inspect(&f, p), "directory_items", "msg_dir:personal:one").is_null());
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["directory_open"], false);
}
fn settings() -> Value {
    json!({"apps":[{"id":"pi","display_name":"Pi","tier":"stable","installed":true,"configured":true,"status":"configured","detail":"Connected","paths":["/tmp/settings.json"]},{"id":"codex","display_name":"Codex","tier":"stable","installed":true,"configured":false,"status":"not_configured","detail":"Click Set Up to connect","paths":[]},{"id":"cursor","display_name":"Cursor","tier":"experimental","installed":false,"configured":false,"status":"not_configured","detail":"Click Set Up to connect","paths":[]}],"channels":[{"id":"channel","display_name":"Human Channel","sf_symbol":"paperplane.fill","tint":"blue","enabled":true,"configured":false,"status":"hub_ready","detail":"Ready","hub_provided":false,"pairing_supported":true,"fields":[{"key":"secret","scope":"hub","label":"Secret","type":"secret","required":true,"placeholder":"Token","value":"secret","configured":true},{"key":"account","scope":"user","label":"Account","type":"string","required":true,"placeholder":"Account","value":"","configured":false}]}],"desktop_banner_enabled":true,"hubs":[{"name":"personal","url":"local://","auth_mode":"none","enabled":true,"is_default":true,"status":"connected","user":"alice","error":null},{"name":"work","url":"local://","auth_mode":"none","enabled":true,"is_default":false,"status":"connected","user":"alice","error":null}],"host_hub":{"enabled":false,"running":false,"port":4291,"endpoint_url":"http://127.0.0.1:4291","token":"","invite_command":"","error":null},"launch_at_login":false})
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
        assert_eq!(
            node(&s, "settings_items", &format!("settings_app_status:{id}"))["text"],
            if configured {
                "● Connected"
            } else {
                "○ Not Set Up"
            }
        );
        assert_eq!(
            node(&s, "settings_items", &format!("setup_app:{id}"))["text"],
            if configured { "Re-check" } else { "Set Up" }
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
    f.send(p,json!({"op":"test_settings_input","id":"channel_field:channel:secret","value":"new-secret"}));
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
            v["op"] == "channel_save"
                && v["values"]["account"] == "server-account"
                && v["values"]["secret"] == "new-secret"
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
        true
    );
    assert!(!f.frames(p).iter().any(|v| v["op"] == "setup_uninstall_all"));
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
            v["op"] == "setup_save_hub" && v["name"] == "shared" && v["token"] == "test-token"
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
            "cito setup --hub lan --url {endpoint} --auth token --token lan-test-token --default"
        )
    );
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
            .any(|v| v["op"] == "setup_save_hub")
    });
    let action = client
        .frames(ui)
        .into_iter()
        .find(|v| v["op"] == "setup_save_hub")
        .unwrap();
    assert_eq!(action["url"], endpoint);
    assert_eq!(action["auth_mode"], "token");
    assert_eq!(action["token"], "lan-test-token");
    assert_eq!(action["set_default"], true);
    let (status, result) = client.action(action);
    assert_eq!(status, 200, "{result}");
    let (status, result) = client.action(json!({"op":"test_hub","hub":"lan"}));
    assert_eq!(status, 200, "{result}");
    assert!(result["streams"].is_number());
    let configured: Value =
        serde_json::from_str(&fs::read_to_string(&client.config).unwrap()).unwrap();
    assert_eq!(configured["default_hub"], "lan");
    let (status, exported) = client.action(json!({"op":"generate_hub_invite","hub":"lan"}));
    assert_eq!(status, 200);
    assert_eq!(exported["invite"]["url"], endpoint);
    assert_eq!(exported["invite"]["authMode"], "token");
    assert_eq!(exported["invite"]["token"], "lan-test-token");

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
            format!("cito setup --hub lan --url http://{authority}:{port} --default")
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

#[test]
fn appkit_fragmented_utf8_invalid_frames_wake_banner_and_eof() {
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
    f.send(p, banner());
    wait(|| f.output(p, true).contains("display "));
    wait(|| f.output(p, true).contains("banner hidden"));
    assert!(!f.exited(p));
    assert!(f.output(p, true).contains("invalid frame"));
    assert!(f.output(p, true).contains("oversized frame"));
    f.send(p, banner());
    f.send(p, json!({"op":"test_click"}));
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
            == 5
    });
    let frames = frames();
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["op"] == "composer_result")
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            json!({"op":"composer_result","ok":true,"error":null}),
            json!({"op":"composer_result","ok":true,"error":null}),
            json!({"op":"composer_result","ok":true,"error":null}),
            json!({"op":"composer_result","ok":false,"error":"INVALID_INPUT: UI action exceeds 1 MiB"}),
            json!({"op":"composer_result","ok":true,"error":null}),
        ]
    );
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["op"] == "error")
            .cloned()
            .collect::<Vec<_>>(),
        vec![json!({"op":"error","error":"INVALID_INPUT: UI action exceeds 1 MiB"})]
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
    let f = Fixture::new(json!({}));
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
    f.stop(daemon);
    wait(|| unsafe { libc::kill(pid, 0) } != 0);
}
