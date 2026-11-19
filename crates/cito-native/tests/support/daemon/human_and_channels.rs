use super::*;

#[test]
fn listen_preflight_preserves_sibling_titles_and_blank_descriptions_use_defaults() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"local":{"url":"local://"},"work":{"url":"local://"}}}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    for hub in ["local", "work"] {
        assert_eq!(
            f.tool(
                "cito_listen",
                json!({"hub":hub,"about":"Original"}),
                meta.clone()
            )["ok"],
            true
        );
    }
    let before =
        f.rows("SELECT hub,title,about FROM local_listeners WHERE harness='fixture' ORDER BY hub");
    let (status,error) = f.action(json!({"op":"listen_session","hub":"local","harness":"fixture","session_id":"session-1","title":"Uncommitted title","scope":"invalid"}));
    assert_eq!(status, 400, "{error}");
    assert_eq!(
        f.rows("SELECT hub,title,about FROM local_listeners WHERE harness='fixture' ORDER BY hub"),
        before
    );
    assert!(f.rows("SELECT * FROM session_custom_titles").is_empty());
    let (status,result) = f.action(json!({"op":"listen_session","hub":"local","harness":"fixture","session_id":"session-1","title":"Useful title","about":"   ","scope":"self"}));
    assert_eq!(status, 200, "{result}");
    assert_eq!(
        f.rows("SELECT about FROM local_listeners WHERE hub='local' AND harness='fixture'")[0]
            ["about"],
        "Useful title"
    );
    let state = f.db("state.sqlite");
    state.execute("CREATE TRIGGER reject_title BEFORE INSERT ON session_custom_titles BEGIN SELECT RAISE(ABORT,'title maintenance unavailable'); END", &[]).unwrap();
    let (status,result)=f.action(json!({"op":"listen_session","hub":"local","harness":"fixture","session_id":"session-1","title":"Failed title maintenance","about":"Committed enrollment","scope":"self"}));
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["ok"], true);
    assert_eq!(result["committed"], true);
    assert!(result["maintenance_warning"].is_string());
    assert_eq!(
        f.rows("SELECT status,about FROM local_listeners WHERE hub='local' AND harness='fixture'")
            [0],
        json!({"status":"ACTIVE","about":"Committed enrollment"})
    );
    assert_eq!(
        f.rows("SELECT title FROM session_custom_titles WHERE harness='fixture'")[0]["title"],
        "Useful title"
    );
    state.execute("DROP TRIGGER reject_title", &[]).unwrap();
    let (status, result) = f.action(
        json!({"op":"listen_session","hub":"local","harness":"human","about":"   ","scope":"self"}),
    );
    assert_eq!(status, 200, "{result}");
    assert_eq!(
        f.rows("SELECT about FROM local_listeners WHERE hub='local' AND harness='human'")[0]
            ["about"],
        "Human"
    );
    let bytes = fs::read(&f.config).unwrap();
    let (status,result)=f.action(json!({"op":"setup_save_hub","name":"local","url":"https://hub.example.com","auth_mode":"none"}));
    assert_eq!(status, 400, "{result}");
    assert_eq!(fs::read(&f.config).unwrap(), bytes);
    for args in [
        vec![
            "setup",
            "--hub",
            "local",
            "--url",
            "https://hub.example.com",
        ],
        vec![
            "setup",
            "--hub",
            "work",
            "--url",
            "https://hub.example.com",
            "--user",
            "invalid user",
        ],
    ] {
        let (ok, out, error) = f.command(NATIVE, &args, "");
        assert!(!ok, "{out} {error}");
        assert_eq!(fs::read(&f.config).unwrap(), bytes);
    }
}

#[test]
fn disabled_hub_retirement_reaches_durable_agent_and_human_bindings() {
    let _lock = serial();
    for route in [
        "mcp",
        "cli-agent",
        "cli_human_device",
        "cli-human",
        "unlisten_session",
        "unlisten_session-hub",
    ] {
        let f = Fixture::new(json!({}));
        let (_, port) = f.hub();
        fs::write(&f.config, json!({"hubs":{"local":{"url":"local://"},"work":{"url":format!("http://127.0.0.1:{port}")}}}).to_string()).unwrap();
        let meta = f.provider("accepted");
        f.daemon(&[]);
        let agent = route == "mcp" || route == "cli-agent";
        let harness = if agent { "fixture" } else { "human" };
        let registration = if agent {
            f.tool(
                "cito_listen",
                json!({"hub":"work","about":"Receiver"}),
                meta.clone(),
            )
        } else {
            let (status, result) =
                f.action(json!({"op":"cli_human_device","hub":"work","attach":true}));
            assert_eq!(status, 200, "{result}");
            result
        };
        assert!(registration["listener_id"].is_string());
        let before = f.rows(&format!(
            "SELECT * FROM local_listeners WHERE hub='work' AND harness='{harness}'"
        ))[0]
            .clone();
        let id = before["listener_id"].as_str().unwrap();
        let (status, result) =
            f.action(json!({"op":"setup_toggle_hub","hub":"work","enabled":false}));
        assert_eq!(status, 200, "{result}");
        match route {
            "mcp" => {
                let result = f.tool_result("cito_unlisten", json!({"hub":"work"}), meta);
                assert_eq!(result["isError"], true, "{result}");
                assert!(result.to_string().contains("DETACH_PENDING"), "{result}");
            }
            "cli-agent" | "cli-human" => {
                let mut args = vec!["unlisten", "--hub", "work", "--harness", harness, "--json"];
                if agent {
                    args.extend(["--session", "session-1"]);
                }
                let (ok, out, err) = f.command(NATIVE, &args, "");
                assert!(!ok, "{out} {err}");
                assert!(
                    format!("{out} {err}").contains("DETACH_PENDING"),
                    "{out} {err}"
                );
            }
            _ => {
                let action = if route == "cli_human_device" {
                    json!({"op":route,"hub":"work","attach":false})
                } else if route == "unlisten_session-hub" {
                    json!({"op":"unlisten_session","hub":"work","harness":"human","session_id":"default"})
                } else {
                    json!({"op":"unlisten_session","harness":"human","session_id":"default"})
                };
                let (status, result) = f.action(action);
                assert_eq!(status, 400, "{result}");
                let expected = if route == "unlisten_session-hub" {
                    "INVALID_INPUT: Unknown or disabled hub"
                } else {
                    "DETACH_PENDING"
                };
                assert!(result.to_string().contains(expected), "{result}");
                assert_ne!(result["committed"], true);
            }
        }
        let after = f.rows(&format!(
            "SELECT * FROM local_listeners WHERE hub='work' AND listener_id='{id}'"
        ))[0]
            .clone();
        let denied = route == "unlisten_session-hub";
        assert_eq!(
            after["status"],
            if denied { "ACTIVE" } else { "UNLISTENED" }
        );
        if denied {
            assert_eq!(after["user_stopped"], before["user_stopped"]);
        }
        assert_eq!(after["hub_url"], before["hub_url"]);
        assert_eq!(
            f.rows(&format!(
                "SELECT * FROM pending_hub_detaches WHERE hub='work' AND listener_id='{id}'"
            ))
            .len(),
            usize::from(!denied)
        );
        let remote = f
            .db("hub/hub.sqlite")
            .query("SELECT value FROM hub_kv WHERE key=?", &[json!(id)])
            .unwrap();
        let remote: Value = serde_json::from_str(remote[0]["value"].as_str().unwrap()).unwrap();
        assert_eq!(remote["state"], "ACTIVE");
        if !agent {
            assert_ne!(
                remote["human_paused"], true,
                "Detaching a device must not change inbox policy"
            );
        }
    }
}

