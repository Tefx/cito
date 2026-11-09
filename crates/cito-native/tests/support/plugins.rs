use super::support::*;

#[test]
fn cached_fingerprint_does_not_authorize_an_unrelated_process_command() {
    let _lock = serial();
    for (id, bin) in [("pi", PI), ("codex", CODEX), ("claude", CLAUDE)] {
        let f = Fixture::new(json!({}));
        let path = f.dir.join("unrelated.cjs");
        fs::write(&path, include_str!("endpoint.cjs")).unwrap();
        let p = f.spawn(
            "node",
            &[path.to_str().unwrap()],
            &[("FIXTURE_HARNESS", id)],
        );
        wait(|| f.frames(p).iter().any(|v| v["op"] == "ready"));
        if id == "codex" {
            let db = cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite"))
                .unwrap();
            db.execute("CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,updated_at INTEGER);INSERT INTO threads VALUES('session-a','/repo',0,1)",&[]).unwrap();
        }
        let mut request = json!({"op":"resolve_caller","endpoint_pid":f.pid(p),"cwd":"/repo","require_open":true});
        if id == "codex" {
            request["meta"] = json!({"threadId":"session-a"});
        }
        let response = f.plugin(bin, request.clone());
        assert_eq!(response["ok"], true, "{response}");
        f.send(p, json!({"op":"offline"}));
        wait(|| f.frames(p).iter().any(|v| v["op"] == "offline"));
        if id == "codex" {
            fs::remove_file(f.home.join(".codex/state_5.sqlite")).unwrap();
        }
        request["require_open"] = json!(false);
        request["cached_caller"] = response["caller"].clone();
        request["meta"] = json!({"expected_harness":id,"expected_session_id":"session-a"});
        let denied = f.plugin(bin, request);
        assert_eq!(denied["error_code"], "BINDING_REQUIRED");
        assert!(denied["message"]
            .as_str()
            .unwrap()
            .contains("process command does not match"));
    }
}
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
};

