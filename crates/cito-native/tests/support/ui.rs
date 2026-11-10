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
    for id in [
        "channel_field:channel:address",
        "pair_channel:channel",
        "test_channel:channel",
        "save_channel:channel",
    ] {
        assert_eq!(node(&s, "settings_items", id)["enabled"], false, "{id}");
        assert_eq!(node(&s, "settings_items", id)["alpha"], 0.55);
    }
    assert_eq!(
        node(&s, "settings_items", "skip_channel:channel")["enabled"],
        true
    );
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
    l.until(|s| node(s, "settings_items", "add_hub_error")["text"] == "Hub name is required.");
    l.snapshot("settings", "r3_add_hub_validation.png");
    l.input("settings_hub_name", "INVALID!");
    l.input("settings_hub_url", "local://");
    l.control("submit_add_hub");
    l.until(|s| {
        node(s, "settings_items", "add_hub_error")["text"]
            .as_str()
            .is_some_and(|t| t.contains("Invalid hub name"))
    });
    l.snapshot("settings", "r3_add_hub_daemon_error.png");
    l.input("settings_hub_name", "scratch");
    l.control("submit_add_hub");
    l.until(|s| {
        node(s, "settings_items", "add_hub_form").is_null()
            && !node(s, "settings_items", "settings_hub:scratch").is_null()
    });
    // The same form remains open through validation and daemon errors, and closes
    // only on a real settings/state acknowledgement of the saved Hub.
    l.input("host_hub_port", &port().to_string());
    l.control("apply_host_hub_port");
    l.until(|s| node(s, "settings_items", "apply_host_hub_port")["text"] == "✓ Saved");
    l.control("toggle_host_hub");
    l.until(|s| node(s, "settings_items", "toggle_host_hub")["text"] == "● Sharing");
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = occupied.local_addr().unwrap().port().to_string();
    l.input("host_hub_port", &busy);
    l.control("apply_host_hub_port");
    l.until(|s| !node(s, "settings_items", "settings_error_banner").is_null());
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
    l.until(|s| !node(s, "settings_items", "settings_hub:pasted").is_null());
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
            .is_some_and(|t| t.contains("SELF_SEND_FORBIDDEN"))
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
    for (name, op) in [("INVALID!", "error"), ("scratch", "settings")] {
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
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "stratum_hub:channel")["height"],
        36
    );
    control(&f, p, "edit_hub_service:channel");
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
        for (op, error) in [
            ("hub_action", "OIDC cancelled"),
            ("setup_save_hub", "Invite failed"),
            ("paste_hub_invite", "Invalid invite"),
        ] {
            f.send(
                p,
                json!({"op":"error","source_op":op,"request_id":null,"error":error}),
            );
            let s = inspect(&f, p);
            assert!(!node(&s, "settings_items", "add_hub_form").is_null());
            assert_eq!(
                node(&s, "settings_items", "submit_add_hub")["enabled"],
                false,
                "An unrelated {op} error must not settle the active submission"
            );
            assert!(node(&s, "settings_items", "add_hub_error").is_null());
            assert!(!node(&s, "settings_items", "settings_error_banner").is_null());
            assert_eq!(
                node(&s, "settings_items", "settings_error_label")["text"],
                error
            );
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
    let path = f.dir.join(name);
    f.send(
        p,
        json!({"op":"test_render_png","surface":surface,"path":path}),
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
        } else {
            760
        }
    );
    if surface == "settings" {
        assert_eq!(height, 920);
    }
    assert!(height > 100);
    // Opt-in temporary visual-review artifacts; normal test runs leave no residue.
    if let Some(dir) = std::env::var_os("CITO_TEST_SNAPSHOT_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::copy(&path, std::path::Path::new(&dir).join(name)).unwrap();
    }
}