#[test]
fn oversized_committed_channel_values_get_payload_free_success_receipts() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    channel_provider(
        &f,
        "channel",
        json!([{"key":"credential","scope":"hub","type":"string","required":true},{"key":"address","scope":"user","type":"string","required":true}]),
    );
    let ui = f.recording_ui();
    f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    let large = "canonical-value".repeat(21_000);
    assert!(large.len() > 260 * 1024);
    for (op, request) in [
        ("channel_save", "large-save"),
        ("save_hub_service", "large-service"),
        ("channel_toggle", "large-toggle"),
    ] {
        let action = json!({"op":op,"hub":"local","channel_id":"channel","enabled":true,
            "request_id":request,"values":{"credential":large,"address":"room"}});
        let (status, response) = f.action(action);
        assert_eq!(status, 200, "{response}");
        wait(|| {
            fs::read_to_string(f.home.join("ui-frames"))
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|frame| frame["request_id"] == request && frame["op"] == "settings")
        });
        let frames = fs::read_to_string(f.home.join("ui-frames")).unwrap();
        let receipt: Value = frames
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|frame| frame["request_id"] == request && frame["op"] == "settings")
            .unwrap();
        assert_eq!(receipt["ok"], true, "{receipt}");
        assert_eq!(receipt["committed"], true);
        assert_eq!(receipt["hub"], "local");
        assert_eq!(receipt["channel"], "channel");
        assert_eq!(receipt["action"], op);
        assert_eq!(receipt["authority_changed"], false);
        assert!(receipt["warning"].is_string());
        assert!(receipt.get("settings").is_none());
        let rows = f
            .db("hub.sqlite")
            .query(
                "SELECT config_json FROM hub_plugin_services WHERE plugin_id='channel'",
                &[],
            )
            .unwrap();
        let saved: Value = serde_json::from_str(rows[0]["config_json"].as_str().unwrap()).unwrap();
        assert_eq!(saved["credential"], large);
    }
}

#[test]
fn lifecycle_channel_test_failures_emit_results_before_configured_refresh() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    channel_provider(
        &f,
        "channel",
        json!([{"key":"credential","scope":"hub","type":"secret","required":true},{"key":"address","scope":"user","type":"string","required":true}]),
    );
    let ui = f.recording_ui();
    f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    assert_eq!(f.action(json!({"op":"channel_save","channel_id":"channel","enabled":true,"values":{"credential":"saved","address":"room"}})).0, 200);
    let frames = || ui_frames(&f);
    for op in ["channel_test", "test_human_channel"] {
        for (id, expected_error, expected_status) in [
            ("channel", "Invalid token", 200),
            (
                "missing",
                "INVALID_INPUT: Unknown human channel plugin: missing",
                400,
            ),
        ] {
            let before = frames().len();
            let action = json!({"op":op,"channel_id":id,"values":if id == "channel" {
                json!({"credential":"invalid"})
            } else {
                json!({"botToken":"x"})
            }});
            let (status, result) = f.action(action);
            assert_eq!(status, expected_status, "{result}");
            assert_eq!(result["ok"], false);
            assert_eq!(result["error"], expected_error);
            wait(|| {
                let frames = frames();
                let Some(index) = frames.iter().enumerate().skip(before).find_map(|(i, v)| {
                    (v["op"] == "channel_test_result" && v["channel_id"] == id).then_some(i)
                }) else {
                    return false;
                };
                assert_eq!(frames[index]["ok"], false);
                assert_eq!(frames[index]["error"], expected_error);
                assert_eq!(frames[index]["hub"], "local");
                let subsequent = &frames[index + 1..];
                for op in ["settings", "state"] {
                    let Some(refresh) = subsequent.iter().find(|v| v["op"] == op) else {
                        return false;
                    };
                    let channel = refresh["settings"]["channels"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|ch| ch["id"] == "channel")
                        .unwrap();
                    assert_eq!(channel["configured"], true);
                    assert_eq!(channel["status"], "ready");
                }
                true
            });
        }
    }
}

#[test]
fn lifecycle_remote_only_channel_schema_and_selected_settings_hub() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let plugin = host.state.join("hub/plugins/remote-channel");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"),json!({"id":"remote-channel","kind":"human-channel","displayName":"Remote channel","scope":"hub",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],"setupGuide":["Configure service, then pair."],"pairing":{"supported":true},
        "fields":[{"key":"credential","scope":"hub","type":"secret","required":true},{"key":"address","scope":"user","type":"string","required":true}]}).to_string()).unwrap();
    let (_, port) = host.hub();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"shared":{"url":format!("http://127.0.0.1:{port}"),"autoListen":false}},"default_hub":"local"}),
    );
    f.daemon(&[]);
    assert_eq!(
        f.action(json!({"op":"cli_human_device","hub":"shared","attach":true}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"cli_human_device","hub":"shared","attach":false}))
            .0,
        200
    );
    assert!(!f.state.join("plugins/remote-channel").exists());
    let (status,result)=f.action(json!({"op":"channel_save","hub":"shared","channel_id":"remote-channel","enabled":true,"values":{"credential":"saved","address":"room"}}));
    assert_eq!(status, 200, "{result}");
    let db = cito_native::database::Database::open(&host.state.join("hub/hub.sqlite")).unwrap();
    let before = db.query("SELECT * FROM hub_plugin_services", &[]).unwrap();
    assert_eq!(f.action(json!({"op":"channel_test","hub":"shared","channel_id":"remote-channel","values":{"credential":"candidate","address":"test-room"}})).0,200);
    assert_eq!(
        db.query("SELECT * FROM hub_plugin_services", &[]).unwrap(),
        before
    );
    let found = f.tool(
        "cito_find",
        json!({"hub":"shared","harness":"human"}),
        json!({}),
    );
    let peer = &found["listeners"][0];
    assert_eq!(found["matched_count"], 1);
    assert_eq!(found["matched_count"], found["online_count"]);
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE hub='shared' AND harness='human' AND status='ACTIVE'")
        .is_empty());
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    let projected = snapshot["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "shared")
        .unwrap();
    let settings = snapshot["settings"]["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "shared")
        .unwrap();
    assert_eq!(projected["human_availability"], "ACTIVE");
    assert_eq!(projected["human_state_status"], "current");
    for key in [
        "human_listener_id",
        "human_paused",
        "human_about",
        "human_control_revision",
        "human_availability",
        "human_scope",
        "human_state_status",
        "attachments",
    ] {
        assert_eq!(projected[key], settings[key], "settings projection: {key}");
        assert_eq!(projected[key], peer[key], "directory projection: {key}");
    }
    let attachment = projected["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["location"] == "hub")
        .unwrap();
    assert_eq!(attachment["location"], "hub");
    assert_eq!(attachment["plugin_id"], "remote-channel");
    assert_eq!(attachment["availability"], "ACTIVE");
    assert_eq!(attachment["is_local"], false);
    assert!(attachment.get("owner_token").is_none());
    assert_eq!(snapshot["settings"]["settings_hub"], "shared");
    let ch = &snapshot["settings"]["channels"][0];
    assert_eq!(ch["status"], "ready");
    assert_eq!(ch["fields"][0]["hidden"], false);
    assert_eq!(ch["setup_guide"], json!(["Configure service, then pair."]));
    let bindings = db.query("SELECT * FROM hub_human_channels", &[]).unwrap();
    assert_eq!(f.action(json!({"op":"human_inbox_update","hub":"shared","expected_control_revision":projected["human_control_revision"],"paused":true})).0,200);
    let paused = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(paused["settings"]["channels"][0]["inbox_paused"], true);
    assert_eq!(paused["settings"]["channels"][0]["delivery_enabled"], false);
    assert_eq!(
        f.action(json!({"op":"human_inbox_update","hub":"shared","paused":false}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["delivery_enabled"],
        true
    );
    assert_eq!(
        db.query("SELECT * FROM hub_human_channels", &[]).unwrap(),
        bindings
    );
    assert_eq!(f.action(json!({"op":"channel_toggle","hub":"shared","channel_id":"remote-channel","enabled":false})).1["channels"][0]["status"],"paused");
    assert_eq!(
        f.action(json!({"op":"channel_pair","hub":"shared","channel_id":"remote-channel"}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"channel_cancel_pair","hub":"shared","channel_id":"remote-channel"}))
            .0,
        200
    );
    assert!(db
        .query(
            "SELECT code FROM hub_pairing_codes WHERE consumed_ms IS NULL",
            &[]
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        f.action(json!({"op":"channel_unpair","hub":"shared","channel_id":"remote-channel"}))
            .1["channels"][0]["status"],
        "hub_ready"
    );
    let paired =
        f.action(json!({"op":"channel_pair","hub":"shared","channel_id":"remote-channel"}));
    assert_eq!(paired.0, 200, "{paired:?}");
    assert_eq!(
        f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["status"],
        "pairing_pending"
    );
    let code = db
        .query(
            "SELECT code FROM hub_pairing_codes WHERE consumed_ms IS NULL",
            &[],
        )
        .unwrap()[0]["code"]
        .clone();
    // The plugin redeems on the remote Hub, outside the daemon's action path.
    rpc(
        port,
        "human_channels_test",
        json!({"plugin_id":"remote-channel","user_config":{"address":format!("redeem:{}", code.as_str().unwrap())}}),
    );
    wait(|| {
        db.query(
            "SELECT code FROM hub_pairing_codes WHERE consumed_ms IS NULL",
            &[],
        )
        .unwrap()
        .is_empty()
    });
    wait(|| {
        f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["status"] == "ready"
    });
    let personal = db
        .query("SELECT user_config_json FROM hub_human_channels", &[])
        .unwrap();
    assert_eq!(f.action(json!({"op":"clear_channel_service","hub":"shared","channel_id":"remote-channel","reset_user":false})).0,200);
    assert_eq!(
        db.query("SELECT user_config_json FROM hub_human_channels", &[])
            .unwrap(),
        personal,
        "Clearing service credentials must preserve personal destinations"
    );
    assert_eq!(f.action(json!({"op":"save_hub_service","hub":"shared","channel_id":"remote-channel","values":{"credential":"replacement"}})).1["channels"][0]["status"],"ready");
    assert_eq!(f.action(json!({"op":"clear_channel_service","hub":"shared","channel_id":"remote-channel","reset_user":true})).0,200);
    assert_eq!(
        db.query("SELECT user_config_json FROM hub_human_channels", &[])
            .unwrap()[0]["user_config_json"],
        "{}"
    );
    assert_eq!(f.action(json!({"op":"disconnect_human_channel","hub":"shared","channel_id":"remote-channel","reset_hub":true})).1["channels"][0]["status"],"not_configured");
}
#[test]
fn selected_remote_channel_refresh_is_nonblocking_and_recovers_invalidated_slots() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let plugin = host.state.join("hub/plugins/remote-channel");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"), json!({"id":"remote-channel","kind":"human-channel","displayName":"Remote channel","scope":"hub",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],
        "fields":[{"key":"credential","scope":"hub","type":"secret","required":true},{"key":"address","scope":"user","type":"string","required":true}]}).to_string()).unwrap();
    let (_, hub_port) = host.hub();
    let proxy_port = port();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"shared":{"url":format!("http://127.0.0.1:{proxy_port}"),"user":"alice"}},"default_hub":"local"}),
    );
    let proxy_script = f.dir.join("inventory-proxy.py");
    script(
        &proxy_script,
        r#"#!/usr/bin/env python3
# purpose: Gate inventory responses by identity, credential, and binding state.
# usage: python3 inventory-proxy.py LISTEN_PORT HUB_PORT
# effects: Loopback HTTP only; Fixture owns and reaps the process.
# requires: Python standard library and the sandbox Hub.
import http.server, json, socketserver, sys, threading, urllib.request
lock = threading.Lock()
gates = []
pending = {}
completed = []
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def reply(self, body):
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        try: self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError): pass
    def do_GET(self):
        with lock:
            if self.path.startswith('/release/'):
                pending[self.path.rsplit('/', 1)[1]].set()
            body = json.dumps({'waiting': [name for name, gate in pending.items() if not gate.is_set()],
                'started': list(pending), 'completed': completed}).encode()
        self.reply(body)
    def do_POST(self):
        body = self.rfile.read(int(self.headers['Content-Length']))
        if self.path == '/gate':
            with lock: gates.append(json.loads(body))
            self.reply(b'{}')
            return
        request = urllib.request.Request('http://127.0.0.1:' + sys.argv[2] + '/v1/rpc', data=body,
            headers={k: v for k, v in self.headers.items() if k.lower() in ('authorization', 'x-cito-user', 'content-type')})
        with urllib.request.urlopen(request, timeout=2) as response: result = response.read()
        gate = None
        name = None
        if json.loads(body)['op'] == 'human_channels_list':
            channel = json.loads(result)['result']['channels'][0]
            with lock:
                for spec in gates:
                    if spec['name'] not in pending and spec['user'] == self.headers.get('X-Cito-User') and (
                        'Bearer ' + spec['token'] == self.headers.get('Authorization')) and (
                        spec['configured'] == channel['configured']):
                        name = spec['name']
                        gate = pending[name] = threading.Event()
                        break
            if gate and not gate.wait(10): return
        self.reply(result)
        if gate:
            with lock: completed.append(name)
