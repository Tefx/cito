use super::*;

fn open(f: &Fixture, p: usize, cfg: &Value, tab: &str) {
    let mut frame = state();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(f, p, "open_settings");
    control(f, p, &format!("settings_tab:{tab}"));
    inspect(f, p);
}
fn input(f: &Fixture, p: usize, id: &str, value: &str) {
    f.send(p, json!({"op":"test_settings_input","id":id,"value":value}));
}
fn latest(f: &Fixture, p: usize, op: &str) -> Value {
    inspect(f, p);
    f.frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == op)
        .unwrap()
}
fn focus(f: &Fixture, p: usize, id: &str) {
    f.send(p, json!({"op":"test_focus_field","id":id}));
    assert_eq!(inspect(f, p)["field_editors"][0]["id"], id);
}

#[test]
fn bounded_previews_preserve_updates_and_incomplete_passive_collections_keep_caches() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    open(&f, p, &settings(), "hubs");
    let mut frame = state();
    frame["listeners"][0]["cwd"] = json!(format!("/tmp/{}", "long-path/".repeat(20)));
    frame["activity"] = json!([{"hub":"personal","signal_id":"signal","listener_id":"c_abcdefghijkl","from_user":"bob","from_host_name":"laptop","from_title":"Sender","target_harness":"fixture","target_title":"Target","target_scoped_id":"personal:c_abcdefghijkl","unread":true,"for_me":true,"message":"message ".repeat(100),"state":"RECEIVED","created_at":"2026-01-01T00:00:00Z"}]);
    frame["unread_count"] = json!(7);
    frame["hubs"][0]["status"] = json!("disconnected");
    let bounded: Value =
        serde_json::from_slice(&cito_native::core::daemon::bounded_frame(frame.clone()).unwrap())
            .unwrap();
    assert!(bounded.get("truncated").is_none());
    f.send(p, bounded);
    let observed = inspect(&f, p);
    assert_eq!(observed["unread_count"], 7);
    assert_eq!(observed["count"], 1);

    frame["listeners"] = json!([]);
    frame["hubs"] = json!([]);
    frame["activity"] = json!([]);
    frame["truncated"] = json!(true);
    frame["unread_count"] = json!(3);
    f.send(p, frame.clone());
    let passive = inspect(&f, p);
    assert_eq!(passive["count"], 1);
    assert_eq!(passive["unread_count"], 3);

    frame["request_id"] = json!("correlated-stop");
    frame["source_op"] = json!("unlisten_all");
    f.send(p, frame);
    assert_eq!(inspect(&f, p)["count"], 0);
}

#[test]
fn real_host_and_nested_invite_receipts_settle_pending_controls_after_bounding() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let recorder = f.recording_ui();
    f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", recorder.to_str().unwrap()),
    ]);
    let p = start(&f);
    f.send(p, f.action(json!({"op":"get_snapshot"})).1);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    inspect(&f, p);
    for operation in ["set_host_hub", "paste_hub_invite"] {
        if operation == "set_host_hub" {
            control(&f, p, "toggle_host_hub");
        } else {
            f.send(
                p,
                json!({"op":"test_clipboard","text":"http://127.0.0.1:1"}),
            );
            control(&f, p, "paste_hub_invite");
        }
        let request = latest(&f, p, operation);
        let (status, response) = f.action(request.clone());
        assert_eq!(status, 200, "{response}");
        assert_eq!(response["committed"], true);
        let mut receipt = Value::Null;
        wait(|| {
            receipt = fs::read_to_string(f.home.join("ui-frames"))
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|frame| {
                    frame["op"] == "settings"
                        && frame["request_id"] == request["request_id"]
                        && frame["ok"] == true
                })
                .unwrap_or(Value::Null);
            !receipt.is_null()
        });
        assert_eq!(receipt["committed"], true);
        // Nested invite dispatch produces the save operation's real receipt.
        assert_eq!(
            receipt["source_op"],
            if operation == "paste_hub_invite" {
                "setup_save_hub"
            } else {
                operation
            }
        );
        if operation == "paste_hub_invite" {
            receipt["settings"]["hubs"][0]["url"] = json!(format!(
                "https://hub.example.com/{}",
                "x".repeat(cito_native::core::daemon::FRAME_LIMIT)
            ));
        }
        let bounded: Value =
            serde_json::from_slice(&cito_native::core::daemon::bounded_frame(receipt).unwrap())
                .unwrap();
        if operation == "paste_hub_invite" {
            assert!(bounded.get("settings").is_none());
        }
        f.send(p, bounded.clone());
        let settled = inspect(&f, p);
        assert_eq!(settled["pending_controls"], json!([]));
        if operation == "paste_hub_invite" {
            assert_eq!(settled["hub_invite_pending"], false);
            assert_eq!(
                node(&settled, "settings_items", "settings_error_label")["text"],
                bounded["warning"]
            );
        }
    }
}

