use super::*;

fn fence_endpoint(f: &Fixture, harness: &str) -> usize {
    let app = f.dir.join(format!("{harness}.mjs"));
    fs::write(&app, "console.log('ready'); setInterval(()=>{},1000)").unwrap();
    let endpoint = f.spawn("node", &[app.to_str().unwrap()], &[]);
    wait(|| f.output(endpoint, false).contains("ready"));
    endpoint
}

fn claude_sidecar(f: &Fixture) -> (u32, usize) {
    let app = f.dir.join("claude.mjs");
    fs::write(&app, "console.log('ready'); setInterval(()=>{},1000)").unwrap();
    let endpoint = spawn_interactive(f, "node", &[app.to_str().unwrap()], &[]);
    wait(|| f.output(endpoint, false).contains("ready"));
    let pid = f.pid(endpoint);
    let sidecar = f.spawn(CLAUDE, &[], &[]);
    let mut env = f.env.clone();
    env.insert("CLAUDE_SESSION_ID".into(), "claude-session".into());
    f.send(sidecar, json!({"op":"stdio_sidecar","state_dir":f.state,"env":env,"endpoint_pid":pid,"cwd":"/repo"}));
    wait(|| {
        assert!(
            !f.exited(sidecar),
            "Sidecar exited: {:?} {}",
            f.frames(sidecar),
            f.output(sidecar, true)
        );
        f.frames_for(sidecar, "ready").next().is_some()
    });
    (pid, sidecar)
}