socketserver.ThreadingTCPServer.allow_reuse_address = True
server = socketserver.ThreadingTCPServer(('127.0.0.1', int(sys.argv[1])), Handler)
print(json.dumps({'op': 'ready'}), flush=True)
server.serve_forever()
"#,
    );
    let proxy = f.spawn(
        "python3",
        &[
            proxy_script.to_str().unwrap(),
            &proxy_port.to_string(),
            &hub_port.to_string(),
        ],
        &[],
    );
    wait(|| f.frames(proxy).iter().any(|frame| frame["op"] == "ready"));
    f.daemon(&[]);
    assert_eq!(f.action(json!({"op":"channel_save","hub":"shared","channel_id":"remote-channel","enabled":true,"values":{"credential":"saved","address":"room"}})).1["channels"][0]["status"], "ready");
    assert_eq!(
        f.action(json!({"op":"open_settings","hub":"shared"})).0,
        200
    );
    let hub_db = host.db("hub/hub.sqlite");
    let current_channel =
        || f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0].clone();
    wait(|| current_channel()["status"] == "ready");
    hub_db
        .execute(
            "UPDATE hub_plugin_services SET service_state_json=? WHERE plugin_id='remote-channel'",
            &[json!(
                json!({"poll_error":"HTTP 409: another getUpdates request"}).to_string()
            )],
        )
        .unwrap();
    wait(|| current_channel()["warning"] == true);
    assert!(current_channel()["detail"]
        .as_str()
        .unwrap()
        .contains("409"));
    hub_db.execute("UPDATE hub_plugin_services SET service_state_json='{}' WHERE plugin_id='remote-channel'", &[]).unwrap();
    wait(|| current_channel()["warning"] == false);
    hub_db.execute("INSERT INTO hub_channel_deliveries(plugin_id,principal_id,signal_id,seq,status,delivered_ms,last_error) VALUES('remote-channel','none#alice','unknown-effect',1,'effect_unknown',CAST(strftime('%s','now') AS INTEGER)*1000,'Response lost')", &[]).unwrap();
    wait(|| current_channel()["warning"] == true);
    assert!(current_channel()["detail"]
        .as_str()
        .unwrap()
        .contains("outcome unknown"));
    hub_db.execute("UPDATE hub_channel_deliveries SET status='delivered',last_error=NULL WHERE signal_id='unknown-effect'", &[]).unwrap();
    wait(|| current_channel()["warning"] == false);
    let query = "  remote-mac-42  ";
    let (status, response) = f.action(
        json!({"op":"query_directory","mode":"activity","query":query,"request_id":"padded-query"}),
    );
    assert_eq!(status, 200);
    assert_eq!(response["query"], query);
    assert_eq!(response["request_id"], "padded-query");
    let gate = |name: &str, user: &str, token: &str, configured: bool| {
        assert_eq!(
            http(
                proxy_port,
                "/gate",
                Some(&json!({"name":name,"user":user,"token":token,"configured":configured}))
            )
            .0,
            200
        );
    };
    let seen = |name: &str| {
        http(proxy_port, "/status", None).1["started"]
            .as_array()
            .is_some_and(|names| names.contains(&json!(name)))
    };
    let release =
        |name: &str| assert_eq!(http(proxy_port, &format!("/release/{name}"), None).0, 200);
    gate("alice-old", "alice", "rotated-credential", true);
    gate("bob-current", "bob", "rotated-credential", false);
    gate("bob-periodic", "bob", "rotated-credential", false);
    let mut config: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    config["hubs"]["shared"]["token"] = json!("rotated-credential");
    fs::write(&f.config, config.to_string()).unwrap();
    wait(|| seen("alice-old"));
    // The server has accepted the RPC and holds its response until explicit release.
    let local = |path: &str| -> Value {
        ureq::get(&format!("http://127.0.0.1:{}{path}", f.port))
            .timeout(std::time::Duration::from_millis(750))
            .call()
            .unwrap()
            .into_json()
            .unwrap()
    };
    assert_eq!(local("/health")["ok"], true);
    let daemon_port = f.port;
    for op in ["open_settings", "settings_select_hub"] {
        let opened = std::thread::spawn(move || {
            ureq::post(&format!("http://127.0.0.1:{daemon_port}/ui"))
                .timeout(std::time::Duration::from_millis(750))
                .send_json(json!({"op":op,"hub":"shared"}))
                .map_err(|error| error.to_string())
                .and_then(|response| {
                    response
                        .into_json::<Value>()
                        .map_err(|error| error.to_string())
                })
        })
        .join()
        .unwrap()
        .unwrap();
        assert_eq!(opened["ok"], true);
        assert_eq!(
            http(proxy_port, "/status", None).1["waiting"],
            json!(["alice-old"]),
            "{op} must share the held inventory request"
        );
        assert_eq!(local("/health")["ok"], true);
    }
    let snapshot: Value = ureq::post(&format!("http://127.0.0.1:{daemon_port}/ui"))
        .timeout(std::time::Duration::from_millis(750))
        .send_json(json!({"op":"get_snapshot"}))
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(snapshot["settings"]["settings_hub"], "shared");
    assert_eq!(
        snapshot["settings"]["channels"][0]["configured"], true,
        "Same-principal credential renewal retains the confirmed personal inventory"
    );
    // Replace the slot again while Alice's response is in flight. Bob must never
    // receive Alice's configured personal binding, including after its late reply.
    config["hubs"]["shared"]["user"] = json!("bob");
    fs::write(&f.config, config.to_string()).unwrap();
    wait(|| seen("bob-current"));
    assert_eq!(local("/health")["ok"], true);
    release("bob-current");
    wait(|| {
        f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["status"] == "hub_ready"
    });
    release("alice-old");
    wait(|| {
        http(proxy_port, "/status", None).1["completed"]
            .as_array()
            .is_some_and(|ids| ids.contains(&json!("alice-old")))
    });
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(snapshot["settings"]["channels"][0]["configured"], false);
    assert_eq!(snapshot["settings"]["channels"][0]["status"], "hub_ready");

    // A mutation queued behind an old inventory must await its own schema and
    // post-mutation inventory before acknowledging, without retaining service.
    // Exercise a real extra periodic response; it cannot consume a later profile's gates.
    wait(|| seen("bob-periodic"));
    release("bob-periodic");
    gate("save-inventory", "bob", "next-credential", false);
    gate("save-schema", "bob", "next-credential", false);
    gate("save-post", "bob", "next-credential", true);
    config["hubs"]["shared"]["token"] = json!("next-credential");
    fs::write(&f.config, config.to_string()).unwrap();
    wait(|| seen("save-inventory"));
    assert_eq!(
        f.action(json!({"op":"settings_select_hub","hub":"local"}))
            .0,
        200
    );
    std::thread::scope(|scope| {
        let saved = scope.spawn(move || {
            ureq::post(&format!("http://127.0.0.1:{daemon_port}/ui"))
                .timeout(std::time::Duration::from_secs(8))
                .send_json(json!({"op":"channel_save","hub":"shared","channel_id":"remote-channel","enabled":true,"values":{"address":"bob-room"}}))
                .map_err(|error|error.to_string())
                .and_then(|response| response.into_json::<Value>().map_err(|error|error.to_string()))
        });
        // The mutation selects shared before waiting for the held inventory owner.
        // Observe that dispatch, then select local while inventory is still held.
        // This stops new periodic remote reads; any already queued read shares
        // save-inventory's generation instead of consuming the schema gate.
        wait(|| f.action(json!({"op":"get_snapshot"})).1["settings"]["settings_hub"] == "shared");
        assert_eq!(
            f.action(json!({"op":"settings_select_hub","hub":"local"}))
                .0,
            200
        );
        assert_eq!(local("/health")["ok"], true);
        assert!(!seen("save-schema"));
        release("save-inventory");
        wait(|| seen("save-schema"));
        // Only the dispatched mutation can now own this RPC. Restore the remote
        // selection while its schema is held, before checking the remaining flow.
        assert_eq!(
            f.action(json!({"op":"settings_select_hub","hub":"shared"}))
                .0,
            200
        );
        assert_eq!(
            local("/health")["ok"],
            true,
            "Schema lookup must release service"
        );
        release("save-schema");
        wait(|| seen("save-post"));
        assert!(
            !saved.is_finished(),
            "The acknowledgement must await post-mutation inventory"
        );
        assert_eq!(
            local("/health")["ok"],
            true,
            "Post-mutation inventory must release service"
        );
        assert_eq!(
            f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["configured"],
            false
        );
        release("save-post");
        let result = saved.join().unwrap().unwrap();
        assert_eq!(result["channels"][0]["status"], "ready");
    });
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(snapshot["settings"]["channels"][0]["status"], "ready");
    assert_eq!(snapshot["settings"]["channels"][0]["configured"], true);

    // Preserve a committed receipt under its original authority without applying
    // its personal inventory or UI completion to the replacement identity.
    gate("unpair-post", "bob", "next-credential", false);
    gate("replacement", "charlie", "replacement-credential", false);
    std::thread::scope(|scope| {
        let changed = scope.spawn(move || {
            ureq::post(&format!("http://127.0.0.1:{daemon_port}/ui"))
                .timeout(std::time::Duration::from_secs(8))
                .send_json(
                    json!({"op":"channel_unpair","hub":"shared","channel_id":"remote-channel"}),
                )
                .map_err(Box::new)
        });
        wait(|| seen("unpair-post"));
        config["hubs"]["shared"]["token"] = json!("replacement-credential");
        config["hubs"]["shared"]["user"] = json!("charlie");
        fs::write(&f.config, config.to_string()).unwrap();
        wait(|| seen("replacement"));
        let receipt: Value = changed.join().unwrap().unwrap().into_json().unwrap();
        assert_eq!(receipt["committed"], true);
        assert_eq!(receipt["authority_changed"], true);
        assert_eq!(receipt["receipt_authority"]["hub"], "shared");
        assert_eq!(receipt["receipt_authority"]["principal_id"], "none#bob");
        assert!(receipt["maintenance_warning"]
            .as_str()
            .unwrap()
            .contains("authority changed"));
    });
    assert_eq!(local("/health")["ok"], true);
    release("replacement");
    wait(|| {
        f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["status"] == "hub_ready"
    });
    release("unpair-post");
    wait(|| {
        http(proxy_port, "/status", None).1["completed"]
            .as_array()
            .is_some_and(|ids| ids.contains(&json!("unpair-post")))
    });
    assert_eq!(
        f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["configured"],
        false
    );
    config["hubs"]["shared"]["enabled"] = json!(false);
    fs::write(&f.config, config.to_string()).unwrap();
    for op in ["open_settings", "settings_select_hub"] {
        assert_eq!(
            f.action(json!({"op":op,"hub":"shared"})).0,
            200,
            "Disabled hubs must remain configurable"
        );
    }
}

