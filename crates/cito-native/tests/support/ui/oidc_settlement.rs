use super::settlement::{input, latest, open_settings as open};
use super::*;

#[test]
fn payload_free_save_errors_have_only_the_form_as_their_owner() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["host_hub"]["enabled"] = json!(true);
    open(&f, p, &cfg, "hubs");
    control(&f, p, "open_add_hub");
    control(&f, p, "settings_hub_auth:oidc");
    assert_eq!(inspect(&f, p).settings("add_hub_form")["height"], 252);
    input(&f, p, "settings_hub_url", "https://hub.example.com");
    control(&f, p, "submit_add_hub");
    f.send(p, json!({"op":"test_settings_scroll","offset":10000}));
    let request = latest(&f, p, "setup_save_hub");
    f.send(p, json!({"op":"settings","source_op":"setup_save_hub","request_id":request["request_id"],"ok":false,"warning":"Save failed"}));
    let s = inspect(&f, p);
    let error = s.settings("add_hub_error");
    assert_eq!(error["text"], "Save failed");
    assert_eq!(
        s.settings("add_hub_form")["height"].as_f64().unwrap(),
        298.0 + error["height"].as_f64().unwrap() + 6.0
    );
    assert_eq!(s.settings("settings_hub_issuer")["text"], "");
    assert!(
        s.settings("settings_hubs_tab")["content_y"]
            .as_f64()
            .unwrap()
            <= s.settings("add_hub_form")["y"].as_f64().unwrap()
    );
    assert!(s.settings("settings_error_banner").is_null());
    assert!(s.menu("menu_error_row").is_null());
}

fn focus(f: &Fixture, p: usize, id: &str) {
    f.send(p, json!({"op":"test_focus_field","id":id}));
    assert_eq!(inspect(f, p)["field_editors"][0]["id"], id);
}

fn assert_same_active_editor(before: &Value, after: &Value, field: &str, scroll: &str) {
    let editor = &before["field_editors"][0];
    assert_eq!(editor["id"], field);
    assert_eq!(editor["attached"], true);
    for key in [
        "instance",
        "id",
        "attached",
        "marked",
        "can_undo",
        "selection_location",
        "selection_length",
    ] {
        assert_eq!(
            after["field_editors"][0][key], editor[key],
            "{field}: {key}"
        );
    }
    assert_eq!(
        after.settings(field)["instance"],
        before.settings(field)["instance"]
    );
    for key in ["content_x", "content_y"] {
        assert_eq!(
            after.settings(scroll)[key],
            before.settings(scroll)[key],
            "{scroll}: {key}"
        );
    }
}