#[test]
fn stdio_transport_provenance_allows_real_thread_provider_but_fences_bridge_claims() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let source =
        cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite")).unwrap();
    source.execute("CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,recency_at_ms INTEGER); INSERT INTO threads VALUES('thread-a','/repo',0,1)", &[]).unwrap();
    for (id, bin) in [("codex", CODEX), ("pi", PI), ("claude", CLAUDE)] {
        install_provider_command(&f, id, json!([bin]));
    }
    let (claude_pid, sidecar) = claude_sidecar(&f);
    let foreign = f.plugin(
        CODEX,
        json!({"op":"resolve_caller","cwd":"/repo",
        "meta":{"_stdio":{"endpoint_pid":claude_pid}}}),
    );
    assert_eq!(foreign["ok"], false, "{foreign}");
    assert_eq!(foreign["error_code"], "BINDING_REQUIRED", "{foreign}");
    let request = json!({"op":"resolve_caller","meta":{"endpoint_pid":std::process::id(),"threadId":"thread-a"}});
    let pi_rejection = f.plugin(PI, request.clone());
    assert_eq!(
        pi_rejection["error_code"], "BINDING_REQUIRED",
        "{pi_rejection}"
    );
    let codex_rejection = f.plugin(CODEX, request);
    assert_eq!(
        codex_rejection["error_code"], "INVALID_INPUT",
        "{codex_rejection}"
    );
    for (meta, code) in [
        (json!({"threadId":"thread-a"}), "INVALID_INPUT"),
        (
            json!({"expected_harness":"codex","expected_session_id":"thread-a"}),
            "BINDING_REQUIRED",
        ),
    ] {
        let reply = f.plugin(
            CODEX,
            json!({"op":"resolve_caller","endpoint_pid":std::process::id(),"meta":meta}),
        );
        assert_eq!(reply["ok"], false, "{reply}");
        assert_eq!(reply["error_code"], code, "{reply}");
    }
    let expected = json!({"expected_harness":"codex","endpoint_pid":std::process::id(),"expected_session_id":"thread-a"});
    let direct = f.plugin(
        CODEX,
        json!({"op":"resolve_caller","explicit_session_id":"thread-a","meta":expected}),
    );
    assert_eq!(direct["error_code"], "BINDING_REQUIRED", "{direct}");
    let claude_meta =
        json!({"endpoint_pid":claude_pid,"expected_session_id":"claude-session","cwd":"/repo"});
    let direct = f.plugin(
        CODEX,
        json!({"op":"resolve_caller","explicit_session_id":"claude-session","meta":claude_meta}),
    );
    assert_eq!(direct["error_code"], "BINDING_REQUIRED", "{direct}");
    let direct = f.plugin(
        CLAUDE,
        json!({"op":"resolve_caller","explicit_session_id":"claude-session","meta":claude_meta}),
    );
    assert_eq!(direct["caller"]["harness"], "claude", "{direct}");
    // Incompatible harness metadata cannot seed the thread provider directly.
    let direct = f.plugin(CODEX, json!({"op":"resolve_caller","explicit_session_id":"claude-session","meta":{"expected_harness":"claude"}}));
    assert_eq!(direct["ok"], false, "{direct}");
    assert_eq!(direct["error_code"], "INVALID_INPUT", "{direct}");
    for meta in [json!({}), json!({"expected_session_id":"thread-a"})] {
        let direct = f.plugin(CLAUDE, json!({"op":"resolve_caller","endpoint_pid":std::process::id(),"explicit_session_id":"thread-a","meta":meta}));
        assert_eq!(direct["error_code"], "BINDING_REQUIRED", "{direct}");
    }
    f.daemon(&[]);
    let routed = f.tool(
        "cito_listen",
        json!({"about":"Unfenced transport routing"}),
        json!({"_stdio":{"endpoint_pid":claude_pid},"cwd":"/repo"}),
    );
    assert_eq!(routed["ok"], true, "{routed}");
    assert_eq!(
        f.rows("SELECT * FROM local_listeners WHERE harness='codex'")
            .len(),
        0
    );
    assert_eq!(
        f.rows("SELECT * FROM local_listeners WHERE harness='claude'")[0]["session_id"],
        "claude-session"
    );
    // Explicit thread metadata seeds a Codex cache entry for this same transport.
    let seeded = f.tool("cito_status", json!({}),
        json!({"expected_harness":"codex","threadId":"thread-a","_stdio":{"endpoint_pid":claude_pid},"cwd":"/repo"}));
    assert_eq!(seeded["ok"], true, "{seeded}");
    assert_eq!(seeded["registered"], false, "{seeded}");
    source.execute("DELETE FROM threads", &[]).unwrap();
    let routed = f.tool("cito_status", json!({}),
        json!({"_stdio":{"endpoint_pid":claude_pid},"expected_session_id":"claude-session","cwd":"/repo"}));
    assert_eq!(routed["ok"], true, "{routed}");
    assert_eq!(routed["registered"], true, "{routed}");
    // A stale command must reject the cache before checking conflicting metadata.
    for (bin, harness) in [(CODEX, "codex"), (CLAUDE, "claude"), (PI, "pi")] {
        let cached =
            json!({"harness":harness,"session_id":"thread-a","endpoint_pid":std::process::id()});
        let direct = f.plugin(bin, json!({"op":"resolve_caller","require_open":false,
            "cached_caller":cached,"meta":{"_stdio":{"endpoint_pid":std::process::id()},"expected_session_id":"other-session"}}));
        assert_eq!(
            direct["error_code"], "BINDING_REQUIRED",
            "{harness}: {direct}"
        );
    }
    source
        .execute("INSERT INTO threads VALUES('thread-a','/repo',0,1)", &[])
        .unwrap();
    let uncached = f.plugin(
        CODEX,
        json!({"op":"resolve_caller","require_open":false,
        "cached_caller":null,"cwd":"/repo","meta":{"expected_session_id":"thread-a"}}),
    );
    assert_eq!(uncached["ok"], true, "{uncached}");
    assert_eq!(uncached["caller"]["session_id"], "thread-a", "{uncached}");
    let rejected = f.tool_result("cito_status", json!({}), expected);
    assert_eq!(rejected["isError"], true, "{rejected}");
    assert!(
        rejected["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("BINDING_REQUIRED"),
        "{rejected}"
    );
    let routed = f.tool_result(
        "cito_listen",
        json!({"about":"Cross-provider expected session routing"}),
        claude_meta,
    );
    assert_ne!(routed["isError"], true, "{routed}");
    let routed: Value =
        serde_json::from_str(routed["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(routed["ok"], true, "{routed}");
    let routed = f.tool("cito_listen", json!({"about":"Unharnessed transport session routing"}),
        json!({"_stdio":{"endpoint_pid":claude_pid},"expected_session_id":"claude-session","cwd":"/repo"}));
    assert_eq!(routed["ok"], true, "{routed}");
    let rejected = f.tool_result(
        "cito_status",
        json!({}),
        json!({"endpoint_pid":std::process::id(),"expected_session_id":"thread-a"}),
    );
    assert_eq!(rejected["isError"], true, "{rejected}");
    assert!(
        rejected["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("BINDING_REQUIRED"),
        "{rejected}"
    );
    let rows = f.rows("SELECT * FROM local_listeners WHERE harness='claude'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["session_id"], "claude-session");
    for thread in [false, true] {
        let mut meta = json!({"_stdio":{"endpoint_pid":std::process::id()},"cwd":"/repo"});
        if thread {
            meta["threadId"] = json!("thread-a");
        }
        let result = f.tool(
            "cito_listen",
            json!({"about":"Transport identity regression"}),
            meta,
        );
        assert_eq!(result["ok"], true, "{result}");
        let rows = f.rows("SELECT * FROM local_listeners WHERE harness='codex'");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["session_id"], "thread-a");
    }
    let explicit = f.tool("cito_listen", json!({"about":"New thread before database projection"}),
        json!({"_stdio":{"endpoint_pid":std::process::id()},"threadId":"not-yet-indexed","cwd":"/repo"}));
    assert_eq!(explicit["ok"], true);
    assert_eq!(
        f.rows("SELECT * FROM local_listeners WHERE session_id='not-yet-indexed'")
            .len(),
        1
    );
    let rejected = f.tool_result(
        "cito_status",
        json!({}),
        json!({"endpoint_pid":std::process::id(),"threadId":"thread-a"}),
    );
    assert_eq!(rejected["isError"], true, "{rejected}");
    assert!(
        rejected["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("INVALID_INPUT"),
        "{rejected}"
    );
    let rejected = f.tool_result(
        "cito_status",
        json!({}),
        json!({"expected_harness":"codex","endpoint_pid":std::process::id(),"threadId":"thread-a"}),
    );
    assert_eq!(rejected["isError"], true, "{rejected}");
    assert!(
        rejected["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("INVALID_INPUT"),
        "{rejected}"
    );
    f.eof(sidecar);
    wait(|| f.exited(sidecar));
}

#[test]
fn cached_generic_session_fences_allow_all_providers_to_continue() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for (bin, harness) in [(CODEX, "codex"), (CLAUDE, "claude"), (PI, "pi")] {
        let endpoint = fence_endpoint(&f, harness);
        let pid = f.pid(endpoint);
        let cached = cito_native::support::remember_process(
            json!({"harness":harness,"session_id":"cached-session","endpoint_pid":pid}),
        );
        assert!(cached["process_identity"].is_object());
        let binding = f.state.join(format!("claude/endpoints/{pid}.json"));
        if harness == "claude" {
            fs::create_dir_all(binding.parent().unwrap()).unwrap();
            fs::write(
                &binding,
                json!({"session_id":"cached-session","cwd":"/repo",
                    "start":cito_native::support::process_start(pid),
                    "process_identity":cached["process_identity"]})
                .to_string(),
            )
            .unwrap();
        }
        for key in ["meta", "_meta"] {
            let mut req = json!({"op":"resolve_caller","require_open":false,"cwd":"/repo",
                "http_client_pid":pid,"http_transport":true,"cached_caller":cached});
            req[key] = json!({"expected_session_id":"other-session"});
            let direct = f.plugin(bin, req.clone());
            assert_eq!(
                direct["error_code"], "BINDING_REQUIRED",
                "{harness}/{key}: {direct}"
            );
            // A harness fence turns the same live-process mismatch into a conflict.
            for harness_key in ["meta", "_meta"] {
                let mut fenced = req.clone();
                if !fenced[harness_key].is_object() {
                    fenced[harness_key] = json!({});
                }
                fenced[harness_key]["expected_harness"] = json!(harness);
                let direct = f.plugin(bin, fenced.clone());
                assert_eq!(
                    direct["error_code"], "INVALID_INPUT",
                    "{harness}/{key}/{harness_key}: {direct}"
                );
                fenced["cached_caller"]["process_identity"]["started_at"] = json!("old-invocation");
                let stale = f.plugin(bin, fenced);
                assert_eq!(
                    stale["error_code"], "BINDING_REQUIRED",
                    "{harness}: {stale}"
                );
            }
        }
        if harness == "claude" {
            let req = json!({"op":"resolve_caller","require_open":false,
                "http_client_pid":pid,"http_transport":true,"cached_caller":cached,"cwd":"/repo"});
            let unchanged = f.plugin(bin, req.clone());
            assert_eq!(
                unchanged["caller"]["session_id"], "cached-session",
                "{unchanged}"
            );
            for key in ["meta", "_meta"] {
                for metadata in [
                    json!({"claudeSessionId":"resumed-session"}),
                    json!({"expected_harness":"claude","expected_session_id":"resumed-session"}),
                ] {
                    let mut fenced = req.clone();
                    fenced[key] = metadata;
                    let conflict = f.plugin(bin, fenced);
                    assert_eq!(conflict["error_code"], "INVALID_INPUT", "{conflict}");
                }
            }
            let mut changed: Value = read_json(&binding);
            changed["session_id"] = json!("resumed-session");
            fs::write(&binding, changed.to_string()).unwrap();
            let switched = f.plugin(bin, req.clone());
            assert_eq!(switched["error_code"], "BINDING_REQUIRED", "{switched}");
            for key in ["meta", "_meta"] {
                for metadata in [
                    json!({"claudeSessionId":"resumed-session"}),
                    json!({"expected_harness":"claude","expected_session_id":"resumed-session"}),
                ] {
                    let mut fenced = req.clone();
                    fenced[key] = metadata;
                    let switched = f.plugin(bin, fenced.clone());
                    assert_eq!(switched["error_code"], "BINDING_REQUIRED", "{switched}");
                    assert_eq!(
                        switched["message"], "Endpoint session binding changed",
                        "{switched}"
                    );
                    let mut malformed = fenced.clone();
                    malformed["cached_caller"]["session_id"] = json!("");
                    let rejected = f.plugin(bin, malformed);
                    assert_eq!(rejected["error_code"], "INVALID_INPUT", "{rejected}");
                    let mut stale = fenced.clone();
                    stale["cached_caller"]["process_identity"]["started_at"] =
                        json!("old-invocation");
                    let rejected = f.plugin(bin, stale);
                    assert_eq!(rejected["error_code"], "BINDING_REQUIRED", "{rejected}");
                    assert_ne!(
                        rejected["message"], "Endpoint session binding changed",
                        "{rejected}"
                    );
                    fenced["cached_caller"] = Value::Null;
                    let resolved = f.plugin(bin, fenced);
                    assert_eq!(
                        resolved["caller"]["session_id"], "resumed-session",
                        "{resolved}"
                    );
                }
            }
            changed["start"] = json!("old-invocation");
            fs::write(&binding, changed.to_string()).unwrap();
            let obsolete = f.plugin(bin, req);
            assert_eq!(
                obsolete["caller"]["session_id"], "cached-session",
                "{obsolete}"
            );
        }
        f.stop(endpoint);
    }
}

#[test]
fn provider_session_metadata_has_envelope_parity() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for (bin, harness, keys) in [
        (
            CLAUDE,
            "claude",
            &["claudeSessionId", "sessionId", "claude_session_id"][..],
        ),
        (
            CODEX,
            "codex",
            &["openai/threadId", "threadId", "codexThreadId"][..],
        ),
    ] {
        let mut metadata: Vec<Value> = keys
            .iter()
            .map(|key| json!({*key:"selected-session"}))
            .collect();
        metadata.push(json!({"expected_harness":harness,"expected_session_id":"selected-session"}));
        metadata.push(json!({"expected_harness":format!(" {harness} "),"expected_session_id":"selected-session"}));
        if harness == "codex" {
            metadata.push(json!({"x-codex-turn-metadata":"{\"thread_id\":\"selected-session\"}"}));
        }
        for envelope in ["meta", "_meta"] {
            for fields in &metadata {
                let mut req = json!({"op":"resolve_caller","require_open":false,"cwd":"/repo","http_transport":true});
                req[envelope] = fields.clone();
                let resolved = f.plugin(bin, req.clone());
                assert_eq!(
                    resolved["caller"]["session_id"], "selected-session",
                    "{harness}/{envelope}/{fields}: {resolved}"
                );
                let other = if envelope == "meta" { "_meta" } else { "meta" };
                req[other] = json!({keys[0]:"conflicting-session"});
                let rejected = f.plugin(bin, req);
                assert_eq!(rejected["error_code"], "INVALID_INPUT", "{rejected}");
            }
        }
    }
    let source =
        cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite")).unwrap();
    source.execute("CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,git_branch TEXT); INSERT INTO threads VALUES('indexed-session','/repo',0,'main')", &[]).unwrap();
    let req =
        json!({"op":"resolve_caller","require_open":false,"cwd":"/repo","http_transport":true});
    let unfenced = f.plugin(CODEX, req.clone());
    assert_eq!(unfenced["error_code"], "BINDING_REQUIRED", "{unfenced}");
    for envelope in ["meta", "_meta"] {
        let mut fenced = req.clone();
        fenced[envelope] = json!({"expected_harness":" codex "});
        let resolved = f.plugin(CODEX, fenced);
        assert_eq!(
            resolved["caller"]["session_id"], "indexed-session",
            "{resolved}"
        );
    }
    for sql in [
        "UPDATE threads SET id=' selected-session '",
        "ALTER TABLE threads DROP COLUMN git_branch",
    ] {
        source.execute(sql, &[]).unwrap();
        for envelope in ["meta", "_meta"] {
            let mut req = json!({"op":"resolve_caller","require_open":false,"cwd":"/repo"});
            req[envelope] = json!({"expected_session_id":"selected-session"});
            let rejected = f.plugin(CODEX, req);
            assert_eq!(
                rejected["error_code"], "BINDING_REQUIRED",
                "{sql}: {rejected}"
            );
        }
    }
    source
        .execute("UPDATE threads SET id='selected-session'", &[])
        .unwrap();
    for (bin, harness) in [(PI, "pi"), (CODEX, "codex"), (CLAUDE, "claude")] {
        // Execute real providers below a recognizable harness. HTTP transport
        // metadata must suppress that ancestry, while a client PID selects it.
        let app = f.dir.join(format!("{harness}.mjs"));
        fs::write(&app, r#"import fs from 'node:fs';import net from 'node:net';import readline from 'node:readline';import {spawn} from 'node:child_process';
const dir=process.env.CITO_STATE_DIR+'/sockets';fs.mkdirSync(dir,{recursive:true});
net.createServer(c=>c.once('data',()=>c.end(JSON.stringify({ok:true,session_id:'selected-session',cwd:'/repo',pi_pid:process.pid})+'\n')))
.listen(dir+'/pi-pid-'+process.pid+'.sock',()=>console.log('ready'));
readline.createInterface({input:process.stdin}).on('line',line=>{
 const req=JSON.parse(line);
 const child=spawn(process.argv[2],{stdio:['pipe','inherit','inherit']});
 child.stdin.end(JSON.stringify(req)+'\n');
});
"#).unwrap();
        let endpoint = f.spawn("node", &[app.to_str().unwrap(), bin], &[]);
        wait(|| f.output(endpoint, false).contains("ready"));
        let pid = f.pid(endpoint);
        if harness == "claude" {
            let binding = f.state.join(format!("claude/endpoints/{pid}.json"));
            fs::create_dir_all(binding.parent().unwrap()).unwrap();
            fs::write(
                binding,
                json!({"session_id":"selected-session","cwd":"/repo",
                    "start":cito_native::support::process_start(pid)})
                .to_string(),
            )
            .unwrap();
        }
        for envelope in ["meta", "_meta"] {
            let mut req = json!({"op":"resolve_caller","require_open":false,"cwd":"/repo",
                "http_transport":true});
            req[envelope] = json!({"http_client_pid":pid,"expected_session_id":"selected-session"});
            if harness == "codex" {
                req[envelope]["threadId"] = json!("selected-session");
            }
            let resolved = f.plugin(bin, req.clone());
            assert_eq!(
                resolved["caller"]["session_id"], "selected-session",
                "{harness}/{envelope}/http_client_pid: {resolved}"
            );
            assert_eq!(
                resolved["caller"]["endpoint_pid"], pid,
                "{harness}/{envelope}/http_client_pid: {resolved}"
            );
            if harness == "pi" {
                req[envelope]["expected_session_id"] = json!("other-session");
                let rejected = f.plugin(bin, req);
                assert_eq!(rejected["error_code"], "INVALID_INPUT", "{rejected}");
            }
        }
        for envelope in [None, Some("meta"), Some("_meta")] {
            let mut req = json!({"op":"resolve_caller","require_open":false,"cwd":"/repo",
                "state_dir":f.state,"env":f.env});
            if let Some(key) = envelope {
                req[key] = json!({"http_transport":true});
            }
            let before = f.frames(endpoint).len();
            f.send(endpoint, req);
            wait(|| f.frames(endpoint).len() > before);
            let reply = f.frames(endpoint).last().unwrap().clone();
            // Claude never infers a caller PID from its parent process, even
            // without HTTP metadata; Pi and Codex provide positive controls.
            if envelope.is_some() || harness == "claude" {
                assert_eq!(
                    reply["error_code"], "BINDING_REQUIRED",
                    "{harness}/{envelope:?}/http_transport: {reply}"
                );
            } else {
                assert_eq!(
                    reply["caller"]["session_id"], "selected-session",
                    "{harness}/ancestor control: {reply}"
                );
            }
        }
        f.stop(endpoint);
    }
}

#[test]
fn codex_padded_metadata_and_raw_cached_session_have_consistent_fences() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    for thread_id in [" selected-session ", "   "] {
        f.env.insert("CODEX_THREAD_ID".into(), thread_id.into());
        for envelope in ["meta", "_meta"] {
            let mut req = json!({"op":"resolve_caller","require_open":false,"cwd":"/repo","http_transport":true});
            req[envelope] = json!({"threadId":"selected-session","expected_harness":" codex ","expected_session_id":"selected-session"});
            let resolved = f.plugin(CODEX, req.clone());
            if thread_id.trim().is_empty() {
                assert_eq!(
                    resolved["caller"]["session_id"], "selected-session",
                    "{resolved}"
                );
            } else {
                assert_eq!(resolved["error_code"], "INVALID_INPUT", "{resolved}");
                req[envelope]["threadId"] = json!(thread_id);
                req[envelope]["expected_session_id"] = json!(thread_id);
                let matched = f.plugin(CODEX, req);
                assert_eq!(matched["caller"]["session_id"], thread_id, "{matched}");
            }
        }
    }
    f.env.remove("CODEX_THREAD_ID");
    let endpoint = fence_endpoint(&f, "codex");
    let cached = cito_native::support::remember_process(
        json!({"harness":"codex","session_id":" selected-session ","endpoint_pid":f.pid(endpoint)}),
    );
    assert!(cached["process_identity"].is_object());
    for op in ["resolve_caller", "verify"] {
        let req = json!({"op":op,"require_open":false,"http_transport":true,
            "http_client_pid":f.pid(endpoint),"cached_caller":cached});
        let resolved = f.plugin(CODEX, req.clone());
        assert_eq!(
            resolved["caller"]["session_id"], " selected-session ",
            "{resolved}"
        );
        for envelope in ["meta", "_meta"] {
            let mut fenced = req.clone();
            fenced[envelope] = json!({"expected_session_id":"selected-session"});
            let rejected = f.plugin(CODEX, fenced.clone());
            assert_eq!(rejected["error_code"], "BINDING_REQUIRED", "{rejected}");
            fenced[envelope]["expected_harness"] = json!(" codex ");
            let rejected = f.plugin(CODEX, fenced);
            assert_eq!(rejected["error_code"], "INVALID_INPUT", "{rejected}");
        }
    }
    f.stop(endpoint);
}

#[test]
fn pi_padded_bridge_identity_survives_live_and_cached_resolution() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let app = f.dir.join("pi.mjs");
    fs::write(&app, r#"import fs from 'node:fs';import net from 'node:net';import readline from 'node:readline';
const dir=process.env.CITO_STATE_DIR+'/sockets';fs.mkdirSync(dir,{recursive:true});
let failNext=false, session=' selected-session ';
net.createServer(c=>c.once('data',()=>{
 if(failNext){failNext=false;c.end('unavailable\n');}
 else c.end(JSON.stringify({ok:true,session_id:session,cwd:'/repo',pi_pid:process.pid})+'\n');
})).listen(dir+'/pi-pid-'+process.pid+'.sock',()=>console.log(JSON.stringify({op:'ready'})));
readline.createInterface({input:process.stdin}).on('line',line=>{
 const req=JSON.parse(line);failNext=req.failNext;session=req.session;console.log(JSON.stringify({op:'reset',token:req.token}));
});
"#).unwrap();
    let endpoint = f.spawn("node", &[app.to_str().unwrap()], &[]);
    wait(|| f.frames_for(endpoint, "ready").next().is_some());
    let pid = f.pid(endpoint);
    let mut cached = Value::Null;
    let mut token = 0;
    for use_cache in [false, true] {
        for envelope in [None, Some("meta"), Some("_meta")] {
            token += 1;
            // A transient lookup failure exercises cached resolution. Preserve a
            // distinct cached display title so a fresh resolution cannot pass for it.
            if use_cache {
                cached["title"] = json!("Prior display title");
            }
            f.send(
                endpoint,
                json!({"failNext":use_cache,"session":" selected-session ","token":token}),
            );
            wait(|| {
                f.frames(endpoint)
                    .iter()
                    .any(|v| v["op"] == "reset" && v["token"] == token)
            });
            let mut req = json!({"op":"resolve_caller","require_open":false,
                "endpoint_pid":pid,"cached_caller":if use_cache {cached.clone()} else {Value::Null}});
            if let Some(key) = envelope {
                req[key] = json!({"expected_session_id":" selected-session "});
            }
            let resolved = f.plugin(PI, req);
            assert_eq!(
                resolved["caller"]["session_id"], " selected-session ",
                "{use_cache}/{envelope:?}: {resolved}"
            );
            assert_eq!(resolved["caller"]["endpoint_pid"], pid, "{resolved}");
            if use_cache {
                assert_eq!(resolved["caller"], cached);
            }
            cached = resolved["caller"].clone();
            assert!(cached["process_identity"].is_object());
        }
    }
    // A cache with the same PID and process identity still needs a live session match.
    token += 1;
    f.send(
        endpoint,
        json!({"failNext":true,"session":"selected-session","token":token}),
    );
    wait(|| {
        f.frames(endpoint)
            .iter()
            .any(|v| v["op"] == "reset" && v["token"] == token)
    });
    let rejected = f.plugin(PI, json!({"op":"resolve_caller","require_open":false,"endpoint_pid":pid,"cached_caller":cached}));
    assert_eq!(
        rejected["error_code"], "PI_BRIDGE_UNAVAILABLE",
        "{rejected}"
    );
    f.stop(endpoint);
}

#[test]
fn uncached_generic_session_fences_allow_disk_providers_to_continue() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let source =
        cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite")).unwrap();
    source.execute("CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER); INSERT INTO threads VALUES('thread-a','/repo',0)", &[]).unwrap();
    let endpoint = fence_endpoint(&f, "claude");
    let pid = f.pid(endpoint);
    let binding = f.state.join(format!("claude/endpoints/{pid}.json"));
    fs::create_dir_all(binding.parent().unwrap()).unwrap();
    fs::write(
        &binding,
        json!({"session_id":"claude-session","cwd":"/repo",
        "start":cito_native::support::process_start(pid)})
        .to_string(),
    )
    .unwrap();
    for (bin, harness, session) in [
        (CODEX, "codex", "thread-a"),
        (CLAUDE, "claude", "claude-session"),
    ] {
        for key in ["meta", "_meta"] {
            let mut req = json!({"op":"resolve_caller","require_open":false,"cwd":"/repo",
                "http_client_pid":pid,"http_transport":true});
            // Codex can infer a DB thread without borrowing Claude's transport.
            if harness == "codex" {
                req.as_object_mut().unwrap().remove("http_client_pid");
                req["http_transport"] = json!(false);
            }
            req[key] = json!({"expected_session_id":session});
            let matching = f.plugin(bin, req.clone());
            assert_eq!(
                matching["caller"]["session_id"], session,
                "{harness}/{key}: {matching}"
            );
            req[key]["expected_session_id"] = json!("other-session");
            let generic = f.plugin(bin, req.clone());
            assert_eq!(
                generic["error_code"], "BINDING_REQUIRED",
                "{harness}/{key}: {generic}"
            );
            for harness_key in ["meta", "_meta"] {
                let mut fenced = req.clone();
                if !fenced[harness_key].is_object() {
                    fenced[harness_key] = json!({});
                }
                // Fence the inferred provider without turning the generic ID into
                // provider-specific metadata in the same envelope.
                fenced[harness_key]["expected_harness"] = json!("another-provider");
                let conflict = f.plugin(bin, fenced);
                assert_eq!(
                    conflict["error_code"], "INVALID_INPUT",
                    "{harness}/{key}/{harness_key}: {conflict}"
                );
            }
        }
    }
    f.stop(endpoint);
}

#[test]
fn daemon_attaches_rejected_caches_and_routes_to_the_next_real_provider() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let source =
        cito_native::database::Database::open(&f.home.join(".codex/state_5.sqlite")).unwrap();
    source.execute("CREATE TABLE threads(id TEXT,cwd TEXT,archived INTEGER,recency_at_ms INTEGER); INSERT INTO threads VALUES('thread-a','/repo',0,1)", &[]).unwrap();
    // Record requests and replies while forwarding unchanged to the actual providers.
    let forwarder = f.dir.join("provider-trace.py");
    fs::write(
        &forwarder,
        r#"import json,subprocess,sys
request=sys.stdin.read()
result=subprocess.run([sys.argv[1]],input=request,text=True,capture_output=True)
with open(sys.argv[2],'a') as trace:
 trace.write(json.dumps({'request':json.loads(request),'reply':json.loads(result.stdout)})+'\n')
sys.stdout.write(result.stdout);sys.stderr.write(result.stderr);sys.exit(result.returncode)
"#,
    )
    .unwrap();
    let trace = f.dir.join("provider-trace.jsonl");
    for (harness, bin) in [("codex", CODEX), ("claude", CLAUDE), ("pi", PI)] {
        install_provider_command(&f, harness, json!(["python3", forwarder, bin, trace]));
    }
    let (pid, sidecar) = claude_sidecar(&f);
    f.daemon(&[]);
    let seeded = f.tool(
        "cito_status",
        json!({}),
        json!({"expected_harness":"claude","_stdio":{"endpoint_pid":pid},"cwd":"/repo"}),
    );
    assert_eq!(seeded["ok"], true, "{seeded}");
    f.eof(sidecar);
    wait(|| f.exited(sidecar));
    assert!(f
        .state
        .join(format!("claude/endpoints/{pid}.json"))
        .exists());
    fs::write(&trace, "").unwrap();
    let routed = f.tool("cito_status", json!({}),
        json!({"_stdio":{"endpoint_pid":pid},"threadId":"thread-a","expected_session_id":"thread-a","cwd":"/repo"}));
    assert_eq!(routed["ok"], true, "{routed}");
    let calls: Vec<Value> = fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rejected = calls
        .iter()
        .position(|call| call["request"]["cached_caller"]["harness"] == "claude")
        .unwrap();
    assert_eq!(
        calls[rejected]["request"]["cached_caller"]["session_id"],
        "claude-session"
    );
    assert_eq!(calls[rejected]["reply"]["error_code"], "BINDING_REQUIRED");
    assert!(calls[rejected + 1..]
        .iter()
        .any(|call| call["reply"]["caller"]["harness"] == "codex"
            && call["reply"]["caller"]["session_id"] == "thread-a"));

    // An HTTP forwarder has no explicit/stdio PID priority. Seed its Codex cache,
    // remove the DB projection, then require cache rejection before Claude wins.
    let seeded = f.tool(
        "cito_status",
        json!({}),
        json!({"expected_harness":"codex","threadId":"thread-a","cwd":"/repo"}),
    );
    assert_eq!(seeded["ok"], true, "{seeded}");
    source.execute("DELETE FROM threads", &[]).unwrap();
    fs::write(&trace, "").unwrap();
    let routed = f.tool("cito_status", json!({}),
        json!({"claudeSessionId":"claude-session","expected_session_id":"claude-session","cwd":"/repo"}));
    assert_eq!(routed["ok"], true, "{routed}");
    let calls: Vec<Value> = fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rejected = calls
        .iter()
        .position(|call| call["request"]["cached_caller"]["harness"] == "codex")
        .unwrap();
    assert_eq!(
        calls[rejected]["request"]["cached_caller"]["session_id"],
        "thread-a"
    );
    assert!(calls[rejected]["request"]["meta"]["_stdio"].is_null());
    assert_eq!(calls[rejected]["reply"]["error_code"], "BINDING_REQUIRED");
    assert!(calls[rejected + 1..]
        .iter()
        .any(|call| call["reply"]["caller"]["harness"] == "claude"
            && call["reply"]["caller"]["session_id"] == "claude-session"));
}

#[test]
fn nested_harness_stdio_never_claims_an_outer_bridge_but_shells_can() {
    let _lock = serial();
    for (outer, inner) in [("pi", "claude"), ("claude", "pi")] {
        for shell in [false, true] {
            let f = Fixture::new(json!({}));
            for (id, bin) in [("pi", PI), ("claude", CLAUDE), ("codex", CODEX)] {
                install_provider_command(&f, id, json!([bin]));
            }
            let inner_app = f.dir.join(format!("{inner}.mjs"));
            fs::write(
                &inner_app,
                r#"import {spawn} from 'node:child_process';
const mcp=spawn(process.argv[2],['mcp-stdio'],{stdio:['pipe','inherit','inherit']});
process.stdin.pipe(mcp.stdin);
process.on('SIGTERM',()=>{mcp.kill();process.exit()});
"#,
            )
            .unwrap();
            let outer_app = f.dir.join(format!("{outer}.mjs"));
            fs::write(&outer_app, r#"import fs from 'node:fs';import net from 'node:net';import {spawn} from 'node:child_process';
const path=process.env.CITO_STATE_DIR+'/sockets/'+process.argv[2]+'-pid-'+process.pid+'.sock';
fs.mkdirSync(process.env.CITO_STATE_DIR+'/sockets',{recursive:true});
const server=net.createServer(c=>{c.once('data',()=>c.end(JSON.stringify({ok:true,session_id:'outer-session',cwd:'/repo',pi_pid:process.pid})+'\n'))});
server.listen(path,()=>{
 const child=process.argv[4]==='shell'
  ? spawn('/bin/sh',['-c','"$TEST_CITO" mcp-stdio; status=$?; exit "$status"'],{stdio:['pipe','inherit','inherit'],env:{...process.env,TEST_CITO:process.argv[3]}})
  : spawn(process.execPath,[process.argv[4],process.argv[3]],{stdio:['pipe','inherit','inherit']});
 process.stdin.pipe(child.stdin);
 console.log(JSON.stringify({child:child.pid}));
 process.on('SIGTERM',()=>{child.kill();server.close();process.exit()});
});
"#).unwrap();
            f.daemon(&[]);
            let owner = f.spawn(
                "node",
                &[
                    outer_app.to_str().unwrap(),
                    outer,
                    NATIVE,
                    if shell {
                        "shell"
                    } else {
                        inner_app.to_str().unwrap()
                    },
                ],
                &[],
            );
            wait(|| f.frames(owner).iter().any(|r| r["child"].is_u64()));
            f.send(owner, json!({"jsonrpc":"2.0","id":77,"method":"tools/call","params":{"name":"cito_listen","arguments":{"about":"Nested transport boundary"}}}));
            wait(|| f.frames(owner).iter().any(|r| r["id"] == 77));
            let reply = f.frames(owner).into_iter().find(|r| r["id"] == 77).unwrap();
            if shell {
                assert_ne!(reply["result"]["isError"], true, "{outer}: {reply}");
                let rows = f.rows("SELECT * FROM local_listeners WHERE session_id='outer-session'");
                assert_eq!(rows.len(), 1, "{outer}: {reply}");
                assert_eq!(rows[0]["harness"], outer);
            } else {
                assert_eq!(reply["result"]["isError"], true, "{outer}: {reply}");
                assert!(f
                    .rows("SELECT * FROM local_listeners WHERE session_id='outer-session'")
                    .is_empty());
            }
        }
    }
}

#[test]
fn long_lived_stdio_recovers_after_daemon_restart_and_configured_port_change() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    f.env.remove("CITO_DAEMON_PORT");
    fs::write(&f.config, json!({"daemonPort":f.port}).to_string()).unwrap();
    let first = f.daemon(&[]);
    let stdio = f.spawn(NATIVE, &["mcp-stdio"], &[]);
    f.send(
        stdio,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    wait(|| f.frames(stdio).iter().any(|r| r["id"] == 1));
    assert!(f
        .frames(stdio)
        .iter()
        .find(|r| r["id"] == 1)
        .unwrap()
        .get("result")
        .is_some());
    f.stop(first);
    f.port = port();
    let mut config: Value = read_json(&f.config);
    config["daemonPort"] = json!(f.port);
    fs::write(&f.config, config.to_string()).unwrap();
    f.daemon(&[]);
    f.send(
        stdio,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    );
    wait(|| f.frames(stdio).iter().any(|r| r["id"] == 2));
    let response = f.frames(stdio).into_iter().find(|r| r["id"] == 2).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    assert!(
        response["result"]["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "{response}"
    );
    f.eof(stdio);
    wait(|| f.exited(stdio));
}
#[test]
fn pending_business_ack_requires_original_url_and_principal() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    let daemon = f.daemon(&[]);
    let human = f
        .rows("SELECT * FROM local_listeners WHERE hub='local' AND harness='human'")
        .remove(0);
    let sent = f.tool(
        "cito_send",
        json!({"to":human["public_id"],"message":"Keep unread"}),
        json!({}),
    );
    let id = sent["signal_id"].clone();
    let delivered = f.tool(
        "cito_send",
        json!({"to":human["public_id"],"message":"Admitted before replacement"}),
        json!({}),
    )["signal_id"]
        .clone();
    let state = f.db("state.sqlite");
    let hub = f.db("hub.sqlite");
    wait(|| {
        !state
            .query(
                "SELECT 1 FROM local_signals WHERE signal_id=?",
                std::slice::from_ref(&id),
            )
            .unwrap()
            .is_empty()
    });
    wait(|| {
        !state
            .query(
                "SELECT 1 FROM local_signals WHERE signal_id=?",
                std::slice::from_ref(&delivered),
            )
            .unwrap()
            .is_empty()
    });
    // Stop the reconciler before changing authority; suspending it can strand
    // the SQLite write lock needed for the local acknowledgement below.
    f.stop(daemon);
    state.execute("UPDATE local_listeners SET hub_url=\'https://replacement.example.com\',principal_id=\'none#bob\' WHERE listener_id=?", std::slice::from_ref(&human["listener_id"])).unwrap();
    let local = cito_native::core::state::LocalState::open(&f.state).unwrap();
    local
        .acknowledge(
            "local",
            human["listener_id"].as_str().unwrap(),
            std::slice::from_ref(&delivered),
            "device:human",
        )
        .unwrap();
    assert!(state
        .query(
            "SELECT * FROM pending_hub_acks WHERE signal_id=?",
            std::slice::from_ref(&delivered)
        )
        .unwrap()
        .is_empty());
    assert!(state
        .query(
            "SELECT acknowledged_at_ms FROM local_signals WHERE signal_id=?",
            std::slice::from_ref(&delivered)
        )
        .unwrap()[0]["acknowledged_at_ms"]
        .is_null());
    state
        .execute(
            "UPDATE local_listeners SET hub_url='local://',principal_id=? WHERE listener_id=?",
            &[human["principal_id"].clone(), human["listener_id"].clone()],
        )
        .unwrap();
    local
        .acknowledge(
            "local",
            human["listener_id"].as_str().unwrap(),
            std::slice::from_ref(&delivered),
            "device:human",
        )
        .unwrap();
    let ack = state
        .query(
            "SELECT * FROM pending_hub_acks WHERE signal_id=?",
            std::slice::from_ref(&delivered),
        )
        .unwrap()
        .remove(0);
    assert_eq!(ack["hub_url"], "local://");
    assert_eq!(ack["principal_id"], human["principal_id"]);
    f.daemon(&[]);
    f.tool("cito_inbox", json!({"ack_all":false}), meta.clone());
    wait(|| {
        state
            .query(
                "SELECT * FROM pending_hub_acks WHERE signal_id=?",
                std::slice::from_ref(&delivered),
            )
            .unwrap()
            .is_empty()
    });
    assert!(!hub
        .query(
            "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
            std::slice::from_ref(&delivered)
        )
        .unwrap()[0]["acked_at_ms"]
        .is_null());
    state.execute("INSERT INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms,hub_url,principal_id) VALUES('local',?,?,'device:human',1,'https://other.example.com','none#bob')",&[human["listener_id"].clone(),id.clone()]).unwrap();
    for (url, principal) in [
        ("local://", "none#bob"),
        (
            "https://other.example.com",
            human["principal_id"].as_str().unwrap(),
        ),
    ] {
        state
            .execute(
                "UPDATE pending_hub_acks SET hub_url=?,principal_id=?",
                &[json!(url), json!(principal)],
            )
            .unwrap();
        f.tool("cito_inbox", json!({"ack_all":false}), meta.clone());
        assert_eq!(
            state
                .query("SELECT * FROM pending_hub_acks", &[])
                .unwrap()
                .len(),
            1
        );
        assert!(hub
            .query(
                "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
                std::slice::from_ref(&id)
            )
            .unwrap()[0]["acked_at_ms"]
            .is_null());
    }
    state.transaction(|c| {
        for i in 0..256 { c.execute("INSERT INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms) VALUES('local',?,?,'device:human',0)",&[human["listener_id"].clone(),json!(format!("unbound-{i}"))])?; }
        c.execute("UPDATE pending_hub_acks SET hub_url='local://',principal_id=? WHERE signal_id=?",&[human["principal_id"].clone(),id.clone()])?;
        Ok(())
    }).unwrap();
    f.tool("cito_inbox", json!({"ack_all":false}), meta);
    wait(|| {
        state
            .query(
                "SELECT * FROM pending_hub_acks WHERE signal_id=?",
                std::slice::from_ref(&id),
            )
            .unwrap()
            .is_empty()
    });
    let retained = state.query("SELECT * FROM pending_hub_acks", &[]).unwrap();
    assert_eq!(
        retained.len(),
        256,
        "Unbound history must not starve a valid later ACK"
    );
    assert!(retained
        .iter()
        .all(|r| r["hub_url"].is_null() && r["principal_id"].is_null()));
    assert!(!hub
        .query(
            "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
            &[id]
        )
        .unwrap()[0]["acked_at_ms"]
        .is_null());
}
#[test]
fn inbox_ordered_pages_explicit_ack_union_and_no_background_wake() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    enroll(&f, meta.clone());
    let row = f.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    let db = f.db("state.sqlite");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    db.execute(
        "INSERT OR REPLACE INTO wake_retries(hub,listener_id,retry_at_ms) VALUES('local',?,?)",
        &[row["listener_id"].clone(), json!(now + 60_000)],
    )
    .unwrap();
    db.transaction(|c|{for i in (1..=55).rev(){c.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,held_reason,hub_url,principal_id) VALUES('local',?,?,?,?,'bob','laptop','Sender','Message','2030-01-01T00:00:00.000Z',?,'STORED_PASSIVE','fixture_hold','local://',?)",&[json!(format!("sig-{i}")),row["listener_id"].clone(),json!(i),row["binding_generation"].clone(),json!(now+60_000),row["principal_id"].clone()])?;}Ok(())}).unwrap();
    let selected = f.tool("cito_inbox", json!({"ack_ids":["sig-55"]}), meta.clone());
    assert_eq!(selected["acknowledged_count"], 1);
    assert!(db
        .query(
            "SELECT * FROM wake_retries WHERE listener_id=?",
            std::slice::from_ref(&row["listener_id"])
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        selected["signals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        (1..=50).collect::<Vec<_>>()
    );
    assert_eq!(
        f.tool("cito_inbox", json!({}), meta.clone())["acknowledged_count"],
        50
    );
    let union = f.tool(
        "cito_inbox",
        json!({"ack_ids":["sig-55"],"ack_all":true}),
        meta.clone(),
    );
    assert_eq!(union["acknowledged_count"], 4);
    assert_eq!(union["signals"].as_array().unwrap().len(), 4);
    assert_eq!(
        f.tool("cito_inbox", json!({}), meta.clone())["signals"],
        json!([])
    );
    assert_eq!(
        f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NOT NULL")
            .len(),
        55
    );
    db.execute(
        "UPDATE local_signals SET message=?,acknowledged_at_ms=NULL",
        &[json!("\\".repeat(65_536))],
    )
    .unwrap();
    let mut seqs = Vec::new();
    while seqs.len() < 55 {
        let result = f.tool_result("cito_inbox", json!({"limit":1000}), meta.clone());
        assert!(result["isError"] == false && result.to_string().len() < 8 * 1024 * 1024);
        let page: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        let signals = page["signals"].as_array().unwrap();
        assert!((1..55).contains(&signals.len()));
        assert_eq!(page["acknowledged_count"], signals.len());
        seqs.extend(signals.iter().map(|signal| signal["seq"].as_u64().unwrap()));
    }
    assert_eq!(seqs, (1..=55).collect::<Vec<_>>());
    // A legacy row can predate the write-side size check. A rejected page must not ACK it.
    db.execute("UPDATE local_signals SET signal_id='nested:id', in_reply_to='nested:parent',message=?,acknowledged_at_ms=NULL WHERE seq=1", &[json!("\"".repeat(2*1024*1024))]).unwrap();
    let rejected = f.tool_result("cito_inbox", json!({}), meta.clone());
    assert_eq!(rejected["isError"], true);
    assert!(rejected["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Signal exceeds inbox transport limit"));
    db.execute(
        "UPDATE local_signals SET message='readable' WHERE seq=1",
        &[],
    )
    .unwrap();
    let page = f.tool("cito_inbox", json!({"ack_all":false}), meta.clone());
    assert_eq!(page["signals"][0]["signal_id"], "local:nested:id");
    assert_eq!(page["signals"][0]["in_reply_to"], "local:nested:parent");
    assert_eq!(
        f.tool("cito_inbox", json!({"ack_ids":["local:nested:id"]}), meta)["acknowledged_count"],
        1
    );
    let calls = fs::read_to_string(f.state.join("plugins/fixture/calls.jsonl")).unwrap();
    assert!(!calls
        .lines()
        .any(|l| serde_json::from_str::<Value>(l).unwrap()["op"] == "wake"));
}
#[test]
fn pending_publish_constraint_failure_recovers_exact_id_and_dedup_sequence() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta);
    let db = f.db("hub.sqlite");
    db.execute("CREATE TRIGGER reject_publish BEFORE INSERT ON hub_signals BEGIN SELECT RAISE(ABORT,'injected failure'); END",&[]).unwrap();
    let args = json!({"to":target["listener_id"],"message":"Recovery","idempotency_key":"same"});
    assert_eq!(send_human_result(&f, args.clone())["http_status"], 400);
    let row = f.rows("SELECT * FROM local_sent_idempotency")[0].clone();
    assert_eq!(
        row["request_hash"],
        cito_native::core::identity::hash(&json!(["Recovery", Value::Null]).to_string())
    );
    assert_eq!(row["status"], "PENDING_PUBLISH");
    db.execute("DROP TRIGGER reject_publish", &[]).unwrap();
    let recovered = send_human(&f, args.clone());
    assert_eq!(recovered["signal_id"], row["signal_id"]);
    assert_eq!(send_human(&f, args)["seq"], recovered["seq"]);
    assert_eq!(
        db.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
            .unwrap()[0]["n"],
        1
    );
}
#[test]
fn shared_hub_private_scope_is_invisible_and_workspace_scope_is_deliverable() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (_, p) = host.hub();
    let config = json!({"hubs":{"work":{"url":format!("http://127.0.0.1:{p}"),"user":"alice"}},"defaultHub":"work"});
    let a = Fixture::new(config);
    let meta = a.provider("delivered_unconfirmed");
    a.daemon(&[]);
    let target = a.tool(
        "cito_listen",
        json!({"about":"Private target","scope":"self"}),
        meta.clone(),
    );
    let mut b = Fixture::new(
        json!({"hubs":{"work":{"url":format!("http://127.0.0.1:{p}"),"user":"bob"}},"defaultHub":"work"}),
    );
    b.env.insert("CITO_USER".into(), "bob".into());
    b.daemon(&[]);
    assert_eq!(
        b.tool("cito_find", json!({"harness":"fixture"}), json!({}))["listeners"],
        json!([])
    );
    let denied = send_human_result(&b, json!({"to":target["listener_id"],"message":"Private"}));
    assert_eq!(denied["http_status"], 400);
    assert!(denied.to_string().contains("FORBIDDEN_TARGET_SCOPE"));
    let public = a.tool(
        "cito_listen",
        json!({"about":"Workspace target","scope":"workspace"}),
        meta,
    );
    send_human(
        &b,
        json!({"to":public["listener_id"],"message":"Workspace"}),
    );
    wait(|| {
        a.rows("SELECT * FROM local_signals WHERE state='DISPATCHED'")
            .len()
            == 1
    });
    assert_eq!(
        a.rows("SELECT from_user FROM local_signals")[0]["from_user"],
        "bob"
    );
}
#[test]
fn same_channel_takeover_fences_old_owner_and_preserves_winner() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (_, p) = host.hub();
    let config =
        json!({"hubs":{"work":{"url":format!("http://127.0.0.1:{p}")}},"defaultHub":"work"});
    let a = Fixture::new(config.clone());
    let ma = a.provider("delivered_unconfirmed");
    a.daemon(&[]);
    let old = enroll(&a, ma);
    let sibling = Fixture::new(config.clone());
    let sibling_meta = sibling.provider("delivered_unconfirmed");
    sibling.daemon(&[]);
    let attached = enroll(&sibling, sibling_meta);
    assert_eq!(attached["binding_generation"], old["binding_generation"]);
    wait(|| {
        [&a, &sibling].iter().all(|fixture| {
            fixture
                .rows("SELECT status FROM local_listeners WHERE hub=\'work\' AND harness=\'human\'")
                .iter()
                .any(|row| row["status"] == "ACTIVE")
        })
    });
    let device =
        a.rows("SELECT * FROM local_listeners WHERE hub='work' AND harness='human'")[0].clone();
    let sibling_device = sibling
        .rows("SELECT * FROM local_listeners WHERE hub='work' AND harness='human'")[0]
        .clone();
    a.action_ok(
        json!({"op":"unlisten_session","hub":"work","harness":"human","session_id":"default"}),
    );
    assert_eq!(
        a.rows("SELECT status FROM local_listeners WHERE hub='work' AND harness='human'")[0]
            ["status"],
        "UNLISTENED"
    );
    let retained = sibling
        .rows("SELECT * FROM local_listeners WHERE hub='work' AND harness='human'")[0]
        .clone();
    assert_eq!(retained["status"], "ACTIVE");
    assert_eq!(retained["owner_token"], sibling_device["owner_token"]);
    let work = |rows: &Value| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|hub| hub["name"] == "work")
            .unwrap()
            .clone()
    };
    assert_eq!(work(&a.snapshot()["hubs"])["human_paused"], false);
    a.action_ok(json!({"op":"cli_human_device","hub":"work","attach":true}));
    let reattached =
        a.rows("SELECT * FROM local_listeners WHERE hub='work' AND harness='human'")[0].clone();
    assert_eq!(reattached["status"], "ACTIVE");
    assert!(reattached["binding_generation"].as_u64() > device["binding_generation"].as_u64());
    assert_ne!(reattached["owner_token"], device["owner_token"]);
    let unchanged = sibling
        .rows("SELECT * FROM local_listeners WHERE hub='work' AND harness='human'")[0]
        .clone();
    assert_eq!(unchanged["scope"], sibling_device["scope"]);
    assert_eq!(unchanged["owner_token"], sibling_device["owner_token"]);
    let sibling_row =
        sibling.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    // Hold only the sibling's dispatch lease; its receiver must keep polling unfenced.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    sibling.db("state.sqlite").execute(
        "UPDATE local_listeners SET dispatcher_token='test-held',lease_until_ms=? WHERE listener_id=?",
        &[json!(now + 60_000), sibling_row["listener_id"].clone()],
    ).unwrap();
    let b = Fixture::new(config);
    b.provider("delivered_unconfirmed");
    fs::write(
        b.state.join("host-id"),
        fs::read(a.state.join("host-id")).unwrap(),
    )
    .unwrap();
    b.daemon(&[]);
    let winner = enroll(
        &b,
        json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
    );
    // Heartbeat fencing must work even before another signal arrives.
    wait(|| {
        a.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"]
            == "FENCED_INACTIVE"
    });
    let sent = send_human(&b, json!({"to":winner["listener_id"],"message":"Winner"}));
    wait(|| {
        b.rows("SELECT * FROM local_signals WHERE state='DISPATCHED'")
            .len()
            == 1
            && sibling.rows("SELECT * FROM local_signals").len() == 1
    });
    assert!(sibling.rows("SELECT held_reason FROM local_signals")[0]["held_reason"].is_null());
    assert_eq!(
        b.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "ACTIVE"
    );
    assert_eq!(winner["binding_generation"], old["binding_generation"]);
    assert_eq!(
        sibling.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "ACTIVE"
    );
    let winner_row = b.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    let old_row = a.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    assert_ne!(winner_row["owner_token"], old_row["owner_token"]);
    let assert_channels_active = || {
        let remote: Value = serde_json::from_str(
            host.db("hub/hub.sqlite")
                .query(
                    "SELECT CAST(value AS TEXT) AS value FROM hub_kv WHERE key=?",
                    &[winner_row["listener_id"].clone()],
                )
                .unwrap()[0]["value"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(remote["state"], "ACTIVE");
        for row in [&winner_row, &sibling_row] {
            let channel = format!("{}:fixture", row["host_id"].as_str().unwrap());
            assert_eq!(remote["channels"][&channel]["state"], "ACTIVE");
            assert_eq!(
                remote["channels"][&channel]["owner_token"],
                row["owner_token"]
            );
        }
    };
    assert_channels_active();
    let mut sent_signals = vec![sent];
    for operation in ["unlisten", "reload"] {
        if operation == "unlisten" {
            assert_eq!(
                a.tool(
                    "cito_unlisten",
                    json!({"hub":"work"}),
                    json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
                )["status"],
                "not_listening"
            );
            assert_eq!(
                a.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
                "UNLISTENED"
            );
        } else {
            // Recreate a reload that captured A's ACTIVE row before heartbeat fencing.
            // Defer background fencing so the reload must exercise remote detach.
            let db = a.db("state.sqlite");
            db.execute(
                "CREATE TRIGGER defer_fencing BEFORE UPDATE OF status ON local_listeners WHEN OLD.harness='fixture' AND NEW.status='FENCED_INACTIVE' BEGIN SELECT RAISE(IGNORE); END",
                &[],
            ).unwrap();
            db.execute(
                "UPDATE local_listeners SET status='ACTIVE' WHERE harness='fixture'",
                &[],
            )
            .unwrap();
            let mut config: Value = read_json(&a.config);
            let url = format!("http://localhost:{p}");
            config["hubs"]["work"]["url"] = json!(url);
            fs::write(&a.config, config.to_string()).unwrap();
            let (status, snapshot) = a.action(json!({"op":"get_snapshot"}));
            // Finish the local fence deferred by the fixture's race injection.
            db.execute(
                "DROP TRIGGER defer_fencing; UPDATE local_listeners SET status='FENCED_INACTIVE' WHERE harness='fixture'",
                &[],
            ).unwrap();
            assert_eq!(status, 200, "{snapshot}");
            assert_eq!(work(&snapshot["settings"]["hubs"])["url"], url);
        }
        assert_channels_active();
        sent_signals.push(send_human(
            &b,
            json!({"to":winner["listener_id"],"message":format!("Winner after {operation}")}),
        ));
        wait(|| {
            b.rows("SELECT * FROM local_signals WHERE state='DISPATCHED'")
                .len()
                == sent_signals.len()
        });
        assert_eq!(
            b.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
            "ACTIVE"
        );
    }
    for (fixture, expected) in [(&a, 0), (&b, sent_signals.len()), (&sibling, 0)] {
        let calls = fs::read_to_string(fixture.state.join("plugins/fixture/calls.jsonl")).unwrap();
        let wakes: Vec<Value> = calls
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|call| call["op"] == "wake")
            .collect();
        assert_eq!(wakes.len(), expected);
        for (wake, sent) in wakes.iter().zip(&sent_signals) {
            assert!(sent["signal_id"].as_str().unwrap().starts_with("work:"));
            assert_eq!(wake["signals"][0]["signal_id"], sent["signal_id"]);
        }
    }
}
#[test]
fn http_origin_guard_strict_tool_schemas_and_singleton_port_ownership() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let daemon = f.daemon(&[]);
    let response = ureq::get(&format!("http://127.0.0.1:{}/health", f.port))
        .set("Origin", "https://foreign.example")
        .call();
    assert!(matches!(response, Err(ureq::Error::Status(403, _))));
    let tools = f.mcp("tools/list", json!({}));
    for tool in tools["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        let name = tool["name"].as_str().unwrap();
        let result = f.tool_result(name, json!({"extra":true}), json!({}));
        assert_eq!(result["isError"], true);
        assert!(result.to_string().contains("INVALID_INPUT"));
    }
    assert!(f.command(NATIVE, &["daemon"], "").0);
    f.env.insert("CITO_DAEMON_PORT".into(), port().to_string());
    assert!(!f.command(NATIVE, &["daemon"], "").0);
    f.stop(daemon);
    f.env.insert("CITO_DAEMON_PORT".into(), f.port.to_string());
    f.daemon(&[]);
}
#[test]
fn cli_formats_real_mcp_error_envelopes_without_json_or_internal_codes() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.provider("delivered_unconfirmed");
    let _daemon = f.daemon(&[]);
    let (ok, _, error) = f.command(
        NATIVE,
        &[
            "send",
            "--harness",
            "fixture",
            "--to",
            "c_0000000000000000",
            "--message",
            "hello",
        ],
        "",
    );
    assert!(!ok, "Unknown targets must fail");
    assert!(error.contains("Target listener does not exist"), "{error}");
    assert!(
        !error.contains("error_code")
            && !error.contains("{")
            && !error.contains("LISTENER_NOT_FOUND"),
        "{error}"
    );
}
#[test]
fn sqlite_pre_v4_and_additive_v7_upgrades_preserve_outbound_recovery_rows() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let db = f.db("state.sqlite");
    db.execute("CREATE TABLE local_listeners(legacy TEXT);CREATE TABLE local_pairwise_wakes(legacy TEXT);PRAGMA user_version=3",&[]).unwrap();
    let daemon = f.daemon(&[]);
    assert_eq!(
        db.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        7
    );
    assert!(db
        .query(
            "SELECT name FROM sqlite_master WHERE name='local_pairwise_wakes'",
            &[]
        )
        .unwrap()
        .is_empty());
    db.execute("INSERT INTO local_sent_idempotency(hub,dedup_id,request_hash,signal_id,to_listener_id,status,expires_at_iso,expires_at_ms) VALUES('work','d','p','s','c_abcdefgh2345','PENDING_PUBLISH','now',0)",&[]).unwrap();
    f.stop(daemon);
    // Reopen the V7 schema that predates route-origin metadata.
    db.execute("DROP INDEX sent_fallback_key; ALTER TABLE local_sent_idempotency DROP COLUMN fallback_key; ALTER TABLE local_sent_idempotency DROP COLUMN fallback_to; ALTER TABLE local_sent_idempotency DROP COLUMN fallback_public_id", &[]).unwrap();
    f.daemon(&[]);
    assert_eq!(
        db.query(
            "SELECT signal_id,fallback_key FROM local_sent_idempotency WHERE hub='work' AND dedup_id='d'",
            &[]
        )
        .unwrap(),
        vec![json!({"signal_id":"s","fallback_key":null})]
    );
}