#[test]
fn human_desktop_replies_commit_publication_and_ack_atomically_then_reconcile_local_state() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"work":{"url":"local://"}},"defaultHub":"work"}));
    let meta = f.provider("delivered_unconfirmed");
    let channel = f.state.join("plugins/channel");
    fs::create_dir_all(&channel).unwrap();
    fs::write(channel.join("plugin.json"), json!({"id":"channel","kind":"human-channel","scope":"hub","displayName":"Fixture Channel",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],
        "fields":[{"key":"credential","scope":"hub","type":"secret","required":true},{"key":"address","scope":"user","type":"string","required":true}]}).to_string()).unwrap();
    let ui = f.recording_ui();
    f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    let target = enroll(&f, meta.clone());
    assert_eq!(f.action(json!({"op":"channel_save","hub":"work","channel_id":"channel","enabled":true,"values":{"credential":"test-only","address":"reply-test"}})).0, 200);
    let hub = f.db("hub.sqlite");
    let channel_frames = || -> Vec<Value> {
        fs::read_to_string(channel.join("frames.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    };
    let ui_results = || -> Vec<Value> {
        fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|frame| frame["op"] == "composer_result")
            .collect()
    };
    let source = || -> String {
        let sent = f.tool(
            "cito_send",
            json!({"hub":"work","to":"@self","message":"Approve this change?"}),
            meta.clone(),
        );
        let id = sent["signal_id"]
            .as_str()
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap()
            .to_owned();
        wait(|| {
            !f.rows(&format!("SELECT signal_id FROM local_signals WHERE signal_id='{id}' AND acknowledged_at_ms IS NULL")).is_empty()
            && hub.query("SELECT signal_id FROM hub_channel_deliveries WHERE signal_id=? AND status='delivered'", &[json!(id)]).unwrap().len() == 1
        });
        id
    };
    let assert_unread = |id: &str| {
        assert!(f.rows(&format!(
            "SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='{id}'"
        ))[0]["acknowledged_at_ms"]
            .is_null());
        assert!(hub
            .query(
                "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
                &[json!(id)]
            )
            .unwrap()[0]["acked_at_ms"]
            .is_null());
    };
    let untouched = source();
    for op in ["send_signal", "send_reply_from_human"] {
        let id = source();
        let results_before = ui_results().len();
        let result = {
            let action = if op == "send_signal" {
                json!({"op":op,"request_id":format!("{op}-{id}"),"to":target["listener_id"],"message":"Approved","in_reply_to":format!("work:{id}")})
            } else {
                json!({"op":op,"request_id":format!("{op}-{id}"),"to_listener_id":target["listener_id"],"text":"Approved","in_reply_to":null,"signal_id":format!("work:{id}")})
            };
            let (status, result) = f.action(action);
            assert_eq!(status, 200, "{result}");
            wait(|| ui_results().len() == results_before + 1);
            assert_eq!(ui_results().last().unwrap()["ok"], true);
            assert_eq!(
                ui_results().last().unwrap()["request_id"],
                format!("{op}-{id}")
            );
            result
        };
        if op == "send_reply_from_human" {
            assert!(!result["signal_id"].as_str().unwrap().contains(':'));
            assert_eq!(
                result["target_display"],
                target["listener_id"]
                    .as_str()
                    .unwrap()
                    .rsplit(':')
                    .next()
                    .unwrap()
            );
            assert_eq!(result["error"], Value::Null);
        }
        let reply = result["signal_id"]
            .as_str()
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap();
        wait(|| {
            !f.rows(&format!(
                "SELECT signal_id FROM local_signals WHERE signal_id='{reply}'"
            ))
            .is_empty()
        });
        let delivered = &f.rows(&format!(
            "SELECT message,in_reply_to FROM local_signals WHERE signal_id='{reply}'"
        ))[0];
        assert_eq!(delivered["message"], "Approved");
        assert_eq!(delivered["in_reply_to"], id);
        assert!(f.rows(&format!(
            "SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='{id}'"
        ))[0]["acknowledged_at_ms"]
            .is_number());
        wait(|| {
            hub.query(
                "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
                &[json!(id)],
            )
            .unwrap()[0]["acked_at_ms"]
                .is_number()
        });
        wait(|| {
            channel_frames()
                .iter()
                .any(|frame| frame["op"] == "on_acked" && frame["signal_ids"] == json!([id]))
        });
        let ack = channel_frames()
            .into_iter()
            .find(|frame| frame["op"] == "on_acked" && frame["signal_ids"] == json!([id]))
            .unwrap();
        assert_eq!(ack["principal_id"], "none#alice");
        let mut external = ack["external_msg_ids"].as_array().unwrap().clone();
        external.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        assert_eq!(
            external,
            json!([format!("{id}-1"), format!("{id}-2")])
                .as_array()
                .unwrap()
                .clone()
        );
        assert_unread(&untouched);
    }

    let retried = source();
    hub.execute("CREATE TRIGGER reject_ack BEFORE UPDATE OF acked_at_ms ON hub_signals BEGIN SELECT RAISE(ABORT,'injected ack failure'); END", &[]).unwrap();
    let before = hub
        .query("SELECT COUNT(*) AS n FROM hub_signals", &[])
        .unwrap();
    for op in ["send_signal", "send_reply_from_human"] {
        let args = json!({"to":target["listener_id"],"message":"Retry reply","in_reply_to":format!("work:{retried}"),"idempotency_key":format!("retry-{op}")});
        {
            let mut action = args;
            action["op"] = json!(op);
            let count = ui_results().len();
            let (status, result) = f.action(action);
            assert_eq!(status, 400, "{result}");
            assert!(result.to_string().contains("injected ack failure"));
            wait(|| ui_results().len() == count + 1);
            assert_eq!(ui_results().last().unwrap()["ok"], false);
        }
        assert_unread(&retried);
        assert_eq!(
            hub.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
                .unwrap(),
            before
        );
        assert!(f
            .rows(&format!(
                "SELECT signal_id FROM pending_hub_acks WHERE signal_id='{retried}'"
            ))
            .is_empty());
    }
    hub.execute("DROP TRIGGER reject_ack", &[]).unwrap();
    let args = json!({"to":target["listener_id"],"message":"Retry reply","in_reply_to":format!("work:{retried}"),"idempotency_key":"retry-send_reply_from_human","op":"send_reply_from_human"});
    let count = ui_results().len();
    let (status, reply) = f.action(args.clone());
    assert_eq!(status, 200, "{reply}");
    wait(|| ui_results().len() == count + 1);
    assert_eq!(ui_results().last().unwrap()["ok"], true);
    assert_eq!(f.action(args).1["signal_id"], reply["signal_id"]);
    wait(|| ui_results().len() == count + 2);
    assert_eq!(ui_results().last().unwrap()["ok"], true);
    assert_eq!(
        hub.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
            .unwrap()[0]["n"]
            .as_u64()
            .unwrap(),
        before[0]["n"].as_u64().unwrap() + 1
    );
    wait(|| {
        channel_frames()
            .iter()
            .any(|frame| frame["op"] == "on_acked" && frame["signal_ids"] == json!([retried]))
    });
    assert!(f.rows(&format!(
        "SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='{retried}'"
    ))[0]["acknowledged_at_ms"]
        .is_number());
    assert!(f
        .rows(&format!(
            "SELECT signal_id FROM pending_hub_acks WHERE signal_id='{retried}'"
        ))
        .is_empty());

    // A local commit failure leaves the reservation pending after the Hub commits.
    // Retrying must reuse the Hub's signal ID and reconcile the local source.
    let recovered = source();
    let local = f.db("state.sqlite");
    local.execute("CREATE TRIGGER reject_sent_commit BEFORE UPDATE ON local_sent_idempotency BEGIN SELECT RAISE(ABORT,'injected local commit failure'); END", &[]).unwrap();
    let args = json!({"hub":"work","to":target["listener_id"],"message":"Recover local commit","in_reply_to":format!("work:{recovered}"),"idempotency_key":"local-commit"});
    let result = send_human_result(&f, args.clone());
    assert_eq!(result["http_status"], 200);
    assert_eq!(result["ok"], true);
    assert!(
        result["maintenance_warning"]
            .as_str()
            .is_some_and(|warning| warning.contains("local sent history")),
        "{result}"
    );
    let committed = hub.query("SELECT seq,signal_id FROM hub_signals WHERE json_extract(CAST(data AS TEXT),'$.in_reply_to')=?", &[json!(recovered)]).unwrap();
    assert_eq!(committed.len(), 1);
    assert_eq!(result["seq"], committed[0]["seq"]);
    assert!(result["signal_id"]
        .as_str()
        .unwrap()
        .ends_with(committed[0]["signal_id"].as_str().unwrap()));
    assert!(hub
        .query(
            "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
            &[json!(recovered)]
        )
        .unwrap()[0]["acked_at_ms"]
        .is_number());
    local
        .execute("DROP TRIGGER reject_sent_commit", &[])
        .unwrap();
    let reply = send_human(&f, args);
    assert_eq!(
        reply["signal_id"],
        format!("work:{}", committed[0]["signal_id"].as_str().unwrap())
    );
    assert_eq!(reply["seq"], committed[0]["seq"]);
    assert_eq!(hub.query("SELECT seq,signal_id FROM hub_signals WHERE json_extract(CAST(data AS TEXT),'$.in_reply_to')=?", &[json!(recovered)]).unwrap(), committed);
    assert!(f.rows(&format!(
        "SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='{recovered}'"
    ))[0]["acknowledged_at_ms"]
        .is_number());

    let failed = source();
    let before = hub
        .query("SELECT COUNT(*) AS n FROM hub_signals", &[])
        .unwrap();
    for op in ["send_signal", "send_reply_from_human"] {
        for (message, reply_to) in [
            ("   ", format!("work:{failed}")),
            ("Wrong hub", format!("elsewhere:{failed}")),
        ] {
            let count = ui_results().len();
            let request_id = format!("{op}-failure-{count}");
            let (status, result) = f.action(json!({"op":op,"request_id":request_id,"to":target["listener_id"],"message":message,"in_reply_to":reply_to}));
            assert_eq!(status, 400, "{result}");
            wait(|| ui_results().len() == count + 1);
            assert_eq!(ui_results().last().unwrap()["ok"], false);
            assert_eq!(ui_results().last().unwrap()["request_id"], request_id);
            assert_unread(&failed);
        }
    }
    hub.execute("CREATE TRIGGER reject_reply BEFORE INSERT ON hub_signals BEGIN SELECT RAISE(ABORT,'injected reply failure'); END", &[]).unwrap();
    let count = ui_results().len();
    assert_eq!(f.action(json!({"op":"send_signal","to":target["listener_id"],"message":"Rejected publish","in_reply_to":format!("work:{failed}")})).0, 400);
    wait(|| ui_results().len() == count + 1);
    assert_eq!(ui_results().last().unwrap()["ok"], false);
    hub.execute("DROP TRIGGER reject_reply", &[]).unwrap();
    assert_unread(&failed);
    f.tool("cito_unlisten", json!({"hub":"work"}), meta);
    for op in ["send_signal", "send_reply_from_human"] {
        let count = ui_results().len();
        assert_eq!(f.action(json!({"op":op,"to":target["listener_id"],"message":"Revoked target","in_reply_to":format!("work:{failed}")})).0, 400);
        wait(|| ui_results().len() == count + 1);
        assert_eq!(ui_results().last().unwrap()["ok"], false);
        assert_unread(&failed);
    }
    assert_eq!(
        hub.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
            .unwrap(),
        before
    );
    // A request/response barrier drains the channel's stdin before checking absence.
    assert_eq!(
        f.action(json!({"op":"channel_test","hub":"work","channel_id":"channel"}))
            .0,
        200
    );
    assert!(!channel_frames()
        .iter()
        .any(|frame| frame["op"] == "on_acked"
            && frame["signal_ids"].as_array().is_some_and(
                |ids| ids.contains(&json!(failed)) || ids.contains(&json!(untouched))
            )));
}

#[test]
fn ui_ipc_actions_mutate_real_state_and_only_sandbox_host_services() {
    let _lock = serial();
    let owner = Fixture::new(json!({}));
    let manifest = json!({"id":"channel","kind":"human-channel","scope":"hub","displayName":"Fixture Channel",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],
        "pairing":{"supported":true},"fields":[
            {"key":"credential","scope":"hub","type":"secret","required":true},
            {"key":"address","scope":"user","type":"string","required":true}]});
    let remote_plugin = owner.state.join("hub/plugins/channel");
    fs::create_dir_all(&remote_plugin).unwrap();
    fs::write(remote_plugin.join("plugin.json"), manifest.to_string()).unwrap();
    let (_, hub_port) = owner.hub();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":format!("http://127.0.0.1:{hub_port}")}},"defaultHub":"local"}),
    );
    f.provider("delivered_unconfirmed");
    let plugin = f.state.join("plugins/channel");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"), manifest.to_string()).unwrap();
    let ui = f.recording_ui();
    let daemon = f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    for op in [
        "refresh_available",
        "open_settings",
        "close_settings",
        "open_directory",
        "query_directory",
        "close_directory",
    ] {
        assert_eq!(f.action(json!({"op":op})).0, 200, "{op}");
    }
    for (harness, session_id, error) in [
        ("missing", "s1", "UNSUPPORTED_HARNESS: Unknown harness"),
        ("channel", "s1", "UNSUPPORTED_HARNESS: Unknown harness"),
        (
            "fixture",
            "closed-session",
            "BINDING_REQUIRED: Caller session mismatch",
        ),
    ] {
        let (status, result) = f.action(json!({"op":"listen_session","hub":"local","harness":harness,"session_id":session_id,"about":"bad"}));
        assert_eq!(status, 400, "{result}");
        assert_eq!(result["error"], error);
        assert!(f
            .rows("SELECT * FROM local_listeners WHERE harness <> 'human'")
            .is_empty());
        assert!(f
            .db("hub.sqlite")
            .query(
                "SELECT * FROM hub_kv WHERE json_extract(CAST(value AS TEXT),'$.harness') <> 'human'",
                &[],
            )
            .unwrap()
            .is_empty());
    }
    assert_eq!(f.action(json!({"op":"listen_session","harness":"fixture","session_id":"session-1","about":"IPC receiver","scope":"workspace"})).0,200);
    let row = f.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    let id = row["listener_id"].clone();
    assert_eq!(
        f.action(json!({"op":"switch_hub","from_hub":"local","to_hub":"work","listener_id":id}))
            .0,
        200
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE hub='local' AND harness='fixture'")[0]
            ["status"],
        "UNLISTENED"
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE hub='work' AND harness='fixture'")[0]
            ["status"],
        "ACTIVE"
    );
    let hub_db = owner.db("hub/hub.sqlite");
    assert_eq!(
        f.rows("SELECT scope FROM local_listeners WHERE hub='work' AND harness='fixture'")[0]
            ["scope"],
        "workspace"
    );
    assert_eq!(
        hub_db
            .query(
                "SELECT scope FROM hub_kv WHERE key=?",
                std::slice::from_ref(&id)
            )
            .unwrap()[0]["scope"],
        "workspace"
    );
    for scope in ["self", "workspace"] {
        assert_eq!(
            f.action(json!({"op":"set_scope","hub":"work","listener_id":id,"scope":scope}))
                .0,
            200
        );
        assert_eq!(
            f.rows("SELECT scope FROM local_listeners WHERE hub='work' AND harness='fixture'")[0]
                ["scope"],
            scope
        );
        let registration = hub_db
            .query(
                "SELECT scope,CAST(value AS TEXT) AS value FROM hub_kv WHERE key=?",
                std::slice::from_ref(&id),
            )
            .unwrap();
        assert_eq!(registration[0]["scope"], scope);
        let value: Value =
            serde_json::from_str(registration[0]["value"].as_str().unwrap()).unwrap();
        assert_eq!(value["scope"], scope);
    }
    for op in ["setup_apply", "setup_remove_app"] {
        assert_eq!(f.action(json!({"op":op,"harness":"fixture"})).0, 200);
    }
    let calls = fs::read_to_string(f.state.join("plugins/fixture/calls.jsonl")).unwrap();
    assert!(calls
        .lines()
        .any(|line| serde_json::from_str::<Value>(line).unwrap()["op"] == "setup_remove"));
    for op in ["channel_save", "channel_test", "channel_pair"] {
        let (s, v) = f.action(json!({"op":op,"channel_id":"missing"}));
        assert_eq!(s, 400);
        assert_eq!(
            v["error"],
            "INVALID_INPUT: Unknown human channel plugin: missing"
        );
    }
    let (status, saved) = f.action(
        json!({"op":"channel_save","hub":"work","channel_id":"channel",
        "enabled":true,"values":{"credential":"fixture-secret","address":"fixture-account"}}),
    );
    assert_eq!(status, 200, "{saved}");
    let channel = saved["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "channel")
        .unwrap();
    assert_eq!(channel["configured"], true);
    let service = hub_db
        .query(
            "SELECT config_json FROM hub_plugin_services WHERE plugin_id='channel'",
            &[],
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(service[0]["config_json"].as_str().unwrap()).unwrap(),
        json!({"credential":"fixture-secret"})
    );
    let binding = hub_db.query("SELECT enabled,user_config_json FROM hub_human_channels WHERE plugin_id='channel' AND principal_id='none#alice'", &[]).unwrap();
    assert_eq!(binding[0]["enabled"], 1);
    assert_eq!(
        serde_json::from_str::<Value>(binding[0]["user_config_json"].as_str().unwrap()).unwrap(),
        json!({"address":"fixture-account"})
    );
    let (status, tested) =
        f.action(json!({"op":"channel_test","hub":"work","channel_id":"channel"}));
    assert_eq!(status, 200, "{tested}");
    assert_eq!(tested["ok"], true);
    assert_eq!(tested["detail"], "Fixture test succeeded");
    let (status, paired) =
        f.action(json!({"op":"channel_pair","hub":"work","channel_id":"channel"}));
    assert_eq!(status, 200, "{paired}");
    let code = paired["code"].as_str().unwrap();
    assert!(code.starts_with("cito-"));
    assert_eq!(
        paired["instruction"],
        format!("Send {code} to fixture-account")
    );
    let pairing = hub_db.query("SELECT code,instruction,expires_ms,consumed_ms FROM hub_pairing_codes WHERE plugin_id='channel' AND principal_id='none#alice'", &[]).unwrap();
    assert_eq!(pairing[0]["code"], paired["code"]);
    assert_eq!(pairing[0]["instruction"], paired["instruction"]);
    assert_eq!(pairing[0]["expires_ms"], paired["expires_at_ms"]);
    assert!(pairing[0]["consumed_ms"].is_null());
    wait(|| {
        let frames: Vec<Value> = fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        frames.iter().any(|v| {
            v["op"] == "channel_test_result"
                && v["hub"] == "work"
                && v["channel_id"] == "channel"
                && v["ok"] == true
                && v["detail"] == "Fixture test succeeded"
        }) && frames.iter().any(|v| {
            v["op"] == "pair_code"
                && v["hub"] == "work"
                && v["channel_id"] == "channel"
                && v["code"] == paired["code"]
                && v["instruction"] == paired["instruction"]
                && v["expires_at_ms"] == paired["expires_at_ms"]
        })
    });
    assert_eq!(
        f.action(json!({"op":"set_launch_at_login","enabled":true}))
            .0,
        200
    );
    assert!(f
        .home
        .join("Library/LaunchAgents/com.tefx.cito.daemon.plist")
        .exists());
    assert_eq!(
        f.action(json!({"op":"set_launch_at_login","enabled":false}))
            .0,
        200
    );
    assert!(!f
        .home
        .join("Library/LaunchAgents/com.tefx.cito.daemon.plist")
        .exists());
    let p = port();
    assert_eq!(f.action(json!({"op":"set_host_hub","enabled":true,"port":p,"bind":"127.0.0.1","auth_mode":"none"})).0,200);
    assert!(rpc(p, "kv_snapshot", json!({}))["entries"].is_array());
    let configured: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    let token = configured["host_hub"]["token"].as_str().unwrap();
    assert_eq!(token.len(), 64);
    assert!(token
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":false})).0,
        200
    );
    // Disconnect now revokes this harness's listeners. Reconnect explicitly before testing delivery.
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
        .is_empty());
    assert_eq!(
        f.action(json!({"op":"setup_apply","harness":"fixture"})).0,
        200
    );
    assert_eq!(f.action(json!({"op":"listen_session","hub":"work","harness":"fixture","session_id":"session-1","about":"IPC receiver","scope":"workspace"})).0,200);
    let sent = f.action(json!({"op":"send_signal","hub":"work","to":id,"message":"Agent inbox"}));
    assert_eq!(sent.0, 200, "{}", sent.1);
    wait(|| !f.rows("SELECT * FROM local_signals").is_empty());
    f.tool(
        "cito_inbox",
        json!({"hub":"work"}),
        json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
    );
    enroll(&f, human());
    f.tool(
        "cito_send",
        json!({"hub":"local","to":"@self","message":"Human inbox"}),
        json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
    );
    wait(|| {
        !f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL")
            .is_empty()
    });
    assert_eq!(
        f.action(json!({"op":"ack_signals","hub":"local","ack_all":true}))
            .0,
        200
    );
    assert!(f
        .rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL")
        .is_empty());
    assert_eq!(
        f.action(json!({"op":"unlisten","hub":"work","listener_id":id}))
            .0,
        200
    );
    assert_eq!(f.action(json!({"op":"unlisten_all"})).0, 200);
    assert_eq!(
        f.action(json!({"op":"setup_save_hub","name":"extra","url":"local://","auth_mode":"none"}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"setup_toggle_hub","hub":"extra","enabled":false}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"setup_remove_hub","hub":"extra"})).0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"hub_action","hub":"work","action":"reconnect"}))
            .0,
        200
    );
    assert_eq!(f.action(json!({"op":"quit"})).0, 200);
    wait(|| f.exited(daemon));
    let g = Fixture::new(json!({}));
    fs::write(g.state.join("retain-data"), "keep").unwrap();
    let daemon = g.daemon(&[]);
    assert_eq!(
        g.action(json!({"op":"setup_uninstall_all","purge_data":false}))
            .0,
        200
    );
    wait(|| g.exited(daemon));
    assert_eq!(
        fs::read_to_string(g.state.join("retain-data")).unwrap(),
        "keep"
    );
}

