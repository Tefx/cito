use crate::support::*;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
fn matches_process(command: &str) -> bool {
    let Some((program, script)) = process_entrypoint(command) else {
        return false;
    };
    let entry = script.unwrap_or(program);
    let name = Path::new(entry)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if script.is_none() {
        return matches!(name, "pi" | "pi-coding-agent");
    }
    matches!(name, "pi" | "pi.js" | "pi.mjs")
        || (entry.contains("/pi-coding-agent/") && matches!(name, "cli.js" | "cli.mjs"))
}
fn paths(req: &Value) -> (PathBuf, PathBuf) {
    (
        config_home(req, "CITO_PI_HOME", "~/.pi/agent"),
        str_at(req, "plugin_dir")
            .map(|s| expand(req, s))
            .unwrap_or_else(|| state(req).join("runtime/plugins/pi")),
    )
}
fn setup(req: &Value) -> Result<Value> {
    let (home, package) = paths(req);
    let settings = home.join("settings.json");
    let mcp_paths = [home.join("mcp.json"), home.join("mcp/global.mcp.json")];
    let legacy = home.join("extensions/cito-bridge.ts");
    let legacy_package = state(req).join("plugins/pi");
    let runtime_package = state(req).join("runtime/plugins/pi");
    let op = str_at(req, "op").unwrap_or("");
    if op == "setup_inspect" {
        let v = read_json(&settings)?;
        // Host registration survives missing staged assets; setup owns integrity checks.
        let mut configured = v["packages"]
            .as_array()
            .map(|a| {
                a.iter().any(|x| {
                    x.as_str() == package.to_str()
                        || (req.get("plugin_dir").is_none_or(Value::is_null)
                            && x.as_str() == legacy_package.to_str())
                })
            })
            .unwrap_or(false)
            && !legacy.exists();
        for path in &mcp_paths {
            configured &= read_json(path)?["mcpServers"].get("cito").is_none();
        }
        let mut paths = vec![settings];
        paths.extend(mcp_paths);
        paths.push(legacy);
        return Ok(
            json!({"ok":true,"installed":home.exists(),"configured":configured,"paths":paths}),
        );
    }
    let remove = op == "setup_remove";
    let mut actions = vec![json_mutate(req, "pi", &settings, |v| {
        let mut packages = match v.get("packages") {
            Some(Value::Array(a)) => a.clone(),
            None => vec![],
            _ => return Err("Pi packages must be an array".into()),
        };
        packages.retain(|x| {
            x.as_str() != Some("npm:@tefx/cito")
                && x.as_str() != Some("@tefx/cito")
                && x.as_str() != package.to_str()
                && x.as_str() != legacy_package.to_str()
                && x.as_str() != runtime_package.to_str()
        });
        if !remove {
            packages.push(json!(package));
        }
        if packages.is_empty() {
            v.as_object_mut().map(|o| o.remove("packages"));
        } else {
            v["packages"] = json!(packages);
        }
        Ok(())
    })?];
    for path in &mcp_paths {
        actions.push(json_mutate(req, "pi", path, |v| {
            if let Some(o) = v.get_mut("mcpServers").and_then(Value::as_object_mut) {
                o.remove("cito");
                if o.is_empty() {
                    v.as_object_mut().map(|o| o.remove("mcpServers"));
                }
            }
            Ok(())
        })?);
    }
    actions.push(mutate(req, "pi", &legacy, |_| Ok(None))?);
    if remove && req["dry_run"] != true {
        if let Ok(entries) = fs::read_dir(state(req).join("sockets")) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("pi-pid-") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
    Ok(json!({"ok":true,"actions":actions}))
}
fn identify(req: &Value, p: u32) -> Result<Value> {
    let response = socket_call(
        &state(req).join(format!("sockets/pi-pid-{p}.sock")),
        &json!({"op":"identify"}),
    )?;
    if response["ok"] != true {
        let _ = fs::remove_file(state(req).join(format!("pi/endpoints/{p}.json")));
    }
    Ok(response)
}
fn allow_process_only(req: &Value) -> bool {
    env_value(req, "CITO_PI_PROCESS_DISCOVERY").as_deref() == Some("1")
        && env_value(req, "CITO_PI_REQUIRE_BRIDGE").as_deref() != Some("1")
}
fn interactive_candidate(req: &Value, pid: u32) -> Result<bool> {
    if req["op"] != "discover_sessions" {
        return Ok(true);
    }
    let command = process_command(pid)?;
    if !process_has_tty(pid)
        && env_value(req, "CITO_PI_ALLOW_HEADLESS_DISCOVERY").as_deref() != Some("1")
    {
        return Ok(false);
    }
    let args: Vec<_> = command.split_whitespace().collect();
    Ok(!args.iter().any(|a| {
        matches!(
            *a,
            "--print" | "-p" | "--no-session" | "--version" | "--help"
        ) || matches!(*a, "--mode=rpc" | "--mode=json")
    }) && !args
        .windows(2)
        .any(|w| w[0] == "--mode" && matches!(w[1], "rpc" | "json")))
}
// Session files are hints for live, same-user processes, never evidence of bridge readiness.
fn process_candidates(req: &Value, out: &mut Vec<(u32, Value)>) -> Result<()> {
    if env_value(req, "CITO_PI_PROCESS_DISCOVERY").as_deref() == Some("0") {
        return Ok(());
    }
    let inspection = setup(
        &json!({"op":"setup_inspect","env":req["env"],"state_dir":req["state_dir"],"plugin_dir":req["plugin_dir"]}),
    )?;
    if inspection["configured"] != true {
        return Ok(());
    }
    let mut processes: Vec<u32> = user_processes()?
        .into_iter()
        .filter(|p| {
            !out.iter().any(|(pid, _)| pid == p)
                && process_command(*p).is_ok_and(|command| matches_process(&command))
        })
        .collect();
    // Stable ordering makes the shared-cwd session assignment consistent across requests.
    processes.sort_unstable();
    for p in processes {
        // Socket endpoints own their discovery result; never replace a slow bridge
        // with a process-only candidate that incorrectly asks the user to reload.
        if state(req).join(format!("sockets/pi-pid-{p}.sock")).exists()
            || !interactive_candidate(req, p).unwrap_or(false)
        {
            continue;
        }
        let identity = match process_identity(p) {
            Ok(identity) => identity,
            Err(_) => continue,
        };
        if !matches_process(str_at(&identity, "command").unwrap_or("")) {
            continue;
        }
        let Ok(cwd) = process_cwd(p) else { continue };
        let cwd = cwd.as_str();
        let basename = Path::new(cwd)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(cwd);
        let encoded = format!("--{}--", cwd.trim_start_matches('/').replace('/', "-"));
        let directory = paths(req).0.join("sessions").join(encoded);
        let latest = fs::read_dir(directory)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|s| s == "jsonl"))
            .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
            .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut v = json!({"session_id":format!("pid-{p}"),"session_name":basename,"cwd":cwd});
        if let Some((modified, path)) = latest {
            use std::io::BufRead;
            let projection = cached_file_projection(req, "pi/transcript-cache", &path, || {
                let mut value =
                    json!({"session_id":path.file_stem().and_then(|s| s.to_str()).unwrap_or("")});
                let file = fs::File::open(&path).map_err(|e| e.to_string())?;
                for line in std::io::BufReader::new(file).lines() {
                    let line = line.map_err(|e| e.to_string())?;
                    let Ok(row) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if row["type"] == "session" || row.get("type").is_none() {
                        if let Some(id) = str_at(&row, "id") {
                            value["session_id"] = json!(id);
                        }
                    }
                    if let Some(name) =
                        str_at(&row, "session_name").or_else(|| str_at(&row, "name"))
                    {
                        value["session_name"] = json!(name);
                    }
                }
                Ok(value)
            });
            if let Ok(projection) = projection {
                v["session_id"] = projection["session_id"].clone();
                if let Some(name) = projection.get("session_name") {
                    v["session_name"] = name.clone();
                }
            }
            v["updated_at_ms"] = json!(modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0));
        }
        if out
            .iter()
            .any(|(_, earlier)| earlier["session_id"] == v["session_id"])
        {
            v["session_id"] = json!(format!("pid-{p}"));
            v["session_name"] = json!(basename);
        }
        if !allow_process_only(req) {
            v["needs_reload"] = json!(true);
            v["reload_hint"] = json!("Run /reload in Pi to enable 1-click listen");
        }
        // Fence PID reuse while vnode and session metadata were read.
        match process_identity(p) {
            Ok(current) if current == identity => out.push((p, v)),
            Ok(_) => {}
            Err(_) => {}
        }
    }
    Ok(())
}
fn socket_candidates(req: &Value) -> Result<Vec<(u32, Value)>> {
    let request = req.clone();
    inspect_socket_endpoints(req, "pi-pid-", move |p, _| {
        let req = &request;
        let cache_path = state(req).join(format!("pi/endpoints/{p}.json"));
        let response = match identify(req, p) {
            Ok(v) => {
                if v["ok"] == true {
                    if let Ok(identity) = process_identity(p) {
                        let _ = atomic_write(
                            &cache_path,
                            json!({"identity":identity,"session":v})
                                .to_string()
                                .as_bytes(),
                        );
                    }
                }
                Ok(v)
            }
            Err(error) if error == "Socket request timed out" => {
                let cached = read_json(&cache_path).ok();
                match (cached, process_identity(p)) {
                    (Some(cached), Ok(identity))
                        if alive(p)
                            && cached["identity"] == identity
                            && matches_process(str_at(&identity, "command").unwrap_or("")) =>
                    {
                        Ok(cached["session"].clone())
                    }
                    _ => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        let v = response.ok()?;
        (v["ok"] == true
            && interactive_candidate(req, p).unwrap_or(false)
            && str_at(&v, "session_id").is_some()
            && str_at(&v, "cwd").is_some())
        .then_some(v)
    })
}
type Candidates = (Vec<(u32, Value)>, bool, Option<String>);
fn candidates(req: &Value) -> Result<Candidates> {
    let resolve = matches!(str_at(req, "op"), Some("resolve_caller" | "verify"));
    if resolve || pid(req).is_some() {
        let roots = if resolve {
            caller_pid_roots(req).unwrap_or_default()
        } else {
            pid(req).into_iter().collect()
        };
        let mut responded = false;
        let mut searched = Vec::new();
        for mut p in roots {
            for _ in 0..=if resolve { 5 } else { 0 } {
                searched.push(p);
                if let Ok(v) = identify(req, p) {
                    responded = true;
                    if v["ok"] != true {
                        return Ok((
                            vec![],
                            true,
                            Some(response_diagnostic(&v, "Pi bridge rejected identification")),
                        ));
                    }
                    if v["ok"] == true
                        && str_at(&v, "session_id").is_some()
                        && str_at(&v, "cwd").is_some()
                        && v.get("pi_pid")
                            .is_none_or(|id| id.as_u64() == Some(p as u64))
                    {
                        return Ok((vec![(p, v)], true, None));
                    }
                }
                let Some(parent) = process_parent(p) else {
                    break;
                };
                p = parent;
            }
        }
        // A responding bridge with no open session must not be bypassed.
        let mut live = vec![];
        if !responded {
            live = socket_candidates(req)?;
            process_candidates(req, &mut live)?;
            // Prefer the nearest caller ancestor, just as socket resolution does.
            // An explicit endpoint must not become ambiguous with its host ancestors.
            live = searched
                .iter()
                .find_map(|p| live.iter().find(|(pid, _)| pid == p).cloned())
                .into_iter()
                .collect();
        }
        return Ok((live, responded, None));
    }
    let mut out = socket_candidates(req)?;
    process_candidates(req, &mut out)?;
    Ok((out, false, None))
}
fn as_caller(req: &Value, p: u32, v: &Value) -> Value {
    caller(
        req,
        "pi",
        str_at(v, "session_id").unwrap_or(""),
        str_at(v, "cwd").unwrap_or(""),
        Some(p),
        str_at(v, "session_name"),
    )
}
pub fn handle(input: &Value) -> Result<Value> {
    let req = input;
    let op = str_at(req, "op").unwrap_or("");
    if op.starts_with("setup_") {
        return setup(req);
    }
    if matches!(op, "resolve_caller" | "verify") {
        if let Err(e) = explicit_pid(req) {
            return Ok(error(
                if e.starts_with("Invalid") {
                    "PI_BRIDGE_UNAVAILABLE"
                } else {
                    "INVALID_INPUT"
                },
                e,
            ));
        }
    }
    let (cs, endpoint_responded, rejection) = if matches!(
        op,
        "resolve_caller" | "verify" | "probe" | "discover_sessions"
    ) {
        candidates(req)?
    } else {
        (vec![], false, None)
    };
    match op {
        "discover_sessions" => Ok(
            json!({"ok":true,"sessions":cs.iter().map(|(p,v)|{let mut c=as_caller(req,*p,v);c["updated_at_ms"]=v["updated_at_ms"].clone();c["needs_reload"]=v["needs_reload"].clone();c["reload_hint"]=v["reload_hint"].clone();c}).collect::<Vec<_>>()}),
        ),
        "resolve_caller" | "verify" => {
            if cs.iter().any(|(_, v)| v["needs_reload"] == true) {
                return Ok(error(
                    "PI_BRIDGE_UNAVAILABLE",
                    "Run /reload in Pi to enable 1-click listen",
                ));
            }
            if cs.is_empty() && endpoint_responded {
                return Ok(error(
                    "PI_BRIDGE_UNAVAILABLE",
                    rejection.unwrap_or_else(|| "No caller socket bridge found".into()),
                ));
            }
            if cs.is_empty() {
                match cached_caller(req, "pi", None, matches_process) {
                    Ok(Some(c)) => {
                        if !allow_process_only(req)
                            && !pid(&c).is_some_and(|p| {
                                identify(req, p).is_ok_and(|v| {
                                    v["ok"] == true && v["session_id"] == c["session_id"]
                                })
                            })
                        {
                            return Ok(error(
                                "PI_BRIDGE_UNAVAILABLE",
                                "Run /reload in Pi to enable 1-click listen",
                            ));
                        }
                        return Ok(json!({"ok":true,"caller":c}));
                    }
                    Err(e) => return Ok(e),
                    Ok(None) => {}
                }
            }
            let had_live = !cs.is_empty();
            let matches: Vec<_> = cs
                .into_iter()
                .filter(|(_, v)| {
                    str_at(req, "explicit_session_id")
                        .or_else(|| str_at(&req["meta"], "expected_session_id"))
                        .map(|s| v["session_id"] == s)
                        .unwrap_or(true)
                })
                .collect();
            if matches.len() != 1 {
                return Ok(error(
                    if had_live && matches.is_empty() {
                        "INVALID_INPUT"
                    } else if explicit_pid(req).ok().flatten().is_some() {
                        "PI_BRIDGE_UNAVAILABLE"
                    } else {
                        "BINDING_REQUIRED"
                    },
                    if had_live && matches.is_empty() {
                        "Conflicting caller metadata"
                    } else {
                        "No caller socket bridge found"
                    },
                ));
            }
            let (p, v) = &matches[0];
            let c = as_caller(req, *p, v);
            if let Err(e) = verify(&c, req) {
                return Ok(error("INVALID_INPUT", e));
            }
            Ok(json!({"ok":true,"caller":remember_process(c)}))
        }
        "probe" => {
            if let Some(reason) = rejection {
                return Ok(json!({"ok":true,"status":"transient_error","reason":reason}));
            }
            if endpoint_responded && cs.is_empty() {
                return Ok(
                    json!({"ok":true,"status":"closed_definitive","reason":"No open session"}),
                );
            }
            if pid(req).is_some_and(|p| {
                alive(p) && state(req).join(format!("sockets/pi-pid-{p}.sock")).exists()
            }) && cs.first().is_none_or(|(_, v)| v["needs_reload"] == true)
            {
                return Ok(
                    json!({"ok":true,"status":"transient_error","reason":"Pi bridge is temporarily unavailable"}),
                );
            }
            let Some((_, v)) = cs.first() else {
                return Ok(
                    json!({"ok":true,"status":"closed_definitive","reason":"Pi bridge is unavailable"}),
                );
            };
            if v["needs_reload"] == true {
                return Ok(
                    json!({"ok":true,"status":"closed_definitive","reason":"Run /reload in Pi to enable 1-click listen"}),
                );
            }
            let sid = str_at(v, "session_id");
            Ok(
                json!({"ok":true,"status":if sid==str_at(req,"session_id"){"alive"}else{"switched"},"current_session_id":sid,"reason":null}),
            )
        }
        "wake" | "update_badge" => {
            let Some(p) = pid(req) else {
                return Ok(error("BINDING_REQUIRED", "Endpoint PID is required"));
            };
            let request = if op == "wake" {
                json!({"op":"deliver","session_id":req["session_id"],"batch_id":req["batch_id"],"content":req["text"],"wake":req["wake"]!=false})
            } else {
                json!({"op":"status","session_id":req["session_id"],"badge":req["badge"]})
            };
            let path = state(req).join(format!("sockets/pi-pid-{p}.sock"));
            if op == "update_badge" {
                let clearing = req["badge"].is_null();
                // An absent bridge has no status bar left to clear.
                if clearing && !path.exists() {
                    return Ok(json!({"ok":true}));
                }
                return match socket_call(&path, &request) {
                    Ok(reply) if clearing && reply["error"] == "SESSION_MISMATCH" => {
                        Ok(json!({"ok":true}))
                    }
                    Err(_) if clearing && process_identity(p).is_err() => {
                        let _ = fs::remove_file(&path);
                        Ok(json!({"ok":true}))
                    }
                    reply => reply,
                };
            }
            for attempt in 0..3 {
                match socket_call(&path, &request) {
                    Ok(reply) if reply["ok"] == true => {
                        return Ok(json!({"ok":true,"outcome":"delivered","reason":null}))
                    }
                    Ok(reply) if reply["error"] == "SESSION_MISMATCH" => {
                        return Ok(json!({"ok":true,"outcome":"session_switched","reason":null}))
                    }
                    Ok(reply) => {
                        return Ok(
                            json!({"ok":true,"outcome":"driver_unavailable","reason":response_diagnostic(&reply,"Pi bridge rejected delivery")}),
                        );
                    }
                    Err(_) => {}
                }
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_secs(attempt + 1));
                }
            }
            Ok(
                json!({"ok":true,"outcome":"driver_unavailable","reason":"Pi bridge is unavailable"}),
            )
        }
        _ => Ok(error("INVALID_INPUT", "Unknown plugin operation")),
    }
}