#[test]
fn agent_and_human_sends_share_unicode_control_sanitization() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta.clone());
    let hub = f.db("hub.sqlite");
    let error = "INVALID_INPUT: message must contain 1..65,536 characters";
    let agent = f.tool_result(
        "cito_send",
        json!({"to":"@self","message":"\u{0081}"}),
        meta.clone(),
    );
    assert_eq!(agent["isError"], true);
    let agent_error: Value =
        serde_json::from_str(agent["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        agent_error,
        json!({"ok":false,"error":{"code":"INVALID_INPUT","message":"message must contain 1..65,536 characters","retryable":false}})
    );
    let (status, human) =
        f.action(json!({"op":"send_signal","to":target["listener_id"],"message":"\u{0081}"}));
    assert_eq!(status, 400);
    assert_eq!(human["error"], error);
    assert_eq!(
        hub.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
            .unwrap(),
        vec![json!({"n":0})]
    );
    for (message, expected) in [
        ("a\u{0081}b", "ab"),
        (" \ta\u{0081}b\u{2003}", "ab"),
        (
            "a\n\r\t\u{000b}\u{000c}\u{0085}b",
            "a\n\r\t\u{000b}\u{000c}\u{0085}b",
        ),
    ] {
        let agent = f.tool(
            "cito_send",
            json!({"to":"@self","message":message}),
            meta.clone(),
        );
        let (status, human) =
            f.action(json!({"op":"send_signal","to":target["listener_id"],"message":message}));
        assert_eq!(status, 200, "{human}");
        for sent in [agent, human] {
            let id = sent["signal_id"]
                .as_str()
                .unwrap()
                .rsplit(':')
                .next()
                .unwrap();
            assert_eq!(
                hub.query(
                    "SELECT json_extract(CAST(data AS TEXT),'$.message') AS message FROM hub_signals WHERE signal_id=?",
                    &[json!(id)],
                ).unwrap(),
                vec![json!({"message":expected})]
            );
        }
    }
}

