use super::*;

#[test]
fn sigterm_wakes_an_active_oidc_callback_after_browser_exit() {
    use std::io::Write;
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let provider = f.spawn("python3", &["-u", "-c", r#"import http.server,json,socketserver
class Handler(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_GET(self):
  issuer='http://127.0.0.1:'+str(self.server.server_address[1])
  raw=json.dumps({'issuer':issuer,'authorization_endpoint':issuer+'/authorize','token_endpoint':issuer+'/token','scopes_supported':['openid']}).encode()
  self.send_response(200);self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)
s=socketserver.TCPServer(('127.0.0.1',0),Handler)
print(json.dumps({'port':s.server_address[1]}),flush=True);s.serve_forever()
"#], &[]);
    wait(|| !f.frames(provider).is_empty());
    let issuer = format!("http://127.0.0.1:{}", f.frames(provider)[0]["port"]);
    fs::write(&f.config, json!({"default_hub":"work","hubs":{"local":{"url":"local://"},"work":{"url":"http://127.0.0.1:1","authMode":"oidc","issuer":issuer,"clientId":"client","redirectPort":0}}}).to_string()).unwrap();
    // The opener records its URL and exits successfully without sending a callback.
    let opener = f.dir.join("browser");
    script(
        &opener,
        "#!/bin/sh\nprintf '%s' \"$1\" > \"$HOME/callback-url\"\n",
    );
    let daemon = f.daemon(&[("CITO_BROWSER_BIN", opener.to_str().unwrap())]);
    let port = f.port;
    let login = std::thread::spawn(move || {
        http(
            port,
            "/ui",
            Some(&json!({"op":"hub_action","action":"login","hub":"work"})),
        )
    });
    wait(|| f.home.join("callback-url").exists());
    let url = url::Url::parse(&fs::read_to_string(f.home.join("callback-url")).unwrap()).unwrap();
    let callback = url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .unwrap()
        .1
        .into_owned();
    let callback = url::Url::parse(&callback).unwrap();
    // Creating or editing a different Hub must leave the default Hub's callback live.
    for action in [
        json!({"op":"setup_save_hub","name":"personal","url":"local://"}),
        json!({"op":"setup_save_hub","edit":true,"name":"personal","autoListen":false}),
    ] {
        let result = f.action(action);
        assert_eq!(result.0, 200, "{result:?}");
    }
    // A bad callback proves the login worker is inside the callback accept loop.
    let mut probe = std::net::TcpStream::connect(("127.0.0.1", callback.port().unwrap())).unwrap();
    probe
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .unwrap();
    writeln!(probe, "GET /callback?state=invalid HTTP/1.1\r").unwrap();
    let mut response = String::new();
    std::io::Read::read_to_string(&mut probe, &mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    std::thread::sleep(std::time::Duration::from_millis(100));
    let start = std::time::Instant::now();
    f.signal(daemon, libc::SIGTERM);
    let until = start + std::time::Duration::from_secs(2);
    wait(|| {
        assert!(
            std::time::Instant::now() < until,
            "Daemon stalled during callback cancellation"
        );
        f.exited(daemon)
    });
    let result = login.join().unwrap();
    assert_eq!(result.0, 400, "{result:?}");
    assert!(result.1["error"].as_str().unwrap().contains("cancelled"));
    assert!(!f.state.join("oidc-token-work.json").exists());
    assert!(
        f.rows("SELECT * FROM daemon_owner").is_empty(),
        "Graceful shutdown clears ownership"
    );
}

#[test]
fn ui_retries_after_initial_executable_failure() {
    use std::os::unix::fs::PermissionsExt;
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let ui = f.recording_ui();
    fs::set_permissions(&ui, fs::Permissions::from_mode(0o600)).unwrap();
    f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    assert!(ui_frames(&f).is_empty());
    fs::set_permissions(&ui, fs::Permissions::from_mode(0o700)).unwrap();
    wait(|| ui_frames(&f).iter().any(|r| r["op"] == "state"));
}

#[test]
fn shared_endpoint_oidc_profiles_accept_either_login_order_until_subjects_collide() {
    let _lock = serial();
    for [first, second] in [["personal", "work"], ["work", "personal"]] {
        let f = Fixture::new(json!({}));
        let keys = root().join("crates/cito-native/tests/hub/fixtures");
        let provider = f.spawn("python3", &["-u", "-c", r#"import base64,http.server,json,pathlib,subprocess,sys,time,urllib.parse as url
keys=pathlib.Path(sys.argv[1])
def b64(raw): return base64.urlsafe_b64encode(raw).rstrip(b'=')
def part(obj): return b64(json.dumps(obj).encode())
class Provider(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def reply(self,obj):
  raw=json.dumps(obj).encode(); self.send_response(200); self.send_header('Content-Length',str(len(raw))); self.end_headers(); self.wfile.write(raw)
 def do_GET(self):
  if self.path=='/jwks':
   n=bytes.fromhex((keys/'rsa-modulus.txt').read_text().strip().split('=')[1]); self.reply({'keys':[{'kty':'RSA','kid':'fixture','alg':'RS256','n':b64(n).decode(),'e':'AQAB'}]})
  else: self.reply({'issuer':issuer,'authorization_endpoint':issuer+'/authorize','token_endpoint':issuer+'/token','jwks_uri':issuer+'/jwks'})
 def do_POST(self):
  p=url.parse_qs(self.rfile.read(int(self.headers['Content-Length'])).decode()); claims={'iss':issuer,'sub':'shared','aud':p['client_id'][0],'exp':int(time.time())+600,'nonce':p['code'][0]}
  payload=part({'alg':'RS256','kid':'fixture'})+b'.'+part(claims); signature=subprocess.check_output(['openssl','dgst','-sha256','-sign',keys/'rsa-private.pem'],input=payload)
  self.reply({'id_token':(payload+b'.'+b64(signature)).decode()})
s=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider); issuer='http://127.0.0.1:'+str(s.server_port); print(json.dumps(issuer),flush=True); s.serve_forever()
"#, keys.to_str().unwrap()], &[]);
        wait(|| !f.frames(provider).is_empty());
        let issuer = f.frames(provider).remove(0);
        let opener = f.dir.join("browser");
        script(&opener, "#!/usr/bin/env python3\nimport sys, urllib.parse as u, urllib.request as r\np=u.parse_qs(u.urlparse(sys.argv[1]).query)\nr.urlopen(p['redirect_uri'][0]+'?'+u.urlencode({'state':p['state'][0], 'code':p['nonce'][0]}), timeout=5).read()\n");
        f.daemon(&[("CITO_BROWSER_BIN", opener.to_str().unwrap())]);
        for (name, url) in [
            ("personal", "http://127.0.0.1:1/"),
            ("work", "http://127.0.0.1:1"),
        ] {
            f.action_ok(json!({"op":"setup_save_hub","name":name,"url":url,"authMode":"oidc","issuer":issuer,"clientId":name,"redirectPort":0}));
        }
        for name in [first, second] {
            let result = f.action(json!({"op":"hub_action","action":"login","hub":name}));
            let cache = f.state.join(format!("oidc-token-{name}.json"));
            assert!(
                cache.is_file(),
                "{name} never finished authentication: {result:?}"
            );
            let verified = read_json(cache);
            assert_eq!(verified["client_id"], name);
            assert_eq!(
                verified["verified_principal_id"],
                format!("oidc:{}|shared", issuer.as_str().unwrap())
            );
        }
        let snapshot = f.snapshot();
        let runtime = snapshot["hubs"].as_array().unwrap();
        assert!(runtime.iter().any(|h| h["name"] == first), "{runtime:?}");
        assert!(!runtime.iter().any(|h| h["name"] == second), "{runtime:?}");
        let suppressed = snapshot["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == second)
            .unwrap();
        assert_eq!(
            suppressed["error"],
            format!("INVALID_INPUT: Hub URL is already configured under '{first}'")
        );
    }
}

#[test]
fn background_oidc_recovery_does_not_block_health_or_a_healthy_hub() {
    use std::{
        io::Write,
        net::TcpListener,
        time::{Duration, Instant},
    };
    let _lock = serial();
    let issuer = TcpListener::bind("127.0.0.1:0").unwrap();
    let issuer_url = format!("http://{}", issuer.local_addr().unwrap());
    let (accepted, ready) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let (finished, done) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        use std::os::fd::AsRawFd;
        let mut fds = [libc::pollfd {
            fd: issuer.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        assert!(
            cito_native::support::poll_until(
                &mut fds,
                Some(Instant::now() + Duration::from_secs(10))
            )
            .unwrap(),
            "No issuer connection"
        );
        let (mut stream, _) = issuer.accept().unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        accepted.send(()).unwrap();
        let _ = released.recv_timeout(Duration::from_secs(5));
        let _ = stream.write_all(
            b"HTTP/1.1 503 Unavailable\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
        let _ = finished.send(());
    });
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let saved = f.action(json!({"op":"setup_save_hub","name":"work","url":"http://127.0.0.1:1","authMode":"oidc","issuer":issuer_url,"clientId":"client"}));
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let in_flight = done.try_recv().is_err();
    let start = Instant::now();
    let health = http(f.port, "/health", None);
    let latency = start.elapsed();
    let start = Instant::now();
    let snapshot = f.action(json!({"op":"get_snapshot"}));
    let snapshot_latency = start.elapsed();
    let _ = release.send(());
    worker.join().unwrap();
    assert_eq!(saved.0, 200);
    assert!(in_flight, "Probe runs during a blocked OIDC request");
    assert_eq!(health.0, 200);
    assert!(latency < Duration::from_millis(500), "{latency:?}");
    assert_eq!(snapshot.0, 200);
    assert!(
        snapshot_latency < Duration::from_millis(500),
        "{snapshot_latency:?}"
    );
    assert!(snapshot.1["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["name"] == "local" && h["status"] == "connected"));
}
#[test]
fn gui_purge_removes_credentials_idempotency_hub_history_and_personal_profile_data() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    let human = f
        .rows("SELECT * FROM local_listeners WHERE harness='human'")
        .remove(0);
    f.tool("cito_listen", json!({"about":"Delete this session"}), meta);
    f.tool(
        "cito_send",
        json!({"to":human["public_id"],"message":"Delete this history"}),
        json!({}),
    );
    assert!(!f.rows("SELECT * FROM local_sent_idempotency").is_empty());
    let updated = f.action(json!({"op":"human_inbox_update","hub":"local","paused":true,"about":"Delete this profile"}));
    assert_eq!(updated.0, 200);
    fs::write(
        f.state.join("oidc-token-work.json"),
        "delete this credential",
    )
    .unwrap();
    fs::create_dir_all(f.state.join("activity/fixture")).unwrap();
    fs::write(f.state.join("activity/fixture/source.json"), "{}").unwrap();
    let result = f.action(json!({"op":"setup_disconnect_all","purge_data":true}));
    assert_eq!(result.0, 200, "{}", result.1);
    assert!(!f.state.join("oidc-token-work.json").exists());
    assert!(!f.state.join("activity").exists());
    for table in [
        "local_signals",
        "local_sent_idempotency",
        "pending_hub_acks",
        "pending_hub_detaches",
        "session_custom_titles",
    ] {
        assert!(
            f.rows(&format!("SELECT * FROM {table}")).is_empty(),
            "{table}"
        );
    }
    let hub = f.db("hub.sqlite");
    assert!(hub
        .query("SELECT * FROM hub_signals", &[])
        .unwrap()
        .is_empty());
    let fresh = f.snapshot();
    assert_eq!(fresh["hubs"][0]["human_paused"], false);
    assert_eq!(fresh["hubs"][0]["human_about"], "Human");
    assert_eq!(fresh["hubs"][0]["human_control_revision"], 1);
}
#[test]
fn u8_token_auth_generates_and_persists_token_while_sharing_is_off() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[("CITO_TAILSCALE_HOST", "mac-studio.tailnet.ts.net.")]);
    f.action_ok(json!({"op":"set_host_hub","enabled":false,"auth_mode":"token"}));
    let host = f.snapshot()["settings"]["host_hub"].clone();
    assert_eq!(host["enabled"], false);
    assert_eq!(host["running"], false);
    assert_eq!(host["token"].as_str().unwrap().len(), 64);
    assert_eq!(
        host["endpoint_url"],
        "http://mac-studio.tailnet.ts.net:4291"
    );
    assert!(host["invite_command"]
        .as_str()
        .unwrap()
        .contains("--url http://mac-studio.tailnet.ts.net:4291"));
    assert!(host["invite_command"]
        .as_str()
        .unwrap()
        .contains("--hub mac-studio "));
    assert_eq!(host["tailscale_url"], host["endpoint_url"]);
    assert!(host["lan_url"].as_str().unwrap().starts_with("http://"));
    assert_eq!(host["tailscale_invite_command"], host["invite_command"]);
    let saved: Value = read_json(&f.config);
    assert_eq!(saved["host_hub"]["token"], host["token"]);
    f.action_ok(json!({"op":"set_host_hub","enabled":false,"auth_mode":"token"}));
    assert_eq!(f.snapshot()["settings"]["host_hub"]["token"], host["token"]);
    // An explicit listener interface must not advertise an unrelated Tailscale address.
    for bind in ["127.0.0.1", "192.0.2.10", "::1"] {
        f.action_ok(json!({"op":"set_host_hub","enabled":false,"bind":bind}));
        let host = f.snapshot()["settings"]["host_hub"].clone();
        let authority = if bind.contains(':') {
            format!("[{bind}]")
        } else {
            bind.into()
        };
        assert_eq!(host["endpoint_url"], format!("http://{authority}:4291"));
        assert_eq!(host["lan_url"], host["endpoint_url"]);
        assert!(host["tailscale_url"].is_null());
        assert!(host["tailscale_invite_command"].is_null());
    }
}

#[test]
fn lifecycle_sign_in_enables_disabled_hub_before_bounded_discovery_failure() {
    let _lock = serial();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let server = std::thread::spawn(move || {
        use std::io::Write;
        while !flag.load(std::sync::atomic::Ordering::SeqCst) {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                );
            } else {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    });
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":format!("http://127.0.0.1:{port}"),"enabled":false,"authMode":"oidc","issuer":format!("http://127.0.0.1:{port}"),"clientId":"client"}},"default_hub":"local"}),
    );
    f.daemon(&[]);
    let result = f.action(json!({"op":"hub_action","action":"login","hub":"work"}));
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    server.join().unwrap();
    assert_eq!(result.0, 400, "{result:?}");
    let raw: Value = read_json(&f.config);
    assert_eq!(raw["hubs"]["work"]["enabled"], true);
    assert!(
        !f.home.join("mock-calls").exists(),
        "discovery failure must never open a browser"
    );
}
#[test]
fn lifecycle_hub_edit_preserves_secrets_disabled_state_default_and_host_error_rollback() {
    let _lock = serial();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":"http://127.0.0.1:1","authMode":"token","token":"saved","enabled":false,"clientSecret":"retained"}},"default_hub":"local"}),
    );
    f.daemon(&[]);
    f.action_ok(json!({"op":"setup_save_hub","edit":true,"original_name":"work","name":"renamed","url":"http://127.0.0.1:2","auth_mode":"token","token":"","clientSecret":""}));
    let raw: Value = read_json(&f.config);
    assert_eq!(raw["default_hub"], "local");
    assert!(raw["hubs"].get("work").is_none());
    assert_eq!(raw["hubs"]["renamed"]["token"], "saved");
    assert_eq!(raw["hubs"]["renamed"]["clientSecret"], "retained");
    assert_eq!(raw["hubs"]["renamed"]["enabled"], false);
    let snapshot = f.snapshot();
    assert!(!snapshot["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["name"] == "renamed"));
    let h = snapshot["settings"]["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "renamed")
        .unwrap();
    assert_eq!(h["status"], "disconnected");
    assert_eq!(h["connected"], false);
    f.action_ok(
        json!({"op":"set_host_hub","enabled":true,"port":0,"bind":"127.0.0.1","auth_mode":"none"}),
    );
    let running = f.snapshot()["settings"]["host_hub"].clone();
    let blocked = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = blocked.local_addr().unwrap().port();
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":true,"port":port}))
            .0,
        400
    );
    let failed = f.snapshot()["settings"]["host_hub"].clone();
    assert_eq!(failed["port"], running["port"]);
    assert!(failed["error"].is_string());
    assert_eq!(failed["running"], true);
    f.action_ok(json!({"op":"set_host_hub","enabled":true,"port":0,"auth_mode":"token"}));
    let success = f.snapshot()["settings"]["host_hub"].clone();
    assert!(success["error"].is_null());
    assert_eq!(success["auth_mode"], "token");
}
#[test]
fn live_user_reload_and_auth_mode_canonicalization_survive_restart() {
    let _lock = serial();
    let mut f =
        Fixture::new(json!({"user":"alice","hubs":{"local":{"url":"local://","authMode":"NONE"}}}));
    f.env.remove("CITO_USER");
    let daemon = f.daemon(&[]);
    let a = enroll(&f, human());
    assert!(a["listener_id"].as_str().unwrap().starts_with("alice-"));
    let mut config: Value = read_json(&f.config);
    config["user"] = json!("bob");
    fs::write(&f.config, config.to_string()).unwrap();
    let b = enroll(&f, human());
    assert!(
        b["listener_id"].as_str().unwrap().starts_with("bob-"),
        "{b}"
    );
    f.stop(daemon);
    f.daemon(&[]);
    assert!(enroll(&f, human())["listener_id"]
        .as_str()
        .unwrap()
        .starts_with("bob-"));
    // Disabled slots lose their in-memory predecessor, but retained bindings still
    // belong to the original principal and URL across edits and restarts.
    f.action_ok(
        json!({"op":"unlisten_session","hub":"local","harness":"human","session_id":"default"}),
    );
    config["hubs"]["local"]["enabled"] = json!(false);
    fs::write(&f.config, config.to_string()).unwrap();
    f.action(json!({"op":"get_snapshot"}));
    config["user"] = json!("carol");
    config["hubs"]["local"]["enabled"] = json!(true);
    fs::write(&f.config, config.to_string()).unwrap();
    assert!(enroll(&f, human())["listener_id"]
        .as_str()
        .unwrap()
        .starts_with("carol-"));
    assert!(f
        .rows("SELECT status FROM local_listeners WHERE principal_id=\'none#bob\'")
        .iter()
        .all(|r| r["status"] == "FENCED_INACTIVE"));
    config["hubs"]["local"]["enabled"] = json!(false);
    fs::write(&f.config, config.to_string()).unwrap();
    f.action(json!({"op":"get_snapshot"}));
    config["hubs"]["local"]["url"] = json!("http://127.0.0.1:0");
    config["hubs"]["local"]["enabled"] = json!(true);
    fs::write(&f.config, config.to_string()).unwrap();
    f.action(json!({"op":"get_snapshot"}));
    let rows =
        f.rows("SELECT status,hub_url FROM local_listeners WHERE principal_id=\'none#carol\'");
    assert!(!rows.is_empty());
    assert!(rows
        .iter()
        .all(|r| r["status"] == "FENCED_INACTIVE" && r["hub_url"] == "local://"));
}
#[test]
fn ui_invites_and_config_mutations_validate_before_persistent_effects() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let before = fs::read(&f.config).unwrap();
    for invite in ["[]", "42", "true", "\"invite\"", "null"] {
        let (s, v) = f.action(json!({"op":"paste_hub_invite","invite":invite}));
        assert_eq!(s, 400);
        assert_eq!(v["error"], "INVALID_INPUT: Invalid hub invite");
        assert_eq!(fs::read(&f.config).unwrap(), before);
    }
    f.action_ok(json!({"op":"paste_hub_invite","invite":"{\"name\":\"work\",\"url\":\"local://\",\"authMode\":\"none\"}"}));
    f.action_ok(json!({"op":"set_auto_listen","hub":"local","enabled":false}));
    f.action_ok(json!({"op":"setup_save_hub","name":"other","url":"local://","auth_mode":"none"}));
    let config: Value = read_json(&f.config);
    assert_eq!(config["hubs"]["local"]["autoListen"], false);
    assert_eq!(config["hubs"]["other"]["url"], "local://");
    for (name,invite) in [
        ("remote",format!("http://127.0.0.1:{}",port())),
        ("joined",format!("cito setup --hub joined --url http://127.0.0.1:{} --auth token --token fixture-token --default",port())),
        ("json",json!({"name":"json","url":format!("http://127.0.0.1:{}",port()),"auth_mode":"none"}).to_string()),
        ("equals",format!("cito setup --hub=equals --url=http://127.0.0.1:{} --default",port())),
    ] {
        let (ok,out,err)=f.command(NATIVE,&["setup","--invite",&invite,"--json"],""); assert!(ok,"{out} {err}");
        let config:Value=read_json(&f.config);
        assert!(config["hubs"][name].is_object()); assert_eq!(config["default_hub"],name);
    }
    let joined = || {
        f.snapshot()["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == "json")
            .unwrap()
            .clone()
    };
    assert_eq!(joined()["connected"], false);
    f.action_ok(json!({"op":"setup_save_hub","edit":true,"original_name":"json","name":"json","url":"local://","auth_mode":"none"}));
    let recovered = joined();
    assert_eq!(recovered["connected"], true);
    assert!(recovered["error"].is_null());

    f.action_ok(json!({"op":"set_host_hub","enabled":true,"port":0,"bind":"127.0.0.1"}));
    let snapshot = f.snapshot();
    let hosted = &snapshot["settings"]["host_hub"];
    let (ok, out, err) = f.command(NATIVE, &["setup", "--print-invite", "--json"], "");
    assert!(ok, "{out} {err}");
    let printed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(printed["invite_command"], hosted["invite_command"]);
    let (ok, _, err) = f.command(
        NATIVE,
        &[
            "setup",
            "--invite",
            hosted["endpoint_url"].as_str().unwrap(),
        ],
        "",
    );
    assert!(!ok);
    assert!(
        err.contains("This invite code belongs to this Mac's own shared Hub"),
        "{err}"
    );
    let port = hosted["port"].as_u64().unwrap();
    let committed = fs::read(&f.config).unwrap();
    let endpoint = hosted["endpoint_url"].as_str().unwrap();
    let (ok, _, err) = f.command(
        NATIVE,
        &[
            "setup", "--hub", "mirror", "--url", endpoint, "--auth", "none", "--json",
        ],
        "",
    );
    assert!(!ok && err.contains("own shared Hub"), "{err}");
    let (status, error) =
        f.action(json!({"op":"setup_save_hub","name":"mirror","url":endpoint,"auth_mode":"none"}));
    assert_eq!(status, 400);
    assert!(error["error"].as_str().unwrap().contains("own shared Hub"));
    assert_eq!(fs::read(&f.config).unwrap(), committed);
    let hostname = f.env["CITO_HOST_NAME"].clone();
    for host in [
        "127.0.0.1",
        "127.0.0.2",
        "localhost",
        "[::1]",
        &hostname,
        &format!("{}.local", hostname.trim_end_matches(".local")),
    ] {
        let invite =
            json!({"name":"self-invite","url":format!("http://{host}:{port}"),"auth_mode":"none"});
        let (status, error) =
            f.action(json!({"op":"paste_hub_invite","invite":invite.to_string()}));
        assert_eq!(status, 400, "{host}: {error}");
        assert_eq!(error["error"], "INVALID_INPUT: This invite code belongs to this Mac's own shared Hub (already active as 'local'). Paste it in Cito on your other Mac to connect.");
        assert_eq!(fs::read(&f.config).unwrap(), committed);
        let (ok, out, err) = f.command(
            NATIVE,
            &["setup", "--invite", &invite.to_string(), "--json"],
            "",
        );
        assert!(!ok, "{host}: {out}");
        assert!(
            err.contains("This invite code belongs to this Mac's own shared Hub")
                && !err.contains("INVALID_INPUT:"),
            "{host}: {err}"
        );
        assert_eq!(fs::read(&f.config).unwrap(), committed);
    }
    let root = ureq::get(hosted["endpoint_url"].as_str().unwrap())
        .call()
        .unwrap();
    assert_eq!(
        root.header("Content-Type"),
        Some("text/html; charset=utf-8")
    );
    let landing = root.into_string().unwrap();
    assert!(landing.contains("On another Mac"));
    assert!(landing.contains("Join from Clipboard"));
    // The same host on another port remains a valid remote Hub.
    let different = if port == 65535 { port - 1 } else { port + 1 };
    let invite = json!({"name":"different-port","url":format!("http://127.0.0.1:{different}"),"auth_mode":"none"});
    f.action_ok(json!({"op":"paste_hub_invite","invite":invite.to_string()}));
    let url = &invite["url"];
    f.action_ok(
        json!({"op":"setup_save_hub","name":"alias","url":url,"authMode":"none","enabled":false}),
    );
    let committed = fs::read(&f.config).unwrap();
    for action in [
        json!({"op":"hub_action","hub":"alias","action":"reconnect"}),
        json!({"op":"hub_action","hub":"alias","action":"login"}),
        json!({"op":"setup_toggle_hub","hub":"alias","enabled":true}),
        json!({"op":"setup_save_hub","name":"alias","url":url,"authMode":"none"}),
    ] {
        let (status, result) = f.action(action);
        assert_eq!(status, 400, "{result}");
        assert_eq!(
            result["error"],
            "INVALID_INPUT: Hub URL is already configured under 'different-port'"
        );
        assert_eq!(fs::read(&f.config).unwrap(), committed);
    }
    let (ok, _, err) = f.command(NATIVE, &["setup", "--enable-hub", "alias"], "");
    assert!(
        !ok && err.contains("already configured under 'different-port'"),
        "{err}"
    );
    let (status, result) = f.action(json!({"op":"set_host_hub","port":different}));
    assert_eq!(status, 400);
    assert!(result["error"].as_str().unwrap().contains("own shared Hub"));
    assert_eq!(fs::read(&f.config).unwrap(), committed);
    assert_eq!(f.snapshot()["settings"]["host_hub"]["port"], port);
}

#[test]
fn cli_plugin_enable_refreshes_a_running_daemons_cached_inspection() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let configured = || {
        f.snapshot()["settings"]["apps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "pi")
            .unwrap()["configured"]
            .clone()
    };
    assert_eq!(configured(), false);
    let (ok, out, err) = f.command(NATIVE, &["setup", "--enable", "pi", "--json"], "");
    assert!(ok, "{out} {err}");
    assert_eq!(configured(), true);
}

#[test]
fn r1_background_hub_recovery_clears_transient_errors_but_preserves_validation_errors() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (server, port) = host.hub();
    let f = Fixture::new(
        json!({"hubs":{"shared":{"url":format!("http://127.0.0.1:{port}")}},"defaultHub":"shared"}),
    );
    f.recording_daemon();
    let connected = |rows: &Value| {
        rows.as_array()?.iter().find(|h| h["name"] == "shared")?["connected"].as_bool()
    };
    wait(|| connected(&f.snapshot()["hubs"]) == Some(true));
    host.stop(server);
    wait(|| {
        let failed = f.snapshot();
        connected(&failed["hubs"]) == Some(false)
            && connected(&failed["settings"]["hubs"]) == Some(false)
    });
    let frames_path = f.home.join("ui-frames");
    wait(|| {
        ui_frames(&f)
            .iter()
            .any(|frame| frame["op"] == "state" && connected(&frame["hubs"]) == Some(false))
    });
    let before = fs::read_to_string(&frames_path).unwrap().len();
    host.spawn(
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
            "none",
        ],
        &[],
    );
    // No UI or MCP request drives recovery: the daemon's background transport
    // work must publish a fresh state before we read either projection.
    wait(|| {
        fs::read_to_string(&frames_path)
            .unwrap_or_default()
            .get(before..)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|frame| {
                frame["op"] == "state"
                    && frame["hubs"].as_array().is_some_and(|hubs| {
                        hubs.iter().any(|h| {
                            h["name"] == "shared"
                                && h["status"] == "connected"
                                && h["error"].is_null()
                        })
                    })
            })
    });
    let recovered = f.snapshot();
    f.action_ok(json!({"op":"open_settings","hub":"shared"}));
    let mut settings = Value::Null;
    wait(|| {
        settings = fs::read_to_string(&frames_path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .rfind(|frame| frame["op"] == "settings" && frame["hub"] == "shared")
            .map(|frame| frame["settings"].clone())
            .unwrap_or(Value::Null);
        settings.is_object()
    });
    for hubs in [
        &recovered["hubs"],
        &recovered["settings"]["hubs"],
        &settings["hubs"],
    ] {
        assert!(hubs
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == "shared")
            .unwrap()["error"]
            .is_null());
    }
    let (status, invalid) =
        f.action(json!({"op":"setup_save_hub","name":"shared","url":"invalid"}));
    assert_eq!(status, 400);
    assert!(invalid["error"]
        .as_str()
        .unwrap()
        .starts_with("INVALID_INPUT:"));
    // A healthy connection must not hide a rejected configuration edit, even
    // after another background poll has observed the recovered transport.
    f.action_ok(json!({"op":"refresh_available"}));
    let snapshot = f.snapshot();
    assert_eq!(
        snapshot["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == "shared")
            .unwrap()["error"],
        invalid["error"]
    );
}

#[test]
fn removing_or_disabling_default_hubs_persists_a_restartable_fallback() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for (key, op, remaining, expected) in [
        (
            "default_hub",
            "setup_remove_hub",
            json!({"local":{"url":"local://"}}),
            "local",
        ),
        (
            "defaultHub",
            "setup_remove_hub",
            json!({"local":{"url":"local://"}}),
            "local",
        ),
        ("default_hub", "setup_remove_hub", json!({}), "local"),
        (
            "defaultHub",
            "setup_save_hub",
            json!({"local":{"url":"local://"}}),
            "local",
        ),
        (
            "defaultHub",
            "setup_remove_hub",
            json!({"personal":{"url":"local://","enabled":false},"shared":{"url":"local://"}}),
            "shared",
        ),
        (
            "default_hub",
            "setup_remove_hub",
            json!({"local":{"url":"local://","enabled":false},"shared":{"url":"local://","enabled":true},"zeta":{"url":"local://"}}),
            "shared",
        ),
        (
            "defaultHub",
            "setup_remove_hub",
            json!({"local":{"url":"local://","disabled":true},"shared":{"url":"local://"}}),
            "shared",
        ),
        (
            "default_hub",
            "setup_toggle_hub",
            json!({"local":{"url":"local://"},"shared":{"url":"local://"}}),
            "local",
        ),
        (
            "defaultHub",
            "hub_action",
            json!({"local":{"url":"local://","enabled":false},"shared":{"url":"local://","enabled":true},"zeta":{"url":"local://"}}),
            "shared",
        ),
    ] {
        let mut initial = json!({"hubs":remaining});
        initial["hubs"]["work"] = json!({"url":"local://"});
        initial[key] = json!("work");
        fs::write(&f.config, initial.to_string()).unwrap();
        let daemon = f.daemon(&[]);
        assert_eq!(f.snapshot()["hub"], "work");
        let action = if op == "setup_save_hub" {
            json!({"op":op,"edit":true,"name":"work","enabled":false,"set_default":true})
        } else {
            json!({"op":op,"hub":"work","enabled":false,"action":"disconnect"})
        };
        let (status, response) = f.action(action);
        assert_eq!(status, 200, "{op}: {response}");
        let saved: Value = read_json(&f.config);
        assert_eq!(saved["default_hub"], expected, "{op}: {saved}");
        assert!(saved.get("defaultHub").is_none());
        if matches!(op, "setup_toggle_hub" | "hub_action" | "setup_save_hub") {
            assert_eq!(saved["hubs"]["work"]["enabled"], false);
        } else {
            assert!(saved["hubs"].get("work").is_none());
        }
        assert_eq!(f.snapshot()["hub"], expected);
        let (ok, out, err) = f.command(NATIVE, &["setup", "--status", "--json"], "");
        assert!(ok, "{err}");
        let inspection: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(inspection["defaultHub"], expected);
        assert!(inspection["hubs"].get(expected).is_some());
        let (ok, out, err) = f.command(NATIVE, &["find", "--user", "absent", "--json"], "");
        assert!(ok, "{err}");
        let found: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(found["ok"], true);
        assert_eq!(found["listeners"], json!([]));
        f.stop(daemon);
        let restarted = f.daemon(&[]);
        let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
        assert_eq!(status, 200, "{snapshot}");
        assert_eq!(snapshot["hub"], expected);
        f.stop(restarted);
    }
}
#[test]
fn peer_mutations_return_committed_receipts_only_after_success() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.recording_daemon();
    let human = f
        .rows("SELECT * FROM local_listeners WHERE harness='human'")
        .remove(0);
    let mut actions = [
        json!({"op":"human_inbox_update","paused":true}),
        json!({"op":"set_scope","listener_id":human["listener_id"],"scope":"workspace"}),
        json!({"op":"cli_human_device","attach":false}),
        json!({"op":"cli_human_device","attach":true}),
        json!({"op":"listen_session","harness":"fixture","session_id":"session-1"}),
        json!({"op":"rename_session","harness":"fixture","session_id":"session-1","title":"Renamed"}),
        json!({"op":"cli_rename","meta":meta,"title":"CLI renamed"}),
    ];
    for (index, action) in actions.iter_mut().enumerate() {
        let request = format!("mutation-{index}");
        action["request_id"] = json!(request);
        let (status, result) = f.action(action.clone());
        assert_eq!(status, 200, "{action}: {result}");
        assert_eq!(result["ok"], true);
        assert_eq!(result["committed"], true, "{action}: {result}");
        wait(|| {
            ui_frames(&f).into_iter().any(|frame| {
                frame["request_id"] == request
                    && frame["source_op"] == action["op"]
                    && frame["ok"] == true
                    && frame["committed"] == true
            })
        });
    }
    let agent = f
        .rows("SELECT * FROM local_listeners WHERE harness='fixture'")
        .remove(0);
    assert_eq!(agent["title"], "CLI renamed");
    let (status, result) = f.action(json!({"op":"unlisten_session","hub":"local","harness":agent["harness"],"session_id":agent["session_id"]}));
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["committed"], true);
    assert_eq!(
        f.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "UNLISTENED"
    );
    let (status, failed) = f.action(
        json!({"op":"rename_session","harness":"fixture","session_id":"session-1","title":""}),
    );
    assert_eq!(status, 400, "{failed}");
    assert_ne!(failed["committed"], true);
}

#[test]
fn cli_removal_purges_only_removed_hub_across_reload_and_restart() {
    let _lock = serial();
    let f = Fixture::new(
        json!({"default_hub":"local","hubs":{"local":{"url":"local://","enabled":false},"work":{"url":"local://","enabled":false}}}),
    );
    let daemon = f.daemon(&[]);
    let db = f.db("state.sqlite");
    for hub in ["local", "work"] {
        db.execute("INSERT INTO local_listeners(hub,listener_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status,hub_url) VALUES(?,'c_0123456789abcdef','none#alice','alice','host','desktop','fixture','session','','Work','Work','workspace','UNLISTENED','local://')", &[json!(hub)]).unwrap();
        db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,hub_url,principal_id) VALUES(?,'signal','c_0123456789abcdef',1,0,'alice','desktop','Sender','Retained history','',?,'STORED_PASSIVE','local://','none#alice')", &[json!(hub),json!(u64::MAX / 2)]).unwrap();
        db.execute("INSERT INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms) VALUES(?,'node','signal','channel',1)", &[json!(hub)]).unwrap();
        db.execute(
            "INSERT INTO pending_hub_detaches VALUES(?,'node','local://')",
            &[json!(hub)],
        )
        .unwrap();
        db.execute("INSERT INTO local_sent_idempotency(hub,dedup_id,request_hash,signal_id,to_listener_id,status,expires_at_iso,expires_at_ms) VALUES(?,'dedup','hash','signal','node','wake_accepted','',?)", &[json!(hub),json!(u64::MAX / 2)]).unwrap();
        db.execute(
            "INSERT INTO wake_retries VALUES(?,'node',?)",
            &[json!(hub), json!(u64::MAX / 2)],
        )
        .unwrap();
        fs::write(
            f.state.join(format!("oidc-token-{hub}.json")),
            "retained credential",
        )
        .unwrap();
    }
    let tables = [
        "local_listeners",
        "local_signals",
        "pending_hub_acks",
        "pending_hub_detaches",
        "local_sent_idempotency",
        "wake_retries",
    ];
    let surviving: Vec<_> = tables
        .iter()
        .map(|table| {
            db.query(&format!("SELECT * FROM {table} WHERE hub='local'"), &[])
                .unwrap()
        })
        .collect();
    // A disabled Hub retains data; reload must distinguish it from removal.
    f.action_ok(json!({"op":"get_snapshot"}));
    for table in tables {
        assert_eq!(
            db.query(&format!("SELECT * FROM {table} WHERE hub='work'"), &[])
                .unwrap()
                .len(),
            1
        );
    }
    let (ok, out, err) = f.command(
        NATIVE,
        &["setup", "--remove-hub", "work", "--no-login-item", "--json"],
        "",
    );
    assert!(ok, "{out}\n{err}");
    for restart in [false, true] {
        if restart {
            f.stop(daemon);
            f.daemon(&[]);
        }
        f.action_ok(json!({"op":"get_snapshot"}));
        for (table, expected) in tables.iter().zip(&surviving) {
            assert!(
                db.query(&format!("SELECT * FROM {table} WHERE hub='work'"), &[])
                    .unwrap()
                    .is_empty(),
                "{table}"
            );
            assert_eq!(
                &db.query(&format!("SELECT * FROM {table} WHERE hub='local'"), &[])
                    .unwrap(),
                expected,
                "{table}"
            );
        }
        assert!(!f.state.join("oidc-token-work.json").exists());
        assert_eq!(
            fs::read_to_string(f.state.join("oidc-token-local.json")).unwrap(),
            "retained credential"
        );
    }
}

