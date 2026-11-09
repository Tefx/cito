use super::support::*;
use serde_json::{json, Value};
use std::fs;

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
    assert_eq!(s["icon_width"], 18);
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
    json!({"apps":[],"channels":[{"id":"channel","display_name":"Human Channel","sf_symbol":"paperplane.fill","tint":"blue","enabled":true,"configured":false,"status":"hub_ready","detail":"Ready","hub_provided":false,"pairing_supported":true,"fields":[{"key":"secret","scope":"hub","label":"Secret","type":"secret","required":true,"placeholder":"Token","value":"secret","configured":true},{"key":"account","scope":"user","label":"Account","type":"string","required":true,"placeholder":"Account","value":"","configured":false}]}],"desktop_banner_enabled":true,"hubs":[{"name":"personal","url":"local://","auth_mode":"none","enabled":true,"is_default":true,"status":"connected","user":"alice","error":null},{"name":"work","url":"local://","auth_mode":"none","enabled":true,"is_default":false,"status":"connected","user":"alice","error":null}],"host_hub":{"enabled":false,"running":false,"port":4291,"endpoint_url":"http://127.0.0.1:4291","token":"","invite_command":"","error":null},"launch_at_login":false})
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
    assert_eq!(inspect(&f, p)["settings_open"], true);
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
fn daemon_owns_headless_ui_child_and_reaps_it_on_shutdown() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let fake = f.dir.join("ui.py");
    script(&fake,"#!/usr/bin/env python3\nimport os,sys\nopen(os.environ['HOME']+'/ui-pid','w').write(str(os.getpid()))\nwith open(os.environ['HOME']+'/ui-frames','a') as out:\n for line in sys.stdin:\n  out.write(line); out.flush()\n");
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