#[test]
fn scoped_pause_all_preserves_sibling_hubs_and_human_attachment_projection() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (_, port) = host.hub();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":format!("http://127.0.0.1:{port}")}},"default_hub":"local"}),
    );
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    for hub in ["local", "work"] {
        f.tool(
            "cito_listen",
            json!({"hub":hub,"about":"Review work"}),
            meta.clone(),
        );
    }
    wait(|| {
        f.rows("SELECT * FROM local_listeners WHERE harness='human' AND status='ACTIVE'")
            .len()
            == 2
    });
    assert_eq!(
        f.rows("SELECT * FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
            .len(),
        2
    );
    assert_eq!(f.action(json!({"op":"unlisten_all","hub":"work"})).0, 200);
    assert_eq!(
        f.rows("SELECT hub,status FROM local_listeners WHERE harness='fixture' ORDER BY hub"),
        vec![
            json!({"hub":"local","status":"ACTIVE"}),
            json!({"hub":"work","status":"UNLISTENED"})
        ]
    );
    assert_eq!(
        f.rows("SELECT * FROM local_listeners WHERE harness='human' AND status='ACTIVE'")
            .len(),
        2
    );
    let human = f
        .rows("SELECT * FROM local_listeners WHERE hub='local' AND harness='human'")
        .remove(0);
    let db = f.db("hub.sqlite");
    let row = db
        .query(
            "SELECT CAST(value AS TEXT) AS value FROM hub_kv WHERE key=?",
            &[human["listener_id"].clone()],
        )
        .unwrap()
        .remove(0);
    let mut remote: Value = serde_json::from_str(row["value"].as_str().unwrap()).unwrap();
    assert!(remote["channels"]
        .as_object()
        .unwrap()
        .contains_key(&format!("{}:human", human["host_id"].as_str().unwrap())));
    remote["channels"]["remote-host:human"] = json!({"state":"ACTIVE","host_id":"remote-host","host_name":"other-mac.local","updated_at_ms":cito_native::plugins::now_ms()});
    remote["channels"]["hub:external"] =
        json!({"state":"ACTIVE","host_id":"hub","display_name":"Telegram","host_name":"Hub"});
    db.execute(
        "UPDATE hub_kv SET value=CAST(? AS BLOB),revision=revision+1 WHERE key=?",
        &[json!(remote.to_string()), human["listener_id"].clone()],
    )
    .unwrap();
    wait(|| {
        f.action(json!({"op":"get_snapshot"})).1["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["hub"] == "local" && row["harness"] == "human")
            .is_some_and(|row| {
                row["channels"].as_array().is_some_and(|names| {
                    names.contains(&json!("Telegram"))
                        && names.contains(&json!("other-mac.local"))
                        && !names.contains(&human["host_name"])
                })
            })
    });
    assert_eq!(f.action(json!({"op":"unlisten_all"})).0, 200);
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
        .is_empty());
    assert_eq!(
        f.rows("SELECT * FROM local_listeners WHERE harness='human' AND status='ACTIVE'")
            .len(),
        2
    );
}

