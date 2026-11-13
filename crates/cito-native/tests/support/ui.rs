use super::support::*;
use serde_json::{json, Value};
use std::fs;

/// Real daemon subprocess with its production dispatch_action path and UI mailbox.
/// The recording endpoint preserves daemon frame order; no state/settings/result
/// frames are synthesized by this relay. All children use Fixture's cleanroom API.
struct ClosedLoop<'a> {
    f: &'a Fixture,
    ui: usize,
    actions: usize,
    frames: Vec<Value>,
    results: Vec<(Value, u16, Value)>,
}
impl<'a> ClosedLoop<'a> {
    fn new(f: &'a Fixture) -> Self {
        let recorder = f.recording_ui();
        f.daemon(&[
            ("CITO_DISABLE_UI", "0"),
            ("CITO_UI_BINARY", recorder.to_str().unwrap()),
        ]);
        Self {
            f,
            ui: start(f),
            actions: 0,
            frames: Vec::new(),
            results: Vec::new(),
        }
    }
    fn pump(&mut self) {
        let actions = self.f.frames(self.ui);
        for action in &actions[self.actions..] {
            let (status, result) = self.f.action(action.clone());
            self.results.push((action.clone(), status, result));
        }
        self.actions = actions.len();
        let frames: Vec<Value> = fs::read_to_string(self.f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        for frame in &frames[self.frames.len()..] {
            self.f.send(self.ui, frame.clone());
            inspect(self.f, self.ui);
        }
        self.frames = frames;
    }
    fn until(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
        loop {
            self.pump();
            let s = inspect(self.f, self.ui);
            if predicate(&s) {
                return s;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Closed-loop timeout: {s}; results={:?}",
                self.results
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    fn control(&mut self, id: &str) {
        control(self.f, self.ui, id);
        inspect(self.f, self.ui);
    }
    fn input(&mut self, id: &str, value: &str) {
        self.f.send(
            self.ui,
            json!({"op":"test_settings_input","id":id,"value":value}),
        );
        inspect(self.f, self.ui);
    }
    fn snapshot(&mut self, surface: &str, name: &str) {
        self.pump();
        inspect(self.f, self.ui);
        render_png(self.f, self.ui, surface, name);
        inspect(self.f, self.ui);
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
            "titled":true,"full_size_content":true,"hidden_title":true,"transparent_titlebar":true,"hidden_window_buttons":true
        }, {
            "titled":true,"full_size_content":true,"hidden_title":true,"transparent_titlebar":true,"hidden_window_buttons":true
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
    save_secret("replacement secure value + punctuation");

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
        "Pause"
    );
    assert_eq!(
        node(&s, "settings_items", "stratum_hub:channel")["height"],
        36
    );
    l.control("toggle_channel:channel");
    l.until(|s| node(s, "settings_items", "toggle_channel:channel")["text"] == "Resume");
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
            == "Enter a valid Hub URL (http://ip:4291 or https://host)."
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
    l.input("settings_hub_url", "local://");
    l.control("submit_add_hub");
    l.until(|s| {
        node(s, "settings_items", "add_hub_form").is_null()
            && !node(s, "settings_items", "settings_hub:scratch").is_null()
    });
    // The same form remains open through validation and daemon errors, and closes
    // only on a real settings/state acknowledgement of the saved Hub.
    l.control("toggle_host_hub");
    l.until(|s| node(s, "settings_items", "toggle_host_hub")["text"] == "● Sharing");
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
            .contains("Cannot bind host hub listener:")
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
    // Both Apply submissions own identities; the sharing toggle only uses the active port.
    assert_eq!(
        l.results
            .iter()
            .filter(
                |(action, _, _)| action["op"] == "set_host_hub" && action["request_id"].is_string()
            )
            .count(),
        2
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
fn r1_save_acknowledgements_clear_only_matching_secret_intent_not_tests_or_newer_drafts() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    cfg["channels"][0]["fields"][0]["value"] = json!("");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "edit_hub_service:channel");
    let save = |value: &str| {
        f.send(
            p,
            json!({"op":"test_settings_input","id":"channel_field:channel:secret","value":value}),
        );
        control(&f, p, "save_hub_service:channel");
        inspect(&f, p);
        f.frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "save_hub_service")
            .unwrap()
    };
    let first = save("one");
    let second = save("two");
    assert_ne!(first["request_id"], second["request_id"]);
    f.send(p,json!({"op":"settings","ok":true,"source_op":"save_hub_service","channel_id":"channel","request_id":first["request_id"],"settings":cfg}));
    assert!(
        !node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:secret"
        )
        .is_null(),
        "An older save cannot close the current edit"
    );
    f.send(p,json!({"op":"settings","ok":true,"source_op":"channel_test","channel_id":"channel","settings":cfg}));
    assert!(
        !node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:secret"
        )
        .is_null(),
        "A test is not a save acknowledgement"
    );
    f.send(p,json!({"op":"test_settings_input","id":"channel_field:channel:secret","value":"newer-draft"}));
    f.send(p,json!({"op":"settings","ok":true,"source_op":"save_hub_service","channel_id":"channel","request_id":second["request_id"],"settings":cfg}));
    let newer = inspect(&f, p);
    assert!(
        !node(&newer, "settings_items", "channel_field:channel:secret").is_null(),
        "A matching acknowledgement must keep a newer draft editable"
    );
    assert_ne!(
        node(&newer, "settings_items", "channel_summary:channel")["text"],
        "✓ Saved"
    );
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    let third = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "save_hub_service")
        .unwrap();
    assert_eq!(third["values"], json!({"secret":"newer-draft"}));
    f.send(p,json!({"op":"settings","ok":false,"source_op":"save_hub_service","channel_id":"channel","request_id":third["request_id"],"settings":cfg}));
    assert!(!node(
        &inspect(&f, p),
        "settings_items",
        "channel_field:channel:secret"
    )
    .is_null());
    f.send(p,json!({"op":"settings","ok":true,"source_op":"save_hub_service","channel_id":"channel","request_id":third["request_id"],"settings":cfg}));
    assert!(
        !node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:secret"
        )
        .is_null(),
        "A late success cannot acknowledge an already failed submission"
    );
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    let retry = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "save_hub_service")
        .unwrap();
    assert_ne!(retry["request_id"], third["request_id"]);
    assert_eq!(retry["values"], json!({"secret":"newer-draft"}));
    f.send(p,json!({"op":"settings","ok":true,"source_op":"save_hub_service","channel_id":"channel","request_id":retry["request_id"],"settings":cfg}));
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "stratum_hub:channel")["height"],
        36
    );
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "save_hub_service")
            .unwrap()["values"],
        json!({})
    );
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["paired"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "edit_user_channel:channel");
    f.send(p,json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"submitted"}));
    control(&f, p, "save_channel:channel");
    inspect(&f, p);
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "channel_save")
        .unwrap();
    f.send(p,json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"newer-account"}));
    cfg["channels"][0]["fields"][1]["value"] = json!("submitted");
    f.send(p,json!({"op":"settings","ok":true,"source_op":"channel_save","channel_id":"channel","request_id":request["request_id"],"settings":cfg}));
    let newer = inspect(&f, p);
    assert_eq!(
        node(&newer, "settings_items", "channel_field:channel:account")["text"],
        "newer-account"
    );
    assert_eq!(
        node(&newer, "settings_items", "save_channel:channel")["text"],
        "Save"
    );
    control(&f, p, "save_channel:channel");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "channel_save")
            .unwrap()["values"]["account"],
        "newer-account"
    );
    cfg["host_hub"]["enabled"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "settings_tab:hubs");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"4501"}),
    );
    control(&f, p, "apply_host_hub_port");
    inspect(&f, p);
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "set_host_hub")
        .unwrap();
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"4502"}),
    );
    cfg["host_hub"]["port"] = json!(4501);
    f.send(p,json!({"op":"settings","ok":true,"source_op":"set_host_hub","request_id":request["request_id"],"settings":cfg}));
    let newer = inspect(&f, p);
    assert_eq!(
        node(&newer, "settings_items", "host_hub_port")["text"],
        "4502"
    );
    assert_eq!(
        node(&newer, "settings_items", "apply_host_hub_port")["text"],
        "Apply",
        "An old acknowledgement must not mark a newer port draft saved"
    );
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":" 4502 "}),
    );
    control(&f, p, "apply_host_hub_port");
    inspect(&f, p);
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "set_host_hub")
        .unwrap();
    cfg["host_hub"]["port"] = json!(4502);
    f.send(p,json!({"op":"settings","ok":true,"source_op":"set_host_hub","request_id":request["request_id"],"settings":cfg}));
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["text"],
        "✓ Saved"
    );
}