#[test]
fn url_only_oidc_edit_discovers_destination_metadata_and_preserves_overrides() {
    use cito_native::core::config::{discover_hub_config, merge_hub_config};
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["hubs"][0] = json!({"name":"personal","url":"https://old.example.com","auth_mode":"oidc","enabled":true,"is_default":true,"status":"connected","issuer":"https://old-id.example.com","client_id":"old-client","redirect_port":8400,"is_builtin":false});
    open(&f, p, &cfg, "hubs");
    control(&f, p, "edit_hub:personal");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        assert_eq!(line, "GET /health HTTP/1.1\r\n");
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
        }
        let body = json!({"auth_mode":"oidc","oidc":{"issuer":"https://new-id.example.com","client_id":"new-client","redirect_port":8500}}).to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    input(&f, p, "settings_hub_url", &url);
    control(&f, p, "submit_add_hub");
    let request = latest(&f, p, "setup_save_hub");
    for key in ["issuer", "client_id", "redirect_port"] {
        assert!(request.get(key).is_none(), "{request}");
    }
    let adopted = discover_hub_config(merge_hub_config(&cfg["hubs"][0], &request)).unwrap();
    server.join().unwrap();
    assert_eq!(adopted["issuer"], "https://new-id.example.com");
    assert_eq!(adopted["clientId"], "new-client");
    assert_eq!(adopted["redirectPort"], 8500);
    f.send(p, json!({"op":"error","source_op":"setup_save_hub","request_id":request["request_id"],"error":"Retry"}));
    input(&f, p, "settings_hub_issuer", "https://override.example.com");
    input(&f, p, "settings_hub_client_id", "");
    input(&f, p, "settings_hub_redirect_port", "0");
    control(&f, p, "submit_add_hub");
    let explicit = latest(&f, p, "setup_save_hub");
    assert_eq!(explicit["issuer"], "https://override.example.com");
    assert_eq!(explicit.get("client_id"), Some(&Value::Null));
    assert_eq!(explicit["redirect_port"], 0);
}

#[test]
fn v49_oidc_two_rows_discovery_edit_clear_and_invite_aliases() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["hubs"][0]["is_builtin"] = json!(false);
    cfg["hubs"][0]["auth_mode"] = json!("oidc");
    cfg["hubs"][0]["url"] = json!("https://hub.example.com");
    cfg["hubs"][0]["redirect_port"] = json!(4317);
    cfg["hubs"][0]["issuer"] = json!("https://id.example.com");
    cfg["hubs"][0]["client_id"] = json!("desktop");
    open(&f, p, &cfg, "hubs");
    control(&f, p, "edit_hub:personal");
    let s = inspect(&f, p);
    assert_eq!(node(&s, "settings_items", "add_hub_form")["height"], 298);
    assert_eq!(
        node(&s, "settings_items", "settings_hub_redirect_port")["text"],
        "4317"
    );
    let client = node(&s, "settings_items", "settings_hub_client_id");
    let port = node(&s, "settings_items", "settings_hub_redirect_port");
    assert!(client["width"].as_f64().unwrap() >= 165.0);
    assert!(port["width"].as_f64().unwrap() >= 165.0);
    assert_eq!(client["y"], port["y"]);
    assert_eq!(client["placeholder"], "Auto-discovered");
    assert!(
        node(&s, "settings_items", "settings_hub_issuer")["y"]
            .as_f64()
            .unwrap()
            > client["y"].as_f64().unwrap()
    );
    control(&f, p, "submit_add_hub");
    let saved = latest(&f, p, "setup_save_hub");
    assert_eq!(saved["redirect_port"], 4317);
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "submit_add_hub")["text"],
        "Saving…"
    );
    f.send(p,json!({"op":"settings","source_op":"setup_save_hub","request_id":saved["request_id"],"ok":false,"error":"Try again"}));
    for id in [
        "settings_hub_issuer",
        "settings_hub_client_id",
        "settings_hub_redirect_port",
    ] {
        input(&f, p, id, "");
    }
    control(&f, p, "submit_add_hub");
    let cleared = latest(&f, p, "setup_save_hub");
    let empty = inspect(&f, p);
    let client = node(&empty, "settings_items", "settings_hub_client_id");
    assert_eq!(client["text"], "");
    assert!(client["label_measured_width"].as_f64().unwrap() <= 110.0);
    for key in ["issuer", "client_id", "redirect_port"] {
        assert_eq!(cleared.get(key), Some(&Value::Null));
    }
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["pending_hub_save"], "");
    control(&f, p, "open_add_hub");
    control(&f, p, "settings_hub_auth:oidc");
    input(&f, p, "settings_hub_url", "https://hub.example.com");
    control(&f, p, "submit_add_hub");
    let discovered = latest(&f, p, "setup_save_hub");
    assert!(discovered.get("issuer").is_none());
    assert!(discovered.get("client_id").is_none());
    control(&f, p, "cancel_add_hub");
    control(&f, p, "open_add_hub");
    for alias in ["redirect_port", "redirectPort", "oidcRedirectPort"] {
        input(
            &f,
            p,
            "settings_hub_url",
            "cito setup --url https://hub.example.com",
        );
        let request = latest(&f, p, "paste_hub_invite");
        let mut config =
            json!({"url":"https://hub.example.com","auth_mode":"oidc","clientId":"desktop"});
        config[alias] = json!(4319);
        f.send(
            p,
            json!({"op":"hub_invite","request_id":request["request_id"],"config":config}),
        );
        let s = inspect(&f, p);
        assert_eq!(
            node(&s, "settings_items", "settings_hub_redirect_port")["text"],
            "4319"
        );
        assert_eq!(
            node(&s, "settings_items", "settings_hub_client_id")["text"],
            "desktop"
        );
    }
}