#[test]
fn delayed_hub_save_failure_preserves_the_active_draft_editor() {
    let _lock = serial();
    for marked in [false, true] {
        let (f, p) = ui_fixture();
        let mut cfg = settings();
        cfg["host_hub"]["enabled"] = json!(true);
        open(&f, p, &cfg, "hubs");
        control(&f, p, "open_add_hub");
        control(&f, p, "settings_hub_auth:oidc");
        let field = "settings_hub_url";
        input(&f, p, field, "https://hub.example.com");
        control(&f, p, "submit_add_hub");
        let request = latest(&f, p, "setup_save_hub");
        focus(&f, p, field);
        f.send(
            p,
            json!({"op":"test_clipboard","text":"https://draft.example.com"}),
        );
        f.send(p, json!({"op":"test_settings_key","key":"command-v"}));
        if marked {
            f.send(p, json!({"op":"test_editor_marked","value":"/draft"}));
        }
        f.send(p, json!({"op":"test_settings_scroll","offset":0}));
        let before = inspect(&f, p);
        assert_eq!(before["field_editors"][0]["marked"], marked);
        assert_eq!(before["field_editors"][0]["can_undo"], true);
        assert_eq!(before["pending_hub_save"], request["request_id"]);
        assert!(before.settings("add_hub_error").is_null());

        let mut failure = json!({"op":"settings","source_op":"setup_save_hub","request_id":request["request_id"],"ok":false,"error":"Save failed"});
        if marked {
            failure["settings"] = cfg;
        }
        f.send(p, failure);
        let after = inspect(&f, p);
        let scroll = "settings_hubs_tab";
        assert_same_active_editor(&before, &after, field, scroll);
        assert_eq!(after["pending_hub_save"], "");
        assert_eq!(after.settings("submit_add_hub")["enabled"], true);
        let error = after.settings("add_hub_error");
        assert_eq!(error["text"], "Save failed");
        assert_eq!(error["hidden"], false);
        let error_y =
            after.settings("add_hub_form")["y"].as_f64().unwrap() + error["y"].as_f64().unwrap();
        let viewport = after.settings(scroll);
        let bottom = viewport["content_y"].as_f64().unwrap();
        assert!(
            error_y >= bottom
                && error_y + error["height"].as_f64().unwrap()
                    <= bottom + viewport["height"].as_f64().unwrap()
        );
        assert!(after.settings("settings_error_banner").is_null());
        if !marked {
            f.send(p, json!({"op":"test_settings_key","key":"command-z"}));
            assert_eq!(
                inspect(&f, p).settings(field)["text"],
                "https://hub.example.com"
            );
        }

        f.send(p, json!({"op":"test_end_editing"}));
        let ended = inspect(&f, p);
        assert_eq!(ended["settings_rebuild_deferred"], false);
        assert_eq!(ended.settings("add_hub_error")["text"], "Save failed");
        assert_eq!(ended.settings("settings_hub_issuer")["text"], "");
    }
}

#[test]
fn delayed_sibling_save_failures_keep_the_active_editor() {
    let _lock = serial();
    for marked in [false, true] {
        for (tab, field, save, op, draft, error_id) in [
            (
                "hubs",
                "host_hub_port",
                "apply_host_hub_port",
                "set_host_hub",
                "5302",
                "host_hub_error",
            ),
            (
                "channels",
                "channel_field:channel:account",
                "save_channel:channel",
                "channel_save",
                "draft",
                "settings_error_label",
            ),
        ] {
            let (f, p) = ui_fixture();
            let mut cfg = settings();
            cfg["host_hub"]["enabled"] = json!(true);
            open(&f, p, &cfg, tab);
            if tab == "hubs" {
                input(&f, p, field, "5301");
            }
            control(&f, p, save);
            let request = latest(&f, p, op);
            focus(&f, p, field);
            f.send(p, json!({"op":"test_clipboard","text":draft}));
            f.send(p, json!({"op":"test_settings_key","key":"command-v"}));
            if marked {
                f.send(p, json!({"op":"test_editor_marked","value":"3"}));
            }
            let before = inspect(&f, p);
            assert_eq!(before["field_editors"][0]["marked"], marked);
            assert_eq!(before["field_editors"][0]["can_undo"], true);
            assert!(!before["pending_controls"].as_array().unwrap().is_empty());
            let mut failure = json!({"op":"settings","source_op":op,"request_id":request["request_id"],"ok":false,"error":"Save failed"});
            if tab == "hubs" && marked {
                cfg["host_hub"]["error"] = json!("Save failed");
                failure["settings"] = cfg;
            }
            f.send(p, failure);
            let after = inspect(&f, p);
            assert_same_active_editor(&before, &after, field, &format!("settings_{tab}_tab"));
            assert_eq!(after["pending_controls"], json!([]));
            assert_eq!(after.settings(save)["enabled"], true);
            assert_eq!(after.settings(error_id)["text"], "Save failed");
        }
    }
}