#[test]
fn snapshot_unread_count_precedes_activity_limit_and_utf8_frame_bounding() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let daemon = f.daemon(&[]);
    enroll(&f, human());
    let row = f.rows("SELECT * FROM local_listeners WHERE harness='human'")[0].clone();
    f.stop(daemon);
    let db = f.db("state.sqlite");
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 60_000;
    db.transaction(|c|{for i in 0..100 {c.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,hub_url,principal_id) VALUES('local',?,?,?,?,'bob','laptop','Sender',?,'2030-01-01T00:00:00.000Z',?,'STORED_PASSIVE','local://',?)",&[json!(format!("sig-{i}")),row["listener_id"].clone(),json!(i),row["binding_generation"].clone(),json!("🦀".repeat(10000)),json!(expires),row["principal_id"].clone()])?;}Ok(())}).unwrap();
    f.daemon(&[]);
    let (s, frame) = f.action(json!({"op":"get_snapshot"}));
    assert_eq!(s, 200);
    assert_eq!(frame["unread_count"], 100);
    assert_eq!(frame["activity"].as_array().unwrap().len(), 80);
    assert!(serde_json::to_vec(&frame).unwrap().len() + 1 < 256 * 1024);
    assert!(frame["activity"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["message"].as_str().unwrap().len() <= 512));
    assert_eq!(
        db.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        7
    );
    assert_eq!(
        db.query("SELECT COUNT(*) AS n FROM local_signals", &[])
            .unwrap()[0]["n"],
        100
    );
}
#[test]
fn mcp_http_initialize_tool_schemas_and_malformed_boundaries() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let init=f.mcp("initialize",json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}));
    assert_eq!(init["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    let list = f.mcp("tools/list", json!({}));
    let names: Vec<_> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "cito_status",
            "cito_listen",
            "cito_unlisten",
            "cito_find",
            "cito_send",
            "cito_inbox"
        ]
    );
    let listen = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "cito_listen")
        .unwrap();
    assert_eq!(
        listen["inputSchema"]["properties"]["title"],
        json!({"type":"string","minLength":1,"maxLength":128})
    );
    let find = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "cito_find")
        .unwrap();
    assert_eq!(
        find["inputSchema"]["properties"]["harness"]["pattern"],
        "^[a-z0-9][a-z0-9_-]{0,31}$"
    );
    assert_eq!(
        f.tool("cito_find", json!({"harness":"1agent"}), json!({}))["listeners"],
        json!([])
    );
    let expected_listeners = f.tool("cito_find", json!({}), json!({}))["listeners"].clone();
    for v in [
        json!([]),
        json!(42),
        json!(true),
        json!("request"),
        Value::Null,
    ] {
        for path in ["/ui", "/control", "/action", "/mcp"] {
            assert_eq!(http(f.port, path, Some(&v)).0, 400);
        }
        assert_eq!(f.mcp("tools/call", v.clone())["isError"], true);
        let result = f.tool("cito_find", json!({}), v);
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["listeners"], expected_listeners, "{result}");
    }
    assert_eq!(f.mcp("ping", json!({})), json!({}));
    assert_eq!(
        http(
            f.port,
            "/mcp",
            Some(&json!({"jsonrpc":"2.0","method":"ping"}))
        )
        .0,
        202
    );
    assert_eq!(
        http(
            f.port,
            "/mcp",
            Some(&json!({"jsonrpc":"2.0","method":"ping","id":null}))
        ),
        (200, json!({"jsonrpc":"2.0","id":null,"result":{}}))
    );
}
#[test]
fn v29_readable_aliases_reject_ambiguous_live_prefixes_even_for_one_principal() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta);
    let canonical = target["canonical_listener_id"].as_str().unwrap();
    let prefix = &canonical[2..6];
    for length in [8, 12, 16] {
        let query = format!("alice/fixture#{}", &canonical[2..2 + length]);
        let found = f.tool("cito_find", json!({"query":query}), json!({}));
        assert_eq!(found["matched_count"], 1, "{found}");
        assert_eq!(found["listeners"][0]["listener_id"], canonical);
    }
    for to in [
        format!("alice/fixture#{prefix}"),
        format!("fixture#{prefix}"),
        format!("alice#{prefix}"),
        format!("local:alice/fixture#{prefix}"),
    ] {
        assert_eq!(
            send_human(&f, json!({"to":to,"message":"Alias routing"}))["to"],
            target["listener_id"]
        );
    }
    let duplicate = format!("c_{prefix}ffffffffffff");
    let row = json!({"listener_id":duplicate,"public_id":format!("alice-{}",&duplicate[2..]),"owner_user":"alice","principal_id":"none#alice","harness":"fixture","state":"ACTIVE","scope":"workspace","updated_at_ms":cito_native::core::plugins::now_ms(),"binding_generation":1});
    f.db("hub.sqlite").execute("INSERT INTO hub_kv(key,revision,created_ms,value,principal_id,scope) VALUES(?,1,?,CAST(? AS BLOB),'none#alice','workspace')",&[json!(duplicate),json!(cito_native::core::plugins::now_ms()),json!(row.to_string())]).unwrap();
    wait(|| {
        f.tool("cito_find", json!({"harness":"fixture"}), json!({}))["listeners"]
            .as_array()
            .unwrap()
            .len()
            == 2
    });
    for to in [
        format!("alice/fixture#{prefix}"),
        format!("fixture#{prefix}"),
        format!("alice#{prefix}"),
    ] {
        let result = send_human_result(&f, json!({"to":to,"message":"Must disambiguate"}));
        assert_eq!(result["http_status"], 400, "{result}");
        assert_eq!(
            result["error"],
            "INVALID_INPUT: Ambiguous target; use full listener_id"
        );
    }
    let mut retained = row;
    retained["updated_at_ms"] = json!(cito_native::core::plugins::now_ms() - 120_000);
    f.db("hub.sqlite")
        .execute(
            "UPDATE hub_kv SET value=CAST(? AS BLOB) WHERE key=?",
            &[json!(retained.to_string()), json!(duplicate)],
        )
        .unwrap();
    wait(|| {
        let page = f.tool(
            "cito_find",
            json!({"harness":"fixture","limit":1}),
            json!({}),
        );
        page["matched_count"] == 2
            && page["online_count"] == 1
            && page["listeners"].as_array().unwrap().len() == 1
    });
}