#[test]
fn u3_u11_visual_states_strata_navigation_labels_and_retina_snapshots() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("local");
    cfg["channels"][0]["status"] = json!("not_configured");
    cfg["channels"][0]["fields"][0]["configured"] = json!(false);
    cfg["channels"][0]["fields"][0]["value"] = json!("");
    cfg["channels"][0]["setup_guide"] = json!([
        "1. Create a gateway credential with your provider.",
        "2. Save it once for every human on this Hub."
    ]);
    cfg["hubs"][0]["name"] = json!("local");
    let mut frame = state();
    frame["settings"] = cfg.clone();
    frame["listeners"] = json!([]);
    frame["available"] =
        json!([{"harness":"fixture","session_id":"s1","title":"Work session","cwd":"/tmp"}]);
    f.send(p, frame.clone());
    let s = inspect(&f, p);
    assert_eq!(node(&s, "items", "human_paused_card")["height"], 52);
    assert_eq!(node(&s, "items", "listen_human")["text"], "Enable");
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
    assert_eq!(
        node(&s, "settings_items", "goto_channels_tab")["text"],
        "Human Channels →"
    );
    assert_eq!(node(&s, "settings_items", "skip_next_step")["text"], "Skip");
    assert_eq!(
        node(&s, "settings_items", "settings_app_tier:cursor")["width"],
        88
    );
    render_png(&f, p, "settings", "settings_apps.png");
    control(&f, p, "toggle_onboarding_guide");
    for tab in ["apps", "channels", "hubs"] {
        control(&f, p, &format!("settings_tab:{tab}"));
        let s = inspect(&f, p);
        let guide = node(&s, "settings_items", "onboarding_banner");
        let viewport = node(&s, "settings_items", &format!("settings_{tab}_tab"));
        assert_eq!(guide["y"], 348);
        assert_eq!(guide["height"], 58);
        assert_eq!(viewport["y"], 44);
        assert_eq!(viewport["height"], 300);
        assert!(
            viewport["y"].as_f64().unwrap() + viewport["height"].as_f64().unwrap()
                < guide["y"].as_f64().unwrap()
        );
    }
    render_png(&f, p, "settings", "settings_guide_open.png");
    control(&f, p, "dismiss_onboarding_guide");
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "onboarding_banner").is_null());
    assert_eq!(
        node(&s, "settings_items", "settings_channel_status:channel")["text"],
        "○ Not Configured"
    );
    let hub = node(&s, "settings_items", "stratum_hub:channel");
    let user = node(&s, "settings_items", "stratum_user:channel");
    assert!(
        hub["y"].as_f64().unwrap() > user["y"].as_f64().unwrap() + user["height"].as_f64().unwrap()
    );
    assert!(nodes(&hub["children"])
        .iter()
        .any(|n| n["id"] == "channel_field:channel:secret"));
    assert!(!nodes(&hub["children"])
        .iter()
        .any(|n| n["id"] == "channel_field:channel:account"));
    assert!(nodes(&user["children"])
        .iter()
        .any(|n| n["id"] == "skip_channel:channel"));
    assert_eq!(
        node(&s, "settings_items", "stratum_hub_title:channel")["text"],
        "1. HUB GATEWAY · ONCE PER HUB"
    );
    assert!(
        node(&s, "settings_items", "stratum_user_title:channel")["text"]
            .as_str()
            .unwrap()
            .contains("@alice")
    );
    render_png(&f, p, "settings", "settings_channels_unconfigured.png");
    f.send(p, json!({"op":"test_settings_scroll","offset":0}));
    render_png(&f, p, "settings", "settings_channels_unconfigured_node.png");
    f.send(p, json!({"op":"test_settings_scroll","offset":100000}));
    let before = f.frames(p).len();
    control(&f, p, "skip_channel:channel");
    assert_eq!(inspect(&f, p)["settings_tab"], "hubs");
    assert_eq!(f.frames(p).len(), before, "Skip must not mutate Hub state");
    control(&f, p, "settings_tab:apps");
    assert!(node(&inspect(&f, p), "settings_items", "apps_next_step_callout").is_null());
    cfg["channels"][0]["status"] = json!("pairing_pending");
    cfg["channels"][0]["fields"][0]["configured"] = json!(true);
    cfg["channels"][0]["pairing_instruction"] =
        json!("Send /start cito-abc123def4 to your account provider.");
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert_eq!(
        node(&s, "settings_items", "stratum_hub_status:channel")["text"],
        "● Hub Gateway Active"
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
    // Non-admin schemas keep ownership explanation but never reveal gateway fields.
    cfg["channels"][0]["hub_provided"] = json!(true);
    cfg["channels"][0]["fields"][0]["hidden"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert!(node(&s, "settings_items", "channel_field:channel:secret").is_null());
    assert!(node(&s, "settings_items", "clear_channel_service:channel").is_null());
    assert_eq!(
        node(&s, "settings_items", "stratum_hub:channel")["height"],
        50
    );
    render_png(&f, p, "settings", "r1_gateway_non_admin.png");
    assert!(
        nodes(&node(&s, "settings_items", "stratum_hub:channel")["children"])
            .iter()
            .any(|n| n["text"]
                .as_str()
                .is_some_and(|s| s.contains("administrator")))
    );
    cfg["host_hub"]["auth_mode"] = json!("token");
    cfg["host_hub"]["token"] = json!("12345678abcdef0123456789");
    cfg["hubs"][1]["auth_mode"] = json!("oidc");
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "settings_tab:hubs");
    let s = inspect(&f, p);
    assert_eq!(node(&s, "settings_items", "toggle_hub:local")["x"], 396);
    assert_eq!(node(&s, "settings_items", "edit_hub:local")["x"], 338);
    assert_eq!(node(&s, "settings_items", "remove_hub:work")["x"], 396);
    assert_eq!(node(&s, "settings_items", "edit_hub:work")["x"], 260);
    assert_eq!(node(&s, "settings_items", "login_hub:work")["x"], 184);
    assert_eq!(
        node(&s, "settings_items", "copy_host_hub_token")["text"],
        "Token: 12345678… Copy"
    );
    render_png(&f, p, "settings", "settings_hubs_token.png");
    control(&f, p, "copy_host_hub_token");
    let s = inspect(&f, p);
    assert_eq!(s["clipboard"], cfg["host_hub"]["token"]);
    assert_eq!(
        node(&s, "settings_items", "copy_host_hub_token")["text"],
        "✓ Token Copied"
    );
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
        "✓ Saved",
        "Apply must retain the original request identity and submitted port across toggles"
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
    control(&f, p, "edit_hub:local");
    let s = inspect(&f, p);
    assert!(
        node(&s, "settings_items", "host_hub_error").is_null(),
        "A cleared daemon error must not linger locally"
    );
    assert_eq!(
        node(&s, "settings_items", "settings_hub_user")["placeholder"],
        "e.g. alice"
    );
    assert_eq!(
        node(&s, "settings_items", "settings_hub_name")["hidden"],
        true
    );
    render_png(&f, p, "settings", "settings_hubs_edit_local.png");
    control(&f, p, "cancel_add_hub");
    control(&f, p, "open_add_hub");
    render_png(&f, p, "settings", "settings_hubs_add_remote.png");
    for mode in ["none", "token", "oidc"] {
        control(&f, p, &format!("settings_hub_auth:{mode}"));
        let s = inspect(&f, p);
        let labels: Vec<_> = nodes(&node(&s, "settings_items", "add_hub_form")["children"])
            .into_iter()
            .filter_map(|n| n["text"].as_str().map(String::from))
            .collect();
        assert!(labels.contains(&"HUB NAME".to_owned()));
        assert!(labels.contains(&"HUB URL (http://ip:4291 or https://host)".to_owned()));
        assert!(labels.contains(&"AUTH:".to_owned()));
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
fn u3_paused_human_card_is_per_hub_and_excludes_offline_targets() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    let mut human = listener();
    human["harness"] = json!("human");
    human["session_id"] = json!("default");
    frame["listeners"] = json!([human]);
    frame["available"] =
        json!([{"harness":"fixture","session_id":"s2","title":"Session","cwd":"/tmp"}]);
    f.send(p, frame.clone());
    assert!(node(&inspect(&f, p), "items", "human_paused_card").is_null());
    control(&f, p, "target_hub:work");
    assert!(!node(&inspect(&f, p), "items", "human_paused_card").is_null());
    frame["hubs"][1]["status"] = json!("disconnected");
    f.send(p, frame.clone());
    assert!(node(&inspect(&f, p), "items", "human_paused_card").is_null());
    frame["listeners"] = json!([]);
    f.send(p, frame.clone());
    assert!(!node(&inspect(&f, p), "items", "human_paused_card").is_null());
    frame["hubs"][0]["status"] = json!("disconnected");
    f.send(p, frame);
    assert!(node(&inspect(&f, p), "items", "human_paused_card").is_null());
}

#[test]
fn u4_u6_guide_is_user_controlled_even_without_apps_and_callout_skip_is_local() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    for app in cfg["apps"].as_array_mut().unwrap() {
        app["configured"] = json!(false);
    }
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    for tab in ["apps", "channels", "hubs"] {
        control(&f, p, &format!("settings_tab:{tab}"));
        assert!(node(&inspect(&f, p), "settings_items", "onboarding_banner").is_null());
        control(&f, p, "toggle_onboarding_guide");
        assert!(!node(&inspect(&f, p), "settings_items", "onboarding_banner").is_null());
        control(&f, p, "dismiss_onboarding_guide");
        assert!(node(&inspect(&f, p), "settings_items", "onboarding_banner").is_null());
    }
    cfg["apps"][0]["configured"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "settings_tab:apps");
    let s = inspect(&f, p);
    assert!(!node(&s, "settings_items", "apps_next_step_callout").is_null());
    assert!(!s.to_string().contains("Mobile Alerts"));
    let before = f.frames(p).len();
    control(&f, p, "skip_next_step");
    assert!(node(&inspect(&f, p), "settings_items", "apps_next_step_callout").is_null());
    assert_eq!(f.frames(p).len(), before);
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
                if ok {
                    "○ Not Configured"
                } else {
                    "⚠ Error"
                }
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
            assert_eq!(
                node(&s, "settings_items", "settings_channel_detail:channel")["text"],
                "Ready",
                "{action} must clear the candidate test result, ok={ok}"
            );
            assert_eq!(
                node(&s, "settings_items", "settings_channel_status:channel")["text"],
                if action == "clear_channel_service" {
                    "Hub Service: ● Ready"
                } else {
                    "○ Not Configured"
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
    let snapshot: Value = serde_json::from_str(
        output
            .lines()
            .rev()
            .find_map(|l| l.strip_prefix("cito-ui: menu "))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(snapshot["truncated_labels"], json!([]));
    assert_eq!(snapshot["overlapping_siblings"], json!([]));
    assert_eq!(snapshot["clipped_views"], json!([]));
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
            "INVALID_INPUT: Message exceeds maximum length"
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
        json!({"op":"error","error":"INVALID_INPUT: UI action exceeds 1 MiB"}),
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
        "↵ Copy Selected ID    ⎋ Close"
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
    let (status, result) = client.action(json!({"op":"test_hub","hub":"lan"}));
    assert_eq!(status, 200, "{result}");
    assert!(result["streams"].is_number());
    let configured: Value =
        serde_json::from_str(&fs::read_to_string(&client.config).unwrap()).unwrap();
    assert_eq!(configured["default_hub"], "lan");
    assert_eq!(configured["hubs"]["lan"]["url"], endpoint);
    assert_eq!(configured["hubs"]["lan"]["authMode"], "token");
    assert_eq!(configured["hubs"]["lan"]["token"], "lan-test-token");
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