#[test]
fn delayed_save_failures_keep_the_refocused_unchanged_editor() {
    let _lock = serial();
    for (tab, field, save, op, value, error_id, full) in [
        (
            "hubs",
            "settings_hub_url",
            "submit_add_hub",
            "setup_save_hub",
            "https://hub.example.com",
            "add_hub_error",
            false,
        ),
        (
            "hubs",
            "settings_hub_url",
            "submit_add_hub",
            "setup_save_hub",
            "https://hub.example.com",
            "add_hub_error",
            true,
        ),
        (
            "channels",
            "channel_field:channel:account",
            "save_channel:channel",
            "channel_save",
            "alice",
            "settings_error_label",
            true,
        ),
    ] {
        let (f, p) = ui_fixture();
        let mut cfg = settings();
        cfg["host_hub"]["enabled"] = json!(true);
        if tab == "channels" {
            let mut second = cfg["channels"][0].clone();
            second["id"] = json!("second");
            cfg["channels"].as_array_mut().unwrap().push(second);
        }
        open(&f, p, &cfg, tab);
        if tab == "hubs" {
            control(&f, p, "open_add_hub");
            control(&f, p, "settings_hub_auth:oidc");
        }
        input(&f, p, field, value);
        focus(&f, p, field);
        control(&f, p, save);
        let request = latest(&f, p, op);
        if tab == "channels" {
            f.send(p, json!({"op":"test_end_editing"}));
        }
        assert_eq!(inspect(&f, p)["field_editors"], json!([]));
        focus(&f, p, field);
        let offset = if tab == "hubs" { 12 } else { 10_000 };
        f.send(p, json!({"op":"test_settings_scroll","offset":offset}));
        let before = inspect(&f, p);
        let scroll = format!("settings_{tab}_tab");
        assert!(before.settings(&scroll)["content_y"].as_f64().unwrap() > 0.0);
        assert_eq!(before["field_editors"][0]["marked"], false);
        assert_eq!(before.settings(field)["text"], value);
        assert_eq!(before.settings(save)["enabled"], false);
        assert!(before.settings(error_id).is_null());

        let mut failure = json!({"op":"settings","source_op":op,"request_id":request["request_id"],"ok":false,"error":"Save failed"});
        if full {
            failure["settings"] = cfg;
        }
        if tab == "channels" {
            failure["hub"] = request["hub"].clone();
            failure["channel_id"] = request["channel_id"].clone();
        }
        f.send(p, failure);
        let after = inspect(&f, p);
        assert_same_active_editor(&before, &after, field, &scroll);
        assert_eq!(after.settings(field)["text"], value);
        assert_eq!(after["pending_hub_save"], "");
        assert_eq!(after["pending_controls"], json!([]));
        assert_eq!(
            after.settings(save)["instance"],
            before.settings(save)["instance"]
        );
        assert_eq!(after.settings(save)["enabled"], true);
        let error = after.settings(error_id);
        assert_eq!(error["text"], "Save failed");
        assert_eq!(error["hidden"], false);
        if tab == "hubs" {
            let error_y = after.settings("add_hub_form")["y"].as_f64().unwrap()
                + error["y"].as_f64().unwrap();
            let viewport = after.settings(&scroll);
            let bottom = viewport["content_y"].as_f64().unwrap();
            assert!(error_y >= bottom);
            assert!(
                error_y + error["height"].as_f64().unwrap()
                    <= bottom + viewport["height"].as_f64().unwrap()
            );
        }
    }
}

#[test]
fn real_host_and_invite_receipts_settle_the_initiating_request() {
    let _lock = serial();
    let f = Fixture::new(json!({"host_hub":{"bind":"127.0.0.1","port":port()}}));
    f.recording_daemon();
    let p = start(&f);
    f.send(p, f.snapshot());
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
        // Nested invite dispatch preserves the initiating operation in its receipt.
        assert_eq!(receipt["source_op"], operation);
        f.send(p, receipt);
        let settled = inspect(&f, p);
        assert_eq!(settled["pending_controls"], json!([]));
        if operation == "paste_hub_invite" {
            assert_eq!(settled["hub_invite_pending"], false);
        }
    }
}

