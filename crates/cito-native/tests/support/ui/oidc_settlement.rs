use super::settlement::{input, latest, open_settings as open};
use super::*;

#[test]
fn payload_free_save_errors_have_only_the_form_as_their_owner() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    open(&f, p, &settings(), "hubs");
    control(&f, p, "open_add_hub");
    input(&f, p, "settings_hub_url", "https://hub.example.com");
    control(&f, p, "submit_add_hub");
    let request = latest(&f, p, "setup_save_hub");
    f.send(p, json!({"op":"settings","source_op":"setup_save_hub","request_id":request["request_id"],"ok":false,"warning":"Save failed"}));
    let s = inspect(&f, p);
    assert_eq!(s.settings("add_hub_error")["text"], "Save failed");
    assert!(s.settings("settings_error_banner").is_null());
    assert!(s.menu("menu_error_row").is_null());
}

fn focus(f: &Fixture, p: usize, id: &str) {
    f.send(p, json!({"op":"test_focus_field","id":id}));
    assert_eq!(inspect(f, p)["field_editors"][0]["id"], id);
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
    control(&f, p, "submit_add_hub");
    let saved = latest(&f, p, "setup_save_hub");
    assert_eq!(saved["redirect_port"], 4317);
    assert_eq!(inspect(&f, p).settings("submit_add_hub")["text"], "Saving…");
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
    assert!(discovered.get("issuer").is_none());
    assert!(discovered.get("client_id").is_none());
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
    }
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
fn passive_bounded_hubs_preserve_complete_nested_attachments_and_update_scalars() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    frame["hubs"][0]["attachments"] = json!((0..129).map(|i| json!({"channel_id":format!("device-{i}"),"location":"device","display_name":format!("Device {i}"),"availability":"ACTIVE","is_local":false})).collect::<Vec<_>>());
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p)["hub_attachment_counts"]["personal"], 129);
    frame["unread_count"] = json!(9);
    frame["hubs"][0]["status"] = json!("disconnected");
    // A normal 128-row preview fits on the wire. Force byte-budget shedding
    // of the second Hub so the retained first row is a partial Hub projection.
    let preview: Value =
        serde_json::from_slice(&cito_native::core::daemon::bounded_frame(frame.clone()).unwrap())
            .unwrap();
    assert_eq!(
        preview["hubs"][0]["attachments"].as_array().unwrap().len(),
        128
    );
    assert!(preview.get("truncated").is_none());
    frame["hubs"][1]["attachments"] = json!((0..128).map(|i| json!({"channel_id":format!("remote-{i}"),"location":"device","display_name":"x".repeat(2048),"availability":"ACTIVE","is_local":false})).collect::<Vec<_>>());
    let bounded: Value =
        serde_json::from_slice(&cito_native::core::daemon::bounded_frame(frame).unwrap()).unwrap();
    assert_eq!(
        bounded["hubs"][0]["attachments"].as_array().unwrap().len(),
        128
    );
    assert_eq!(bounded["truncated"], true);
    assert_eq!(bounded["hubs"].as_array().unwrap().len(), 1);
    assert!(bounded["truncated_collections"]
        .as_array()
        .unwrap()
        .contains(&json!("hubs")));
    f.send(p, bounded);
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