#[test]
fn incoming_messages_update_unread_state_and_activity_until_read() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.recording_daemon();
    let frames = || ui_frames(&f);
    for message in ["Review requested", "Build completed"] {
        let sent = f.tool(
            "cito_send",
            json!({"to":"@self","message":message}),
            meta.clone(),
        );
        assert_eq!(sent["ok"], true, "{sent}");
    }
    wait(|| {
        frames()
            .iter()
            .any(|frame| frame["op"] == "state" && frame["unread_count"] == 2)
    });
    assert_eq!(
        f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL")
            .len(),
        2
    );
    let (status, activity) = f.action(json!({"op":"query_directory","filter":"me"}));
    assert_eq!(status, 200);
    assert_eq!(activity["activity_total"], 2);
    assert_eq!(f.snapshot()["hubs"][0]["human_paused"], false);
    f.action_ok(json!({"op":"ack_signals","filter":"me","ack_all":true}));
    wait(|| {
        f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL")
            .is_empty()
    });
    wait(|| {
        frames()
            .iter()
            .rev()
            .find(|frame| frame["op"] == "state")
            .is_some_and(|frame| frame["unread_count"] == 0)
    });
}
#[test]
fn activity_filter_clear_and_read_preserve_hidden_unconfirmed_agent_obligations() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let agent = f.tool("cito_listen", json!({"about":"Pending work"}), meta.clone());
    send_human(
        &f,
        json!({"to":agent["listener_id"],"message":"Pending agent work"}),
    );
    f.tool(
        "cito_send",
        json!({"to":"@self","message":"Human notice"}),
        meta,
    );
    wait(|| f.rows("SELECT * FROM local_signals").len() == 2);
    let (status, me) = f.action(json!({"op":"query_directory","filter":"me"}));
    assert_eq!(status, 200);
    assert_eq!(me["activity_total"], 1);
    assert_eq!(me["activity_has_any"], true);
    f.action_ok(json!({"op":"ack_signals","filter":"me","ack_all":true}));
    assert!(
        f.rows("SELECT * FROM local_signals WHERE message='Pending agent work'")[0]
            ["acknowledged_at_ms"]
            .is_null()
    );
    f.action_ok(json!({"op":"clear_activity_history","filter":"me"}));
    assert_eq!(
        f.rows("SELECT * FROM local_signals WHERE activity_hidden=0")
            .len(),
        1
    );
    f.action_ok(json!({"op":"clear_activity_history","filter":"all"}));
    f.action_ok(json!({"op":"ack_signals","filter":"all","ack_all":true}));
    let pending = f
        .rows("SELECT * FROM local_signals WHERE message='Pending agent work'")
        .remove(0);
    assert_eq!(pending["activity_hidden"], 1);
    assert!(pending["acknowledged_at_ms"].is_null());
    let ledger = f
        .db("hub.sqlite")
        .query(
            "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
            &[pending["signal_id"].clone()],
        )
        .unwrap();
    assert!(ledger[0]["acked_at_ms"].is_null());
    assert_eq!(f.snapshot()["unread_count"], 0);
}
#[test]
fn v29_sender_auto_enrollment_readable_reply_and_clear_close_the_pathway() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let sent = f.tool(
        "cito_send",
        json!({"to":"@self","message":"Agent initiated"}),
        meta.clone(),
    );
    assert_eq!(sent["ok"], true, "{sent}");
    let sender = f.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    assert_eq!(sender["status"], "ACTIVE");
    wait(|| {
        f.rows("SELECT * FROM local_signals WHERE message='Agent initiated'")
            .len()
            == 1
    });
    let early = f.snapshot();
    assert!(
        early["activity"][0]["from_display_address"]
            .as_str()
            .unwrap()
            .contains("/fixture#"),
        "{early}"
    );
    let (status, snapshot) = f.action(json!({"op":"query_directory","filter":"all"}));
    assert_eq!(status, 200, "{snapshot}");
    let activity = &snapshot["activity"][0];
    assert_eq!(activity["from_scoped_id"], sender["public_id"]);
    assert!(activity["from_display_address"]
        .as_str()
        .unwrap()
        .contains("/fixture#"));
    let reply = send_human(
        &f,
        json!({"to":activity["from_display_address"],"message":"Human reply","in_reply_to":activity["signal_id"]}),
    );
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["to"], sender["public_id"]);
    wait(|| f.rows("SELECT * FROM local_signals").len() == 2);
    // Clearing hides Activity rows without deleting the delivery ledger.
    f.action_ok(json!({"op":"clear_activity_history","hub":"local","signal_id":"missing"}));
    assert_eq!(f.rows("SELECT * FROM local_signals").len(), 2);
    f.action_ok(
        json!({"op":"clear_activity_history","hub":"local","signal_id":activity["signal_id"]}),
    );
    let remaining = f.rows("SELECT * FROM local_signals WHERE activity_hidden=0");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["message"], "Human reply");
    f.action_ok(json!({"op":"clear_activity_history"}));
    assert!(f
        .rows("SELECT * FROM local_signals WHERE activity_hidden=0")
        .is_empty());
    assert_eq!(f.rows("SELECT * FROM local_signals").len(), 2);
    f.tool("cito_unlisten", json!({}), meta.clone());
    assert_eq!(
        f.tool(
            "cito_send",
            json!({"to":"@self","message":"Explicitly paused"}),
            meta
        )["ok"],
        true
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "UNLISTENED"
    );
}