#[test]
fn url_only_oidc_edit_discovers_destination_metadata_and_preserves_overrides() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    let _lock = serial();
    let f = Fixture::new(
        json!({"hubs":{"personal":{"url":"https://old.example.com","authMode":"oidc","issuer":"https://old-id.example.com","clientId":"old-client","redirectPort":8400}}}),
    );
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
    input(&f, p, "settings_hub_redirect_port", " 8400 ");
    control(&f, p, "submit_add_hub");
    let request = latest(&f, p, "setup_save_hub");
    for key in ["issuer", "client_id", "redirect_port"] {
        assert!(request.get(key).is_none(), "{request}");
    }
    f.daemon(&[]);
    let (status, response) = f.action(request.clone());
    assert_eq!(status, 200, "{response}");
    server.join().unwrap();
    let saved: Value = read_json(&f.config);
    let adopted = &saved["hubs"]["personal"];
    assert_eq!(adopted["issuer"], "https://new-id.example.com");
    assert_eq!(adopted["clientId"], "new-client");
    assert_eq!(adopted["redirectPort"], 8500);
    f.send(p, json!({"op":"error","source_op":"setup_save_hub","request_id":request["request_id"],"error":"Retry"}));
    input(&f, p, "settings_hub_issuer", "https://override.example.com");
    input(&f, p, "settings_hub_client_id", "");
    input(&f, p, "settings_hub_redirect_port", " 0 ");
    control(&f, p, "submit_add_hub");
    let explicit = latest(&f, p, "setup_save_hub");
    assert_eq!(explicit["issuer"], "https://override.example.com");
    assert_eq!(explicit.get("client_id"), Some(&Value::Null));
    assert_eq!(explicit["redirect_port"], 0);
}

#[test]
fn v49_oidc_two_rows_discovery_edit_clear_and_invite_aliases() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["hubs"][0]["is_builtin"] = json!(false);
    cfg["hubs"][0]["auth_mode"] = json!("oidc");
    cfg["hubs"][0]["url"] = json!("https://hub.example.com");
    cfg["hubs"][0]["redirect_port"] = json!(4317);
    cfg["hubs"][0]["issuer"] = json!("https://id.example.com");
    cfg["hubs"][0]["client_id"] = json!("desktop");
    open(&f, p, &cfg, "hubs");
    control(&f, p, "edit_hub:personal");
    let collapsed = inspect(&f, p);
    assert_eq!(collapsed.settings("add_hub_form")["height"], 252);
    let summary = collapsed.settings("hub_oidc_discovery_summary");
    assert_eq!(summary["text"], "OIDC: id.example.com · Auto-discovered");
    assert_eq!(
        summary["tooltip"],
        "Issuer: https://id.example.com\nClient ID: desktop\nRedirect port: 4317"
    );
    assert!(collapsed.settings("settings_hub_issuer").is_null());
    control(&f, p, "manual_hub_oidc");
    let s = inspect(&f, p);
    assert_eq!(s.settings("add_hub_form")["height"], 298);
    assert_eq!(s.settings("settings_hub_redirect_port")["text"], "4317");
    let client = s.settings("settings_hub_client_id");
    let port = s.settings("settings_hub_redirect_port");
    assert!(client["width"].as_f64().unwrap() >= 165.0);
    assert!(port["width"].as_f64().unwrap() >= 165.0);
    assert_eq!(client["y"], port["y"]);
    assert_eq!(client["placeholder"], "Auto-discovered");
    assert!(
        s.settings("settings_hub_issuer")["y"].as_f64().unwrap() > client["y"].as_f64().unwrap()
    );
    control(&f, p, "manual_hub_oidc");
    assert_eq!(inspect(&f, p).settings("add_hub_form")["height"], 252);
    control(&f, p, "submit_add_hub");
    let saved = latest(&f, p, "setup_save_hub");
    assert_eq!(saved["redirect_port"], 4317);
    assert_eq!(inspect(&f, p).settings("submit_add_hub")["text"], "Saving…");
    f.send(p,json!({"op":"settings","source_op":"setup_save_hub","request_id":saved["request_id"],"ok":false,"settings":cfg,"error":"Try again"}));
    let failed = inspect(&f, p);
    assert_eq!(failed.settings("add_hub_error")["text"], "Try again");
    assert_eq!(
        failed.settings("settings_hub_redirect_port")["text"],
        "4317"
    );
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
    let client = empty.settings("settings_hub_client_id");
    assert_eq!(client["text"], "");
    assert!(
        client["label_measured_width"].as_f64().unwrap() <= client["label_width"].as_f64().unwrap()
    );
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
    assert_eq!(inspect(&f, p).settings("add_hub_form")["height"], 252);
    assert!(discovered.get("issuer").is_none());
    assert!(discovered.get("client_id").is_none());
    assert!(discovered.get("redirect_port").is_none());
    control(&f, p, "cancel_add_hub");
    control(&f, p, "open_add_hub");
    for (alias, port_value) in ["redirect_port", "redirectPort", "oidcRedirectPort"]
        .into_iter()
        .flat_map(|alias| [(alias, json!(4319)), (alias, json!("4319"))])
    {
        input(
            &f,
            p,
            "settings_hub_url",
            "cito setup --url https://hub.example.com",
        );
        let request = latest(&f, p, "paste_hub_invite");
        let mut config =
            json!({"url":"https://hub.example.com","auth_mode":"oidc","clientId":"desktop"});
        config[alias] = port_value;
        f.send(
            p,
            json!({"op":"hub_invite","request_id":request["request_id"],"config":config}),
        );
        let s = inspect(&f, p);
        assert_eq!(s.settings("settings_hub_redirect_port")["text"], "4319");
        assert_eq!(s.settings("settings_hub_client_id")["text"], "desktop");
        assert_eq!(s.settings("add_hub_form")["height"], 298);
    }
    // Drafts keep their fields visible even when the manual disclosure flag is off.
    input(&f, p, "settings_hub_redirect_port", "65536");
    control(&f, p, "manual_hub_oidc");
    assert_eq!(inspect(&f, p).settings("add_hub_form")["height"], 298);
    control(&f, p, "submit_add_hub");
    assert_eq!(
        inspect(&f, p).settings("add_hub_error")["text"],
        "Redirect port must be an integer from 0 to 65535."
    );
    assert_eq!(latest(&f, p, "setup_save_hub"), discovered);
}