#[test]
fn r1_overlapping_channel_scopes_acknowledge_secrets_and_gateway_edits_independently() {
    let _lock = serial();
    for hub_first in [false, true] {
        let f = Fixture::new(json!({}));
        let p = start(&f);
        let mut cfg = settings();
        cfg["settings_hub"] = json!("personal");
        cfg["channels"][0]["fields"][0]["value"] = json!("");
        cfg["channels"][0]["fields"][1]["type"] = json!("secret");
        cfg["channels"][0]["fields"][1]["configured"] = json!(true);
        let mut frame = state();
        frame["settings"] = cfg.clone();
        f.send(p, frame);
        control(&f, p, "open_settings");
        control(&f, p, "settings_tab:channels");
        control(&f, p, "edit_hub_service:channel");
        for (field, value, control_id) in [
            ("secret", "gateway-token", "save_hub_service:channel"),
            ("account", "personal-token", "save_channel:channel"),
        ] {
            f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{field}"),"value":value}));
            control(&f, p, control_id);
        }
        inspect(&f, p);
        let latest = |op: &str| {
            f.frames(p)
                .into_iter()
                .rev()
                .find(|a| a["op"] == op)
                .unwrap()
        };
        let gateway = latest("save_hub_service");
        let personal = latest("channel_save");
        assert_eq!(gateway["values"], json!({"secret":"gateway-token"}));
        assert_eq!(personal["values"], json!({"account":"personal-token"}));
        assert_ne!(gateway["request_id"], personal["request_id"]);
        let ordered = if hub_first {
            [&gateway, &personal]
        } else {
            [&personal, &gateway]
        };
        for (index, action) in ordered.into_iter().enumerate() {
            f.send(p, json!({"op":"settings","ok":true,"source_op":action["op"],"channel_id":"channel","request_id":action["request_id"],"settings":cfg}));
            let s = inspect(&f, p);
            assert_eq!(
                node(&s, "settings_items", "stratum_hub:channel")["height"] == 36,
                hub_first || index == 1,
                "Only a gateway acknowledgement may collapse the gateway editor"
            );
            if !hub_first && index == 0 {
                control(&f, p, "save_channel:channel");
                inspect(&f, p);
                assert_eq!(latest("channel_save")["values"], json!({}));
                assert!(!node(&s, "settings_items", "channel_field:channel:secret").is_null());
            }
        }
        control(&f, p, "edit_hub_service:channel");
        control(&f, p, "save_hub_service:channel");
        control(&f, p, "save_channel:channel");
        inspect(&f, p);
        for op in ["save_hub_service", "channel_save"] {
            assert_eq!(
                latest(op)["values"],
                json!({}),
                "Acknowledged secrets must not be resubmitted: {op}"
            );
        }
    }
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
            ("settings_hub_url", "local://"),
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
        assert_eq!(submitted["url"], "local://");
        assert!(!submitted["request_id"].as_str().unwrap().is_empty());
        cfg["hubs"].as_array_mut().unwrap().push(json!({"name":"submitted","url":"local://","auth_mode":"token","enabled":true,"is_default":false,"status":"connected","user":"alice","error":null}));
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
                ("settings_hub_url", "local://"),
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
        cfg["hubs"].as_array_mut().unwrap().push(json!({"name":"submitted","url":"local://","auth_mode":"token","enabled":true,"is_default":false,"status":"connected","user":"alice","error":null}));
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
            json!({"op":"error","source_op":"","request_id":current["request_id"],"error":"Source-less correlated transport failure"}),
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

fn render_png(f: &Fixture, p: usize, surface: &str, name: &str) {
    if std::env::var_os("CITO_TEST_SNAPSHOT_DIR").is_some() {
        let stem = name.strip_suffix(".png").unwrap_or(name);
        for appearance in ["light", "dark"] {
            render_png_as(
                f,
                p,
                surface,
                &format!("{stem}_{appearance}.png"),
                Some(appearance),
            );
        }
    } else {
        render_png_as(f, p, surface, name, None);
    }
}
fn render_png_as(f: &Fixture, p: usize, surface: &str, name: &str, appearance: Option<&str>) {
    inspect_as(f, p, appearance);
    let Some(dir) = std::env::var_os("CITO_TEST_SNAPSHOT_DIR") else {
        return;
    };
    fs::create_dir_all(&dir).unwrap();
    let path = std::path::Path::new(&dir).join(name);
    f.send(
        p,
        json!({"op":"test_render_png","surface":surface,"path":path,"appearance":appearance}),
    );
    wait(|| {
        f.output(p, true)
            .contains(&format!("rendered {}", path.display()))
    });
    let data = fs::read(&path).unwrap();
    assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n");
    assert!(data.len() > 1000);
    let width = u32::from_be_bytes(data[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(data[20..24].try_into().unwrap());
    assert_eq!(
        width,
        if surface == "settings" {
            1040
        } else if surface == "directory" {
            1120
        } else if surface == "banner" {
            776
        } else {
            760
        }
    );
    if surface == "settings" {
        assert_eq!(height, 920);
    }
    assert!(height > 100);
    inspect_as(f, p, appearance);
}

#[test]
fn c1_c2_human_only_counter_and_real_disable_enable_normalize_title() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let mut l = ClosedLoop::new(&f);
    let s = l.until(|s| {
        nodes(&s["items"])
            .iter()
            .any(|n| n["text"] == "You (@alice)")
    });
    assert_eq!(s["count"], 0);
    assert_eq!(s["button_title"], "");
    assert_eq!(node(&s, "items", "header_subtitle")["text"], "Idle");
    assert!(node(&s, "items", "header_hub_strip").is_null());
    assert!(node(&s, "items", "drawer:local").is_null());
    assert!(node(&s, "items", "stop_all").is_null());
    assert_eq!(node(&s, "items", "open_settings")["x"], 12);
    let quit = node(&s, "items", "quit");
    assert_eq!(
        quit["x"].as_f64().unwrap() + quit["width"].as_f64().unwrap(),
        368.0
    );
    assert_eq!(
        f.rows("SELECT title, about FROM local_listeners WHERE harness='human'"),
        vec![json!({"title":"Human","about":"Human receiver"})]
    );
    let id = f.rows("SELECT public_id FROM local_listeners WHERE harness='human'")[0]["public_id"]
        .as_str()
        .unwrap()
        .to_string();
    let row_id = format!("session:{id}");
    assert_eq!(node(&s, "items", &row_id)["height"], 64);
    let pause = node(&s, "items", &format!("stop:{id}"));
    assert_eq!(pause["text"], "Pause");
    assert_eq!(pause["tooltip"], "Pause personal notifications");
    assert!(node(&s, "items", &format!("switch_hub:{id}")).is_null());
    l.control(&format!("stop:{id}"));
    l.until(|s| !node(s, "items", "listen_human").is_null());
    l.control("listen_human");
    let s = l.until(|s| !node(s, "items", &row_id).is_null());
    assert_eq!(node(&s, "items", &row_id)["height"], 64);
    assert!(nodes(&s["items"])
        .iter()
        .any(|n| n["text"] == "You (@alice)"));
    assert_eq!(
        f.rows("SELECT title FROM local_listeners WHERE harness='human'")[0]["title"],
        "Human"
    );
    assert_eq!(f.action(json!({"op":"setup_save_hub","name":"archive","url":"https://hub.example.com","auth_mode":"none","enabled":false})).0,200);
    l.control("open_settings");
    l.control("settings_tab:hubs");
    let s = l.until(|s| !node(s, "settings_items", "settings_hub:archive").is_null());
    assert!(node(&s, "settings_items", "toggle_hub:local").is_null());
    assert_eq!(
        node(&s, "settings_items", "toggle_hub:archive")["text"],
        "Enable"
    );
    assert!(node(&s, "items", "hub:archive").is_null());
    let edit = node(&s, "settings_items", "edit_hub:local");
    assert_eq!(
        edit["x"].as_f64().unwrap() + edit["width"].as_f64().unwrap(),
        468.0
    );
    l.control("edit_hub:local");
    assert_eq!(
        node(&inspect(&f, l.ui), "settings_items", "submit_add_hub")["text"],
        "Save"
    );
    l.control("cancel_add_hub");
    l.control("settings_tab:channels");
    let channels = inspect(&f, l.ui);
    assert!(node(&channels, "settings_items", "channel_hub_selector:archive").is_null());
    l.control("close_settings");
    for appearance in ["light", "dark"] {
        let s = inspect_as(&f, l.ui, Some(appearance));
        assert_eq!(s["low_contrast_labels"], json!([]), "{appearance}");
        render_png_as(
            &f,
            l.ui,
            "menu",
            &format!("c2_human_reenabled_{appearance}.png"),
            Some(appearance),
        );
    }
    let enrolled = f.tool(
        "cito_listen",
        json!({"about":"Human receiver"}),
        serde_json::from_str(HUMAN).unwrap(),
    );
    assert_eq!(enrolled["title"], "Human");
    l.control("open_directory");
    let s = l.until(|s| !node(s, "directory_items", &format!("dir_title:{id}")).is_null());
    assert_eq!(
        node(&s, "directory_items", &format!("dir_title:{id}"))["text"],
        "@alice (You)"
    );
    assert_eq!(
        node(&s, "directory_items", &format!("dir_meta:{id}"))["text"],
        "Human · mac-studio"
    );
    f.tool(
        "cito_listen",
        json!({"about":"Release approvals"}),
        serde_json::from_str(HUMAN).unwrap(),
    );
    l.until(|s| node(s, "items", &row_id)["height"] == 78);
    l.control(&format!("stop:{id}"));
    let paused = l.until(|s| !node(s, "items", "listen_human").is_null());
    assert_eq!(node(&paused, "items", "human_paused_card")["height"], 78);
    assert_eq!(
        node(&paused, "items", "human_paused_about")["text"],
        "Release approvals"
    );
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(snapshot["hubs"][0]["human_paused"], true);
    assert_eq!(snapshot["hubs"][0]["human_about"], "Release approvals");
    l.control("listen_human");
    l.until(|s| node(s, "items", &row_id)["height"] == 78);
    assert_eq!(
        f.rows("SELECT about FROM local_listeners WHERE harness='human'")[0]["about"],
        "Release approvals"
    );
}

#[test]
fn appkit_directory_human_labels_normalize_legacy_defaults_and_preserve_custom_details() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    f.send(p, state());
    control(&f, p, "open_directory");
    for (title, about, expected_title, expected_about) in [
        ("", "Human", "@bob", ""),
        ("Human", "Human", "@bob", ""),
        ("@bob", "@bob", "@bob", ""),
        ("@bob (Me)", "@bob (Me)", "@bob", ""),
        ("Human Node (@old)", "Human Node (@old)", "@bob", ""),
        ("Review desk", "Review desk", "Review desk", ""),
        (
            "Review desk",
            "Release approvals",
            "Review desk",
            "Release approvals",
        ),
    ] {
        let mut human = peer("human");
        human["harness"] = json!("human");
        human["display_address"] = json!("@bob");
        human["title"] = json!(title);
        human["about"] = json!(about);
        let mut agent = peer("agent");
        agent["title"] = json!("Human");
        agent["about"] = json!("Human");
        send_fixture(&f, p, json!({"op":"directory","peers":[human,agent]}));
        let s = inspect(&f, p);
        assert_eq!(
            node(&s, "directory_items", "dir_title:personal:human")["text"],
            expected_title
        );
        assert_eq!(
            node(&s, "directory_items", "dir_meta:personal:human")["text"],
            if expected_about.is_empty() {
                "Human · laptop".into()
            } else {
                format!("Human · laptop · {expected_about}")
            }
        );
        assert_eq!(
            node(&s, "directory_items", "dir_title:personal:agent")["text"],
            "Fixture"
        );
        assert_eq!(
            node(&s, "directory_items", "dir_meta:personal:agent")["text"],
            " · laptop"
        );
    }
    let mut frame = state();
    frame["hubs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"local","status":"connected","auth_mode":"none","user":"bob"}));
    f.send(p, frame.clone());
    let mut agent = peer("home");
    agent["session_id"] = json!("home-session");
    agent["title"] = json!("bob");
    agent["about"] = json!("Release audit");
    agent["cwd"] = json!("~");
    agent["repo"] = json!("");
    agent["branch"] = json!("");
    agent["host_name"] = json!("laptop.LOCAL");
    agent["channels"] = json!([
        "Mac",
        "Hub",
        "human",
        "Desktop notifications",
        "laptop.local",
        "mac-studio.local",
        "mac-studio"
    ]);
    frame["my_remote_listeners"] = json!([agent.clone()]);
    f.send(p, frame);
    let remote = node(&inspect(&f, p), "items", "remote:personal:home");
    let remote_info = nodes(&remote["children"])
        .into_iter()
        .find(|n| n["text"] == "laptop · Release audit")
        .unwrap();
    assert_eq!(remote_info["tooltip"], "personal:home");
    let mut shadow = agent.clone();
    shadow["hub"] = json!("local");
    shadow["scoped_id"] = json!("local:home");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[shadow,agent.clone()]}),
    );
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "directory_items", "dir_title:personal:home")["text"],
        "Release audit"
    );
    assert_eq!(
        node(&s, "directory_items", "dir_meta:personal:home")["text"],
        " · laptop, mac-studio"
    );
    assert_eq!(
        node(&s, "directory_items", "dir_repo:personal:home")["text"],
        " · ~"
    );
    assert!(node(&s, "directory_items", "dir_title:local:home").is_null());
    agent["about"] = json!("");
    send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
    assert_eq!(
        node(
            &inspect(&f, p),
            "directory_items",
            "dir_title:personal:home"
        )["text"],
        "Fixture"
    );
    // Foundation resolves the account home independently of the fixture's HOME.
    let home_basename = inspect(&f, p)["home_basename"].as_str().unwrap().to_owned();
    let at_home_basename = format!("@{home_basename}");
    // Description aliases share the daemon/UI process HOME, not the account database.
    let description_home = f.home.file_name().unwrap().to_str().unwrap();
    let at_description_home = format!("@{description_home}");
    for raw in [
        "bob",
        "@bob",
        home_basename.as_str(),
        at_home_basename.as_str(),
        "Fixture",
        "",
        "Agent",
        "Agent session",
        "Session",
        "CLI session",
        "Human",
        "Human node",
        "Human channel",
    ] {
        for (about, expected) in [
            ("CLI session", "Fixture"),
            ("bob", "Fixture"),
            ("@bob", "Fixture"),
            (description_home, "Fixture"),
            (at_description_home.as_str(), "Fixture"),
            (
                "account",
                if description_home.eq_ignore_ascii_case("account") {
                    "Fixture"
                } else {
                    "account"
                },
            ),
            (
                "@account",
                if description_home.eq_ignore_ascii_case("account") {
                    "Fixture"
                } else {
                    "@account"
                },
            ),
        ] {
            let expected = if raw == "Fixture" {
                raw
            } else if about.to_lowercase() == raw.to_lowercase() {
                "Fixture"
            } else {
                expected
            };
            agent["title"] = json!(raw);
            agent["about"] = json!(about);
            agent["cwd"] = json!(f.home);
            send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
            assert_eq!(
                node(
                    &inspect(&f, p),
                    "directory_items",
                    "dir_title:personal:home"
                )["text"],
                expected,
                "raw={raw}, about={about}"
            );
            if about == description_home || about == at_description_home {
                assert_eq!(
                    node(&inspect(&f, p), "directory_items", "dir_meta:personal:home")["tooltip"],
                    " · laptop, mac-studio"
                );
            }
        }
    }
    if home_basename.to_lowercase() != description_home.to_lowercase()
        && ![
            "bob",
            "@bob",
            "you (@bob)",
            "fixture",
            "human",
            "human node",
            "human channel",
            "human receiver",
            "you",
            "session",
            "coding session",
            "default",
            "agent",
            "agent session",
            "cli session",
        ]
        .contains(&home_basename.to_lowercase().as_str())
    {
        agent["title"] = json!("bob");
        agent["about"] = json!(home_basename);
        send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
        assert_eq!(
            node(
                &inspect(&f, p),
                "directory_items",
                "dir_title:personal:home"
            )["text"],
            home_basename,
            "A name outside this process's HOME and identity aliases remains distinct"
        );
    }
    agent["title"] = json!("Auth refactor");
    agent["custom_title"] = json!(false);
    for cwd in [json!("~"), json!(f.home)] {
        agent["cwd"] = cwd;
        for about in ["Auth refactor", "", "CLI session"] {
            agent["about"] = json!(about);
            send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
            let s = inspect(&f, p);
            assert_eq!(
                node(&s, "directory_items", "dir_title:personal:home")["text"],
                "Auth refactor"
            );
            assert!(
                !node(&s, "directory_items", "dir_meta:personal:home")["tooltip"]
                    .as_str()
                    .unwrap()
                    .contains("Auth refactor")
            );
        }
    }
    agent["custom_title"] = json!(true);
    for title in ["bob", "Human"] {
        agent["title"] = json!(title);
        send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
        assert_eq!(
            node(
                &inspect(&f, p),
                "directory_items",
                "dir_title:personal:home"
            )["text"],
            title
        );
    }
}

#[test]
fn c1_c3_c5_agent_counter_semantic_contrast_and_explicit_entries_in_both_appearances() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut human = listener();
    human["harness"] = json!("human");
    human["session_id"] = json!("default");
    human["title"] = json!(" ");
    human["about"] = json!("Human receiver");
    human["scoped_id"] = json!("personal:human");
    human["user"] = json!("bob");
    human["display_address"] = json!("@bob");
    let mut frame = state();
    frame["settings"] = settings();
    frame["listeners"] = json!([]);
    f.send(p, frame.clone());
    let idle = inspect(&f, p);
    assert_eq!(idle["button_title"], "");
    assert_eq!(
        node(&idle, "items", "header_subtitle")["text"],
        "Notifications paused"
    );
    for user in ["alice", "bob"] {
        human["user"] = json!(user);
        human["display_address"] = json!(format!("@{user}"));
        frame["listeners"] = json!([human.clone()]);
        f.send(p, frame.clone());
        let s = inspect(&f, p);
        assert_eq!(s["count"], 0);
        assert_eq!(s["button_title"], "");
        let row = node(&s, "items", "session:personal:human");
        assert_eq!(row["height"], 64);
        let title = nodes(&row["children"])
            .into_iter()
            .find(|n| n["text"] == format!("You (@{user})"))
            .unwrap();
        assert_eq!(title["hidden"], false);
    }
    frame["unread_count"] = json!(3);
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p)["button_title"], " 3");
    let mut second = listener();
    second["hub"] = json!("work");
    second["scoped_id"] = json!("work:copy");
    second["display_address"] = json!("work:alice/fixture#abcd");
    frame["listeners"] = json!([human, listener(), second]);
    f.send(p, frame.clone());
    let s = inspect(&f, p);
    assert_eq!(s["count"], 1);
    assert_eq!(s["button_title"], " 3");
    assert_eq!(
        node(&s, "items", "header_subtitle")["text"],
        "1 session listening"
    );
    let strip = node(&s, "items", "header_hub_strip");
    for hub in ["personal", "work"] {
        let pill = node(&s, "items", &format!("hub:{hub}"));
        assert!(pill["x"].as_f64().unwrap() >= 0.0);
        assert!(
            pill["x"].as_f64().unwrap() + pill["width"].as_f64().unwrap()
                <= strip["width"].as_f64().unwrap()
        );
    }
    frame["unread_count"] = json!(0);
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p)["button_title"], "");
    assert_eq!(
        node(&inspect(&f, p), "items", "activity_unread_badge")["text"],
        "History"
    );
    frame["unread_count"] = json!(3);
    frame["available"] = json!([{"harness":"fixture","session_id":"pending","title":"Pending bridge","cwd":"/tmp","needs_reload":true}]);
    f.send(p, frame);
    control(&f, p, "open_directory");
    assert_eq!(
        inspect(&f, p)["directory_mode"],
        "directory",
        "Unread signals cannot hijack Directory entry"
    );
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"online_count":2}),
    );
    for appearance in ["light", "dark"] {
        control(&f, p, "copy:personal:repo-abcdefghijkl");
        let s = inspect_as(&f, p, Some(appearance));
        assert_eq!(
            s["low_contrast_labels"],
            json!([]),
            "menu/directory {appearance}"
        );
        let copied = node(&s, "items", "copy:personal:repo-abcdefghijkl");
        let expected_red = if appearance == "light" { 0.04 } else { 0.28 };
        assert!((copied["foreground"][0].as_f64().unwrap() - expected_red).abs() < 0.001);
        let dir = node(&s, "items", "open_directory");
        let act = node(&s, "items", "open_activity");
        assert_eq!(
            (
                dir["x"].clone(),
                dir["width"].clone(),
                dir["height"].clone()
            ),
            (json!(12), json!(174), json!(28))
        );
        assert_eq!(
            (act["x"].clone(), act["width"].clone()),
            (json!(194), json!(174))
        );
        assert_eq!(dir["text"], "Directory…");
        assert_eq!(act["text"], "Activity…");
        assert_eq!(act["symbol"], "bell.badge.fill");
        render_png_as(
            &f,
            p,
            "menu",
            &format!("c3_menu_{appearance}.png"),
            Some(appearance),
        );
        render_png_as(
            &f,
            p,
            "directory",
            &format!("c3_directory_{appearance}.png"),
            Some(appearance),
        );
        control(&f, p, "open_settings");
        for tab in ["apps", "channels", "hubs"] {
            control(&f, p, &format!("settings_tab:{tab}"));
            let s = inspect_as(&f, p, Some(appearance));
            assert_eq!(
                s["low_contrast_labels"],
                json!([]),
                "settings/{tab} {appearance}"
            );
            render_png_as(
                &f,
                p,
                "settings",
                &format!("c3_settings_{tab}_{appearance}.png"),
                Some(appearance),
            );
        }
        f.send(
            p,
            json!({"op":"error","source_op":"hub_action","error":"Connection unavailable"}),
        );
        let s = inspect_as(&f, p, Some(appearance));
        assert!(!node(&s, "settings_items", "settings_error_banner").is_null());
        assert_eq!(
            s["low_contrast_labels"],
            json!([]),
            "settings error {appearance}"
        );
        control(&f, p, "dismiss_settings_error");
        control(&f, p, "close_settings");
        f.send(
            p,
            json!({"op":"error","source_op":"hub_action","error":"Connection unavailable"}),
        );
        let s = inspect_as(&f, p, Some(appearance));
        assert_eq!(s["settings_open"], false);
        assert!(node(&s, "settings_items", "settings_error_banner").is_null());
        assert!(!node(&s, "items", "menu_error_row").is_null());
        assert_eq!(
            s["low_contrast_labels"],
            json!([]),
            "menu error {appearance}"
        );
        control(&f, p, "dismiss_menu_error");
        assert!(node(&inspect(&f, p), "items", "menu_error_row").is_null());
    }
    control(&f, p, "open_activity");
    assert_eq!(inspect(&f, p)["directory_mode"], "activity");
    assert!(f
        .frames(p)
        .iter()
        .any(|v| v["op"] == "open_directory" && v["mode"] == "activity"));
    assert!(f
        .frames(p)
        .iter()
        .any(|v| v["op"] == "open_directory" && v["mode"] == "directory"));
}