#[test]
fn dual_default_hub_aliases_allow_cli_removal_and_restart() {
    let _lock = serial();
    let f = Fixture::new(json!({
        "defaultHub":"work",
        "default_hub":"work",
        "hubs":{"local":{"url":"local://"},"work":{"url":"local://"}}
    }));
    let (ok, out, err) = f.command(
        NATIVE,
        &["setup", "--remove-hub", "work", "--no-login-item", "--json"],
        "",
    );
    assert!(ok, "{out}\n{err}");
    let saved: Value = read_json(&f.config);
    assert_eq!(saved["default_hub"], "local");
    assert!(saved.get("defaultHub").is_none());
    assert!(saved["hubs"].get("work").is_none());
    let (ok, out, err) = f.command(NATIVE, &["setup", "--status", "--json"], "");
    assert!(ok, "{out}\n{err}");
    let inspection: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(inspection["defaultHub"], "local");
    assert_eq!(inspection["hubs"]["local"]["url"], "local://");
    let daemon = f.daemon(&[]);
    let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
    assert_eq!(status, 200, "{snapshot}");
    assert_eq!(snapshot["hub"], "local");
    let (ok, out, err) = f.command(NATIVE, &["find", "--user", "absent", "--json"], "");
    assert!(ok, "{out}\n{err}");
    let found: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(found["ok"], true);
    assert_eq!(found["listeners"], json!([]));
    f.stop(daemon);
}
#[test]
fn reloading_disabled_defaults_selects_enabled_hubs_unless_overridden() {
    let _lock = serial();
    let f = Fixture::new(json!({"default_hub":"work","hubs":{"work":{"url":"local://"}}}));
    let daemon = f.daemon(&[]);
    assert_eq!(f.snapshot()["hub"], "work");
    for (key, hubs, expected) in [
        (
            "default_hub",
            json!({"work":{"url":"local://","enabled":false},"local":{"url":"local://"},"alpha":{"url":"local://"}}),
            "local",
        ),
        (
            "defaultHub",
            json!({"work":{"url":"local://","disabled":true},"local":{"url":"local://","enabled":false},"shared":{"url":"local://"},"zeta":{"url":"local://"}}),
            "shared",
        ),
    ] {
        let mut config = json!({"hubs":hubs});
        config[key] = json!("work");
        fs::write(&f.config, config.to_string()).unwrap();
        let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
        assert_eq!(status, 200, "{snapshot}");
        assert_eq!(snapshot["hub"], expected);
        assert!(snapshot["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["enabled"] != false && h["name"] != "work"));
        assert!(snapshot["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["name"] == "work" && h["enabled"] == false));
        let found = f.tool("cito_find", json!({"user":"absent"}), json!({}));
        assert_eq!(found["ok"], true, "{found}");
        assert_eq!(found["listeners"], json!([]));
        assert_eq!(fs::read_to_string(&f.config).unwrap(), config.to_string());
    }
    f.stop(daemon);
    let overridden = f.daemon(&[("CITO_HUB", "work")]);
    assert_eq!(f.snapshot()["hub"], "work");
    f.stop(overridden);
}
#[test]
fn disabled_hub_alias_reenable_paths_activate_slots_across_restart() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for op in ["setup_toggle_hub", "hub_action", "cli"] {
        let initial = json!({"defaultHub":"local","hubs":{
            "local":{"url":"local://","disabled":true},
            "work":{"url":"local://","enabled":true,"disabled":true}
        }});
        fs::write(&f.config, initial.to_string()).unwrap();
        let daemon = f.daemon(&[]);
        let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
        assert_eq!(status, 200, "{snapshot}");
        let work = snapshot["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hub| hub["name"] == "work")
            .unwrap();
        assert_eq!(work["enabled"], false, "{op}: {work}");
        assert_eq!(snapshot["hubs"], json!([]));
        assert!(
            !snapshot["settings"]["channels"]
                .as_array()
                .unwrap()
                .is_empty(),
            "disabled local still exposes channel inventory"
        );
        let unavailable = f.tool_result(
            "cito_find",
            json!({"hub":"work","user":"absent"}),
            json!({}),
        );
        assert_eq!(unavailable["isError"], true, "{unavailable}");
        if op == "cli" {
            let (ok, out, err) = f.command(
                NATIVE,
                &["setup", "--enable-hub", "work", "--json", "--no-login-item"],
                "",
            );
            assert!(ok, "{out}\n{err}");
        } else {
            let (status, response) =
                f.action(json!({"op":op,"hub":"work","enabled":true,"action":"reconnect"}));
            assert_eq!(status, 200, "{op}: {response}");
        }
        let saved: Value = read_json(&f.config);
        assert_eq!(saved["hubs"]["work"]["enabled"], true, "{op}: {saved}");
        assert!(
            saved["hubs"]["work"].get("disabled").is_none(),
            "{op}: {saved}"
        );
        assert_eq!(saved["default_hub"], "work", "{op}: {saved}");
        assert!(saved.get("defaultHub").is_none());
        for restart in [false, true] {
            let running = if restart { f.daemon(&[]) } else { daemon };
            let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
            assert_eq!(status, 200, "{snapshot}");
            assert_eq!(snapshot["hub"], "work", "{op}: {snapshot}");
            let work = snapshot["settings"]["hubs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|hub| hub["name"] == "work")
                .unwrap();
            assert_eq!(work["enabled"], true, "{op}: {work}");
            let found = f.tool(
                "cito_find",
                json!({"hub":"work","user":"absent"}),
                json!({}),
            );
            assert_eq!(found["ok"], true, "{op}: {found}");
            assert_eq!(found["listeners"], json!([]));
            f.stop(running);
        }
    }
}
#[test]
fn disabled_hub_alias_saves_preserve_effective_enablement() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"local":{"url":"local://"}}}));
    f.daemon(&[]);
    for (mut action, enabled) in [
        (json!({"disabled":true}), false),
        (json!({"enabled":true,"disabled":true}), false),
        (json!({"enabled":false,"disabled":false}), false),
        (json!({"enabled":true,"disabled":false}), true),
    ] {
        action["url"] = json!("local://");
        action["op"] = json!("setup_save_hub");
        action["name"] = json!("work");
        let (status, response) = f.action(action.clone());
        assert_eq!(status, 200, "{action}: {response}");
        let saved: Value = read_json(&f.config);
        assert_eq!(
            saved["hubs"]["work"]["enabled"], enabled,
            "{action}: {saved}"
        );
        assert!(
            saved["hubs"]["work"].get("disabled").is_none(),
            "{action}: {saved}"
        );
        let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
        assert_eq!(status, 200, "{snapshot}");
        let work = snapshot["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hub| hub["name"] == "work")
            .unwrap();
        assert_eq!(work["enabled"], enabled, "{action}: {work}");
        let result = f.tool_result(
            "cito_find",
            json!({"hub":"work","user":"absent"}),
            json!({}),
        );
        assert_eq!(result["isError"] == true, !enabled, "{action}: {result}");
    }
}
#[test]
fn hub_saves_reject_invalid_candidates_without_poisoning_daemon() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"local":{"url":"local://"}}}));
    let daemon = f.daemon(&[]);
    let before = fs::read(&f.config).unwrap();
    let invalid = [
        json!({"user":"alice smith"}),
        json!({"user":""}),
        json!({"user":"a".repeat(65)}),
        json!({"user":"alice@work"}),
        json!({"user":42}),
        json!({"user":false}),
        json!({"user":[]}),
        json!({"user":{}}),
        json!({"authMode":"token"}),
        json!({"authMode":"token","token":"   "}),
        json!({"authMode":"token","token":42}),
        json!({"authMode":"oidc","client_id":"client"}),
        json!({"authMode":"oidc","issuer":"http://id.example.com","client_id":"client"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com?query=1","client_id":"client"}),
        json!({"authMode":"oidc","issuer":42,"client_id":"client"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","client_id":"   "}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":42}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","scopes":"openid"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","scopes":["profile"]}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","scopes":["openid","invalid scope"]}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","redirectPort":65536}),
    ];
    for fields in &invalid {
        let mut action = json!({"op":"setup_save_hub","name":"work","url":"local://","auth_mode":"none","set_default":true});
        action
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let (status, response) = f.action(action.clone());
        assert_eq!(status, 400, "{action}: {response}");
        assert!(
            response["error"]
                .as_str()
                .unwrap()
                .starts_with("INVALID_INPUT:"),
            "{response}"
        );
        assert_eq!(fs::read(&f.config).unwrap(), before, "{action}");
        let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
        assert_eq!(status, 200, "{snapshot}");
        assert_eq!(snapshot["hub"], "local");
        assert_eq!(snapshot["activity"], json!([]));
        assert_eq!(snapshot["unread_count"], 0);
    }
    for user in [
        Value::Null,
        json!("a"),
        json!("Alice.Smith_1-"),
        json!("a".repeat(64)),
    ] {
        let (status, response) = f.action(
            json!({"op":"setup_save_hub","name":"work","url":"local://","auth_mode":"none","user":user}),
        );
        assert_eq!(status, 200, "{response}");
        let config: Value = read_json(&f.config);
        assert_eq!(config["hubs"]["work"]["user"], user);
    }
    for fields in [
        json!({"authMode":"token","token":"test-token"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","client_id":"client","scopes":["openid","profile"]}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","redirectPort":0}),
        json!({"authMode":"oidc","oidcIssuer":"https://id.example.com","oidcClientId":"client","oidcScopes":["openid"]}),
    ] {
        let mut cfg = fields.clone();
        cfg["url"] = json!("local://");
        cfg["enabled"] = json!(false);
        let (status, response) =
            f.action(json!({"op":"setup_save_hub","name":"work","config":cfg}));
        assert_eq!(status, 200, "{response}");
        let config: Value = read_json(&f.config);
        for (key, value) in fields.as_object().unwrap() {
            let canonical = match key.as_str() {
                "client_id" | "oidcClientId" => "clientId",
                "oidcIssuer" => "issuer",
                "oidcScopes" => "scopes",
                "redirect_port" | "oidcRedirectPort" => "redirectPort",
                "auth_mode" | "auth" => "authMode",
                key => key,
            };
            assert_eq!(&config["hubs"]["work"][canonical], value);
        }
        for key in [
            "client_id",
            "oidcIssuer",
            "oidcClientId",
            "oidcScopes",
            "redirect_port",
            "oidcRedirectPort",
            "auth_mode",
            "auth",
            "request_id",
        ] {
            assert!(config["hubs"]["work"].get(key).is_none());
        }
        let (ok, out, err) = f.command(NATIVE, &["setup", "--status", "--json"], "");
        assert!(ok, "{fields}: {err}");
        let inspection: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(inspection["hubs"]["work"]["authMode"], fields["authMode"]);
        assert_eq!(inspection["hubs"]["work"]["enabled"], false);
        let (ok, out, err) = f.command(NATIVE, &["find", "--user", "absent", "--json"], "");
        assert!(ok, "{fields}: {err}");
        let found: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(found["ok"], true);
        assert_eq!(found["listeners"], json!([]));
    }
    f.stop(daemon);
    f.daemon(&[]);
    f.action_ok(json!({"op":"set_auto_listen","hub":"local","enabled":false}));
    let config: Value = read_json(&f.config);
    assert_eq!(config["hubs"]["local"]["autoListen"], false);
}