#[test]
fn v49_passive_editor_refresh_preserves_undo_and_ime_until_end_editing() {
    let _lock = serial();
    let (f, p) = ui_fixture();
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
    assert_eq!(passive["settings_rebuild_deferred"], false);
    assert_eq!(
        passive.settings("settings_launch_at_login")["accessibility_value"],
        1
    );
    assert_eq!(passive["field_editors"], before["field_editors"]);
    f.send(p, json!({"op":"test_settings_key","key":"command-z"}));
    assert_eq!(inspect(&f, p).settings("settings_hub_name")["text"], "");
    f.send(p, json!({"op":"test_editor_marked","value":"composition"}));
    cfg["launch_at_login"] = json!(false);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let composing = inspect(&f, p);
    assert_eq!(composing["field_editors"][0]["marked"], true);
    assert_eq!(composing["settings_rebuild_deferred"], false);
    assert_eq!(
        composing.settings("settings_launch_at_login")["accessibility_value"],
        0
    );
    f.send(p, json!({"op":"test_end_editing"}));
    let ended = inspect(&f, p);
    assert_eq!(ended["settings_rebuild_deferred"], false);
    assert_eq!(ended["settings_launch_at_login"], false);
}

#[test]
fn legacy_partial_hubs_preserve_complete_nested_attachments_and_update_scalars() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    frame["hubs"][0]["attachments"] = json!((0..129).map(|i| json!({"channel_id":format!("device-{i}"),"location":"device","display_name":format!("Device {i}"),"availability":"ACTIVE","is_local":false})).collect::<Vec<_>>());
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p)["hub_attachment_counts"]["personal"], 129);
    frame["unread_count"] = json!(9);
    frame["hubs"][0]["status"] = json!("disconnected");
    frame["hubs"].as_array_mut().unwrap().truncate(1);
    frame["hubs"][0]["attachments"]
        .as_array_mut()
        .unwrap()
        .pop();
    frame["truncated_collections"] = json!(["hubs"]);
    f.send(p, frame);
    let s = inspect(&f, p);
    assert_eq!(s["hub_attachment_counts"]["personal"], 129);
    assert_eq!(s["unread_count"], 9);
    assert!(s["items"].to_string().contains("Offline"));
}