#[test]
fn c4_wake_action_routes_agent_signals_to_activity_and_human_signals_to_reply() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = f.spawn(
        root().join("bin/cito-ui-darwin-arm64").to_str().unwrap(),
        &[],
        &[("CITO_BANNER_DISMISS_MS", "10000")],
    );
    let mut frame = state();
    frame["activity"] = json!([
        ("personal", "sig_other", false),
        ("personal", "sig_agent", false),
        ("personal", "sig_human", true),
        ("work", "sig_agent", false),
        ("work", "sig_human", true),
    ].into_iter().map(|(hub, id, for_me)| json!({
        "hub":hub,"signal_id":id,"listener_id":"c_other","from_user":"alice",
        "from_host_name":"desktop","from_harness":"fixture","from_title":"Sender",
        "from_scoped_id":format!("{hub}:sender"),"target_harness":if for_me { "human" } else { "fixture" },
        "target_title":"Receiver","target_scoped_id":format!("{hub}:other"),"message":"First line\nSecond line",
        "created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":for_me
    })).collect::<Vec<_>>());
    f.send(p, frame);
    control(&f, p, "open_activity");
    for (harness, signal, hub, selected_index, selected_id) in [
        ("fixture", "personal:sig_agent", "personal", 1, "sig_agent"),
        ("human", "personal:sig_human", "personal", 2, "sig_human"),
        (
            "other-agent",
            "personal:sig_missing",
            "personal",
            0,
            "sig_other",
        ),
        ("human", "personal:sig_missing", "personal", 0, "sig_other"),
        ("fixture", "sig_agent", "personal", 1, "sig_agent"),
        ("fixture", "sig_agent", "work", 3, "sig_agent"),
        ("fixture", "work:sig_agent", "work", 3, "sig_agent"),
        ("fixture", "work:sig_agent", "", 3, "sig_agent"),
        ("human", "sig_human", "work", 4, "sig_human"),
        ("human", "work:sig_human", "", 4, "sig_human"),
        ("fixture", "personal:sig_agent", "", 1, "sig_agent"),
    ] {
        f.send(
            p,
            json!({"op":"test_directory_query","query":"unmatched query"}),
        );
        control(&f, p, "act_filter:unread");
        let mut wake = banner();
        wake["payload"]["target_harness"] = json!(harness);
        wake["payload"]["signal_id"] = json!(signal);
        wake["payload"]["hub"] = json!(hub);
        f.send(p, wake);
        f.send(p, json!({"op":"test_click"}));
        let s = inspect(&f, p);
        assert_eq!(s["directory_mode"], "activity");
        assert_eq!(s["directory_query"], "");
        assert_eq!(node(&s, "directory_items", "directory_search")["text"], "");
        assert_eq!(s["activity_filter"], "all");
        assert_eq!(
            s["directory_selected_id"], selected_id,
            "{harness}: {hub}/{signal}"
        );
        assert_eq!(
            s["directory_selected_index"], selected_index,
            "{harness}: {hub}/{signal}"
        );
        if selected_index == 1 {
            assert_eq!(
                node(&s, "directory_items", "activity_row:sig_other")["height"],
                60
            );
            let height = node(&s, "directory_items", "activity_row:sig_agent")["height"]
                .as_f64()
                .unwrap();
            assert!(height > 60.0 && height < 108.0, "{height}");
        }
        if harness == "human" {
            assert_eq!(s["wake_action_text"], "Click to reply in Cito ↩");
            assert_eq!(s["composer_open"], true);
            assert_eq!(s["composer_target"], "personal:sender-abcdefgh2345");
            assert_eq!(s["composer_reply_signal_id"], signal);
            assert_eq!(
                node(&s, "directory_items", "directory_shortcuts")["text"],
                "↵ Send    ⎋ Cancel Reply"
            );
        } else {
            assert_eq!(s["wake_action_text"], "Click to view in Activity ↗");
            assert_eq!(s["composer_open"], false);
            assert_eq!(s["composer_target"], "");
            assert_eq!(
                node(&s, "directory_items", "directory_shortcuts")["text"],
                "⎋ Close"
            );
            f.send(p, json!({"op":"test_directory_key","key":"return"}));
            assert_eq!(inspect(&f, p)["composer_open"], false);
        }
    }
    for appearance in ["light", "dark"] {
        assert_eq!(
            inspect_as(&f, p, Some(appearance))["low_contrast_labels"],
            json!([])
        );
        render_png_as(
            &f,
            p,
            "directory",
            &format!("c4_activity_agent_{appearance}.png"),
            Some(appearance),
        );
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
    cfg["channels"][0]["fields"][0]["configured"] = json!(false);
    cfg["channels"][0]["fields"][0]["value"] = json!("");
    cfg["channels"][0]["setup_guide"] = json!([
        "1. Create a gateway credential with your provider.",
        "2. Save it once for everyone on this Hub."
    ]);
    cfg["hubs"][0]["name"] = json!("local");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    frame["listeners"] = json!([]);
    frame["available"] =
        json!([{"harness":"fixture","session_id":"s1","title":"Work session","cwd":"/tmp"}]);
    f.send(p, frame.clone());
    let s = inspect(&f, p);
    assert_eq!(node(&s, "items", "human_paused_card")["height"], 64);
    assert_eq!(node(&s, "items", "listen_human")["y"], 18);
    assert!(node(&s, "items", "listen_human")["symbol"].is_null());
    assert_eq!(node(&s, "items", "listen_human")["text"], "Resume");
    render_png(&f, p, "menu", "menu_human_paused.png");
    let rendered = inspect(&f, p);
    for item in nodes(&s["items"]) {
        if let Some(id) = item["id"].as_str().filter(|s| !s.is_empty()) {
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
        ("auth_required", "Sign in above to receive notifications"),
        ("connecting", "Connecting to receive notifications"),
        ("unreachable", "Hub unavailable — reconnect above"),
        ("error", "Hub unavailable — reconnect above"),
    ] {
        let mut unavailable = frame.clone();
        unavailable["hubs"][0]["status"] = json!(status);
        unavailable["hubs"][0]["human_paused"] = json!(false);
        f.send(p, unavailable);
        control(&f, p, "target_hub:personal");
        assert_eq!(
            node(&inspect(&f, p), "items", "human_listening_status")["text"],
            expected
        );
        render_png(&f, p, "menu", &format!("r2_human_{status}.png"));
    }
    let mut distinct = frame.clone();
    distinct["hubs"][0]["human_paused"] = json!(true);
    distinct["hubs"][0]["human_about"] = json!("Release approvals");
    f.send(p, distinct);
    let detail = inspect(&f, p);
    assert_eq!(node(&detail, "items", "human_paused_card")["height"], 78);
    assert_eq!(
        node(&detail, "items", "human_paused_card")["children"][0]["height"],
        72
    );
    assert_eq!(
        node(&detail, "items", "human_paused_about")["text"],
        "Release approvals"
    );
    assert_eq!(
        node(&detail, "items", "human_listening_status")["text"],
        "Notifications paused"
    );
    render_png(&f, p, "menu", "r2_human_paused_about.png");
    for raw_about in [
        Value::Null,
        json!(""),
        json!(" \n "),
        json!("Human"),
        json!("Human receiver"),
        json!("Human Node"),
        json!("Human channel"),
        json!("Session"),
        json!("Coding session"),
        json!("Default"),
        json!("Agent"),
        json!("Agent session"),
        json!("CLI session"),
        json!("alice"),
        json!("@alice"),
        json!(f.home.file_name().unwrap().to_str().unwrap()),
        json!(" hUmAn NoDe "),
        json!("You"),
        json!("You (@alice)"),
        json!(" yOu (@ALICE) "),
        json!(" Release approvals "),
    ] {
        let mut paused = frame.clone();
        paused["hubs"][0]["human_paused"] = json!(true);
        paused["hubs"][0]["human_about"] = raw_about.clone();
        f.send(p, paused.clone());
        let height = if raw_about
            .as_str()
            .is_some_and(|about| about.trim() == "Release approvals")
        {
            78
        } else {
            64
        };
        let measured = inspect(&f, p);
        assert_eq!(
            node(&measured, "items", "human_paused_card")["height"],
            height
        );
        assert_eq!(
            node(&measured, "items", "human_paused_card")["children"][0]["height"],
            height - 6
        );
        if raw_about == json!("You (@alice)") {
            render_png(&f, p, "menu", "r3_human_display_title_about.png");
        }
        control(&f, p, "listen_human");
        inspect(&f, p);
        let resume = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|action| action["op"] == "listen_session")
            .unwrap();
        let trimmed = raw_about.as_str().unwrap_or("").trim();
        assert_eq!(
            resume["about"],
            if trimmed.is_empty() { "Human" } else { trimmed }
        );
        let mut human = listener();
        human["harness"] = json!("human");
        human["session_id"] = json!("default");
        human["title"] = json!("Human");
        human["about"] = resume["about"].clone();
        human["display_address"] = json!("personal:@alice");
        let row_id = format!("session:{}", human["scoped_id"].as_str().unwrap());
        paused["listeners"] = json!([human]);
        f.send(p, paused.clone());
        let active = inspect(&f, p);
        assert_eq!(node(&active, "items", &row_id)["height"], height);
        assert_eq!(
            node(&active, "items", &row_id)["children"][0]["height"],
            height - 6
        );
        if raw_about
            .as_str()
            .is_some_and(|about| about.trim().to_ascii_lowercase().starts_with("you"))
        {
            paused["listeners"][0]["title"] = json!("Review desk");
            paused["listeners"][0]["custom_title"] = json!(true);
            f.send(p, paused);
            assert_eq!(
                node(&inspect(&f, p), "items", &row_id)["height"],
                64,
                "Generic Human aliases remain generic under a custom display title"
            );
        }
    }
    for (title, about, height) in [
        ("café", "CAFÉ", 64),
        ("CAFÉ", "café", 64),
        ("café", "CAFÉ notes", 78),
    ] {
        let mut human = listener();
        human["harness"] = json!("human");
        human["session_id"] = json!("default");
        human["custom_title"] = json!(true);
        human["title"] = json!(title);
        human["about"] = json!(about);
        human["display_address"] = json!("personal:@alice");
        let row_id = format!("session:{}", human["scoped_id"].as_str().unwrap());
        let mut active = frame.clone();
        active["listeners"] = json!([human]);
        f.send(p, active);
        assert_eq!(
            node(&inspect(&f, p), "items", &row_id)["height"],
            height,
            "Unicode title equality must agree with Rust; distinct Unicode details remain visible"
        );
    }
    f.send(p, frame.clone());
    let paused_card = node(&s, "items", "human_paused_card")["children"][0].clone();
    let paused_badge = &paused_card["children"][0];
    assert_eq!(paused_badge["x"], 12);
    assert_eq!(paused_badge["y"], 15);
    assert_eq!(paused_badge["width"], 28);
    assert_eq!(paused_badge["height"], 28);
    assert_eq!(paused_badge["radius"], 7);
    assert_eq!(paused_card["children"][1]["x"], 48);
    for hub_count in [1, 2] {
        let mut active = frame.clone();
        active["hubs"].as_array_mut().unwrap().truncate(hub_count);
        let mut human = listener();
        human["harness"] = json!("human");
        human["session_id"] = json!("default");
        human["title"] = json!("Human");
        human["display_address"] = json!("personal:@alice");
        active["listeners"] = json!([human]);
        f.send(p, active);
        let snapshot = inspect(&f, p);
        let card =
            node(&snapshot, "items", "session:personal:repo-abcdefghijkl")["children"][0].clone();
        for coordinate in ["x", "y", "width", "height", "radius"] {
            assert_eq!(card["children"][0][coordinate], paused_badge[coordinate]);
        }
        assert_eq!(card["children"][1]["x"], 48);
        assert_eq!(card["children"][1]["y"], paused_card["children"][1]["y"]);
    }
    f.send(p, frame.clone());
    control(&f, p, "listen_human");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "listen_session"
                && v["hub"] == "personal"
                && v["harness"] == "human"
                && v["session_id"] == "default"
        })
    });
    control(&f, p, "target_hub:work");
    control(&f, p, "listen_human");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "listen_session" && v["harness"] == "human" && v["hub"] == "work")
    });
    control(&f, p, "open_settings");
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "onboarding_banner").is_null());
    assert!(node(&s, "settings_items", "apps_next_step_callout").is_null());
    assert_eq!(
        node(&s, "settings_items", "settings_app_tier:cursor")["width"],
        88
    );
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
        "○ Not Set Up"
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
        "1. Shared Bot Gateway"
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
    cfg["channels"][0]["fields"][0]["configured"] = json!(true);
    cfg["channels"][0]["pairing_instruction"] =
        json!("Send /start cito-abc123def4 to your account provider.");
    cfg["channels"][0]["pairing_command"] = json!("/start cito-abc123def4");
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "stratum_hub_status:channel")["text"],
        "● Bot Ready"
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
        36
    );
    assert!(node(&paired, "settings_items", "channel_field:channel:account").is_null());
    assert_eq!(
        node(&paired, "settings_items", "channel_summary:channel")["text"],
        "● Paired (@alice · account-42)"
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
        "Bot Gateway"
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
        assert_eq!(toggled["text"], if enabled { "Pause" } else { "Resume" });
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
    // Non-admin schemas keep ownership explanation but never reveal gateway fields.
    cfg["channels"][0]["hub_provided"] = json!(true);
    cfg["channels"][0]["fields"][0]["hidden"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "channel_field:channel:secret").is_null());
    assert!(node(&s, "settings_items", "clear_channel_service:channel").is_null());
    assert_eq!(
        node(&s, "settings_items", "stratum_hub:channel")["height"],
        36
    );
    render_png(&f, p, "settings", "r1_gateway_non_admin.png");
    assert!(
        node(&s, "settings_items", "stratum_hub_title:channel")["tooltip"]
            .as_str()
            .is_some_and(|s| s.contains("administrator"))
    );
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
        cfg["host_hub"]["running"] = json!(running);
        cfg["host_hub"]["error"] = json!(error);
        f.send(p, json!({"op":"settings","settings":cfg}));
        let status = inspect(&f, p);
        let toggle = node(&status, "settings_items", "toggle_host_hub");
        assert_eq!(
            toggle["text"],
            if error.is_some() {
                "● Failed"
            } else if running {
                "● Sharing"
            } else {
                "● Starting"
            }
        );
        assert_eq!(toggle["width"], sharing_toggle["width"]);
        assert_eq!(toggle["x"], sharing_toggle["x"]);
        let prominent = node(&status, "settings_items", "settings_tab:hubs")["background"].clone();
        let invite = node(&status, "settings_items", "copy_hub_invite");
        if running && error.is_none() {
            assert_eq!(toggle["background"], prominent);
            assert_eq!(invite["background"], prominent);
        } else {
            assert_ne!(invite["background"], prominent);
            if error.is_none() {
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
        if !running || error.is_some() {
            assert_eq!(
                warning["text"],
                error
                    .filter(|error| !error.is_empty())
                    .unwrap_or(if running {
                        "Hub sharing is unavailable"
                    } else {
                        "Hub server is not running"
                    })
            );
            assert!(
                node(&status, "settings_items", "copy_hub_invite")["tooltip"]
                    .as_str()
                    .unwrap()
                    .contains("unavailable")
            );
        } else {
            assert!(warning.is_null());
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
    assert_eq!(toggle["text"], "Share");
    for dimension in ["x", "width"] {
        assert_eq!(toggle[dimension], sharing_toggle[dimension]);
    }
    assert_eq!(
        node(&off, "settings_items", "host_hub_error")["text"],
        "Sharing is off — enable Share to accept connections"
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
        "Share"
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
    assert!(node(&s, "settings_items", "copy_host_hub_token").is_null());
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
        toggled["desktop_banner_enabled"] = json!(enabled);
        toggled["host_hub"]["auth_mode"] = json!(if enabled { "token" } else { "none" });
        f.send(p, json!({"op":"settings","settings":toggled}));
        let snapshot = inspect(&f, p);
        let frames: Vec<_> = [
            "toggle_host_hub",
            "toggle_host_hub_auth",
            "toggle_hub:local",
            "edit_hub:local",
            "toggle_desktop_banner",
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
        // Editing a new intent ends prior acknowledgement feedback, even when
        // duplicate daemon refreshes arrive during the feedback timer.
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":"4291"}),
        );
        f.send(p, json!({"op":"settings","settings":cfg}));
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["text"],
            "Apply"
        );
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":"4291"}),
        );
        let before = f.frames(p).len();
        control(&f, p, "apply_host_hub_port");
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["text"],
            "Apply",
            "Do not report success before the daemon acknowledges the save"
        );
        wait(|| {
            f.frames(p)[before..]
                .iter()
                .any(|v| v["op"] == "set_host_hub" && v["port"] == 4291)
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
            "Apply",
            "An uncorrelated {op} snapshot cannot acknowledge the save"
        );
        f.send(
            p,
            json!({"op":"settings","request_id":submitted["request_id"],"settings":cfg}),
        );
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["text"],
            "✓ Saved",
            "An unchanged correlated acknowledgement must settle the save"
        );
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
        assert_eq!(action["port"], cfg["host_hub"]["port"]);
        assert!(
            action["request_id"].is_null(),
            "Toggles cannot own the port save"
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
        "Apply"
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
        json!({"op":"test_settings_input","id":"host_hub_port","value":"5301"}),
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
        "Apply"
    );
    cfg["host_hub"]["error"] = Value::Null;
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
        assert_eq!(track["radius"], 7);
        assert_eq!(track["border_width"], 0);
        assert!((track["background"][3].as_f64().unwrap() - 0.06).abs() < 0.001);
        for candidate in ["none", "token", "oidc"] {
            let pill = node(
                &s,
                "settings_items",
                &format!("settings_hub_auth:{candidate}"),
            );
            assert_eq!(pill["border_width"], if candidate == mode { 1 } else { 0 });
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
fn dynamic_identity_and_error_labels_keep_full_tooltips_without_layout_overflow() {
    let _lock = serial();
    for length in [40, 64] {
        let f = Fixture::new(json!({}));
        let p = start(&f);
        let handle = "w".repeat(length);
        let hub = "h".repeat(40);
        let display_name = "W".repeat(length);
        let clean_error = "Long diagnostic with recovery context. "
            .repeat(10)
            .trim_end()
            .to_owned();
        let error = format!("INVALID_INPUT: {clean_error} (os error 13)");
        let mut cfg = settings();
        cfg["settings_hub"] = json!(hub);
        cfg["hubs"][0]["name"] = json!(hub);
        cfg["hubs"][0]["user"] = json!(handle);
        cfg["channels"][0]["display_name"] = json!(display_name);
        cfg["channels"][0]["display_label"] = json!(handle);
        cfg["channels"][0]["configured"] = json!(true);
        cfg["channels"][0]["status"] = json!("ready");
        for app in cfg["apps"].as_array_mut().unwrap() {
            app["display_name"] = json!(display_name);
            app["detail"] = json!(error);
            app["paths"] = json!([]);
        }
        let mut frame = state();
        frame["hubs"][0]["name"] = json!(hub);
        frame["hubs"][0]["user"] = json!(handle);
        frame["listeners"] = json!([]);
        frame["settings"] = cfg;
        frame["harnesses"][0]["display_name"] = json!(display_name);
        frame["available"] =
            json!([{"harness":"fixture","session_id":"s2","title":"Session","cwd":"/tmp"}]);
        f.send(p, frame.clone());
        // Extreme input intentionally truncates. The strict V29 oracle must detect it
        // while every geometry, contrast, jargon, and address invariant stays intact.
        let stress_inspect = || {
            let snapshot = inspect_unchecked(&f, p, None);
            for key in [
                "overlapping_siblings",
                "clipped_views",
                "low_contrast_labels",
                "jargon_leaks",
                "address_mismatches",
            ] {
                assert_eq!(snapshot[key], json!([]), "{length}-character stress: {key}");
            }
            assert!(!snapshot["truncated_labels"].as_array().unwrap().is_empty());
            snapshot
        };
        let tooltip = |snapshot: &Value, surface: &str, text: &str| {
            assert!(
                nodes(&snapshot[surface])
                    .iter()
                    .any(|n| n["text"] == text && n["tooltip"] == text),
                "Missing full text/tooltip on {surface}: {text}"
            );
        };
        let s = stress_inspect();
        tooltip(&s, "items", &format!("You (@{handle})"));
        let mut active = listener();
        active["hub"] = json!(hub);
        frame["listeners"] = json!([active]);
        f.send(p, frame.clone());
        f.send(p, json!({"op":"error","error":error}));
        let s = stress_inspect();
        tooltip(&s, "items", &clean_error);
        assert!(node(&s, "items", "switch_hub:personal:repo-abcdefghijkl").is_null());
        let pill = node(&s, "items", &format!("hub:{hub}"));
        assert!(pill["tooltip"].as_str().unwrap().contains(&hub));
        tooltip(&s, "items", &hub);
        control(&f, p, "open_settings");
        let s = stress_inspect();
        tooltip(&s, "settings_items", &display_name);
        tooltip(&s, "settings_items", &clean_error);
        control(&f, p, "settings_tab:channels");
        let s = stress_inspect();
        tooltip(&s, "settings_items", &display_name);
        tooltip(&s, "settings_items", &format!("● Connected ({handle})"));
        let summary = node(&s, "settings_items", "channel_summary:channel");
        assert!(summary["tooltip"]
            .as_str()
            .unwrap()
            .contains(&format!("@{handle}")));
        control(&f, p, "edit_user_channel:channel");
        let expanded = stress_inspect();
        let title = node(&expanded, "settings_items", "stratum_user_title:channel");
        assert_eq!(
            title["text"],
            format!("2. Your Personal Account (@{handle})")
        );
        assert_eq!(title["tooltip"], title["text"]);
        f.send(p, json!({"op":"error","error":error}));
        let s = stress_inspect();
        tooltip(&s, "settings_items", &clean_error);
        control(&f, p, "open_directory");
        // Echo the current real request without invoking the normal no-truncation
        // fixture inspector: this test deliberately keeps overflowing chrome visible.
        stress_inspect();
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| matches!(a["op"].as_str(), Some("query_directory" | "open_directory")))
            .unwrap();
        let mut remote = peer("one");
        remote["display_address"] = json!(format!(
            "personal:{}",
            remote["display_address"].as_str().unwrap()
        ));
        f.send(p,json!({"op":"directory","request_id":request["request_id"],"query":request["query"],"scope":request["scope"],"harness":request["harness"],"peers":[remote]}));
        control(&f, p, "msg_dir:personal:one");
        f.send(
            p,
            json!({"op":"test_composer_input","value":"Keep this draft"}),
        );
        control(&f, p, "composer_send");
        stress_inspect();
        let submitted = f
            .frames(p)
            .into_iter()
            .find(|a| a["op"] == "send_signal")
            .unwrap();
        f.send(p,json!({"op":"composer_result","ok":false,"request_id":submitted["request_id"],"error":error}));
        let s = stress_inspect();
        assert_eq!(
            node(&s, "directory_items", "composer_input")["text"],
            "Keep this draft"
        );
        tooltip(&s, "directory_items", &clean_error);
    }
}

#[test]
fn u3_paused_human_card_requires_no_active_person_and_excludes_offline_targets() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    let mut human = listener();
    human["harness"] = json!("human");
    human["session_id"] = json!("default");
    human["display_address"] = json!("@alice");
    frame["listeners"] = json!([human]);
    frame["available"] =
        json!([{"harness":"fixture","session_id":"s2","title":"Session","cwd":"/tmp"}]);
    f.send(p, frame.clone());
    assert!(node(&inspect(&f, p), "items", "human_paused_card").is_null());
    control(&f, p, "target_hub:work");
    let coalesced = inspect(&f, p);
    assert!(!node(&coalesced, "items", "human_paused_card").is_null());
    assert_eq!(coalesced["count"], 0);
    assert_eq!(
        node(&coalesced, "items", "header_subtitle")["text"],
        "Notifications paused"
    );
    assert!(node(&coalesced, "items", "session:personal:repo-abcdefghijkl").is_null());
    let mut empty = frame.clone();
    empty["available"] = json!([]);
    f.send(p, empty.clone());
    let snapshot = inspect(&f, p);
    assert!(node(&snapshot, "items", "available_empty_target").is_null());
    empty["hubs"][1]["auto_listen"] = json!(true);
    f.send(p, empty);
    let snapshot = inspect(&f, p);
    assert!(node(&snapshot, "items", "available_empty_target").is_null());
    assert_eq!(
        node(&snapshot, "items", "toggle_auto_listen")["text"],
        "Auto-Listen: On"
    );
    frame["hubs"][1]["status"] = json!("disconnected");
    f.send(p, frame.clone());
    assert!(node(&inspect(&f, p), "items", "human_paused_card").is_null());
    control(&f, p, "hub:work");
    assert_eq!(
        node(&inspect(&f, p), "items", "drawer:work")["hidden"],
        true
    );
    control(&f, p, "hub:work");
    assert_eq!(
        node(&inspect(&f, p), "items", "drawer:work")["hidden"],
        false
    );
    frame["listeners"] = json!([]);
    f.send(p, frame.clone());
    assert!(node(&inspect(&f, p), "items", "human_paused_card").is_null());
    control(&f, p, "hub:personal");
    assert!(!node(&inspect(&f, p), "items", "human_paused_card").is_null());
    frame["hubs"][0]["status"] = json!("disconnected");
    f.send(p, frame);
    assert!(node(&inspect(&f, p), "items", "human_paused_card").is_null());
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
        if ["save_channel", "pair_channel"].contains(&action) {
            control(&f, p, "edit_user_channel:channel");
        }
        control(&f, p, &format!("{action}:channel"));
        let s = inspect(&f, p);
        assert!(
            node(&s, "settings_items", "settings_channel_detail:channel").is_null(),
            "{action} must clear the previous result"
        );
        assert_eq!(
            node(&s, "settings_items", "settings_channel_status:channel")["text"],
            if configured {
                "● Connected"
            } else {
                "● Bot Gateway Ready"
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
    for action in ["save_hub_service", "clear_channel_service"] {
        for ok in [false, true] {
            cfg["channels"][0]["configured"] = json!(false);
            cfg["channels"][0]["status"] = json!("not_configured");
            cfg["channels"][0]["fields"][0]["configured"] = json!(false);
            cfg["channels"][0]["fields"][0]["value"] = json!("");
            f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
            control(&f, p, "test_channel:channel");
            let detail = if ok { "Test sent" } else { "Invalid token" };
            f.send(p, json!({"op":"channel_test_result","hub":"personal","channel_id":"channel","ok":ok,"detail":detail}));
            f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
            let mut frame = state();
            frame["settings"] = cfg.clone();
            frame["settings"]["desktop_banner_enabled"] = json!(false);
            f.send(p, frame);
            let s = inspect(&f, p);
            assert_eq!(
                node(&s, "settings_items", "settings_channel_detail:channel")["text"],
                detail,
                "{action}, ok={ok}"
            );
            assert_eq!(
                node(&s, "settings_items", "settings_channel_status:channel")["text"],
                if ok { "○ Not Set Up" } else { "⚠ Error" }
            );
            if action == "clear_channel_service" {
                // Clear Token becomes available once the hub service is configured.
                cfg["channels"][0]["status"] = json!("hub_ready");
                cfg["channels"][0]["fields"][0]["configured"] = json!(true);
                f.send(p, json!({"op":"settings","hub":"personal","settings":cfg}));
                assert_eq!(
                    node(
                        &inspect(&f, p),
                        "settings_items",
                        "settings_channel_detail:channel"
                    )["text"],
                    detail
                );
            }
            control(&f, p, &format!("{action}:channel"));
            let s = inspect(&f, p);
            assert!(
                node(&s, "settings_items", "settings_channel_detail:channel").is_null(),
                "{action} must clear the candidate test result, ok={ok}"
            );
            assert_eq!(
                node(&s, "settings_items", "settings_channel_status:channel")["text"],
                if action == "clear_channel_service" {
                    "● Bot Gateway Ready"
                } else {
                    "○ Not Set Up"
                }
            );
        }
    }
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
        node(&s, "settings_items", "stratum_hub:channel")["height"],
        36
    );
    assert!(node(&s, "settings_items", "channel_field:channel:secret").is_null());
    control(&f, p, "edit_hub_service:channel");
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:secret")["secure"],
        true
    );
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:secret")["placeholder"],
        "Saved — enter replacement"
    );
    assert!(node(&s, "settings_items", "channel_setup_guide:channel").is_null());
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
    cfg["channels"][0]["pairing_command"] = json!("/start cito-ABCDEFGHIJ");
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
fn channel_cancel_unpair_and_clear_discard_only_their_scope_drafts() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("saved-account");
    cfg["channels"][0]["fields"][0]["value"] = json!("");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "edit_hub_service:channel");
    for (field, value) in [("secret", "discard-gateway"), ("account", "keep-personal")] {
        f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{field}"),"value":value}));
    }
    control(&f, p, "cancel_edit_hub_service:channel");
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    let latest = |op: &str| {
        f.frames(p)
            .into_iter()
            .rev()
            .find(|v| v["op"] == op)
            .unwrap()
    };
    assert_eq!(latest("save_hub_service")["values"], json!({}));
    f.send(p, json!({"op":"test_settings_input","id":"channel_field:channel:secret","value":"escape-discard"}));
    let mut remote = cfg.clone();
    remote["settings_hub"] = json!("work");
    f.send(p, json!({"op":"settings","hub":"work","settings":remote}));
    control(&f, p, "channel_hub_selector:work");
    control(&f, p, "edit_hub_service:channel");
    for (field, value) in [("secret", "escape-work"), ("account", "keep-work")] {
        f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{field}"),"value":value}));
    }
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    for hub in ["work", "personal"] {
        control(&f, p, &format!("channel_hub_selector:{hub}"));
        control(&f, p, "edit_hub_service:channel");
        control(&f, p, "save_hub_service:channel");
        inspect(&f, p);
        assert_eq!(
            latest("save_hub_service")["values"],
            json!({}),
            "Escape discards gateway drafts on {hub}"
        );
        control(&f, p, "save_channel:channel");
        inspect(&f, p);
        assert_eq!(
            latest("channel_save")["values"],
            json!({"account":if hub == "work" { "keep-work" } else { "keep-personal" }})
        );
    }
    control(&f, p, "save_channel:channel");
    inspect(&f, p);
    assert_eq!(
        latest("channel_save")["values"],
        json!({"account":"keep-personal"})
    );
    control(&f, p, "unpair_channel:channel");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        node(
            &inspect(&f, p),
            "settings_items",
            "channel_field:channel:account"
        )["text"],
        cfg["channels"][0]["fields"][1]["value"]
    );
    for (field, value) in [("secret", "discard-token"), ("account", "discard-account")] {
        f.send(p, json!({"op":"test_settings_input","id":format!("channel_field:channel:{field}"),"value":value}));
    }
    control(&f, p, "clear_channel_service:channel");
    cfg["channels"][0]["configured"] = json!(false);
    cfg["channels"][0]["status"] = json!("not_configured");
    for field in cfg["channels"][0]["fields"].as_array_mut().unwrap() {
        field["configured"] = json!(false);
        field["value"] = json!("");
    }
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "channel_field:channel:account"),
        Value::Null
    );
    control(&f, p, "save_hub_service:channel");
    inspect(&f, p);
    assert_eq!(
        latest("save_hub_service")["values"],
        json!({"secret":""}),
        "An unconfigured gateway submits the empty field, never the discarded secret draft"
    );
    cfg["channels"][0]["fields"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("hub_ready");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "stratum_hub:channel")["height"],
        36,
        "Clearing a gateway also leaves its old edit mode"
    );
    f.send(p, json!({"op":"pair_code","hub":"personal","channel_id":"channel","instruction":"Send the link command below","pairing_command":"/link 123456"}));
    control(&f, p, "copy_pair_command:channel");
    assert_eq!(inspect(&f, p)["clipboard"], "/link 123456");
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
            .any(|v| v["op"] == "set_host_hub" && v["port"] == 4291 && v["auth_mode"] == "token")
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
        ("", "https://local.example.com", "local-2"),
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
        let saved = json!({"name":expected,"url":canonical_url,"auth_mode":action["auth_mode"],"enabled":true,"is_default":false,"status":"connected","user":"alice","error":null});
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
        "Disconnect All"
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
fn appkit_activity_reply_filters_navigation_and_read_all() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    let activity = json!([{"hub":"personal","signal_id":"personal:sig_unread","listener_id":"c_human","from_user":"bob","from_host_name":"laptop","from_harness":"fixture","from_title":"Schema work","from_scoped_id":"personal:one","target_harness":"human","target_title":"Me","target_scoped_id":"personal:me","message":"Approve?","held_reason":null,"in_reply_to":null,"created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true},{"hub":"personal","signal_id":"personal:sig_read","listener_id":"c_other","from_user":"alice","from_host_name":"desktop","from_harness":"fixture","from_title":"Other","from_scoped_id":"personal:two","target_harness":"fixture","target_title":"Other","target_scoped_id":"personal:other","message":"Done","held_reason":null,"in_reply_to":null,"created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":false}]);
    frame["activity"] = activity.clone();
    frame["unread_count"] = json!(1);
    f.send(p, frame);
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"matched_count":1,"online_count":2,"activity":activity,"unread_count":1}),
    );
    control(&f, p, "open_activity");
    let s = inspect(&f, p);
    assert_eq!(s["directory_mode"], "activity");
    assert!(
        (node(&s, "directory_items", "activity_filter_track")["background"][3]
            .as_f64()
            .unwrap()
            - 0.06)
            .abs()
            < 0.001
    );
    let collapsed = node(&s, "directory_items", "act_msg:personal:sig_unread");
    assert_eq!(
        collapsed["y"].as_f64().unwrap() + collapsed["height"].as_f64().unwrap(),
        26.0
    );
    assert_eq!(
        node(&s, "directory_items", "act_filter:unread")["border_width"],
        0
    );
    assert_eq!(
        node(&s, "directory_items", "act_filter:me")["border_width"],
        1
    );
    assert_eq!(
        node(&s, "directory_items", "activity_mark_all_read")["border_width"],
        1
    );
    assert_eq!(
        node(&s, "directory_items", "activity_clear")["border_width"],
        1
    );
    assert_eq!(
        node(&s, "directory_items", "directory_shortcuts")["text"],
        "↵ Reply    ⎋ Close"
    );
    let row = node(&s, "directory_items", "activity_row:personal:sig_unread");
    assert_eq!(row["height"], 60);
    let children = nodes(&row["children"]);
    let accent = children
        .iter()
        .find(|n| n["id"] == "unread_accent_bar")
        .unwrap();
    assert_eq!(accent["hidden"], false);
    assert_eq!(accent["width"], 3);
    assert_eq!(accent["x"], 4);
    assert_eq!(accent["height"], 44);
    assert!(!children.iter().any(|n| n["id"] == "online_presence_dot"));
    assert!(
        node(&s, "directory_items", "act_title:personal:sig_unread")["text"]
            .as_str()
            .unwrap()
            .ends_with(" · Just now")
    );
    assert_eq!(
        node(&s, "directory_items", "act_msg:personal:sig_unread")["width"],
        492
    );
    let delete = node(&s, "directory_items", "delete_activity:personal:sig_unread");
    assert_eq!(delete["symbol"], "xmark");
    assert_eq!(delete["width"], 24);
    assert_eq!(delete["tooltip"], "Delete message");
    control(&f, p, "delete_activity:personal:sig_unread");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "clear_activity_history"
                && v["hub"] == "personal"
                && v["signal_id"] == "personal:sig_unread"
        })
    });
    let deletions = f
        .frames(p)
        .iter()
        .filter(|v| v["op"] == "clear_activity_history")
        .count();
    f.send(p, json!({"op":"test_focus_field","id":"directory_search"}));
    f.send(p, json!({"op":"test_directory_key","key":"delete"}));
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "clear_activity_history")
            .count(),
        deletions,
        "Delete must keep normal text editing semantics"
    );
    control(&f, p, "act_filter:all");
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    let s = inspect(&f, p);
    assert_eq!(s["directory_selected_id"], "personal:sig_read");
    let mut inserted = activity.clone();
    let mut newer = activity[0].clone();
    newer["signal_id"] = json!("newer");
    inserted.as_array_mut().unwrap().insert(0, newer);
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"activity":inserted,"activity_filter":"all"}),
    );
    let refreshed = inspect(&f, p);
    assert_eq!(refreshed["directory_selected_id"], "personal:sig_read");
    assert_eq!(refreshed["directory_selected_index"], 2);
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"activity":activity,"activity_filter":"all"}),
    );
    assert_eq!(inspect(&f, p)["directory_selected_index"], 1);
    f.send(p, json!({"op":"test_focus_field","id":"directory_table"}));
    f.send(p, json!({"op":"test_directory_key","key":"delete"}));
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "clear_activity_history"
                && v["hub"] == "personal"
                && v["signal_id"] == "personal:sig_read"
        })
    });
    assert!(node(&s, "directory_items", "reply_activity:personal:sig_read")["hidden"] == true);
    control(&f, p, "reply_activity:personal:sig_read");
    f.send(p, json!({"op":"test_directory_key","key":"return"}));
    assert_eq!(inspect(&f, p)["composer_open"], false);
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
    let queries = f
        .frames(p)
        .iter()
        .filter(|v| v["op"] == "query_directory")
        .count();
    control(&f, p, "activity_mark_all_read");
    let read = inspect(&f, p);
    assert_eq!(read["unread_count"], 0);
    assert!(nodes(&read["directory_items"]).iter().any(|n| n["text"]
        .as_str()
        .is_some_and(|s| s.ends_with(" · 0 unread"))));
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "query_directory")
            .count(),
        queries + 1
    );
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "ack_signals" && v["ack_all"] == true)
    });
    control(&f, p, "act_filter:all");
    for (reason, label) in [
        ("stale_generation", "session reloaded"),
        ("driver_unavailable", "app unavailable"),
        ("session_switched", "session changed"),
        ("unlistened", "listening paused"),
        ("unlistened_before_dispatch", "listening paused"),
        ("fenced_before_dispatch", "connection changed"),
    ] {
        let mut item = activity[0].clone();
        item["held_reason"] = json!(reason);
        send_fixture(
            &f,
            p,
            json!({"op":"directory","peers":[],"activity":[item],"activity_filter":"all"}),
        );
        let snapshot = inspect(&f, p);
        assert!(node(
            &snapshot,
            "directory_items",
            "act_title:personal:sig_unread"
        )["text"]
            .as_str()
            .unwrap()
            .contains(&format!("Held ({label})")));
    }
    let many: Vec<_> = (0..10)
        .map(|i| {
            let mut item = activity[0].clone();
            item["signal_id"] = json!(format!("row-{i}"));
            item
        })
        .collect();
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":many,"activity_filter":"all"}),
    );
    for _ in 0..9 {
        f.send(p, json!({"op":"test_directory_key","key":"down"}));
    }
    assert_eq!(inspect(&f, p)["directory_selection_visible"], true);
    control(&f, p, "reply_activity:row-9");
    let reply = inspect(&f, p);
    assert_eq!(reply["composer_open"], true);
    assert_eq!(reply["directory_selected_id"], "row-9");
    assert_eq!(reply["directory_selection_visible"], true);
}
#[test]
fn r1_passive_refresh_preserves_viewports_errors_and_pending_composer() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["listeners"] = json!((0..12)
        .map(|i| {
            let mut row = listener();
            row["listener_id"] = json!(format!("row-{i}"));
            row["public_id"] = row["listener_id"].clone();
            row["scoped_id"] = json!(format!("personal:row-{i}"));
            row["session_id"] = row["listener_id"].clone();
            row["display_address"] = json!(format!("alice/fixture#{i:04x}"));
            row
        })
        .collect::<Vec<_>>());
    frame["my_remote_listeners"] = json!((0..12)
        .map(|i| peer(&format!("remote-{i}")))
        .collect::<Vec<_>>());
    frame["available"] = json!((0..6).map(|i|json!({"harness":"fixture","session_id":format!("candidate-{i}"),"title":"Review","cwd":"/tmp","endpoint_pid":4242})).collect::<Vec<_>>());
    f.send(p, frame.clone());
    let scrolls = ["active_scroll", "my_remote_scroll", "available_scroll"];
    for id in scrolls {
        f.send(p, json!({"op":"test_scroll","id":id,"offset":20}));
    }
    let scrolled = inspect(&f, p);
    for id in scrolls {
        assert_eq!(node(&scrolled, "items", id)["content_y"], 20, "{id}");
    }
    frame["unread_count"] = json!(1);
    f.send(p, frame.clone());
    let refreshed = inspect(&f, p);
    for id in scrolls {
        assert_eq!(
            node(&refreshed, "items", id)["content_y"],
            20,
            "{id} must survive a rebuild"
        );
    }
    for (field, scroll_id, delta) in [
        (
            "listeners",
            "active_scroll",
            node(&refreshed, "items", "session:personal:row-0")["height"]
                .as_f64()
                .unwrap(),
        ),
        ("my_remote_listeners", "my_remote_scroll", 34.0),
    ] {
        let mut row = frame[field][0].clone();
        row["scoped_id"] = json!("personal:inserted");
        row["public_id"] = json!("inserted");
        row["session_id"] = json!("inserted");
        frame[field].as_array_mut().unwrap().insert(0, row);
        f.send(p, frame.clone());
        assert_eq!(
            node(&inspect(&f, p), "items", scroll_id)["content_y"]
                .as_f64()
                .unwrap(),
            20.0 + delta,
            "{scroll_id} preserves its viewed row after insertion"
        );
        frame[field].as_array_mut().unwrap().remove(0);
        f.send(p, frame.clone());
        assert_eq!(node(&inspect(&f, p), "items", scroll_id)["content_y"], 20);
    }
    // A new expanded group changes identifiers, never the existing group's key.
    let mut expanded = frame.clone();
    for i in 0..6 {
        expanded["available"].as_array_mut().unwrap().push(json!({"harness":"second","session_id":format!("other-{i}"),"title":"Review","cwd":"/tmp","endpoint_pid":4242}));
    }
    f.send(p, expanded);
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll:fixture")["content_y"],
        20
    );
    control(&f, p, "toggle_group:second");
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll")["content_y"],
        20
    );
    control(&f, p, "toggle_group:second");
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll:fixture")["content_y"],
        20
    );
    let mut inserted = frame["available"][0].clone();
    inserted["session_id"] = json!("inserted");
    frame["available"]
        .as_array_mut()
        .unwrap()
        .insert(0, inserted);
    f.send(p, frame.clone());
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll")["content_y"],
        64,
        "Keep candidate-0 at the same screen position after an insertion"
    );
    frame["available"][0]["needs_reload"] = json!(true);
    f.send(p, frame.clone());
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll")["content_y"],
        78,
        "Keep the anchor after a row above it grows"
    );
    frame["available"].as_array_mut().unwrap().remove(0);
    f.send(p, frame.clone());
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll")["content_y"],
        20
    );
    control(&f, p, "open_directory");
    f.send(p,json!({"op":"error","source_op":"listen_session","error":"BRIDGE_UNAVAILABLE: Retry listening"}));
    f.send(p, json!({"op":"test_directory_query","query":""}));
    f.send(p, json!({"op":"test_menu_open"}));
    assert!(!node(&inspect(&f, p), "items", "menu_error_row").is_null());
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"warnings":["work: Hub temporarily unavailable"]}),
    );
    let warned = inspect(&f, p);
    assert_eq!(
        node(&warned, "directory_items", "directory_warning")["hidden"],
        false
    );
    assert!(
        node(&warned, "directory_items", "directory_warning")["tooltip"]
            .as_str()
            .unwrap()
            .contains("work")
    );
    assert_eq!(
        node(&warned, "directory_items", "directory_scroll")["height"],
        306
    );
    f.send(p,json!({"op":"error","source_op":"query_directory","error":"CONNECTION_FAILED: Remote Hub unavailable (os error 61)"}));
    let failed = inspect(&f, p);
    assert!(
        node(&failed, "directory_items", "directory_warning")["text"]
            .as_str()
            .unwrap()
            .starts_with("Cannot refresh")
    );
    assert!(
        !node(&failed, "directory_items", "dir_peer:personal:one").is_null(),
        "A failed query must preserve usable rows"
    );
    assert!(!node(&failed, "items", "menu_error_row").is_null());
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"warnings":[]}),
    );
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        true
    );
    control(&f, p, "open_settings");
    f.send(
        p,
        json!({"op":"error","source_op":"hub_action","error":"Hub temporarily unavailable"}),
    );
    f.send(p, json!({"op":"test_directory_query","query":""}));
    f.send(p, json!({"op":"test_menu_open"}));
    assert!(!node(&inspect(&f, p), "settings_items", "settings_error_banner").is_null());
    control(&f, p, "dismiss_settings_error");
    assert!(node(&inspect(&f, p), "settings_items", "settings_error_banner").is_null());
    control(&f, p, "close_settings");
    let peers = (0..12)
        .map(|i| peer(&format!("view-{i}")))
        .collect::<Vec<_>>();
    send_fixture(&f, p, json!({"op":"directory","peers":peers}));
    f.send(
        p,
        json!({"op":"test_scroll","id":"directory_scroll","offset":120}),
    );
    let mut more = peers.clone();
    more.insert(0, peer("inserted"));
    send_fixture(&f, p, json!({"op":"directory","peers":more}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
        172,
        "Directory insertion preserves the viewed peer"
    );
    send_fixture(&f, p, json!({"op":"directory","peers":peers}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
        120
    );
    send_fixture(&f, p, json!({"op":"directory","peers":[peer("one")]}));
    frame["activity"]=json!((0..12).map(|i|json!({"hub":"personal","signal_id":format!("sig-{i}"),"listener_id":"me","from_user":"bob","from_host_name":"laptop","from_title":"Review","from_scoped_id":"personal:one","target_harness":"human","target_title":"Me","target_scoped_id":"personal:me","message":"Review ready","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true})).collect::<Vec<_>>());
    f.send(p, frame.clone());
    control(&f, p, "open_activity");
    f.send(
        p,
        json!({"op":"test_scroll","id":"directory_scroll","offset":120}),
    );
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
        120
    );
    frame["activity"][11]["message"] = json!("New details");
    f.send(p, frame.clone());
    let refreshed = inspect(&f, p);
    assert_eq!(refreshed["directory_selected_id"], "sig-0");
    assert_eq!(
        node(&refreshed, "directory_items", "directory_scroll")["content_y"],
        120,
        "Passive refresh must not reveal an unchanged selection"
    );
    let mut inserted = frame["activity"][0].clone();
    inserted["signal_id"] = json!("inserted");
    frame["activity"]
        .as_array_mut()
        .unwrap()
        .insert(0, inserted);
    f.send(p, frame.clone());
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
        180,
        "Insertion above the viewport preserves the viewed Activity row"
    );
    frame["activity"].as_array_mut().unwrap().remove(0);
    f.send(p, frame.clone());
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
        120,
        "Removing the inserted row restores the same anchor"
    );
    control(&f, p, "reply_activity:sig-0");
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
        0,
        "Opening the composer must reveal the selected row"
    );
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Review reply"}),
    );
    control(&f, p, "composer_send");
    control(&f, p, "composer_cancel");
    control(&f, p, "reply_activity:sig-0");
    assert!(
        nodes(&inspect(&f, p)["directory_items"])
            .iter()
            .any(|n| n["text"] == "Sending…"),
        "Reopening must retain pending-send status"
    );
    control(&f, p, "open_settings");
    for op in [
        "delete_activity",
        "clear_activity_history",
        "ack_signals",
        "mark_all_activity_read",
        "copy_activity_message",
    ] {
        // Legacy untagged failures retain their routing; tagged overlap is exercised below.
        f.send(
            p,
            json!({"op":"error","source_op":op,"error":format!("Could not complete {op}")}),
        );
        let s = inspect(&f, p);
        assert_eq!(
            node(&s, "directory_items", "directory_warning")["hidden"],
            false
        );
        assert_eq!(
            node(&s, "directory_items", "directory_warning")["text"],
            "Cannot update Activity. Please try again."
        );
        assert!(node(&s, "directory_items", "directory_warning")["tooltip"]
            .as_str()
            .unwrap()
            .contains(op));
        assert!(node(&s, "settings_items", "settings_error_banner").is_null());
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|r| r["op"] == "query_directory" || r["op"] == "open_directory")
            .unwrap();
        f.send(p,json!({"op":"error","source_op":"query_directory","request_id":request["request_id"],"error":"Directory lookup failed"}));
        assert!(
            node(&inspect(&f, p), "directory_items", "directory_warning")["tooltip"]
                .as_str()
                .unwrap()
                .contains(op),
            "Action error takes precedence over query error"
        );
        f.send(p,json!({"op":"directory","request_id":request["request_id"],"query":request["query"],"scope":request["scope"],"harness":request["harness"],"peers":[],"warnings":[]}));
        assert!(
            node(&inspect(&f, p), "directory_items", "directory_warning")["tooltip"]
                .as_str()
                .unwrap()
                .contains(op),
            "Passive Directory success must retain the action error"
        );
        control(&f, p, "dir_mode:directory");
        assert_eq!(
            node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
            true
        );
        control(&f, p, "dir_mode:activity");
    }
    // Retry B owns feedback before either response arrives.
    let latest_delete = || {
        inspect(&f, p);
        f.frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "clear_activity_history")
            .unwrap()
    };
    control(&f, p, "delete_activity:sig-0");
    let a = latest_delete();
    control(&f, p, "delete_activity:sig-0");
    let b = latest_delete();
    assert!(!a["request_id"].as_str().unwrap().is_empty());
    assert_ne!(a["request_id"], b["request_id"]);
    for op in ["error", "state"] {
        f.send(p,json!({"op":op,"ok":false,"source_op":"clear_activity_history","request_id":a["request_id"],"error":"Late failure from A"}));
        assert_eq!(
            node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
            true,
            "Superseded {op} failure must not own B's feedback"
        );
    }
    f.send(p,json!({"op":"state","ok":false,"source_op":"clear_activity_history","request_id":b["request_id"],"error":"Current failure from B"}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        false,
        "Matching ok:false mutation results must be visible"
    );
    let mut success = frame.clone();
    success["ok"] = json!(true);
    success["source_op"] = json!("clear_activity_history");
    success["request_id"] = a["request_id"].clone();
    f.send(p, success.clone());
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        false,
        "A's success cannot settle B's error"
    );
    success["request_id"] = b["request_id"].clone();
    f.send(p, success);
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        true,
        "B's matching state success settles its feedback"
    );
    f.send(p,json!({"op":"error","source_op":"clear_activity_history","request_id":a["request_id"],"error":"A after B completed"}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        true
    );
    control(&f, p, "delete_activity:sig-0");
    let c = latest_delete();
    f.send(p,json!({"op":"error","source_op":"clear_activity_history","request_id":c["request_id"],"error":"Current failure from C"}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        false
    );
    f.send(p,json!({"op":"directory","ok":true,"source_op":"clear_activity_history","request_id":c["request_id"],"peers":[]}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        true,
        "A matching Directory receipt also settles the action independently of query correlation"
    );
}

#[test]
fn appkit_available_scroll_hub_selector_and_noop_state_diff() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["available"]=json!((0..5).map(|i|json!({"harness":"fixture","session_id":format!("candidate-{i}"),"title":format!("Candidate {i}"),"cwd":if i == 1 { "~/Documents/A-very-long-ancestor/Projects/service" } else if i == 2 { "~/Documents/Projects/VeryLongParentDirectory/project-directory-with-long-name" } else if i == 3 { "/srv/Documents/A-very-long-ancestor/Projects/service" } else if i == 4 { "/srv/Documents/Projects/VeryLongParentDirectory/project-directory-with-long-name" } else { "/tmp/work" },"branch":"main","endpoint_pid":4242,"listening_hubs":["personal"]})).collect::<Vec<_>>());
    f.send(p, frame.clone());
    control(&f, p, "target_hub:work");
    let s = inspect(&f, p);
    assert_eq!(node(&s, "items", "available_scroll")["height"], 176);
    let candidate = node(&s, "items", "candidate:fixture:candidate-0");
    assert_eq!(candidate["height"], 44);
    let title = node(&s, "items", "candidate_title:fixture:candidate-0");
    let path = node(&s, "items", "candidate_cwd:fixture:candidate-0");
    assert_eq!(title["width"], 260);
    assert_eq!(title["x"], 32);
    assert_eq!(path["width"], 260);
    assert_eq!(path["x"], 32);
    assert_eq!(title["y"], 23);
    assert_eq!(path["y"], 6);
    assert_eq!(path["text"], "/tmp/work · main");
    assert_eq!(
        node(&s, "items", "candidate_cwd:fixture:candidate-1")["text"],
        "~/…/Projects/service · main"
    );
    let leaf = node(&s, "items", "candidate_cwd:fixture:candidate-2");
    assert_eq!(leaf["text"], "~/…/project-directory-with-long-name · main");
    assert_eq!(
        leaf["tooltip"],
        "~/Documents/Projects/VeryLongParentDirectory/project-directory-with-long-name · main"
    );
    assert_eq!(
        node(&s, "items", "candidate_cwd:fixture:candidate-3")["text"],
        "/…/Projects/service · main"
    );
    assert_eq!(
        node(&s, "items", "candidate_cwd:fixture:candidate-4")["text"],
        "/…/project-directory-with-long-name · main"
    );
    assert_eq!(s["count"], 0);
    assert_eq!(
        node(&s, "items", "header_subtitle")["text"],
        "Notifications paused"
    );
    assert_eq!(s["available_count"], 5);
    assert_eq!(node(&s, "items", "hub:work")["text"], "work");
    assert!(node(&s, "items", "targetStrip").is_null());
    assert_eq!(
        node(&s, "items", "toggle_auto_listen")["text"],
        "Auto-Listen"
    );
    control(&f, p, "toggle_auto_listen");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "set_auto_listen" && v["hub"] == "work" && v["enabled"] == true)
    });
    control(&f, p, "listen:fixture:candidate-0");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "listen_session" && v["hub"] == "work" && v["session_id"] == "candidate-0"
        })
    });
    assert_eq!(node(&s, "items", "header_hub_strip")["radius"], 11);
    assert_eq!(node(&s, "items", "hub:personal")["border_width"], 0);
    // A semantic track fill must follow the view's appearance without rebuilding.
    for (appearance, dark) in [("dark", true), ("light", false), ("dark", true)] {
        let snapshot = inspect_as(&f, p, Some(appearance));
        let track = node(&snapshot, "items", "header_hub_strip");
        assert_eq!(track["draws_background"], true);
        let background = track["background"].clone();
        let red = background[0].as_f64().unwrap();
        assert!(
            if dark { red > 0.9 } else { red < 0.1 },
            "{appearance} Hub track: {background}"
        );
        assert!((background[3].as_f64().unwrap() - 0.06).abs() < 0.001);
    }
    control(&f, p, "target_hub:personal");
    assert!(!node(&inspect(&f, p), "items", "available_empty_target").is_null());
    f.send(
        p,
        json!({"op":"error","source_op":"hub_action","error":"OIDC cancelled"}),
    );
    let before = inspect(&f, p)["rebuilds"].as_u64().unwrap();
    // An unrelated error must not settle Listen; even an unchanged state must refresh it.
    f.send(p, frame.clone());
    let after = inspect(&f, p)["rebuilds"].as_u64().unwrap();
    assert_eq!(after, before + 1);
    // The next identical state is idle.
    let before = json!(after);
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p)["rebuilds"], before);
    assert!(node(
        &inspect(&f, p),
        "items",
        "switch_hub:personal:repo-abcdefghijkl"
    )
    .is_null());
    control(&f, p, "hub:work");
    assert!(node(
        &inspect(&f, p),
        "items",
        "session:personal:repo-abcdefghijkl"
    )
    .is_null());
    let original_available = frame["available"].clone();
    let auto_before = node(&inspect(&f, p), "items", "toggle_auto_listen");
    // Auto-Listen explains reload blockers and seeds historical groups once.
    frame["hubs"][1]["auto_listen"] = json!(true);
    frame["available"] = json!([
        {"harness":"fixture","session_id":"reload","title":"Reload","cwd":"/tmp","endpoint_pid":4242,"needs_reload":true}
    ]);
    f.send(p, frame.clone());
    let s = inspect(&f, p);
    let auto_after = node(&s, "items", "toggle_auto_listen");
    assert_eq!(auto_after["width"], auto_before["width"]);
    assert_eq!(auto_after["x"], auto_before["x"]);
    assert!(nodes(&s["items"])
        .iter()
        .any(|n| n["text"] == "Auto-Listen is on — run /reload below to finish connecting"));
    frame["available"] = json!([
        {"harness":"fixture","session_id":"recent","title":"History","cwd":"/tmp"}
    ]);
    f.send(p, frame.clone());
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "items", "toggle_group:fixture")["symbol"],
        "chevron.right"
    );
    assert!(node(&s, "items", "candidate:fixture:recent").is_null());
    control(&f, p, "toggle_group:fixture");
    assert!(!node(&inspect(&f, p), "items", "candidate:fixture:recent").is_null());
    frame["unread_count"] = json!(1);
    f.send(p, frame.clone());
    assert!(!node(&inspect(&f, p), "items", "candidate:fixture:recent").is_null());
    frame["available"] = json!([]);
    f.send(p, frame.clone());
    assert!(node(&inspect(&f, p), "items", "available_empty_target").is_null());
    // The sole enabled Hub can differ from the configured default. Listen must
    // still carry the selected network rather than relying on daemon fallback.
    frame["hubs"] = json!([hubs()[1].clone()]);
    frame["available"] = original_available;
    frame["listeners"] = json!([]);
    f.send(p, frame.clone());
    inspect(&f, p);
    control(&f, p, "listen:fixture:candidate-1");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "listen_session" && v["hub"] == "work" && v["session_id"] == "candidate-1"
        })
    });
    // No enabled networks leaves discovery visible, but cannot issue a Listen.
    frame["hubs"] = json!([]);
    f.send(p, frame);
    inspect(&f, p);
    control(&f, p, "listen:fixture:candidate-2");
    inspect(&f, p);
    assert!(!f
        .frames(p)
        .iter()
        .any(|v| { v["op"] == "listen_session" && v["session_id"] == "candidate-2" }));
}
fn hubs() -> Value {
    json!([{"name":"personal","status":"connected","user":"alice","auth_mode":"oidc","error":null},{"name":"work","status":"connected","user":"alice","auth_mode":"none","error":null}])
}
fn listener() -> Value {
    json!({"hub":"personal","listener_id":"c_abcdefghijkl","public_id":"repo-abcdefghijkl","scoped_id":"personal:repo-abcdefghijkl","display_address":"alice/fixture#abcd","harness":"fixture","session_id":"s1","title":"café conversation","cwd":"/tmp","repo":null,"scope":"self"})
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
    inspect_as(f, p, None)
}
fn inspect_unchecked(f: &Fixture, p: usize, appearance: Option<&str>) -> Value {
    let count = f
        .output(p, true)
        .lines()
        .filter(|l| l.starts_with("cito-ui: menu "))
        .count();
    f.send(p, json!({"op":"test_inspect_menu","appearance":appearance}));
    wait(|| {
        f.output(p, true)
            .lines()
            .filter(|l| l.starts_with("cito-ui: menu "))
            .count()
            > count
    });
    let output = f.output(p, true);
    let snapshot: Value = serde_json::from_str(
        output
            .lines()
            .rev()
            .find_map(|l| l.strip_prefix("cito-ui: menu "))
            .unwrap(),
    )
    .unwrap();
    snapshot
}
fn inspect_as(f: &Fixture, p: usize, appearance: Option<&str>) -> Value {
    let snapshot = inspect_unchecked(f, p, appearance);
    assert_eq!(
        snapshot["truncated_labels"],
        json!([]),
        "{:?}",
        ["items", "directory_items", "settings_items", "banner_items"]
            .into_iter()
            .flat_map(|key| nodes(&snapshot[key]))
            .filter(|n| snapshot["truncated_labels"]
                .as_array()
                .is_some_and(|ids| ids.contains(&n["id"])))
            .collect::<Vec<_>>()
    );
    assert_eq!(snapshot["overlapping_siblings"], json!([]));
    assert_eq!(snapshot["clipped_views"], json!([]));
    for key in ["low_contrast_labels", "jargon_leaks", "address_mismatches"] {
        assert_eq!(snapshot[key], json!([]), "{key}");
    }
    snapshot
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
/// Synthetic directory frames echo the real UI request, just as the daemon does.
fn send_fixture(f: &Fixture, p: usize, mut frame: Value) {
    if frame["op"] == "directory" && frame["request_id"].is_null() {
        inspect(f, p);
        if let Some(request) = f
            .frames(p)
            .iter()
            .rev()
            .find(|a| matches!(a["op"].as_str(), Some("query_directory" | "open_directory")))
        {
            for key in ["request_id", "query", "scope", "harness"] {
                frame[key] = request[key].clone();
            }
        }
    }
    f.send(p, frame);
}
fn peer(id: &str) -> Value {
    json!({"hub":"personal","listener_id":format!("c_{id}"),"display_address":format!("bob/fixture#{}", &cito_native::core::identity::hash(id)[..4]),"public_id":id,"scoped_id":format!("personal:{id}"),"user":"bob","host_id":"remote","host_name":"laptop","harness":"fixture","title":id,"about":"Schema work","repo":"acme/service","branch":"main","scope":"workspace","last_seen_at":"2030-01-01T00:00:00Z","is_self_principal":false,"channels":["laptop"]})
}
fn banner() -> Value {
    json!({"op":"banner","payload":{"target_harness":"fixture","target_session_id":"s1","target_title":"café conversation","from_user":"alice","from_host":"host","from_title":"sender","message":"hello café","effective_wake":true,"signal_id":"personal:sig_1","hub":"personal","from_scoped_id":"personal:sender-abcdefgh2345"}})
}
#[test]
fn appkit_card_tree_geometry_icon_hover_copy_and_multi_hub_identity() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let icon = f.home.join("second/fixture/icon.png");
    fs::create_dir_all(icon.parent().unwrap()).unwrap();
    fs::copy(root().join("plugins/pi/icon.png"), icon).unwrap();
    f.env
        .insert("CITO_PLUGIN_PATH".into(), " ~/first : ~/second ".into());
    let p = start(&f);
    let mut initial = state();
    let mut other_network = peer("work-peer");
    other_network["hub"] = json!("work");
    other_network["scoped_id"] = json!("work:work-peer");
    other_network["display_address"] = json!("work:bob/fixture#1234");
    let mut unscoped = peer("unscoped");
    unscoped.as_object_mut().unwrap().remove("hub");
    initial["my_remote_listeners"] = json!([peer("remote"), other_network, unscoped]);
    initial["available"] =
        json!([{"harness":"fixture","session_id":"candidate","title":"alice","cwd":"/tmp"}]);
    f.send(p, initial);
    let s = inspect(&f, p);
    assert_eq!(s["count"], 1);
    assert!(node(&s, "items", "remote:work:work-peer").is_null());
    assert!(!node(&s, "items", "remote:personal:unscoped").is_null());
    assert_eq!(s["icon_template"], true);
    assert_eq!(s["icon_width"], 20);
    assert_eq!(s["button_title"], "");
    let candidate = node(&s, "items", "candidate_title:fixture:candidate");
    assert_eq!(candidate["text"], "Fixture");
    assert_eq!(candidate["tooltip"], "Fixture — /tmp");
    assert_eq!(
        node(&s, "items", "listen:fixture:candidate")["tooltip"],
        "Start listening in Fixture"
    );
    control(&f, p, "listen:fixture:candidate");
    wait(|| f.frames(p).iter().any(|v| v["op"] == "listen_session"));
    assert_eq!(
        f.frames(p)
            .into_iter()
            .find(|v| v["op"] == "listen_session")
            .unwrap()["title"],
        "Fixture"
    );
    initial = state();
    initial["available"] = json!([{"harness":"fixture","session_id":"candidate","title":"alice","custom_title":true,"cwd":"/tmp"}]);
    f.send(p, initial);
    let custom = inspect(&f, p);
    assert_eq!(
        node(&custom, "items", "candidate_title:fixture:candidate")["text"],
        "alice"
    );
    control(&f, p, "listen:fixture:candidate");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "listen_session" && v["title"] == "alice")
    });
    assert!(s["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["width"] == 380));
    assert_eq!(node(&s, "items", "plugin_icon:fixture")["tint"], json!([]));
    for id in ["toggle_my_remote", "toggle_group:fixture"] {
        assert_eq!(node(&s, "items", id)["x"], 12);
        assert_eq!(node(&s, "items", id)["width"], 356);
    }
    assert_eq!(node(&s, "items", "header")["height"], 40);
    assert_eq!(node(&s, "items", "footer")["height"], 36);
    assert_eq!(node(&s, "items", "drawer:personal")["hidden"], true);
    control(&f, p, "hub:personal");
    assert_eq!(
        node(&inspect(&f, p), "items", "drawer:personal")["hidden"],
        true
    );
    control(&f, p, "hub:personal");
    assert_eq!(
        node(&inspect(&f, p), "items", "drawer:personal")["hidden"],
        false,
        "The selected healthy Hub must expose its drawer"
    );
    control(&f, p, "hub:personal");
    assert_eq!(
        node(&inspect(&f, p), "items", "drawer:personal")["hidden"],
        true
    );
    control(&f, p, "copy:personal:repo-abcdefghijkl");
    let copied = inspect(&f, p);
    assert_eq!(copied["clipboard"], "alice/fixture#abcd");
    assert_eq!(
        node(&copied, "items", "copy:personal:repo-abcdefghijkl")["text"],
        "✓ Copied"
    );
    assert_eq!(copied["truncated_labels"], json!([]));
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
    next["hubs"][1]["status"] = json!("disconnected");
    let mut second = listener();
    second["hub"] = json!("work");
    second["scoped_id"] = json!("work:repo-workabcdefgh");
    second["listener_id"] = json!("c_workabcdefgh");
    second["display_address"] = json!("work:alice/fixture#abcd");
    next["listeners"] = json!([listener(), second]);
    f.send(p, next.clone());
    let s = inspect(&f, p);
    assert_eq!(s["count"], 1);
    assert!(node(&s, "items", "session:work:repo-workabcdefgh").is_null());
    assert!(!node(&s, "items", "session:personal:repo-abcdefghijkl").is_null());
    control(&f, p, "hub:work");
    let switched = inspect(&f, p);
    assert!(!node(&switched, "items", "session:work:repo-workabcdefgh").is_null());
    assert!(node(&switched, "items", "session:personal:repo-abcdefghijkl").is_null());
    control(&f, p, "copy:work:repo-workabcdefgh");
    assert_eq!(inspect(&f, p)["clipboard"], "work:alice/fixture#abcd");
    control(&f, p, "hub:personal");
    assert!(!node(
        &inspect(&f, p),
        "items",
        "session:personal:repo-abcdefghijkl"
    )
    .is_null());
    control(&f, p, "copy:personal:repo-abcdefghijkl");
    assert_eq!(inspect(&f, p)["clipboard"], "alice/fixture#abcd");
    control(&f, p, "hub:work");
    assert!(!node(&inspect(&f, p), "items", "session:work:repo-workabcdefgh").is_null());
    assert!(
        node(&inspect(&f, p), "items", "scope:work:repo-workabcdefgh").is_null(),
        "Personal networks hide redundant visibility controls"
    );
    let pause = node(&inspect(&f, p), "items", "stop:work:repo-workabcdefgh");
    assert_eq!(pause["text"], "Pause");
    assert_eq!(pause["tooltip"], "Pause listening for this session");
    assert_eq!(pause["border_width"], 1);
    assert!(pause["symbol"].is_null());
    next["hubs"][1]["auth_mode"] = json!("token");
    f.send(p, next.clone());
    assert!(node(&inspect(&f, p), "items", "scope:work:repo-workabcdefgh").is_null());
    let mut other = next["listeners"][1].clone();
    other["user"] = json!("bob");
    other["listener_id"] = json!("c_other");
    other["scoped_id"] = json!("work:other");
    next["listeners"].as_array_mut().unwrap().push(other);
    f.send(p, next.clone());
    assert!(
        !node(&inspect(&f, p), "items", "scope:work:repo-workabcdefgh").is_null(),
        "Another user makes visibility meaningful even on token Hubs"
    );
    next["listeners"].as_array_mut().unwrap().pop();
    next["hubs"][1]["auth_mode"] = json!("oidc");
    f.send(p, next.clone());
    let scope_before = node(&inspect(&f, p), "items", "scope:work:repo-workabcdefgh");
    assert_eq!(scope_before["width"], 68);
    assert_eq!(scope_before["tooltip"], "Visibility: Only Me (@alice) — only your own sessions and devices on 'work' can message this session. Click to share with Team.");
    control(&f, p, "scope:work:repo-workabcdefgh");
    wait(|| {
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "set_scope")
            .count()
            == 1
    });
    let scope_after = node(&inspect(&f, p), "items", "scope:work:repo-workabcdefgh");
    assert_eq!(scope_after["width"], scope_before["width"]);
    assert_eq!(scope_after["x"], scope_before["x"]);
    let scope_team = scope_after;
    assert_eq!(scope_team["tooltip"], "Visibility: Team — anyone connected to 'work' can discover and message this session. Click to restrict to Only Me.");
    control(&f, p, "scope:work:repo-workabcdefgh");
    let scope_self = node(&inspect(&f, p), "items", "scope:work:repo-workabcdefgh");
    assert_eq!(scope_self["text"], "Self");
    assert_eq!(scope_self["width"], scope_team["width"]);
    assert_eq!(scope_self["x"], scope_team["x"]);
    let scopes: Vec<_> = f
        .frames(p)
        .into_iter()
        .filter(|v| v["op"] == "set_scope")
        .collect();
    assert!(!scopes.iter().any(|v| v["hub"] == "personal"));
    assert!(scopes.iter().any(|v| v["hub"] == "work"));
    assert_eq!(scopes[0]["scope"], "workspace");
    assert_eq!(scopes[1]["scope"], "self");
    control(&f, p, "rename:work:repo-workabcdefgh");
    assert!(!node(
        &inspect(&f, p),
        "items",
        "rename_field:work:repo-workabcdefgh"
    )
    .is_null());
    f.send(p, json!({"op":"test_settings_input","id":"rename_field:work:repo-workabcdefgh","value":"Backend API"}));
    f.send(p, json!({"op":"test_menu_key","key":"escape"}));
    assert!(node(
        &inspect(&f, p),
        "items",
        "rename_field:work:repo-workabcdefgh"
    )
    .is_null());
    control(&f, p, "rename:work:repo-workabcdefgh");
    f.send(p, json!({"op":"test_settings_input","id":"rename_field:work:repo-workabcdefgh","value":"Backend API"}));
    next["unread_count"] = json!(1);
    f.send(p, next);
    assert_eq!(
        node(
            &inspect(&f, p),
            "items",
            "rename_field:work:repo-workabcdefgh"
        )["text"],
        "Backend API"
    );
    f.send(p, json!({"op":"test_menu_key","key":"return"}));
    wait(|| f.frames(p).iter().any(|v| v["op"] == "rename_session"));
    let renamed = f
        .frames(p)
        .into_iter()
        .find(|v| v["op"] == "rename_session")
        .unwrap();
    assert_eq!(renamed["harness"], "fixture");
    assert_eq!(renamed["session_id"], "s1");
    assert_eq!(renamed["title"], "Backend API");
    let after = inspect(&f, p);
    assert!(node(&after, "items", "rename_field:work:repo-workabcdefgh").is_null());
    assert_eq!(after["overlapping_siblings"], json!([]));
    control(&f, p, "stop:work:repo-workabcdefgh");
    wait(|| f.frames(p).iter().any(|v| v["op"] == "unlisten_session"));
    let stop = f
        .frames(p)
        .into_iter()
        .find(|v| v["op"] == "unlisten_session")
        .unwrap();
    assert_eq!(stop["session_id"], "s1");
    assert_eq!(stop["hub"], "work");

    let mut human = listener();
    human["hub"] = json!("local");
    human["harness"] = json!("human");
    human["session_id"] = json!("default");
    human["listener_id"] = json!("human:default");
    human["public_id"] = json!("human:default");
    human["scoped_id"] = json!("local:human:default");
    human["title"] = json!("Human");
    human["user"] = json!("a");
    human["display_address"] = json!("@a");
    let mut short_addresses = state();
    short_addresses["hubs"] =
        json!([{"name":"local","status":"connected","auth_mode":"none","user":"a"}]);
    short_addresses["listeners"] = json!([human]);
    f.send(p, short_addresses);
    let before = inspect(&f, p);
    assert!(!nodes(&before["items"])
        .iter()
        .any(|n| n["text"] == "Desktop notifications"));
    let id = "copy:local:human:default";
    assert_eq!(node(&before, "items", id)["text"], "@a");
    for appearance in ["light", "dark"] {
        control(&f, p, id);
        let copied = inspect_as(&f, p, Some(appearance));
        assert_eq!(copied["clipboard"], "@a");
        assert_eq!(node(&copied, "items", id)["text"], "✓ Copied");
        assert_eq!(
            node(&copied, "items", id)["width"],
            node(&before, "items", id)["width"]
        );
        assert_eq!(copied["clipped_views"], json!([]));
        assert_eq!(copied["truncated_labels"], json!([]));
    }
    // Isolate the address projection while keeping unrelated identity captions short.
    let full = format!("work:{}/fixture#abcd", "a".repeat(64));
    let mut long = listener();
    long["hub"] = json!("work");
    long["scoped_id"] = json!("work:long");
    long["display_address"] = json!(full);
    long["title"] = json!("Review");
    let mut remote = peer("long-remote");
    remote["hub"] = json!("work");
    remote["scoped_id"] = json!("work:long-remote");
    remote["display_address"] = json!(full);
    let mut addresses = state();
    addresses["listeners"] = json!([long]);
    addresses["my_remote_listeners"] = json!([remote.clone()]);
    f.send(p, addresses);
    control(&f, p, "target_hub:work");
    if node(&inspect(&f, p), "items", "copy_remote:work:long-remote").is_null() {
        control(&f, p, "toggle_my_remote");
    }
    render_png(&f, p, "menu", "r2_long_address_menu.png");
    for id in ["copy:work:long", "copy_remote:work:long-remote"] {
        let chip = node(&inspect(&f, p), "items", id);
        let shown = chip["text"].as_str().unwrap();
        assert!(shown.contains('…') && shown.starts_with("work:") && shown.ends_with("#abcd"));
        assert_eq!(chip["tooltip"], full);
        assert!(chip["width"].as_f64().unwrap() <= 220.0);
        control(&f, p, id);
        assert_eq!(inspect(&f, p)["clipboard"], full);
    }
    control(&f, p, "open_directory");
    send_fixture(&f, p, json!({"op":"directory","peers":[remote]}));
    for appearance in ["light", "dark"] {
        let s = inspect_as(&f, p, Some(appearance));
        let chip = node(&s, "directory_items", "copy_dir:work:long-remote");
        let shown = chip["text"].as_str().unwrap();
        assert!(shown.contains('…') && shown.starts_with("work:") && shown.ends_with("#abcd"));
        assert_eq!(chip["tooltip"], full);
        assert!(chip["width"].as_f64().unwrap() <= 190.0);
        render_png_as(
            &f,
            p,
            "directory",
            &format!("r2_long_address_directory_{appearance}.png"),
            Some(appearance),
        );
    }
    control(&f, p, "copy_dir:work:long-remote");
    assert_eq!(inspect(&f, p)["clipboard"], full);
    let mut many = state();
    many["listeners"] = json!((0..6)
        .map(|i| {
            let mut row = listener();
            row["session_id"] = json!(format!("session-{i}"));
            row["scoped_id"] = json!(format!("personal:session-{i}"));
            row
        })
        .collect::<Vec<_>>());
    f.send(p, many);
    control(&f, p, "hub:personal");
    let scrolled = inspect(&f, p);
    assert_eq!(node(&scrolled, "items", "active_scroll")["height"], 256);
    for i in 0..6 {
        assert_eq!(
            node(&scrolled, "items", &format!("session:personal:session-{i}"))["height"],
            64
        );
    }
}
#[test]
fn appkit_directory_keyboard_navigation_shortcuts_and_preserved_peers() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    f.send(p, state());
    control(&f, p, "open_directory");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one"),peer("two")],"matched_count":2,"online_count":5,"unread_count":0,"activity":[]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["directory_open"], true);
    assert_eq!(s["directory_selected_id"], "personal:one");
    assert_eq!(
        node(&s, "directory_items", "dir_scope:mine")["border_width"],
        0
    );
    assert_eq!(
        node(&s, "directory_items", "dir_harness:fixture")["border_width"],
        0
    );
    assert_eq!(
        node(&s, "directory_items", "dir_scope:all")["border_width"],
        1
    );
    assert_eq!(node(&s, "directory_items", "directory_panel")["width"], 560);
    for id in [
        "directory_scope_track",
        "directory_harness_scroll",
        "directory_mode_track",
    ] {
        assert!(
            (node(&s, "directory_items", id)["background"][3]
                .as_f64()
                .unwrap()
                - 0.06)
                .abs()
                < 0.001
        );
    }
    for membership in ["listeners", "hubs"] {
        let queries = f
            .frames(p)
            .iter()
            .filter(|v| v["op"] == "query_directory")
            .count();
        let mut changed = state();
        if membership == "listeners" {
            changed["listeners"] = json!([]);
        } else {
            changed["hubs"][0]["enabled"] = json!(false);
        }
        f.send(p, changed);
        inspect(&f, p);
        assert_eq!(
            f.frames(p)
                .iter()
                .filter(|v| v["op"] == "query_directory")
                .count(),
            queries + 1,
            "{membership}"
        );
        send_fixture(
            &f,
            p,
            json!({"op":"directory","peers":[peer("one"),peer("two")],"matched_count":2,"online_count":5}),
        );
        f.send(p, state());
        inspect(&f, p);
    }
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    assert_eq!(inspect(&f, p)["directory_selected_id"], "personal:two");
    f.send(p, json!({"op":"test_directory_key","key":"return"}));
    assert_eq!(inspect(&f, p)["clipboard"], peer("two")["display_address"]);
    f.send(p, json!({"op":"test_directory_key","key":"up"}));
    assert_eq!(inspect(&f, p)["directory_selected_id"], "personal:one");
    control(&f, p, "msg_dir:personal:one");
    let before = f.frames(p).len();
    for oversized in ["x".repeat(65_537), "e\u{301}".repeat(32_769)] {
        f.send(p, json!({"op":"test_composer_input","value":oversized}));
        control(&f, p, "composer_send");
        let s = inspect(&f, p);
        assert_eq!(
            node(&s, "directory_items", "composer_input")["text"],
            oversized
        );
        assert_eq!(
            node(&s, "directory_items", "composer_error")["text"],
            "Message exceeds maximum length"
        );
        assert_eq!(
            f.frames(p).len(),
            before,
            "Oversized input must not be emitted"
        );
    }
    // Word breaks keep native text layout bounded while testing 65,536 Unicode scalars.
    let maximum = "😀 ".repeat(32_767) + "😀x";
    f.send(p, json!({"op":"test_composer_input","value":maximum}));
    control(&f, p, "composer_send");
    inspect(&f, p);
    let submitted = f.frames(p)[before..]
        .iter()
        .find(|v| v["op"] == "send_signal")
        .unwrap()
        .clone();
    assert_eq!(
        submitted["message"], maximum,
        "Local rejection must not leave the composer pending"
    );
    assert!(serde_json::to_vec(&submitted).unwrap().len() <= 1_048_576);
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":submitted["request_id"]}),
    );
    assert_eq!(inspect(&f, p)["composer_open"], false);
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
        json!({"op":"error","source_op":"hub_action","error":"OIDC cancelled"}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(
        node(&s, "directory_items", "composer_error")["text"],
        "Sending…"
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "send_signal" && v["message"] == "Draft preserved")
            .count(),
        1,
        "Unrelated errors must not permit a duplicate send"
    );
    let submitted = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "send_signal")
        .unwrap();
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":submitted["request_id"]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], false);
    assert_eq!(node(&s, "directory_items", "composer_input")["text"], "");
    control(&f, p, "msg_dir:personal:one");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Draft preserved"}),
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "send_signal" && v["message"] == "Draft preserved")
            .count(),
        2,
        "The composer result must settle the send and allow a new one"
    );
    let failed = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "send_signal")
        .unwrap();
    assert!(!failed["request_id"].as_str().unwrap().is_empty());
    f.send(
        p,
        json!({"op":"composer_result","ok":false,"error":"Hub unavailable. Retry.","request_id":failed["request_id"]}),
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
    // A's trailing generic error can arrive after B has already been submitted.
    control(&f, p, "composer_send");
    inspect(&f, p);
    let retry = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "send_signal")
        .unwrap();
    assert_ne!(retry["request_id"], failed["request_id"]);
    for source_op in ["send_signal", "send_reply_from_human"] {
        f.send(p, json!({"op":"error","source_op":source_op,"request_id":failed["request_id"],"error":"Late send error"}));
    }
    f.send(p, json!({"op":"composer_result","ok":false,"request_id":failed["request_id"],"error":"Late result"}));
    for frame in [
        json!({"op":"composer_result","ok":true}),
        json!({"op":"composer_result","ok":false,"error":"Untagged result"}),
        json!({"op":"error","error":"INVALID_INPUT: Message is too large to send"}),
        json!({"op":"error","source_op":"send_signal","error":"Untagged send error"}),
        json!({"op":"error","source_op":"send_reply_from_human","error":"Untagged reply error"}),
    ] {
        f.send(p, frame);
    }
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(
        node(&s, "directory_items", "composer_error")["text"],
        "Sending…"
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "send_signal" && v["message"] == "Draft preserved")
            .count(),
        3,
        "A's late error must not permit a duplicate of B"
    );
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":retry["request_id"]}),
    );
    assert_eq!(inspect(&f, p)["composer_open"], false);
    control(&f, p, "msg_dir:personal:one");
    f.send(p, json!({"op":"test_composer_input","value":"Next draft"}));
    f.send(p, json!({"op":"error","source_op":"send_signal","request_id":retry["request_id"],"error":"Already settled"}));
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "directory_items", "composer_input")["text"],
        "Next draft"
    );
    assert_eq!(node(&s, "directory_items", "composer_error")["text"], "");
    control(&f, p, "composer_cancel");
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_shortcuts")["text"],
        "↵ Copy Address    ⎋ Close"
    );
    control(&f, p, "dir_mode:activity");
    let activity: Vec<_> = ["reply_a", "reply_b"]
        .into_iter()
        .map(|id| {
            json!({"hub":"personal","signal_id":format!("personal:{id}"),
                "listener_id":"c_human","from_user":"bob","from_host_name":"laptop",
                "from_harness":"fixture","from_title":"Schema work","from_scoped_id":"personal:one",
                "target_harness":"human","target_title":"Me","target_scoped_id":"personal:me",
                "message":"Approve?","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true})
        })
        .collect();
    let mut frame = state();
    frame["activity"] = json!(activity);
    frame["unread_count"] = json!(2);
    f.send(p, frame);
    control(&f, p, "reply_activity:personal:reply_a");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Same reply text"}),
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    let replies: Vec<_> = f
        .frames(p)
        .into_iter()
        .filter(|v| v["op"] == "send_signal" && v["message"] == "Same reply text")
        .collect();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["to"], "personal:one");
    assert_eq!(replies[0]["hub"], "personal");
    assert_eq!(replies[0]["in_reply_to"], "personal:reply_a");
    control(&f, p, "reply_activity:personal:reply_b");
    let s = inspect(&f, p);
    assert_eq!(s["composer_reply_signal_id"], "personal:reply_b");
    assert_eq!(
        node(&s, "directory_items", "composer_input")["text"],
        "Same reply text"
    );
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":replies[0]["request_id"]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(s["composer_target"], "personal:one");
    assert_eq!(s["composer_reply_signal_id"], "personal:reply_b");
    assert_eq!(
        node(&s, "directory_items", "composer_input")["text"],
        "Same reply text"
    );
    assert_eq!(
        node(&s, "directory_items", "composer_error")["text"],
        "Sent. Your newer draft is preserved."
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    let replies: Vec<_> = f
        .frames(p)
        .into_iter()
        .filter(|v| v["op"] == "send_signal" && v["message"] == "Same reply text")
        .collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1]["to"], "personal:one");
    assert_eq!(replies[1]["hub"], "personal");
    assert_eq!(replies[1]["in_reply_to"], "personal:reply_b");
    // Cancel only hides the draft; its in-flight send must still own its snapshot.
    control(&f, p, "composer_cancel");
    control(&f, p, "dir_mode:directory");
    control(&f, p, "msg_dir:personal:one");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Same reply text"}),
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "send_signal" && v["message"] == "Same reply text")
            .count(),
        2,
        "Cancel must not allow a second send before the first result"
    );
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":replies[1]["request_id"]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(s["composer_reply_signal_id"], "");
    assert_eq!(
        node(&s, "directory_items", "composer_input")["text"],
        "Same reply text"
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    let replies: Vec<_> = f
        .frames(p)
        .into_iter()
        .filter(|v| v["op"] == "send_signal" && v["message"] == "Same reply text")
        .collect();
    assert_eq!(replies.len(), 3);
    assert!(replies[2]["in_reply_to"].is_null());
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":replies[2]["request_id"]}),
    );
    assert_eq!(inspect(&f, p)["composer_open"], false);
    f.send(p, state());
    assert!(!node(&inspect(&f, p), "directory_items", "msg_dir:personal:one").is_null());
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["directory_open"], false);
}
fn settings() -> Value {
    json!({"apps":[{"id":"pi","display_name":"Pi","tier":"stable","installed":true,"configured":true,"status":"configured","detail":"Connected","paths":["/tmp/settings.json"]},{"id":"codex","display_name":"Codex","tier":"stable","installed":true,"configured":false,"status":"not_configured","detail":"Click Set Up to connect","paths":[]},{"id":"cursor","display_name":"Cursor","tier":"experimental","installed":false,"configured":false,"status":"not_configured","detail":"Click Set Up to connect","paths":[]}],"channels":[{"id":"channel","display_name":"Human Channel","sf_symbol":"paperplane.fill","tint":"blue","enabled":true,"configured":false,"status":"hub_ready","detail":"Ready","hub_provided":false,"pairing_supported":true,"fields":[{"key":"secret","scope":"hub","label":"Secret","type":"secret","required":true,"placeholder":"Token","value":"secret","configured":true},{"key":"account","scope":"user","label":"Account","type":"string","required":true,"placeholder":"Account","value":"","configured":false}]}],"desktop_banner_enabled":true,"hubs":[{"name":"personal","url":"local://","auth_mode":"none","enabled":true,"is_default":true,"status":"connected","user":"alice","error":null},{"name":"work","url":"http://127.0.0.1:4291","auth_mode":"none","enabled":true,"is_default":false,"status":"connected","user":"alice","error":null}],"host_hub":{"enabled":false,"running":false,"port":4291,"endpoint_url":"http://127.0.0.1:4291","token":"","invite_command":"","error":null},"launch_at_login":false})
}
#[test]
fn v29_oracle_detects_real_appkit_geometry_contrast_jargon_and_address_faults() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = f.spawn(
        root().join("bin/cito-ui-darwin-arm64").to_str().unwrap(),
        &[],
        &[("CITO_BANNER_DISMISS_MS", "60000")],
    );
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
        let mut invalid = banner();
        invalid["payload"]["from_title"] = json!("[Passive] Sender");
        f.send(p, invalid);
        assert_eq!(
            inspect_as(&f, p, Some(appearance))["jargon_leaks"],
            json!([])
        );
        f.send(
            p,
            json!({"op":"test_oracle_calibration","kind":"banner_jargon"}),
        );
        assert!(inspect_unchecked(&f, p, Some(appearance))["jargon_leaks"]
            .as_array()
            .unwrap()
            .contains(&json!("banner_data:1")));
        // A human banner's fixed title is chrome and must be measured for truncation.
        let mut chrome = banner();
        chrome["payload"]["target_harness"] = json!("human");
        f.send(p, chrome);
        assert_eq!(
            node(
                &inspect_unchecked(&f, p, Some(appearance)),
                "banner_items",
                "banner_chrome:0"
            )["text"],
            "Cito · Direct Message"
        );
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
    let p = f.spawn(
        root().join("bin/cito-ui-darwin-arm64").to_str().unwrap(),
        &[],
        &[("CITO_BANNER_DISMISS_MS", "60000")],
    );
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
            "directory" => Some(("directory_items", "directory_harness_scroll", 4.0, 406.0)),
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
                    + if pills.is_empty() { 0.0 } else { 6.0 };
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
                    assert!((first["x"].as_f64().unwrap() - 3.0).abs() < 0.5);
                    let end = pills
                        .iter()
                        .map(|pill| pill["x"].as_f64().unwrap() + pill["width"].as_f64().unwrap())
                        .fold(0.0, f64::max);
                    assert!(
                        (intrinsic - end - 3.0).abs() < 0.5,
                        "{id} must have exactly 3pt trailing padding: {view}"
                    );
                }
                let x = view["x"].as_f64().unwrap();
                match surface {
                    "menu" => assert!((x + width - 368.0).abs() < 0.5),
                    "directory" => assert_eq!(x, 138.0),
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
    frame["listeners"] = json!([agent, human]);
    frame["available"] = json!([{"harness":"codex","session_id":"review","title":"api-server","cwd":"~/Projects/acme/service","branch":"main"}]);
    frame["unread_count"] = json!(2);
    f.send(p, frame);
    let s = inspect(&f, p);
    assert_eq!(s["count"], 1);
    assert_eq!(node(&s, "items", "plugin_icon:pi")["tint"], json!([]));
    assert_eq!(node(&s, "items", "plugin_icon:pi")["x"], 0);
    assert_eq!(node(&s, "items", "plugin_icon:pi")["width"], 28);
    let copy = node(&s, "items", "copy:local:alice-76e846d115ce642f");
    assert!(nodes(&copy["children"])
        .iter()
        .any(|n| n["x"] == 0 && n["width"] == 12));
    assert_eq!(
        node(&s, "items", "plugin_symbol:human")["tint"],
        json!([1, 1, 1, 1])
    );
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
        assert_eq!(pauses.len(), 2);
        for pause in &pauses {
            assert_eq!(pause["text"], "Pause");
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
    let short = json!({"hub":"local","signal_id":"local:short","listener_id":"c_76e846d115ce642f","from_user":"bob","from_host_name":"laptop","from_harness":"pi","from_title":"Review","from_scoped_id":"local:review","from_display_address":"bob/pi#8a4f","target_harness":"human","target_title":"Human","target_scoped_id":"local:alice-76e846d115ce642f","target_display_address":"@alice","message":"All checks passed on release branch. Ship v2.4 now or hold for docs?","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true});
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
        "Human · mac-studio"
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
            for (appearance, dark) in [("light", false), ("dark", true)] {
                let snapshot = inspect_as(&f, p, Some(appearance));
                let track = node(&snapshot, "settings_items", "channel_hub_strip");
                assert_eq!(track["radius"], 11);
                assert_eq!(track["draws_background"], true);
                let red = track["background"][0].as_f64().unwrap();
                assert!(if dark { red > 0.9 } else { red < 0.1 });
                assert!((track["background"][3].as_f64().unwrap() - 0.06).abs() < 0.001);
                assert_eq!(
                    node(&snapshot, "settings_items", "channel_hub_selector:local")["border_width"],
                    1
                );
                assert_eq!(
                    node(&snapshot, "settings_items", "channel_hub_selector:work")["border_width"],
                    0
                );
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
            assert_eq!(node(&s, "settings_items", "host_hub_card")["height"], 136);
            assert_eq!(
                node(&s, "settings_items", "paste_hub_invite")["text"],
                "Paste Invite Code"
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
    assert_eq!(node(&s, "settings_items", "host_hub_card")["height"], 136);
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
        node(&local_error, "settings_items", "add_hub_form")["height"],
        144
    );
    assert!(
        error["y"].as_f64().unwrap()
            >= user["y"].as_f64().unwrap() + user["height"].as_f64().unwrap() + 6.0
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
        true
    );
    capture("settings", "settings_uninstall_modal");
    control(&f, p, "cancel_uninstall_all");
    control(&f, p, "close_settings");
    assert_eq!(inspect(&f, p)["settings_open"], false);

    for (kind, wake, name) in [
        ("human", true, "banner_human_notification"),
        ("pi", true, "banner_agent_notification"),
        ("pi", false, "banner_agent_passive"),
        ("human", false, "banner_human_passive"),
    ] {
        let mut notification = banner();
        notification["payload"]["hub"] = json!("local");
        notification["payload"]["target_harness"] = json!(kind);
        notification["payload"]["target_title"] = json!("Review");
        notification["payload"]["target_session_id"] =
            json!(if kind == "human" { "default" } else { "s1" });
        notification["payload"]["effective_wake"] = json!(wake);
        notification["payload"]["message"] = json!("The review is ready.");
        notification["payload"]["from_user"] = json!("bob");
        notification["payload"]["from_title"] = json!("Review");
        notification["payload"]["from_display_address"] = json!("bob/pi#abcd");
        f.send(p, notification);
        let s = inspect(&f, p);
        let title_id = if kind == "human" {
            "banner_chrome:0"
        } else {
            "banner_data:0"
        };
        assert_eq!(
            node(&s, "banner_items", title_id)["text"],
            if kind == "human" {
                "Cito · Direct Message"
            } else {
                "Cito · Review"
            }
        );
        assert_eq!(node(&s, "banner_items", title_id)["hidden"], false);
        assert_eq!(
            node(&s, "banner_items", "banner_semantic_pill")["children"][0]["text"],
            if wake { "Notification" } else { "Queued" }
        );
        capture("banner", name);
    }
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
        true
    );
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["settings_open"], true);
    let mut notification = banner();
    notification["payload"]["target_harness"] = json!("human");
    notification["payload"]["effective_wake"] = json!(false);
    f.send(p, notification);
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
        assert!(!s["banner_items"].as_array().unwrap().is_empty());
        render_png_as(
            &f,
            p,
            "banner",
            &format!("v29-banner-{appearance}.png"),
            Some(appearance),
        );
    }
}

#[test]
fn v29_full_message_copy_is_correlated_and_never_copies_a_preview() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    let mut l = ClosedLoop::new(&f);
    l.until(|s| node(s, "items", "header").is_object());
    let message = "😀 ".repeat(32_768);
    let sent = f.tool("cito_send", json!({"to":"@self","message":message}), meta);
    assert_eq!(sent["ok"], true, "{sent}");
    wait(|| !f.rows("SELECT signal_id FROM local_signals").is_empty());
    l.control("open_activity");
    let s = l.until(|s| {
        nodes(&s["directory_items"]).iter().any(|n| {
            n["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("copy_activity_msg:"))
        })
    });
    let control_id = nodes(&s["directory_items"])
        .into_iter()
        .find_map(|n| {
            n["id"]
                .as_str()
                .filter(|id| id.starts_with("copy_activity_msg:"))
                .map(str::to_owned)
        })
        .unwrap();
    f.send(l.ui, json!({"op":"test_clipboard","text":"old clipboard"}));
    l.control(&control_id);
    assert_eq!(inspect(&f, l.ui)["clipboard"], "old clipboard");
    let copied = l.until(|s| s["clipboard"] == message.trim());
    assert_eq!(copied["clipboard"], message.trim());
    assert_eq!(
        node(&copied, "directory_items", &control_id)["text"],
        "✓ Copied"
    );
    l.control(&control_id);
    l.pump();
    f.send(
        l.ui,
        json!({"op":"test_expire_copy_feedback","id":control_id}),
    );
    let restored = inspect(&f, l.ui);
    let button = node(&restored, "directory_items", &control_id);
    assert_eq!(button["text"], "Copy");
    assert_eq!(button["accessibility_label"], "Copy message");
    let frames: Vec<_> = l
        .frames
        .iter()
        .filter(|f| f["op"] == "activity_message")
        .collect();
    assert!(frames.len() > 1);
    assert!(frames
        .iter()
        .all(|f| serde_json::to_vec(f).unwrap().len() < 262_144));
    l.control(&control_id);
    let human = f.rows("SELECT public_id FROM local_listeners WHERE harness='human'")[0]
        ["public_id"]
        .as_str()
        .unwrap()
        .to_string();
    l.control(&format!("copy:{human}"));
    l.pump();
    assert_eq!(
        inspect(&f, l.ui)["clipboard"],
        "@alice",
        "A newer clipboard action supersedes in-flight message chunks"
    );
    assert_eq!(inspect(&f, l.ui)["composer_open"], false);
    l.control(&control_id);
    let request = f
        .frames(l.ui)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "copy_activity_message")
        .unwrap();
    l.control("dir_mode:directory");
    f.send(l.ui,json!({"op":"activity_message","request_id":request["request_id"],"part":0,"message":message.trim(),"complete":true}));
    let switched = inspect(&f, l.ui);
    assert_eq!(
        switched["clipboard"],
        message.trim(),
        "A hidden Activity row still completes its copy"
    );
    let addresses: Vec<_> = nodes(&switched["directory_items"])
        .into_iter()
        .filter(|n| {
            n["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("copy_dir:"))
        })
        .collect();
    assert!(!addresses.is_empty());
    assert!(
        addresses.iter().all(|n| n["text"] != "✓ Copied"),
        "An Activity completion must not decorate reused Directory cells"
    );
    l.control(&format!("copy:{human}"));
    l.control("dir_mode:activity");
    l.until(|s| !node(s, "directory_items", &control_id).is_null());
    for invalid_chunk in [false, true] {
        l.control(&control_id);
        let request = f
            .frames(l.ui)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "copy_activity_message")
            .unwrap();
        f.send(l.ui, if invalid_chunk {
            json!({"op":"activity_message","request_id":request["request_id"],"part":1,"message":"out of order","complete":true,"error":"COPY_FAILED: Message unavailable"})
        } else {
            json!({"op":"error","source_op":"copy_activity_message","request_id":request["request_id"],"error":"COPY_FAILED: Message unavailable"})
        });
        let failed = inspect(&f, l.ui);
        assert_eq!(failed["clipboard"], "@alice");
        let status = node(&failed, "directory_items", "directory_summary");
        assert_eq!(status["text"], "Message unavailable");
        assert_eq!(status["tooltip"], "Message unavailable");
        assert_eq!(status["hidden"], false);
        l.pump();
        assert_eq!(inspect(&f, l.ui)["clipboard"], "@alice");
        let query = f
            .frames(l.ui)
            .into_iter()
            .rev()
            .find(|action| action["op"] == "query_directory" || action["op"] == "open_directory")
            .unwrap();
        f.send(l.ui,json!({"op":"directory","request_id":query["request_id"],"query":query["query"],"scope":query["scope"],"harness":query["harness"],"peers":[],"warnings":[]}));
        let warning = node(&inspect(&f, l.ui), "directory_items", "directory_warning");
        assert_eq!(warning["hidden"], false);
        assert_eq!(warning["tooltip"], "Message unavailable");
        l.control(&control_id);
        let success = l.until(|s| s["clipboard"] == message.trim());
        assert_eq!(
            node(&success, "directory_items", "directory_warning")["hidden"],
            true,
            "Only the successful correlated retry settles the copy error"
        );
        assert_eq!(
            node(&success, "directory_items", "directory_summary")["tooltip"],
            ""
        );
        l.control(&format!("copy:{human}"));
        assert_eq!(inspect(&f, l.ui)["clipboard"], "@alice");
    }
    let mut state = f.action(json!({"op":"get_snapshot"})).1;
    state["settings"]["host_hub"]["enabled"] = json!(true);
    state["settings"]["host_hub"]["token"] = json!("fixture-token");
    state["settings"]["host_hub"]["auth_mode"] = json!("token");
    state["settings"]["host_hub"]["invite_command"] =
        json!("cito setup --hub work --url http://127.0.0.1:4291");
    state["settings"]["host_hub"]["lan_url"] = Value::Null;
    state["settings"]["host_hub"]["tailscale_url"] = Value::Null;
    state["settings"]["host_hub"]["lan_invite_command"] = Value::Null;
    state["available"] = json!([{"harness":"fixture","session_id":"reload","title":"Reload bridge","cwd":"/tmp","needs_reload":true}]);
    f.send(l.ui, state);
    l.control("open_settings");
    l.control("settings_tab:hubs");
    for (id, expected) in [
        (
            "copy_hub_invite",
            "cito setup --hub work --url http://127.0.0.1:4291",
        ),
        ("reload:fixture:reload", "/reload"),
    ] {
        l.control(&control_id);
        let request = f
            .frames(l.ui)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "copy_activity_message")
            .unwrap();
        l.control(id);
        f.send(l.ui, json!({"op":"activity_message","request_id":request["request_id"],"part":0,"message":"late full message","complete":true}));
        let copied = inspect(&f, l.ui);
        assert_eq!(copied["clipboard"], expected, "{id}");
        if id == "copy_hub_invite" {
            assert_eq!(
                node(&copied, "settings_items", id)["text"],
                "✓ Invite Copied"
            );
        }
    }
    // Real production actions must emit non-coalesced success receipts, not just passive snapshots.
    l.pump();
    for op in [
        "ack_signals",
        "mark_activity_read",
        "mark_all_activity_read",
        "clear_activity_history",
    ] {
        let request = format!("round4-{op}");
        let (status, result) = f.action(json!({"op":op,"request_id":request,"ack_all":true}));
        assert_eq!(status, 200, "{result}");
        wait(|| {
            l.pump();
            l.frames.iter().any(|frame| {
                frame["op"] == "state"
                    && frame["ok"] == true
                    && frame["request_id"] == request
                    && frame["source_op"] == op
            })
        });
        assert_eq!(
            node(&inspect(&f, l.ui), "directory_items", "directory_warning")["hidden"],
            true
        );
    }
    assert!(f.rows("SELECT signal_id FROM local_signals").is_empty());
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
        assert_eq!(status["hidden"], !configured);
        if configured {
            assert_eq!(status["text"], "● Connected");
        }
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
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "setup_app:pi")["text"],
        "Re-check",
        "A click alone cannot confirm a check"
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
    assert!(page
        .into_string()
        .unwrap()
        .contains("Cito Hub is active on this Mac."));
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
    let (status, result) = client.action(action);
    assert_eq!(status, 200, "{result}");
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