fn socket(path: &std::path::Path, request: Value) -> Value {
    let mut stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
#[test]
fn pi_package_setup_preserves_unrelated_settings_and_removes_legacy_mcp() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let pi = f.home.join(".pi/agent");
    fs::create_dir_all(pi.join("extensions")).unwrap();
    fs::create_dir_all(pi.join("mcp")).unwrap();
    let before = json!({"theme":"dark","packages":["npm:@tefx/cito","npm:other"]});
    fs::write(pi.join("settings.json"), before.to_string()).unwrap();
    fs::write(pi.join("extensions/cito-bridge.ts"), "legacy").unwrap();
    for p in ["mcp.json", "mcp/global.mcp.json"] {
        fs::write(pi.join(p),json!({"mcpServers":{"cito":{"command":"old"},"other":{"command":"keep"}},"extra":true}).to_string()).unwrap();
    }
    assert_eq!(
        f.plugin(PI, json!({"op":"setup_apply","dry_run":true}))["ok"],
        true
    );
    assert_eq!(
        serde_json::from_str::<Value>(&fs::read_to_string(pi.join("settings.json")).unwrap())
            .unwrap(),
        before
    );
    assert_eq!(f.plugin(PI, json!({"op":"setup_apply"}))["ok"], true);
    let v: Value =
        serde_json::from_str(&fs::read_to_string(pi.join("settings.json")).unwrap()).unwrap();
    assert_eq!(
        v,
        json!({"theme":"dark","packages":["npm:other",f.state.join("plugins/pi")]})
    );
    assert!(!pi.join("extensions/cito-bridge.ts").exists());
    for p in ["mcp.json", "mcp/global.mcp.json"] {
        let v: Value = serde_json::from_str(&fs::read_to_string(pi.join(p)).unwrap()).unwrap();
        assert_eq!(
            v,
            json!({"mcpServers":{"other":{"command":"keep"}},"extra":true})
        );
    }
    assert_eq!(
        f.plugin(PI, json!({"op":"setup_inspect"}))["configured"],
        true
    );
    assert!(f.plugin(PI, json!({"op":"setup_apply"}))["actions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["status"] == "unchanged"));
    assert_eq!(f.plugin(PI, json!({"op":"setup_remove"}))["ok"], true);
    let v: Value =
        serde_json::from_str(&fs::read_to_string(pi.join("settings.json")).unwrap()).unwrap();
    assert_eq!(v, json!({"theme":"dark","packages":["npm:other"]}));
}
#[test]
fn codex_multiline_toml_and_complete_subtree_removal() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = f.home.join(".codex/config.toml");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let prefix="model = \"default\"\nitems = [\n  \"one\", # comment\n  \"two\",\n]\nprompt = \"\"\"\n[mcp_servers.cito.fake]\nThis is a multiline string.\n\"\"\"\nliteral = '''\n[still.not.a.table]\n'''\n";
    fs::write(&p,format!("{prefix}[mcp_servers.cito]\ncommand = \"old\"\n[mcp_servers.cito.env]\nKEEP = \"old\"\n[mcp_servers.other]\ncommand = \"other\"\n")).unwrap();
    assert_eq!(f.plugin(CODEX, json!({"op":"setup_apply"}))["ok"], true);
    assert!(fs::read_to_string(&p).unwrap().contains(prefix));
    assert_eq!(f.plugin(CODEX, json!({"op":"setup_remove"}))["ok"], true);
    let text = fs::read_to_string(p).unwrap();
    assert!(text.contains(prefix));
    assert!(!text.contains("KEEP ="));
    assert!(text.contains("[mcp_servers.other]"));
}
#[test]
fn codex_metadata_validation_and_sqlite_thread_projection() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for meta in [
        json!({"openai/threadId":"thread-a"}),
        json!({"threadId":"thread-a"}),
        json!({"codexThreadId":"thread-a"}),
    ] {
        assert_eq!(
            f.plugin(
                CODEX,
                json!({"op":"resolve_caller","meta":meta,"cwd":"/repo"})
            )["caller"]["session_id"],
            "thread-a"
        );
    }
    for headers in [
        json!({"x-codex-turn-metadata":"{\"thread_id\":\"thread-a\"}"}),
        json!({"x-codex-turn-metadata":{"session_id":"thread-a"}}),
    ] {
        assert_eq!(
            f.plugin(CODEX, json!({"op":"resolve_caller","headers":headers}))["caller"]
                ["session_id"],
            "thread-a"
        );
    }
    for meta in [
        json!({"codexThreadId":"a","openai/threadId":"b"}),
        json!({"codexThreadId":"a","pi_pid":123}),
        json!({"endpoint_pid":1,"caller_pid":2}),
    ] {
        assert_eq!(
            f.plugin(CODEX, json!({"op":"resolve_caller","meta":meta}))["error_code"],
            "INVALID_INPUT"
        );
    }
    let db = cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite")).unwrap();
    db.execute("CREATE TABLE threads(id TEXT, cwd TEXT, archived INTEGER, recency_at_ms INTEGER, title TEXT, git_branch TEXT, git_origin_url TEXT); INSERT INTO threads VALUES('active','/repo',0,200,'Active work','main','git@example.com:acme/service.git'),('archived','/repo',1,300,'Archived','old','https://example.com/acme/service.git')",&[]).unwrap();
    let result = f.plugin(
        CODEX,
        json!({"op":"resolve_caller","cli_cmd":"listen","cwd":"/repo"}),
    );
    assert_eq!(result["caller"]["session_id"], "active");
    assert_eq!(result["caller"]["repo"], "acme/service");
    assert_eq!(result["caller"]["branch"], "main");
    assert_eq!(
        f.plugin(
            CODEX,
            json!({"op":"resolve_caller","meta":{"threadId":"archived"},"require_open":true})
        )["error_code"],
        "SESSION_NOT_OPEN"
    );
    assert_eq!(
        f.plugin(
            CODEX,
            json!({"op":"resolve_caller","meta":{"threadId":"archived"},"require_open":false})
        )["caller"]["session_id"],
        "archived"
    );
    assert_eq!(
        f.plugin(CODEX, json!({"op":"probe","session_id":"archived"}))["status"],
        "closed_definitive"
    );
}
#[test]
fn codex_queue_literal_arguments_and_open_retry_do_not_duplicate_insertion() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let queue = f.dir.join("queue");
    script(
        &queue,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$HOME/queue-args\"\n",
    );
    f.env
        .insert("CITO_CODEX_BIN".into(), queue.display().to_string());
    let request = json!({"op":"wake","session_id":"thread-a","text":"a message with spaces"});
    assert_eq!(f.plugin(CODEX, request.clone())["outcome"], "delivered");
    assert_eq!(
        fs::read_to_string(f.home.join("queue-args")).unwrap(),
        "queue\n--resume\n--thread\nthread-a\n--message\na message with spaces\n"
    );
    assert!(fs::read_to_string(f.home.join("mock-calls"))
        .unwrap()
        .contains("codex://threads/wake/thread-a"));
    let mut retry = request;
    retry["queue_written"] = json!(true);
    let result = f.plugin(CODEX, retry.clone());
    assert_eq!(result["outcome"], "delivered_unconfirmed");
    assert_eq!(result["open_confirmed"], true);
    script(
        std::path::Path::new(&f.env["CITO_OPEN_BIN"]),
        "#!/bin/sh\nexit 1\n",
    );
    assert_eq!(f.plugin(CODEX, retry)["open_confirmed"], false);
    assert_eq!(
        fs::read_to_string(f.home.join("queue-args"))
            .unwrap()
            .lines()
            .count(),
        6
    );
}
#[test]
fn claude_setup_preserves_foreign_hooks_and_installs_absolute_paths() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let path = f.home.join(".claude/settings.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path,json!({"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"/old/cito-claude-hook"},{"type":"command","command":"keep"}]}]}}).to_string()).unwrap();
    assert_eq!(f.plugin(CLAUDE, json!({"op":"setup_apply"}))["ok"], true);
    let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        v["mcpServers"]["cito"]["command"],
        json!(f.state.join("bin/cito-mcp"))
    );
    assert!(v.to_string().contains(
        f.state
            .join("plugins/claude/bin/cito-claude-hook")
            .to_str()
            .unwrap()
    ));
    assert_eq!(f.plugin(CLAUDE, json!({"op":"setup_remove"}))["ok"], true);
    let v: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        v,
        json!({"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep"}]}]}})
    );
}
#[test]
fn claude_flock_torn_lines_concurrent_drain_and_passive_stop() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let queue = f.state.join("claude/wakes/session-a.jsonl");
    fs::create_dir_all(queue.parent().unwrap()).unwrap();
    fs::write(&queue, "{\"batch_id\":\"torn\"").unwrap();
    // Bounded four-way concurrency exercises the file lock without creating a process storm.
    let mut contexts = String::new();
    for chunk in 0..6 {
        let mut children = Vec::new();
        for i in chunk * 4..chunk * 4 + 4 {
            let p = f.spawn(CLAUDE, &[], &[]);
            f.send(p,json!({"op":"wake","state_dir":f.state,"env":f.env,"session_id":"session-a","batch_id":format!("b-{i}"),"text":format!("token-{i:03}"),"wake":false}));
            f.eof(p);
            children.push(p);
        }
        let result = f.plugin(
            HOOK,
            json!({"hook_event_name":"UserPromptSubmit","session_id":"session-a","cwd":"/repo"}),
        );
        contexts.push_str(
            result["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap_or(""),
        );
        for p in children {
            wait(|| f.exited(p));
            f.stop(p);
        }
    }
    let result = f.plugin(
        HOOK,
        json!({"hook_event_name":"Stop","session_id":"session-a","cwd":"/repo"}),
    );
    assert!(result.get("decision").is_none());
    contexts.push_str(
        result["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or(""),
    );
    for i in 0..24 {
        assert_eq!(
            contexts.matches(&format!("token-{i:03}")).count(),
            1,
            "{contexts}"
        );
    }
    f.plugin(CLAUDE,json!({"op":"wake","session_id":"session-a","batch_id":"wake","text":"continue","wake":true}));
    assert_eq!(
        f.plugin(
            HOOK,
            json!({"hook_event_name":"Stop","session_id":"session-a"})
        ),
        json!({"decision":"block","reason":"continue"})
    );
    assert_eq!(
        f.plugin(
            HOOK,
            json!({"hook_event_name":"Stop","session_id":"session-a"})
        ),
        json!({})
    );
}
#[test]
fn claude_hook_ack_prefers_environment_daemon_port_over_config() {
    use std::{io::Read, net::TcpListener, time::Duration};

    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let environment = TcpListener::bind("127.0.0.1:0").unwrap();
    let configured = TcpListener::bind("127.0.0.1:0").unwrap();
    environment.set_nonblocking(true).unwrap();
    configured.set_nonblocking(true).unwrap();
    f.env.insert(
        "CITO_DAEMON_PORT".into(),
        format!(" {} ", environment.local_addr().unwrap().port()),
    );
    fs::write(
        &f.config,
        json!({"daemonPort":configured.local_addr().unwrap().port()}).to_string(),
    )
    .unwrap();
    f.plugin(CLAUDE, json!({"op":"wake","session_id":"session-a","batch_id":"ack-port","text":"pending","wake":false}));
    let hook = f.spawn(HOOK, &[], &[]);
    f.send(
        hook,
        json!({"hook_event_name":"UserPromptSubmit","session_id":"session-a","cwd":"/repo"}),
    );
    f.eof(hook);
    let mut connection = None;
    wait(|| match environment.accept() {
        Ok((stream, _)) => {
            connection = Some(stream);
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            assert!(
                !f.exited(hook),
                "hook exited without acknowledging to CITO_DAEMON_PORT: {}",
                f.output(hook, true)
            );
            false
        }
        Err(e) => panic!("environment listener: {e}"),
    });
    let stream = connection.unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "POST /mcp HTTP/1.1\r\n");
    let mut length = None;
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<u64>().unwrap());
            }
        }
    }
    let ack: Value = serde_json::from_reader(reader.by_ref().take(length.unwrap())).unwrap();
    assert_eq!(
        ack,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"cito_inbox","arguments":{"ack_all":false,"ack_ids":["ack-port"]},"_meta":{"claudeSessionId":"session-a","cwd":"/repo"}}})
    );
    reader
        .get_mut()
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
        .unwrap();
    wait(|| f.exited(hook));
    f.stop(hook);
    assert_eq!(
        f.frames(hook),
        vec![
            json!({"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"pending"}})
        ]
    );
    assert_eq!(f.output(hook, true), "");
    assert!(matches!(configured.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}
#[test]
fn claude_sidecar_socket_dedup_session_fencing_and_eof_cleanup() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let endpoint = std::process::id();
    let p = f.spawn(CLAUDE, &[], &[]);
    let mut env = f.env.clone();
    env.insert("CLAUDE_SESSION_ID".into(), "session-a".into());
    f.send(p,json!({"op":"stdio_sidecar","state_dir":f.state,"env":env,"endpoint_pid":endpoint,"cwd":"/repo"}));
    wait(|| f.frames(p).iter().any(|v| v["op"] == "ready"));
    assert_eq!(
        f.frames(p)[0]["capabilities_patch"]["experimental"]["claude/channel"],
        json!({})
    );
    let path = f.state.join(format!("sockets/claude-pid-{endpoint}.sock"));
    assert_eq!(
        socket(&path, json!({"op":"identify"}))["session_id"],
        "session-a"
    );
    socket(
        &path,
        json!({"op":"status","session_id":"session-a","badge":"badge"}),
    );
    let frame = json!({"op":"deliver","session_id":"session-a","batch_id":"batch","content":"literal <card>","wake":true});
    assert_eq!(socket(&path, frame.clone())["deduplicated"], false);
    assert_eq!(socket(&path, frame)["deduplicated"], true);
    assert_eq!(
        socket(
            &path,
            json!({"op":"deliver","session_id":"other","batch_id":"batch","content":"stale","wake":true})
        )["error"],
        "SESSION_MISMATCH"
    );
    assert_eq!(
        fs::read_to_string(f.state.join("claude/wakes/session-a.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    f.eof(p);
    wait(|| f.exited(p));
    assert!(!path.exists());
    assert!(!f.state.join("claude/badges/session-a").exists());
}
#[test]
fn pi_bridge_real_typescript_host_contract_without_npm() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let file = root().join("crates/cito-native/tests/support/pi-bridge.mjs");
    let (ok, out, err) = f.command(
        "node",
        &["--experimental-strip-types", file.to_str().unwrap()],
        "",
    );
    assert!(ok, "{out}\n{err}");
}
#[test]
fn empty_and_unbound_hooks_are_silent_and_malformed_input_preserves_queue() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for input in ["", "{\"hook_event_name\":\"Stop\",\"cwd\":\"/repo\"}\n"] {
        let (ok, out, err) = f.command(HOOK, &[], input);
        assert!(ok);
        assert_eq!(out, "{}\n");
        assert_eq!(err, "");
    }
    f.plugin(CLAUDE, json!({"op":"wake","session_id":"session-a","batch_id":"preserve","text":"pending","wake":true}));
    let queue = f.state.join("claude/wakes/session-a.jsonl");
    let before = fs::read(&queue).unwrap();
    let (ok, out, err) = f.command(HOOK, &[], "not-json\n");
    assert!(ok);
    assert_eq!(out, "{}\n");
    assert!(err.starts_with("cito-claude-hook:"));
    assert_eq!(fs::read(queue).unwrap(), before);
}
#[test]
fn explicit_pid_claims_reject_invalid_conflicting_and_unreachable_endpoints() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for bin in [PI, CODEX, CLAUDE] {
        for meta in [
            json!({"pi_pid":0}),
            json!({"pi_pid":-1}),
            json!({"pi_pid":0.5}),
            json!({"pi_pid":"12"}),
            json!({"pi_pid":123,"caller_pid":124}),
        ] {
            assert_ne!(
                f.plugin(bin, json!({"op":"resolve_caller","meta":meta}))["ok"],
                true
            );
        }
    }
    assert_eq!(
        f.plugin(
            PI,
            json!({"op":"resolve_caller","meta":{"pi_pid":2147483647}})
        )["ok"],
        false
    );
}

#[test]
fn telegram_token_rotation_discards_pending_updates_and_restarts_offset_zero() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let api = root().join("crates/cito-native/tests/support/telegram-api.py");
    let server = f.spawn("python3", &[api.to_str().unwrap()], &[]);
    wait(|| !f.frames(server).is_empty());
    let port = f.frames(server)[0]["port"].as_u64().unwrap();
    let p = f.spawn(TELEGRAM, &[], &[]);
    let config =
        |token: &str| json!({"botToken":token,"apiBaseUrl":format!("http://127.0.0.1:{port}")});
    f.send(
        p,
        json!({"op":"configure","hub_config":config("old"),"cursor":"10"}),
    );
    wait(|| f.frames(p).iter().any(|v| v["op"] == "inbound_reply"));
    // Both pairing and reply updates await acknowledgments; a new token must discard them.
    f.send(
        p,
        json!({"op":"configure","hub_config":config("rotated"),"cursor":"999"}),
    );
    wait(|| {
        fs::read_to_string(f.home.join("api-polls.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .any(|v| v["path"] == "/botrotated/getUpdates" && v["offset"] == 0)
    });
    f.send(
        p,
        json!({"op":"pair_ack","code":"cito-abcdefghij","ok":true,"owner_user":"alice"}),
    );
    f.send(
        p,
        json!({"op":"reply_ack","update_id":"15","ok":true,"target_display":"sender"}),
    );
    f.send(p,json!({"op":"format_pair","req_id":"barrier","code":"cito-new","hub_config":config("rotated")}));
    wait(|| f.frames(p).iter().any(|v| v["req_id"] == "barrier"));
    assert!(
        !f.home.join("api-sends.jsonl").exists(),
        "stale identity maps must not send confirmations for the previous token"
    );
    f.eof(p);
    wait(|| f.exited(p));
}
#[test]
fn telegram_persistent_pair_reply_cursor_and_unicode_chunk_contract() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let api = root().join("crates/cito-native/tests/support/telegram-api.py");
    let server = f.spawn("python3", &[api.to_str().unwrap()], &[]);
    wait(|| !f.frames(server).is_empty());
    let port = f.frames(server)[0]["port"].as_u64().unwrap();
    let config = json!({"botToken":"test","apiBaseUrl":format!("http://127.0.0.1:{port}")});
    let p = f.spawn(TELEGRAM, &[], &[]);
    f.send(
        p,
        json!({"op":"format_pair","req_id":"pair","code":"cito-abcdefghij","hub_config":config}),
    );
    wait(|| f.frames(p).iter().any(|v| v["op"] == "rpc_result"));
    assert_eq!(
        f.frames(p)
            .iter()
            .find(|v| v["op"] == "rpc_result")
            .unwrap()["instruction"],
        "Send \"/start cito-abcdefghij\" to @example_bot"
    );
    f.send(
        p,
        json!({"op":"configure","hub_config":config,"bindings":[],"cursor":"10"}),
    );
    wait(|| f.frames(p).iter().any(|v| v["op"] == "pair_redeem"));
    wait(|| f.frames(p).iter().any(|v| v["op"] == "inbound_reply"));
    assert!(f
        .frames(p)
        .iter()
        .filter(|v| v["op"] == "cursor_save")
        .all(|v| v["cursor"].as_str().unwrap().parse::<u64>().unwrap() <= 12));
    f.send(
        p,
        json!({"op":"pair_ack","code":"cito-abcdefghij","ok":true,"owner_user":"alice"}),
    );
    f.send(
        p,
        json!({"op":"reply_ack","update_id":"15","ok":true,"target_display":"alice-agent"}),
    );
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "cursor_save" && v["cursor"] == "16")
    });
    f.send(p,json!({"op":"deliver","delivery_id":"large","config":{"chatId":"9"},"signal":{"from":{"title":"work"},"message":"😀".repeat(9000)}}));
    wait(|| f.frames(p).iter().any(|v| v["op"] == "delivered"));
    let frame = f
        .frames(p)
        .into_iter()
        .find(|v| v["op"] == "delivered")
        .unwrap();
    assert_eq!(frame["ok"], true);
    assert_eq!(frame["external_msg_ids"].as_array().unwrap().len(), 3);
    let sent = fs::read_to_string(f.home.join("api-sends.jsonl")).unwrap();
    for line in sent.lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        assert!(v["text"].as_str().unwrap().chars().count() <= 4096);
    }
    f.eof(p);
    wait(|| f.exited(p));
}