#[test]
fn oidc_edit_keeps_newer_overrides_when_discovery_arrives_after_the_receipt() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["hubs"][0] = json!({"name":"personal","url":"https://old.example.com","auth_mode":"oidc","enabled":true,"is_default":true,"status":"connected","issuer":"https://old-id.example.com","client_id":"old-client","redirect_port":8400});
    open(&f, p, &cfg, "hubs");
    control(&f, p, "edit_hub:personal");
    input(&f, p, "settings_hub_url", "https://new.example.com");
    control(&f, p, "submit_add_hub");
    let first = latest(&f, p, "setup_save_hub");
    input(&f, p, "settings_hub_issuer", "https://override.example.com");
    f.send(p, json!({"op":"settings","source_op":"setup_save_hub","request_id":first["request_id"],"ok":true,"committed":true}));
    cfg["hubs"][0]["url"] = first["url"].clone();
    cfg["hubs"][0]["issuer"] = json!("https://new-id.example.com");
    cfg["hubs"][0]["client_id"] = json!("new-client");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let form = inspect(&f, p);
    assert_eq!(
        form.settings("settings_hub_client_id")["text"],
        "new-client"
    );
    input(&f, p, "settings_hub_url", "https://old.example.com");
    control(&f, p, "submit_add_hub");
    let second = latest(&f, p, "setup_save_hub");
    assert_eq!(second["original_name"], "personal");
    assert_eq!(second["issuer"], "https://override.example.com");
    assert!(second.get("client_id").is_none());
    assert!(second.get("redirect_port").is_none());
}

#[test]
fn delayed_inventory_error_settles_origin_without_leaking_into_selected_hub() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["settings_hub"] = json!("personal");
    open(&f, p, &cfg, "channels");
    control(&f, p, "save_channel:channel");
    let request = latest(&f, p, "channel_save");
    control(&f, p, "channel_hub_selector:work");
    cfg["settings_hub"] = json!("work");
    cfg["channel_inventory_error"] = json!("Work inventory unavailable");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        inspect(&f, p).settings("channel_inventory_error")["text"],
        "Work inventory unavailable"
    );
    cfg["settings_hub"] = json!("personal");
    cfg["channel_inventory_error"] = json!("Personal inventory unavailable");
    f.send(p,json!({"op":"settings","ok":true,"source_op":"channel_save","request_id":request["request_id"],"hub":"personal","channel_id":"channel","settings":cfg}));
    let s = inspect(&f, p);
    assert_eq!(s["pending_controls"], json!([]));
    assert_eq!(
        s.settings("channel_inventory_error")["text"],
        "Work inventory unavailable"
    );
    control(&f, p, "channel_hub_selector:personal");
    assert_eq!(
        inspect(&f, p).settings("channel_inventory_error")["text"],
        "Personal inventory unavailable"
    );
    control(&f, p, "channel_hub_selector:work");
    cfg["settings_hub"] = json!("work");
    cfg["channel_inventory_error"] = Value::Null;
    cfg["channels"][0]["can_configure_hub"] = json!(false);
    cfg["channels"][0]["hub_ready"] = json!(false);
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert!(!inspect(&f, p).settings("channels_managed_notice").is_null());
    cfg["settings_hub"] = json!("personal");
    cfg["channel_inventory_error"] = json!("Personal inventory unavailable");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let selected = inspect(&f, p);
    assert!(selected.settings("channel_inventory_error").is_null());
    assert!(!selected.settings("channels_managed_notice").is_null());
}