#[test]
fn v49_focused_hub_return_receipts_errors_and_dismissal_are_immediate() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    open(&f, p, &settings(), "hubs");
    for receipt in ["success", "error", "receipt-only", "authority"] {
        control(&f, p, "open_add_hub");
        input(&f, p, "settings_hub_url", "https://hub.example.com");
        focus(&f, p, "settings_hub_url");
        f.send(p, json!({"op":"test_settings_key","key":"return"}));
        let request = latest(&f, p, "setup_save_hub");
        assert!(
            request.get("auth_mode").is_none(),
            "URL-only joins must permit discovery"
        );
        assert_eq!(
            node(&inspect(&f, p), "settings_items", "submit_add_hub")["text"],
            "Adding…"
        );
        let mut result = json!({"op":"settings","source_op":"setup_save_hub","request_id":request["request_id"],"ok":true});
        match receipt {
            "success" => result["settings"] = settings(),
            "error" => {
                result["op"] = json!("error");
                result["error"] = json!("Save rejected");
            }
            "authority" => {
                result["authority_changed"] = json!(true);
                result["warning"] = json!("Hub changed");
            }
            _ => {
                result["committed"] = json!(true);
                result["warning"] = json!("Saved; refresh unavailable");
            }
        }
        f.send(p, result);
        let s = inspect(&f, p);
        assert_eq!(s["pending_hub_save"], "");
        assert_eq!(s["settings_rebuild_deferred"], false);
        if receipt == "error" {
            assert_eq!(
                node(&s, "settings_items", "add_hub_error")["text"],
                "Save rejected"
            );
        }
        if receipt == "authority" {
            assert_eq!(
                node(&s, "settings_items", "settings_error_label")["text"],
                "Hub changed"
            );
        }
        control(&f, p, "cancel_add_hub");
    }
}

#[test]
fn v49_passive_editor_refresh_preserves_undo_and_ime_until_end_editing() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    open(&f, p, &cfg, "hubs");
    control(&f, p, "open_add_hub");
    focus(&f, p, "settings_hub_name");
    f.send(p, json!({"op":"test_clipboard","text":"draft"}));
    f.send(p, json!({"op":"test_settings_key","key":"command-v"}));
    let before = inspect(&f, p);
    assert_eq!(before["field_editors"][0]["can_undo"], true);
    cfg["launch_at_login"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let passive = inspect(&f, p);
    assert_eq!(passive["settings_rebuild_deferred"], true);
    assert_eq!(passive["field_editors"], before["field_editors"]);
    f.send(p, json!({"op":"test_settings_key","key":"command-z"}));
    assert_eq!(inspect(&f, p)["settings_rebuild_deferred"], true);
    f.send(p, json!({"op":"test_editor_marked","value":"composition"}));
    cfg["launch_at_login"] = json!(false);
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(inspect(&f, p)["field_editors"][0]["marked"], true);
    f.send(p, json!({"op":"test_end_editing"}));
    let ended = inspect(&f, p);
    assert_eq!(ended["settings_rebuild_deferred"], false);
    assert_eq!(ended["settings_launch_at_login"], false);
}