#[test]
fn remote_human_scope_updates_admit_cross_principal_activity() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let port = port();
    let hub = host.spawn(
        NATIVE,
        &[
            "hub",
            "--bind",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--data-dir",
            host.state.join("hub").to_str().unwrap(),
            "--auth",
            "token",
            "--token",
            "test-only",
        ],
        &[],
    );
    wait(|| {
        assert!(!host.exited(hub), "{}", host.output(hub, true));
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    let url = format!("http://127.0.0.1:{port}");
    let f = Fixture::new(
        json!({"user":"alice","defaultHub":"work","hubs":{"work":{"url":url,"authMode":"token","token":"test-only"}}}),
    );
    let daemon = f.daemon(&[]);
    let human = || {
        f.rows("SELECT * FROM local_listeners WHERE hub='work' AND harness='human' AND status='ACTIVE'").into_iter().next()
    };
    wait(|| human().is_some());
    let id = human().unwrap()["listener_id"].clone();
    assert_eq!(human().unwrap()["scope"], "self");
    let rpc = |user: &str, op: &str, params: Value| {
        let response: Value = ureq::post(&format!("{url}/v1/rpc"))
            .set("Authorization", "Bearer test-only")
            .set("X-Cito-User", user)
            .send_json(json!({"op":op,"params":params}))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(response["ok"], true, "{response}");
        response["result"].clone()
    };
    let run = |args: &[&str]| {
        let (ok, out, err) = f.command(NATIVE, args, "");
        assert!(ok, "{args:?}: {out} {err}");
        serde_json::from_str::<Value>(&out).unwrap()
    };
    let send_bob = |signal_id: &str| {
        rpc(
            "bob",
            "js_publish",
            json!({"subject":format!("cito.signal.{}", id.as_str().unwrap()),"data":{"to":id,"signal_id":signal_id,"from":{"principal_id":"none#bob","user":"bob","host_name":"other-mac","harness":"human","listener_id":"bob","title":"Bob"},"message":"Cross-principal review"}}),
        );
        let started = std::time::Instant::now();
        wait(|| {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(11),
                "{}; listener: {:?}",
                f.output(daemon, true),
                human()
            );
            !f.rows(&format!("SELECT signal_id FROM local_signals WHERE signal_id='{signal_id}' AND activity_hidden=0")).is_empty()
        });
        let activity = f.action(json!({"op":"get_snapshot"})).1["activity"].clone();
        assert!(
            activity
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["signal_id"] == signal_id),
            "{activity}"
        );
    };
    let resumed = run(&["inbox", "resume", "--hub", "work", "--team", "--json"]);
    assert_eq!(resumed["record"]["scope"], "workspace");
    assert_eq!(human().unwrap()["scope"], "workspace");
    send_bob("cli-scope");
    // A separate client changes policy without touching the daemon's directory cache.
    let private = rpc(
        "alice",
        "human_inbox_update",
        json!({"node_id":id,"expected_control_revision":resumed["record"]["human_control_revision"],"scope":"self"}),
    );
    wait(|| human().unwrap()["scope"] == "self");
    rpc(
        "alice",
        "human_inbox_update",
        json!({"node_id":id,"expected_control_revision":private["record"]["human_control_revision"],"scope":"workspace"}),
    );
    send_bob("other-mac-scope");
    assert_eq!(human().unwrap()["scope"], "workspace");
}