#[test]
fn local_mcp_listen_find_send_inbox_ack_idempotency_and_channel_detach() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta.clone());
    assert_eq!(target["hub"], "local");
    assert_eq!(target["scope"], "workspace");
    let status = f.tool("cito_status", json!({}), meta.clone());
    assert_eq!(status["registered"], true);
    assert_eq!(status["title"], "Native fixture");
    let address = format!(
        "alice/fixture#{}",
        &target["canonical_listener_id"].as_str().unwrap()[2..6]
    );
    assert_eq!(target["display_address"], address);
    assert_eq!(status["display_address"], address);
    assert_eq!(status["unread_count"], 0);
    assert_eq!(status["hubs"][0]["is_default"], true);
    assert_eq!(
        status
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "about",
            "display_address",
            "hubs",
            "ok",
            "registered",
            "scope",
            "title",
            "unread_count"
        ]
    );
    for hub in status["hubs"].as_array().unwrap() {
        assert_eq!(hub.as_object().unwrap().len(), 3);
    }
    for forbidden in [
        "cito_setup",
        "cito_rename",
        "cito_unlisten_all",
        "cito_join_hub",
        "cito_channel_save",
        "cito_disconnect_all",
    ] {
        assert_eq!(
            f.tool_result(forbidden, json!({}), meta.clone())["isError"],
            true
        );
    }
    for title in ["".to_owned(), " ".to_owned(), "x".repeat(129)] {
        assert_eq!(
            f.tool_result(
                "cito_listen",
                json!({"about":"Native receiver","title":title}),
                meta.clone()
            )["isError"],
            true
        );
    }
    f.tool(
        "cito_listen",
        json!({"about":"Native receiver","title":"Backend API"}),
        meta.clone(),
    );
    f.tool(
        "cito_listen",
        json!({"about":"Native receiver"}),
        meta.clone(),
    );
    assert_eq!(
        f.tool("cito_status", json!({}), meta.clone())["title"],
        "Backend API"
    );
    let (ok, out, err) = f.command(
        NATIVE,
        &[
            "rename",
            "Release audit",
            "--harness",
            "fixture",
            "--session",
            "session-1",
            "--json",
        ],
        "",
    );
    assert!(ok, "{out} {err}");
    assert_eq!(
        f.tool("cito_status", json!({}), meta.clone())["title"],
        "Release audit"
    );
    let (ok, out, err) = f.command(NATIVE, &["status", "--json"], "");
    assert!(ok, "{out} {err}");
    let snapshot: Value = serde_json::from_str(&out).unwrap();
    assert!(snapshot["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["title"] == "Release audit"));
    assert!(f
        .command(NATIVE, &["status"], "")
        .1
        .contains("AVAILABLE SESSIONS"));
    let find = f.tool("cito_find", json!({"query":"Native receiver"}), json!({}));
    assert!(find["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["scoped_id"] == target["listener_id"]
            && v["listener_id"] == target["canonical_listener_id"]
            && v["display_address"] == address));
    let long = "Context payload segment ".repeat(100).trim().to_string();
    let args = json!({"to":target["listener_id"],"message":long,"idempotency_key":"stable"});
    let sent = send_human(&f, args.clone());
    assert_eq!(sent["status"], "wake_accepted");
    let again = send_human(&f, args);
    assert_eq!(sent["signal_id"], again["signal_id"]);
    wait(|| f.rows("SELECT * FROM local_signals").len() == 1);
    assert!(
        f.rows("SELECT acknowledged_at_ms FROM local_signals")[0]["acknowledged_at_ms"].is_null()
    );
    let inbox = f.tool("cito_inbox", json!({}), meta.clone());
    assert_eq!(inbox["signals"][0]["message"], long);
    assert_eq!(inbox["signals"][0]["from"]["display_address"], "@alice");
    assert_eq!(inbox["acknowledged_count"], 1);
    wait(|| {
        f.db("hub.sqlite")
            .query(
                "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
                std::slice::from_ref(&sent["signal_id"]),
            )
            .unwrap()
            .first()
            .is_some_and(|r| r["acked_at_ms"].is_number())
    });
    assert_eq!(
        f.db("hub.sqlite")
            .query("SELECT action FROM hub_audit_log WHERE action='ack'", &[])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.tool("cito_inbox", json!({}), meta.clone())["signals"],
        json!([])
    );
    assert_eq!(
        f.tool("cito_unlisten", json!({}), meta.clone())["status"],
        "unlistened"
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "UNLISTENED"
    );
    assert_eq!(
        f.tool("cito_status", json!({}), meta.clone())["registered"],
        false
    );
    let (ok, out, err) = f.command(
        NATIVE,
        &[
            "listen",
            "--title",
            "Billing API",
            "--harness",
            "fixture",
            "--session",
            "session-1",
            "Native receiver",
            "--json",
        ],
        "",
    );
    assert!(ok, "{out} {err}");
    assert_eq!(
        f.tool("cito_status", json!({}), meta)["title"],
        "Billing API"
    );
    let (ok, out, err) = f.command(NATIVE, &["unlisten", "--all", "--json"], "");
    assert!(ok, "{out} {err}");
    let stopped: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(stopped["removed"], 1);
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
        .is_empty());
    let options_path = f.state.join("plugins/fixture/options.json");
    let mut options: Value = read_json(&options_path);
    options["session_id"] = json!("session-home");
    options["cwd"] = json!(f.home);
    options["title"] = json!("alice");
    fs::write(options_path, options.to_string()).unwrap();
    let (ok, out, err) = f.command(
        NATIVE,
        &[
            "listen",
            "--harness",
            "fixture",
            "--session",
            "session-home",
            "Auth refactor",
            "--json",
        ],
        "",
    );
    assert!(ok, "{out} {err}");
    let home: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(home["title"], "Auth refactor");
    assert_eq!(home["about"], "Auth refactor");
    assert_eq!(
        f.rows("SELECT title FROM session_custom_titles WHERE session_id='session-home'")[0]
            ["title"],
        "Auth refactor"
    );
}
#[test]
fn confirmed_delivery_business_ack_and_pending_hub_ack_reconciliation() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    let target = enroll(&f, meta);
    send_human(
        &f,
        json!({"to":target["listener_id"],"message":"confirmed"}),
    );
    wait(|| {
        f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NOT NULL")
            .len()
            == 1
    });
    wait(|| {
        f.db("hub.sqlite")
            .query(
                "SELECT * FROM hub_signals WHERE acked_at_ms IS NOT NULL",
                &[],
            )
            .unwrap()
            .len()
            == 1
    });
    wait(|| f.rows("SELECT * FROM pending_hub_acks").is_empty());
    assert_eq!(
        f.db("hub.sqlite")
            .query("SELECT action FROM hub_audit_log WHERE action='ack'", &[])
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn standalone_http_hub_dedup_atomic_batch_claims_and_business_ack() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let (_, p) = f.hub();
    assert_eq!(
        http(p, "/health", None).1["version"],
        env!("CARGO_PKG_VERSION")
    );
    let id = "c_1111111111111111";
    rpc(
        p,
        "kv_put",
        json!({"key":id,"data_b64":B64.encode(json!({"listener_id":id,"principal_id":"none#alice","owner_user":"alice","harness":"fixture","session_id":"s","scope":"workspace","state":"ACTIVE","binding_generation":1,"channels":{"a":{"state":"ACTIVE"},"b":{"state":"ACTIVE"}}}).to_string())}),
    );
    for signal in ["one", "two", "three"] {
        let publish = json!({"subject":format!("cito.signal.{id}"),"data_b64":B64.encode(json!({"signal_id":signal,"to":id,"target_generation":1,"message":"hello"}).to_string()),"opts":{"msgID":signal}});
        assert_eq!(
            rpc(p, "js_publish", publish.clone())["seq"],
            rpc(p, "js_publish", publish)["seq"]
        );
    }
    assert_eq!(
        rpc(
            p,
            "signal_claim_dispatch",
            json!({"node_id":id,"channel_id":"b","signal_ids":["two"]})
        )["claimed"],
        true
    );
    let conflict = rpc(
        p,
        "signal_claim_dispatch",
        json!({"node_id":id,"channel_id":"a","signal_ids":["one","two"]}),
    );
    assert_eq!(conflict["claimed"], false);
    assert_eq!(conflict["claimed_ids"], json!([]));
    let db = f.db("hub/hub.sqlite");
    assert!(db
        .query(
            "SELECT claimed_by_channel FROM hub_signals WHERE signal_id='one'",
            &[]
        )
        .unwrap()[0]["claimed_by_channel"]
        .is_null());
    rpc(
        p,
        "signal_ack",
        json!({"node_id":id,"channel_id":"b","signal_ids":["two"]}),
    );
    assert_eq!(
        rpc(
            p,
            "signal_claim_dispatch",
            json!({"node_id":id,"channel_id":"a","signal_ids":["three","two"]})
        )["acked_ids"],
        json!(["two"])
    );
    assert!(db
        .query(
            "SELECT claimed_by_channel FROM hub_signals WHERE signal_id='three'",
            &[]
        )
        .unwrap()[0]["claimed_by_channel"]
        .is_null());
}
#[test]
fn two_remote_hubs_isolate_identifiers_and_offline_inbox_remains_readable() {
    let _lock = serial();
    let a = Fixture::new(json!({}));
    let b = Fixture::new(json!({}));
    let (ha, pa) = a.hub();
    let (hb, pb) = b.hub();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://","user":"alice"},"personal":{"url":format!("http://127.0.0.1:{pa}"),"user":"alice"},"work":{"url":format!("http://127.0.0.1:{pb}"),"user":"alice"}},"defaultHub":"personal"}),
    );
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    wait(|| {
        f.snapshot()["available"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["harness"] == "fixture" && row["session_id"] == "session-1")
    });
    for hub in ["personal", "work"] {
        let target = f.tool(
            "cito_listen",
            json!({"hub":hub,"about":"Multi hub","scope":"workspace"}),
            meta.clone(),
        );
        let sent = send_human(&f, json!({"to":target["listener_id"],"message":hub}));
        assert!(sent["signal_id"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{hub}:")));
    }
    f.action_ok(
        json!({"op":"rename_session","harness":"fixture","session_id":"session-1","title":"alice"}),
    );
    // Still available on local: the cached candidate must agree immediately,
    // without waiting for discovery or an eventual-consistency retry.
    let snapshot = f.snapshot();
    let candidate = snapshot["available"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["harness"] == "fixture" && row["session_id"] == "session-1")
        .unwrap();
    assert_eq!(candidate["title"], "alice");
    assert_eq!(candidate["custom_title"], true);
    assert!(snapshot["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["harness"] == "fixture")
        .all(|row| row["title"] == candidate["title"] && row["custom_title"] == true));
    f.tool(
        "cito_listen",
        json!({"hub":"local","about":"Multi hub","scope":"workspace"}),
        meta.clone(),
    );
    let binding = f.rows("SELECT endpoint_pid,binding_generation,owner_token FROM local_listeners WHERE hub=\'work\' AND harness=\'fixture\'")[0].clone();
    f.db("state.sqlite")
        .execute(
            "UPDATE local_listeners SET endpoint_pid=? WHERE hub=\'work\' AND harness=\'fixture\'",
            &[json!(i32::MAX)],
        )
        .unwrap();
    f.tool(
        "cito_listen",
        json!({"hub":"personal","about":"Multi hub"}),
        meta.clone(),
    );
    let retained = f.rows("SELECT endpoint_pid,binding_generation,owner_token FROM local_listeners WHERE hub=\'work\' AND harness=\'fixture\'")[0].clone();
    assert_eq!(
        retained["endpoint_pid"],
        i32::MAX,
        "Registering on personal must not rewrite work endpoint metadata"
    );
    assert_eq!(
        retained["binding_generation"],
        binding["binding_generation"]
    );
    assert_eq!(retained["owner_token"], binding["owner_token"]);
    f.db("state.sqlite")
        .execute(
            "UPDATE local_listeners SET endpoint_pid=? WHERE hub=\'work\' AND harness=\'fixture\'",
            &[binding["endpoint_pid"].clone()],
        )
        .unwrap();
    let all = f.tool("cito_find", json!({"harness":"fixture"}), json!({}));
    assert_eq!(all["listeners"].as_array().unwrap().len(), 1);
    assert_eq!(all["matched_count"], all["online_count"]);
    assert_eq!(all["matched_count"], 1);
    let primary = &all["listeners"][0];
    assert_eq!(primary["hub"], "personal");
    assert_eq!(primary["hubs"], json!(["personal", "local", "work"]));
    assert!(primary
        .as_object()
        .unwrap()
        .keys()
        .all(|key| !key.starts_with("can_")));
    assert!(snapshot["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["can_stop"] == true));
    assert!(all["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["title"] == "alice" && r["custom_title"] == true));
    let local = f.tool(
        "cito_find",
        json!({"hub":"local","harness":"fixture"}),
        json!({}),
    );
    assert_eq!(local["listeners"].as_array().unwrap().len(), 1);
    assert_eq!(local["listeners"][0]["title"], "alice");
    for hub in ["personal", "work"] {
        let stopped = if hub == "personal" {
            f.tool("cito_unlisten", json!({"hub":hub}), meta.clone())
        } else {
            let (ok, out, err) = f.command(
                NATIVE,
                &[
                    "unlisten",
                    "--hub",
                    hub,
                    "--harness",
                    "fixture",
                    "--session",
                    "session-1",
                    "--json",
                ],
                "",
            );
            assert!(ok, "{out} {err}");
            serde_json::from_str(&out).unwrap()
        };
        assert_eq!(stopped["removed"], 1);
        let remaining = f.snapshot();
        assert!(remaining["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["hub"] == "local"
                && row["harness"] == "fixture"
                && row["title"] == "alice"
                && row["custom_title"] == true));
        assert_eq!(f.rows("SELECT title FROM session_custom_titles WHERE harness='fixture' AND session_id='session-1'")[0]["title"], "alice");
        // Re-enrollment must retain the shared custom title too.
        f.tool(
            "cito_listen",
            json!({"hub":hub,"about":"Multi hub"}),
            meta.clone(),
        );
        assert_eq!(
            f.tool("cito_status", json!({"hub":hub}), meta.clone())["title"],
            "alice"
        );
    }
    f.action_ok(
        json!({"op":"unlisten_session","hub":"local","harness":"fixture","session_id":"session-1"}),
    );
    let scoped = f.rows("SELECT hub,status FROM local_listeners WHERE harness='fixture'");
    assert!(scoped
        .iter()
        .any(|v| v["hub"] == "local" && v["status"] == "UNLISTENED"));
    assert!(scoped
        .iter()
        .filter(|v| v["hub"] != "local")
        .all(|v| v["status"] == "ACTIVE"));
    f.tool(
        "cito_listen",
        json!({"hub":"local","about":"Multi hub"}),
        meta.clone(),
    );
    wait(|| f.rows("SELECT * FROM local_signals").len() == 2);
    a.stop(ha);
    b.stop(hb);
    let (status, directory) = f.action(json!({"op":"query_directory","query":"","scope":"all","harness":"all","request_id":"offline-query"}));
    assert_eq!(status, 200, "{directory}");
    let warnings = directory["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|w| w.starts_with("personal:"))),
        "{directory}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|w| w.starts_with("work:"))),
        "{directory}"
    );
    let db = f.db("state.sqlite");
    db.execute("CREATE TRIGGER fail_work_ack BEFORE INSERT ON pending_hub_acks WHEN NEW.hub='work' BEGIN SELECT RAISE(ABORT,'work ack unavailable'); END", &[]).unwrap();
    let rejected = f.tool_result("cito_inbox", json!({}), meta.clone());
    assert_eq!(rejected["isError"], true);
    assert!(rejected.to_string().contains("work ack unavailable"));
    assert!(f.rows("SELECT 1 FROM pending_hub_acks").is_empty());
    assert!(f
        .rows("SELECT 1 FROM local_signals WHERE acknowledged_at_ms IS NOT NULL")
        .is_empty());
    db.execute("DROP TRIGGER fail_work_ack", &[]).unwrap();
    let inbox = f.tool("cito_inbox", json!({}), meta.clone());
    assert_eq!(inbox["signals"].as_array().unwrap().len(), 2);
    assert_eq!(f.rows("SELECT * FROM pending_hub_acks").len(), 2);
    for args in [
        json!({"hub":"personal","to":"@self"}),
        json!({"to":"personal:@self"}),
        json!({"to":"@missing"}),
    ] {
        let mut args = args;
        args["message"] = json!("Do not reroute");
        let rejected = f.tool_result("cito_send", args, meta.clone());
        assert_eq!(rejected["isError"], true, "{rejected}");
        assert!(
            rejected.to_string().contains("HUB_UNAVAILABLE"),
            "{rejected}"
        );
    }
    let sent = f.tool(
        "cito_send",
        json!({"to":"@self","message":"Local wake"}),
        meta.clone(),
    );
    assert!(
        sent["signal_id"].as_str().unwrap().starts_with("local:"),
        "{sent}"
    );
    assert_eq!(
        f.action(json!({"op":"unlisten_session","harness":"fixture","session_id":"session-1"}))
            .0,
        400
    );
    assert!(f
        .rows("SELECT status FROM local_listeners WHERE harness='fixture'")
        .iter()
        .all(|v| v["status"] == "UNLISTENED"));
}
#[test]
fn implicit_send_keeps_the_remote_reservation_after_a_lost_publish_receipt() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (_, upstream) = host.hub();
    let proxy = port();
    let f = Fixture::new(json!({"default_hub":"work","hubs":{
        "work":{"url":format!("http://127.0.0.1:{proxy}")}, "local":{"url":"local://"}
    }}));
    let meta = f.provider("delivered");
    // Drop the first publish response only after the real Hub has committed it.
    // Keep the RPC route offline until the test explicitly reconnects it.
    let relay = f.spawn("node", &["-e", r#"
const http=require('node:http');
let offline=false,first=true;
http.createServer(async(req,res)=>{
 if(req.url==='/reconnect'||req.url==='/disconnect'){offline=req.url==='/disconnect';return res.end('{}');}
 if(offline||req.url!=='/v1/rpc'){res.writeHead(503);return res.end();}
 let body='';for await(const chunk of req)body+=chunk;
 const upstream=await fetch('http://127.0.0.1:'+process.argv[2]+req.url,{method:'POST',body,headers:{'content-type':'application/json','x-cito-user':req.headers['x-cito-user']}});
 const receipt=await upstream.text();
 if(first&&JSON.parse(body).op==='js_publish'&&JSON.parse(receipt).ok){first=false;offline=true;return res.destroy();}
 res.writeHead(upstream.status,{'Content-Length':Buffer.byteLength(receipt)});res.end(receipt);
}).listen(+process.argv[1],'127.0.0.1',()=>console.log('ready'));
"#, &proxy.to_string(), &upstream.to_string()], &[]);
    wait(|| f.output(relay, false).contains("ready"));
    f.daemon(&[]);
    let send = json!({"to":"@self","message":"Reserved on work","idempotency_key":"once"});
    let reject = |args: Value, code: &str| {
        let result = f.tool_result("cito_send", args, meta.clone());
        assert_eq!(result["isError"], true, "{result}");
        let body: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], code, "{body}");
    };
    // The same alias is deliverable on both Hubs, so a fallback would really publish.
    f.tool(
        "cito_send",
        json!({"hub":"local","to":"@self","message":"Local control"}),
        meta.clone(),
    );
    reject(send.clone(), "HUB_UNAVAILABLE");
    let pending = f.rows("SELECT * FROM local_sent_idempotency WHERE hub='work'");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["status"], "PENDING_PUBLISH");
    let remote = host.db("hub/hub.sqlite");
    let committed = remote
        .query("SELECT signal_id,seq FROM hub_signals", &[])
        .unwrap();
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0]["signal_id"], pending[0]["signal_id"]);
    reject(send.clone(), "HUB_UNAVAILABLE");
    let mut changed = send.clone();
    changed["message"] = json!("Different payload");
    reject(changed, "IDEMPOTENCY_KEY_CONFLICT");
    let mut retargeted = send.clone();
    retargeted["to"] = f
        .rows("SELECT listener_id FROM local_listeners WHERE hub='work' AND harness='fixture'")[0]
        ["listener_id"]
        .clone();
    reject(retargeted, "IDEMPOTENCY_KEY_CONFLICT");
    assert_eq!(http(proxy, "/reconnect", None).0, 200);
    let recovered = f.tool("cito_send", send.clone(), meta.clone());
    assert_eq!(
        recovered["signal_id"],
        format!("work:{}", committed[0]["signal_id"].as_str().unwrap())
    );
    assert_eq!(recovered["seq"], committed[0]["seq"]);
    assert_eq!(
        remote
            .query("SELECT signal_id,seq FROM hub_signals", &[])
            .unwrap(),
        committed
    );
    assert_eq!(http(proxy, "/disconnect", None).0, 200);
    reject(send.clone(), "HUB_UNAVAILABLE");
    let local = f.db("hub.sqlite");
    assert_eq!(
        local
            .query("SELECT seq FROM hub_signals", &[])
            .unwrap()
            .len(),
        1
    );
    let mut fresh = send;
    fresh["idempotency_key"] = json!("another-key");
    let new_send = f.tool("cito_send", fresh, meta);
    assert!(new_send["signal_id"]
        .as_str()
        .unwrap()
        .starts_with("local:"));
}

#[test]
fn implicit_send_reuses_local_reservations_after_default_hub_recovers() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let remote = port();
    let f = Fixture::new(json!({"default_hub":"work","hubs":{
        "work":{"url":format!("http://127.0.0.1:{remote}")}, "local":{"url":"local://","user":"bob"}
    }}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    let send = |args| f.tool("cito_send", args, meta.clone());
    let control_args =
        json!({"to":"@self","message":"Explicit route control","idempotency_key":"fresh"});
    let mut explicit = control_args.clone();
    explicit["hub"] = json!("local");
    send(explicit);
    let args = json!({"to":"@self","message":"Committed locally","idempotency_key":"once"});
    let receipt = send(args.clone());
    assert!(receipt["signal_id"].as_str().unwrap().starts_with("local:"));

    // Lose local bookkeeping after a second real publication, leaving it pending.
    let db = f.db("state.sqlite");
    db.execute("CREATE TRIGGER lose_receipt BEFORE UPDATE ON local_sent_idempotency BEGIN SELECT RAISE(ABORT,'lost receipt'); END", &[]).unwrap();
    let pending = json!({"to":"@self","message":"Pending locally","idempotency_key":"pending"});
    let first = send(pending.clone());
    assert!(first["maintenance_warning"].is_string());
    db.execute("DROP TRIGGER lose_receipt", &[]).unwrap();
    let local = f.db("hub.sqlite");
    let signals = || {
        local
            .query("SELECT signal_id,seq FROM hub_signals ORDER BY seq", &[])
            .unwrap()
    };
    // V7 used the physical local dedup ID even when the default route fell back.
    // A direct send produces that same wire ID; remove its post-upgrade marker.
    let legacy = json!({"to":"@self","message":"Before upgrade","idempotency_key":"legacy"});
    let mut direct = legacy.clone();
    direct["hub"] = json!("local");
    let before_upgrade = send(direct);
    let signal_id = before_upgrade["signal_id"]
        .as_str()
        .unwrap()
        .strip_prefix("local:")
        .unwrap();
    assert_eq!(db.execute("UPDATE local_sent_idempotency SET fallback_key=NULL WHERE hub='local' AND signal_id=? AND fallback_key IS NOT NULL", &[json!(signal_id)]).unwrap(), 1);
    let original = signals();
    assert_eq!(original.len(), 4);
    f.action_ok(json!({"op":"setup_toggle_hub","hub":"local","enabled":false}));

    host.spawn(
        NATIVE,
        &[
            "hub",
            "--bind",
            "127.0.0.1",
            "--port",
            &remote.to_string(),
            "--data-dir",
            host.state.join("hub").to_str().unwrap(),
            "--auth",
            "none",
        ],
        &[],
    );
    wait(|| std::net::TcpStream::connect(("127.0.0.1", remote)).is_ok());
    let control = send(control_args);
    let remote_id = control["signal_id"]
        .as_str()
        .unwrap()
        .strip_prefix("work:")
        .unwrap();
    assert_eq!(send(args.clone()), receipt);
    let legacy_blocked = f.tool_result("cito_send", legacy.clone(), meta.clone());
    assert_eq!(legacy_blocked["isError"], true, "{legacy_blocked}");
    assert!(
        legacy_blocked.to_string().contains("HUB_UNAVAILABLE"),
        "{legacy_blocked}"
    );
    let blocked = f.tool_result("cito_send", pending.clone(), meta.clone());
    assert_eq!(blocked["isError"], true, "{blocked}");
    assert!(blocked.to_string().contains("HUB_UNAVAILABLE"), "{blocked}");
    for (field, value) in [
        ("message", "Different payload"),
        ("to", "c_0123456789abcdef"),
        ("to", "@another-user"),
    ] {
        let mut changed = args.clone();
        changed[field] = json!(value);
        let result = f.tool_result("cito_send", changed, meta.clone());
        assert_eq!(result["isError"], true, "{result}");
        let body: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "IDEMPOTENCY_KEY_CONFLICT", "{body}");
    }
    f.action_ok(json!({"op":"setup_toggle_hub","hub":"local","enabled":true}));
    assert_eq!(send(args.clone()), receipt);
    assert_eq!(send(legacy), before_upgrade);
    let mut changed_alias = args;
    changed_alias["to"] = json!("@another-user");
    let rejected = f.tool_result("cito_send", changed_alias, meta.clone());
    assert_eq!(rejected["isError"], true, "{rejected}");
    assert!(
        rejected.to_string().contains("IDEMPOTENCY_KEY_CONFLICT"),
        "{rejected}"
    );
    let replay = send(pending);
    assert_eq!(replay["signal_id"], first["signal_id"]);
    assert_eq!(replay["seq"], first["seq"]);
    assert!(replay["maintenance_warning"].is_null());
    assert!(f
        .rows("SELECT 1 FROM local_sent_idempotency WHERE status='PENDING_PUBLISH'")
        .is_empty());
    assert_eq!(signals(), original);
    assert_eq!(
        host.db("hub/hub.sqlite")
            .query("SELECT signal_id FROM hub_signals", &[])
            .unwrap(),
        vec![json!({"signal_id":remote_id})]
    );
}

#[test]
fn explicit_local_reservations_do_not_intercept_default_hub_sends() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (_, remote) = host.hub();
    let f = Fixture::new(json!({"default_hub":"work","hubs":{
        "work":{"url":format!("http://127.0.0.1:{remote}")}, "local":{"url":"local://"}
    }}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    let send = |args| f.tool("cito_send", args, meta.clone());
    for (i, (field, value)) in [("hub", "local"), ("to", "local:@self")]
        .into_iter()
        .enumerate()
    {
        let implicit = json!({"to":"@self","message":"Same payload","idempotency_key":field});
        let mut explicit = implicit.clone();
        explicit[field] = json!(value);
        let local = send(explicit);
        let work = send(implicit);
        for (db, receipt, hub) in [
            (f.db("hub.sqlite"), local, "local"),
            (host.db("hub/hub.sqlite"), work, "work"),
        ] {
            let rows = db
                .query("SELECT signal_id FROM hub_signals ORDER BY seq", &[])
                .unwrap();
            assert_eq!(rows.len(), i + 1, "{hub}: {rows:?}");
            assert_eq!(
                receipt["signal_id"],
                format!("{hub}:{}", rows[i]["signal_id"].as_str().unwrap())
            );
        }
    }
}

#[test]
fn implicit_send_fallback_requires_an_in_process_local_hub() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (_, remote) = host.hub();
    let offline = port();
    let f = Fixture::new(json!({"default_hub":"work","hubs":{
        "work":{"url":format!("http://127.0.0.1:{offline}")},
        "local":{"url":format!("http://127.0.0.1:{remote}")}
    }}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    f.tool(
        "cito_send",
        json!({"hub":"local","to":"@self","message":"Explicit route"}),
        meta.clone(),
    );
    let rejected = f.tool_result(
        "cito_send",
        json!({"to":"@self","message":"Same-host only"}),
        meta,
    );
    assert_eq!(rejected["isError"], true, "{rejected}");
    assert!(
        rejected.to_string().contains("HUB_UNAVAILABLE"),
        "{rejected}"
    );
    assert!(f
        .rows("SELECT 1 FROM local_signals WHERE message='Same-host only'")
        .is_empty());
}

#[test]
fn stdio_signal_handlers_exit_cleanly_and_daemon_health_owns_sandbox() {
    let _lock = serial();
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let f = Fixture::new(json!({}));
        f.daemon(&[]);
        let p = f.spawn(NATIVE, &["mcp-stdio"], &[]);
        f.send(p,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}));
        wait(|| !f.frames(p).is_empty());
        f.signal(p, signal);
        wait(|| f.exited(p));
        f.stop(p);
        assert_eq!(http(f.port, "/health", None).1["stateDir"], json!(f.state));
    }
}