#[test]
fn cached_pid_identity_requires_same_live_invocation_and_plugin_process_command() {
    let _lock = serial();
    for (id, bin, entry) in [
        (
            "pi",
            PI,
            "node_modules/@example/pi-coding-agent/dist/cli.js",
        ),
        ("codex", CODEX, "node_modules/@example/codex/bin/codex.js"),
        ("claude", CLAUDE, "node_modules/@example/claude-code/cli.js"),
    ] {
        let f = Fixture::new(json!({}));
        let path = f.dir.join(entry);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, include_str!("endpoint.cjs")).unwrap();
        let p = f.spawn(
            "node",
            &[path.to_str().unwrap()],
            &[("FIXTURE_HARNESS", id)],
        );
        wait(|| f.frames(p).iter().any(|v| v["op"] == "ready"));
        let endpoint = f.pid(p);
        if id == "codex" {
            let db = cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite"))
                .unwrap();
            db.execute("CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,updated_at INTEGER);INSERT INTO threads VALUES('session-a','/repo',0,1)",&[]).unwrap();
        }
        let mut request = json!({"op":"resolve_caller","endpoint_pid":endpoint,"cwd":"/repo","require_open":true});
        if id == "codex" {
            request["meta"] = json!({"threadId":"session-a"});
        }
        let resolved = f.plugin(bin, request.clone());
        assert_eq!(resolved["ok"], true, "{resolved}");
        let cached = resolved["caller"].clone();
        assert!(cached["process_identity"]["command"]
            .as_str()
            .unwrap()
            .contains(path.to_str().unwrap()));
        f.send(p, json!({"op":"offline"}));
        wait(|| f.frames(p).iter().any(|v| v["op"] == "offline"));
        if id == "codex" {
            fs::remove_file(f.home.join(".codex/state_5.sqlite")).unwrap();
        }
        request["require_open"] = json!(false);
        request["cached_caller"] = cached.clone();
        request["meta"] = json!({"expected_harness":id,"expected_session_id":"session-a"});
        assert_eq!(f.plugin(bin, request.clone())["caller"], cached);
        for key in ["command", "started_at"] {
            let mut stale = request.clone();
            stale["cached_caller"]["process_identity"][key] = json!("another invocation");
            assert_eq!(f.plugin(bin, stale)["error_code"], "BINDING_REQUIRED");
        }
        let mut wrong = request.clone();
        wrong["meta"]["expected_session_id"] = json!("other");
        assert_eq!(f.plugin(bin, wrong)["error_code"], "INVALID_INPUT");
        f.stop(p);
        assert_eq!(f.plugin(bin, request)["error_code"], "BINDING_REQUIRED");
    }
}