#[test]
fn cli_human_devices_inbox_policy_and_mcp_authority_are_distinct() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    f.tool(
        "cito_listen",
        json!({"about":"Agent receiver"}),
        meta.clone(),
    );
    let run = |args: &[&str]| {
        let (ok, out, err) = f.command(NATIVE, args, "");
        assert!(ok, "{args:?}: {out} {err}");
        serde_json::from_str::<Value>(&out).unwrap()
    };
    run(&["listen", "--harness", "human", "--json"]);
    let paused = run(&[
        "inbox",
        "pause",
        "--hub",
        "local",
        "--scope",
        "self",
        "--about",
        "Release approvals",
        "--json",
    ]);
    assert_eq!(paused["record"]["human_paused"], true);
    assert_eq!(paused["record"]["scope"], "self");
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='human'")[0]["status"],
        "ACTIVE"
    );
    let resumed = run(&["inbox", "resume", "--hub", "local", "--team", "--json"]);
    assert_eq!(resumed["record"]["human_paused"], false);
    assert_eq!(resumed["record"]["about"], "Release approvals");
    assert_eq!(resumed["record"]["scope"], "workspace");
    assert_eq!(
        resumed["record"]["human_control_revision"].as_u64(),
        paused["record"]["human_control_revision"]
            .as_u64()
            .map(|r| r + 1)
    );
    for tool in ["cito_listen", "cito_unlisten"] {
        for (args, caller) in [
            (json!({"harness":"human","about":"Forbidden"}), meta.clone()),
            (json!({"about":"Forbidden"}), human()),
        ] {
            let denied = f.tool_result(tool, args, caller);
            assert_eq!(denied["isError"], true);
            assert!(denied["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("FORBIDDEN"));
        }
    }
    run(&["unlisten", "--harness", "human", "--hub", "local", "--json"]);
    let detached = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(detached["hubs"][0]["human_paused"], false);
    assert_eq!(detached["hubs"][0]["human_about"], "Release approvals");
    assert_eq!(
        detached["hubs"][0]["human_control_revision"],
        resumed["record"]["human_control_revision"]
    );
    assert!(!detached["hubs"][0]["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["is_local"] == true));
    for extra in [
        vec!["--scope", "workspace"],
        vec!["--team"],
        vec!["Device enrollment"],
    ] {
        let args = [vec!["listen", "--harness", "human"], extra].concat();
        let (ok, _, error) = f.command(NATIVE, &args, "");
        assert!(
            !ok && !error.contains("INVALID_INPUT") && error.contains("cito inbox"),
            "{error}"
        );
        assert!(!f
            .rows("SELECT status FROM local_listeners WHERE harness='human'")
            .iter()
            .any(|r| r["status"] == "ACTIVE"));
    }
    run(&["listen", "--harness", "human", "--json"]);
    assert_eq!(
        run(&["unlisten", "--all", "--hub", "local", "--json"])["removed"],
        1
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='human'")[0]["status"],
        "ACTIVE"
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "UNLISTENED"
    );
}

#[test]
fn r1_remote_schema_hub_only_saves_never_create_or_resume_personal_rows() {
    let _lock = serial();
    let owner = Fixture::new(json!({}));
    let manifest = json!({"id":"channel","kind":"human-channel","scope":"hub","displayName":"Messenger",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],"pairing":{"supported":true},"fields":[
        {"key":"credential","scope":"hub","type":"secret","required":true},
        {"key":"address","scope":"user","type":"string","required":true}]});
    let directory = owner.state.join("hub/plugins/channel");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("plugin.json"), manifest.to_string()).unwrap();
    let (_, p) = owner.hub();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"remote":{"url":format!("http://127.0.0.1:{p}")}},"defaultHub":"local"}),
    );
    let mut conflicting = manifest;
    conflicting["fields"][0]["scope"] = json!("user");
    conflicting["fields"][1]["scope"] = json!("hub");
    let directory = f.state.join("plugins/channel");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("plugin.json"), conflicting.to_string()).unwrap();
    f.daemon(&[]);
    // Human enrollment covers both networks, independently of defaultHub.
    // Both Hubs use none auth, so their default Human scope remains workspace.
    wait(|| {
        let snapshot = f.action(json!({"op":"get_snapshot"})).1;
        ["local", "remote"].iter().all(|hub| {
            snapshot["listeners"].as_array().unwrap().iter().any(|row| {
                row["hub"] == *hub && row["harness"] == "human" && row["scope"] == "workspace"
            })
        })
    });
    assert_eq!(
        f.action(
            json!({"op":"unlisten_session","hub":"remote","harness":"human","session_id":"default"})
        )
        .0,
        200
    );
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    assert!(!snapshot["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["hub"] == "remote" && row["harness"] == "human"));
    assert!(snapshot["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["hub"] == "local" && row["harness"] == "human"));
    let rows = || {
        owner
            .db("hub/hub.sqlite")
            .query(
                "SELECT * FROM hub_human_channels WHERE plugin_id='channel'",
                &[],
            )
            .unwrap()
    };
    for op in ["save_hub_service", "channel_save"] {
        let (status,v)=f.action(json!({"op":op,"scope":"hub","hub":"remote","channel_id":"channel","enabled":true,"user_config":{"address":"injected"},"display_label":"injected","values":{"credential":op,"address":"must-not-save"}}));
        assert_eq!(status, 200, "{v}");
        assert!(rows().is_empty());
        let cfg = owner
            .db("hub/hub.sqlite")
            .query(
                "SELECT config_json FROM hub_plugin_services WHERE plugin_id='channel'",
                &[],
            )
            .unwrap();
        let cfg: Value = serde_json::from_str(cfg[0]["config_json"].as_str().unwrap()).unwrap();
        assert_eq!(
            cfg,
            json!({"credential":op}),
            "Selected remote schema must override conflicting local field scopes"
        );
    }
    for op in ["channel_pair", "channel_test"] {
        let (status, value) = f.action(json!({"op":op,"scope":"hub","hub":"remote","channel_id":"channel","values":{"credential":"must-not-save"}}));
        assert_eq!(status, 400, "{value}");
        assert!(value["error"]
            .as_str()
            .unwrap()
            .starts_with("INVALID_INPUT:"));
        assert!(rows().is_empty());
        let config = owner
            .db("hub/hub.sqlite")
            .query(
                "SELECT config_json FROM hub_plugin_services WHERE plugin_id='channel'",
                &[],
            )
            .unwrap();
        let config: Value =
            serde_json::from_str(config[0]["config_json"].as_str().unwrap()).unwrap();
        assert_eq!(config, json!({"credential":"channel_save"}));
    }
    assert_eq!(f.action(json!({"op":"channel_save","hub":"remote","channel_id":"channel","enabled":true,"values":{"address":"personal"}})).0,200);
    assert_eq!(rows()[0]["enabled"], 1);
    assert_eq!(
        f.action(
            json!({"op":"channel_toggle","hub":"remote","channel_id":"channel","enabled":false})
        )
        .0,
        200
    );
    let paused = rows();
    assert_eq!(f.action(json!({"op":"save_hub_service","hub":"remote","channel_id":"channel","values":{"credential":"rotated"}})).0,200);
    assert_eq!(rows(), paused);
    assert_eq!(f.action(json!({"op":"channel_test","hub":"remote","channel_id":"channel","values":{"address":"candidate"}})).0,200);
    assert_eq!(rows(), paused, "Non-persisting tests cannot write bindings");
}
