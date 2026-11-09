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
        state(req).join("plugins/pi"),
    )
}
fn setup(req: &Value) -> Result<Value> {
    let (home, package) = paths(req);
    let settings = home.join("settings.json");
    let mcp_paths = [home.join("mcp.json"), home.join("mcp/global.mcp.json")];
    let legacy = home.join("extensions/cito-bridge.ts");
    let op = str_at(req, "op").unwrap_or("");
    if op == "setup_inspect" {
        let v = read_json(&settings)?;
        let mut configured = v["packages"]
            .as_array()
            .map(|a| a.iter().any(|x| x.as_str() == package.to_str()))
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
    if remove {
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
    socket_call(
        &state(req).join(format!("sockets/pi-pid-{p}.sock")),
        &json!({"op":"identify"}),
    )
}
fn candidates(req: &Value) -> (Vec<(u32, Value)>, bool) {
    let resolve = matches!(str_at(req, "op"), Some("resolve_caller" | "verify"));
    if resolve || pid(req).is_some() {
        let roots = if resolve {
            caller_pid_roots(req).unwrap_or_default()
        } else {
            pid(req).into_iter().collect()
        };
        let mut responded = false;
        for mut p in roots {
            for _ in 0..=if resolve { 5 } else { 0 } {
                if let Ok(v) = identify(req, p) {
                    responded = true;
                    if v["ok"] == true
                        && str_at(&v, "session_id").is_some()
                        && str_at(&v, "cwd").is_some()
                    {
                        if v.get("pi_pid")
                            .is_none_or(|id| id.as_u64() == Some(p as u64))
                        {
                            return (vec![(p, v)], true);
                        }
                    }
                }
                let Some(parent) = process_parent(p) else {
                    break;
                };
                p = parent;
            }
        }
        return (vec![], responded);
    }
    let mut out = vec![];
    if let Ok(entries) = fs::read_dir(state(req).join("sockets")) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if let Some(p) = n
                .strip_prefix("pi-pid-")
                .and_then(|s| s.strip_suffix(".sock"))
                .and_then(|s| s.parse::<u32>().ok())
            {
                if let Ok(v) = identify(req, p) {
                    if v["ok"] == true
                        && str_at(&v, "session_id").is_some()
                        && str_at(&v, "cwd").is_some()
                    {
                        out.push((p, v))
                    }
                }
            }
        }
    }
    (out, false)
}
fn as_caller(p: u32, v: &Value) -> Value {
    caller(
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
    let (cs, endpoint_responded) = if matches!(
        op,
        "resolve_caller" | "verify" | "probe" | "discover_sessions"
    ) {
        candidates(req)
    } else {
        (vec![], false)
    };
    match op {
        "discover_sessions" => Ok(
            json!({"ok":true,"sessions":cs.iter().map(|(p,v)|{let mut c=as_caller(*p,v);c["updated_at_ms"]=v["updated_at_ms"].clone();c}).collect::<Vec<_>>()}),
        ),
        "resolve_caller" | "verify" => {
            if cs.is_empty() && endpoint_responded {
                return Ok(error(
                    "PI_BRIDGE_UNAVAILABLE",
                    "No caller socket bridge found",
                ));
            }
            if cs.is_empty() {
                match cached_caller(req, "pi", None, matches_process) {
                    Ok(Some(c)) => return Ok(json!({"ok":true,"caller":c})),
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
            let c = as_caller(*p, v);
            if let Err(e) = verify(&c, req) {
                return Ok(error("INVALID_INPUT", e));
            }
            Ok(json!({"ok":true,"caller":remember_process(c)}))
        }
        "probe" => {
            let Some((_, v)) = cs.first() else {
                return Ok(
                    json!({"ok":true,"status":"closed_definitive","reason":"Pi bridge is unavailable"}),
                );
            };
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
                return socket_call(&path, &request);
            }
            for attempt in 0..3 {
                match socket_call(&path, &request) {
                    Ok(reply) if reply["ok"] == true => {
                        return Ok(json!({"ok":true,"outcome":"delivered","reason":null}))
                    }
                    Ok(reply) if reply["error"] == "SESSION_MISMATCH" => {
                        return Ok(json!({"ok":true,"outcome":"session_switched","reason":null}))
                    }
                    _ => {}
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