#[test]
fn v49_focused_port_and_channel_receipts_settle_without_hub() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    cfg["host_hub"]["enabled"] = json!(true);
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    open(&f, p, &cfg, "hubs");
    input(&f, p, "host_hub_port", "4501");
    focus(&f, p, "host_hub_port");
    f.send(p, json!({"op":"test_settings_key","key":"return"}));
    let request = latest(&f, p, "set_host_hub");
    assert_eq!(
        node(&inspect(&f, p), "settings_items", "apply_host_hub_port")["text"],
        "Applying…"
    );
    f.send(p,json!({"op":"settings","source_op":"set_host_hub","request_id":request["request_id"],"committed":true,"warning":"Port saved"}));
    assert_eq!(inspect(&f, p)["host_hub_port_pending"], false);
    control(&f, p, "settings_tab:channels");
    for (button, operation, label) in [
        ("save_channel:channel", "channel_save", "Saving…"),
        ("test_channel:channel", "channel_test", "Testing…"),
        ("pair_channel:channel", "channel_pair", "Pairing…"),
    ] {
        focus(&f, p, "channel_field:channel:account");
        control(&f, p, button);
        let request = latest(&f, p, operation);
        let pending = inspect(&f, p);
        assert_eq!(node(&pending, "settings_items", button)["text"], label);
        assert_eq!(node(&pending, "settings_items", button)["enabled"], false);
        let op = if operation == "channel_test" {
            "channel_test_result"
        } else if operation == "channel_pair" {
            "pair_code"
        } else {
            "settings"
        };
        f.send(p,json!({"op":op,"source_op":operation,"request_id":request["request_id"],"channel_id":"channel","ok":true,"settings":cfg,"instruction":"Pair account","detail":"Sent"}));
        let settled = inspect(&f, p);
        assert!(!settled["pending_controls"]
            .as_array()
            .unwrap()
            .contains(&request["request_id"]));
        assert_eq!(settled["settings_rebuild_deferred"], false);
    }
}

#[test]
fn v49_payload_free_failures_settle_app_and_invite_and_truncated_frames_keep_cache() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut cfg = settings();
    cfg["apps"] = json!([{"id":"fixture","display_name":"Fixture","tier":"stable","installed":true,"configured":true,"status":"configured","detail":"Connected","paths":[]}]);
    open(&f, p, &cfg, "apps");
    control(&f, p, "setup_app:fixture");
    let request = latest(&f, p, "setup_apply");
    f.send(p,json!({"op":"settings","source_op":"setup_apply","request_id":request["request_id"],"ok":false,"warning":"Check failed"}));
    let s = inspect(&f, p);
    assert_eq!(s["pending_app_checks"], json!([]));
    assert_eq!(s["pending_controls"], json!([]));
    assert_eq!(
        node(&s, "settings_items", "settings_error_label")["text"],
        "Check failed"
    );
    control(&f, p, "settings_tab:hubs");
    f.send(
        p,
        json!({"op":"test_clipboard","text":"https://hub.example.com"}),
    );
    control(&f, p, "paste_hub_invite");
    let request = latest(&f, p, "paste_hub_invite");
    f.send(p,json!({"op":"settings","source_op":"paste_hub_invite","request_id":request["request_id"],"ok":false,"error":"Invite failed"}));
    assert_eq!(inspect(&f, p)["hub_invite_pending"], false);
    let before = inspect(&f, p);
    cfg["hubs"] = json!([]);
    cfg["apps"] = json!([]);
    f.send(p, json!({"op":"settings","truncated":true,"settings":cfg}));
    let mut truncated = state();
    truncated["truncated"] = json!(true);
    truncated["hubs"] = json!([]);
    truncated["listeners"] = json!([]);
    truncated["settings"] = cfg;
    f.send(p, truncated);
    let after = inspect(&f, p);
    assert_eq!(after["count"], before["count"]);
    assert_eq!(
        node(&after, "settings_items", "settings_hub:personal"),
        node(&before, "settings_items", "settings_hub:personal")
    );
    assert!(!node(&after, "settings_items", "settings_hub:personal").is_null());
}
