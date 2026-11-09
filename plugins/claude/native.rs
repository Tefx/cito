use crate::support::*;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
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
        return name == "claude";
    }
    matches!(name, "claude" | "claude.js" | "claude.mjs")
        || (entry.contains("/claude-code/") && matches!(name, "cli.js" | "cli.mjs"))
}
use std::time::Duration;
const EVENTS: [&str; 3] = ["Stop", "SessionStart", "UserPromptSubmit"];
fn hook_cmd(v: &Value) -> bool {
    v["type"] == "command"
        && v["command"]
            .as_str()
            .map(|s| s == "cito-claude-hook" || s.ends_with("/cito-claude-hook"))
            .unwrap_or(false)
}
fn clean_hooks(v: &mut Value) {
    if let Some(events) = v.get_mut("hooks").and_then(Value::as_object_mut) {
        events.retain(|_, groups| {
            if let Some(a) = groups.as_array_mut() {
                a.retain_mut(|g| {
                    if let Some(hooks) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                        hooks.retain(|h| !hook_cmd(h));
                        !hooks.is_empty()
                    } else {
                        true
                    }
                });
                !a.is_empty()
            } else {
                true
            }
        });
        if events.is_empty() {
            v.as_object_mut().map(|o| o.remove("hooks"));
        }
    }
}
fn setup(req: &Value) -> Result<Value> {
    let dir = config_home(req, "CITO_CLAUDE_HOME", "~/.claude");
    let path = dir.join("settings.json");
    let op = str_at(req, "op").unwrap_or("");
    let command = state(req).join("bin/cito-mcp");
    let hook = state(req).join("plugins/claude/bin/cito-claude-hook");
    let desired = json!({"command":command,"args":[],"env":{"CITO_CLAUDE_CHANNEL":"1"}});
    let v = read_json(&path)?;
    if op == "setup_inspect" {
        let hooks_ok = EVENTS.iter().all(|e| {
            v["hooks"][e]
                .as_array()
                .map(|groups| {
                    groups.iter().any(|g| {
                        g["hooks"]
                            .as_array()
                            .map(|h| h.iter().any(hook_cmd))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        });
        return Ok(
            json!({"ok":true,"installed":dir.exists(),"configured":v["mcpServers"]["cito"]==desired&&hooks_ok,"paths":[path]}),
        );
    }
    if op == "setup_apply" {
        if let Some(cmd) = str_at(&v["mcpServers"]["cito"], "command") {
            if cmd != "cito-mcp" && !cmd.ends_with("/cito-mcp") && req["force"] != true {
                return Ok(
                    json!({"ok":true,"actions":[{"target":"claude","path":path,"status":"conflict","backupPath":null,"detail":"Conflicting MCP command; pass --force to replace"}]}),
                );
            }
        }
    }
    let remove = op == "setup_remove";
    let action = json_mutate(req, "claude", &path, |v| {
        if !remove && req["force"] != true {
            if let Some(cmd) = str_at(&v["mcpServers"]["cito"], "command") {
                if cmd != "cito-mcp" && !cmd.ends_with("/cito-mcp") {
                    return Err("Conflicting MCP command; pass --force to replace".into());
                }
            }
        }
        clean_hooks(v);
        if let Some(servers) = v.get_mut("mcpServers").and_then(Value::as_object_mut) {
            servers.remove("cito");
            if servers.is_empty() {
                v.as_object_mut().map(|o| o.remove("mcpServers"));
            }
        }
        if !remove {
            if v.get("mcpServers").is_none() {
                v["mcpServers"] = json!({})
            }
            if !v["mcpServers"].is_object() {
                return Err("mcpServers must be an object".into());
            }
            v["mcpServers"]["cito"] = desired;
            if v.get("hooks").is_none() {
                v["hooks"] = json!({})
            }
            if !v["hooks"].is_object() {
                return Err("hooks must be an object".into());
            }
            for e in EVENTS {
                if v["hooks"].get(e).is_none() {
                    v["hooks"][e] = json!([])
                }
                let groups = v["hooks"][e]
                    .as_array_mut()
                    .ok_or("Hook groups must be arrays")?;
                groups.push(json!({"hooks":[{"type":"command","command":hook}]}));
            }
        }
        Ok(())
    })?;
    Ok(json!({"ok":true,"actions":[action]}))
}
fn queue(req: &Value, sid: &str) -> Result<PathBuf> {
    Ok(state(req)
        .join("claude/wakes")
        .join(format!("{}.jsonl", validate_session(sid)?)))
}
fn queue_lock(req: &Value, sid: &str) -> Result<FileLock> {
    let path = queue(req, sid)?.with_extension("lock");
    FileLock::acquire(&path)
}
pub fn append_wake(req: &Value, sid: &str) -> Result<()> {
    str_at(req, "batch_id").ok_or("Batch ID is required")?;
    req["text"].as_str().ok_or("Wake text is required")?;
    let _lock = queue_lock(req, sid)?;
    let path = queue(req, sid)?;
    let mut f = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    if f.metadata().map_err(|e| e.to_string())?.len() > 0 {
        f.seek(SeekFrom::End(-1)).map_err(|e| e.to_string())?;
        let mut b = [0];
        f.read_exact(&mut b).map_err(|e| e.to_string())?;
        if b[0] != b'\n' {
            f.write_all(b"\n").map_err(|e| e.to_string())?;
        }
    }
    let row = json!({"batch_id":req["batch_id"],"content":req["text"],"wake":req["wake"]!=false,"created_at_ms":now_ms()});
    writeln!(f, "{row}").map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())
}
pub fn drain_wakes(req: &Value, sid: &str) -> Result<Vec<Value>> {
    let _lock = queue_lock(req, sid)?;
    let path = queue(req, sid)?;
    let prefix = format!(
        "{}.draining-",
        path.file_name()
            .and_then(|s| s.to_str())
            .ok_or("Invalid queue path")?
    );
    let mut paths = vec![];
    if let Some(dir) = path.parent() {
        for e in fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                paths.push(e.path());
            }
        }
    }
    paths.sort();
    paths.push(path);
    let mut rows = vec![];
    let mut seen = HashSet::new();
    for p in paths {
        let Some(s) = read_optional(&p)? else {
            continue;
        };
        for line in s.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let (Some(id), Some(_)) = (str_at(&v, "batch_id"), v["content"].as_str()) {
                    if seen.insert(id.to_string()) {
                        rows.push(v)
                    }
                }
            }
        }
        fs::remove_file(p).map_err(|e| e.to_string())?;
    }
    Ok(rows)
}
fn transcript_dir(req: &Value, cwd: &str) -> PathBuf {
    let slug: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    config_home(req, "CITO_CLAUDE_HOME", "~/.claude")
        .join("projects")
        .join(slug)
}
fn hook_endpoint(req: &Value) -> u32 {
    if let Some(p) = env_value(req, "CITO_ENDPOINT_PID").and_then(|s| s.parse().ok()) {
        return p;
    }
    // SAFETY: getppid has no pointer arguments or ownership effects.
    let first = unsafe { libc::getppid() as u32 };
    if !fs::read_dir(state(req).join("sockets"))
        .ok()
        .is_some_and(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("claude-pid-")
            })
        })
    {
        return first;
    }
    let mut p = first;
    for _ in 0..16 {
        if state(req)
            .join(format!("sockets/claude-pid-{p}.sock"))
            .exists()
        {
            return p;
        }
        let next = process_parent(p);
        match next {
            Some(n) if n > 1 && n != p => p = n,
            _ => break,
        }
    }
    first
}
fn binding_path(req: &Value, p: u32) -> PathBuf {
    state(req).join(format!("claude/endpoints/{p}.json"))
}
fn record_binding(req: &Value, p: u32, sid: &str, cwd: &str) -> Result<()> {
    let identity = process_identity(p).ok();
    let v = json!({"session_id":validate_session(sid)?,"cwd":cwd,"start":identity.as_ref().and_then(|v|str_at(v,"started_at")),"process_identity":identity,"source":str_at(req,"_binding_source").unwrap_or("hook")});
    atomic_write(&binding_path(req, p), v.to_string().as_bytes())
}
fn metadata_session(req: &Value) -> Result<Option<String>> {
    let mut explicit = vec![];
    for s in [
        str_at(req, "explicit_session_id"),
        str_at(&req["meta"], "claudeSessionId"),
        str_at(&req["meta"], "sessionId"),
        str_at(&req["meta"], "claude_session_id"),
        str_at(&req["headers"], "x-claude-session-id"),
        if req["meta"]["expected_harness"] == "claude" {
            str_at(&req["meta"], "expected_session_id")
        } else {
            None
        },
    ]
    .into_iter()
    .flatten()
    {
        explicit.push(validate_session(s)?)
    }
    if let Some(s) = env_value(req, "CLAUDE_SESSION_ID") {
        if !s.trim().is_empty() {
            explicit.push(validate_session(&s)?);
        }
    }
    explicit.sort();
    explicit.dedup();
    if explicit.len() > 1 {
        return Err("Conflicting caller metadata".into());
    }
    Ok(explicit.pop())
}
fn resolve(req: &Value) -> Result<String> {
    let explicit = metadata_session(req)?;
    let cwd = str_at(req, "cwd").unwrap_or("");
    if let Some(p) = pid(req) {
        if let Ok(v) = read_json(&binding_path(req, p)) {
            if str_at(&v, "start").map(str::to_string) == process_start(p)
                && str_at(&v, "start").is_some()
                && v["cwd"] == cwd
            {
                if let Some(s) = str_at(&v, "session_id") {
                    if explicit.as_ref().map(|id| id != s).unwrap_or(false) {
                        return Err("Conflicting caller metadata for the endpoint".into());
                    }
                    let current = process_identity(p)?;
                    if !matches_process(
                        str_at(&current, "command").ok_or("Endpoint command is unavailable")?,
                    ) {
                        return Err("Endpoint process command does not match the plugin".into());
                    }
                    if v["process_identity"].is_object() && v["process_identity"] != current {
                        return Err("Endpoint process command or start time changed".into());
                    }
                    return validate_session(s);
                }
            }
        }
    }
    if let Some(s) = explicit {
        return Ok(s);
    }
    let mut transcripts = vec![];
    if let Ok(entries) = fs::read_dir(transcript_dir(req, cwd)) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if let Some(s) = n.strip_suffix(".jsonl") {
                if s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
                    transcripts.push(s.to_string());
                }
            }
        }
    }
    if transcripts.len() == 1 && (pid(req).is_none() || req["cli_cmd"].is_string()) {
        Ok(transcripts.remove(0))
    } else {
        Err("A session bound to this endpoint is required; workspace transcripts are missing or ambiguous".into())
    }
}
fn title(req: &Value, sid: &str, cwd: &str) -> Option<String> {
    let p = transcript_dir(req, cwd).join(format!("{sid}.jsonl"));
    let mut f = fs::File::open(p).ok()?;
    let size = f.metadata().ok()?.len();
    let start = size.saturating_sub(128 * 1024);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    s.lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| {
            if v["type"] != "custom-title" {
                return None;
            }
            str_at(&v, "customTitle").map(|s| {
                s.chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .take(200)
                    .collect()
            })
        })
}
fn badge(req: &Value, sid: &str) -> Result<()> {
    let path = state(req)
        .join("claude/badges")
        .join(validate_session(sid)?);
    if let Some(s) = req["badge"].as_str() {
        atomic_write(&path, format!("{s}\n").as_bytes())
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}
fn bridge(req: &Value) -> Result<Value> {
    let p = pid(req).ok_or("Endpoint PID is required")?;
    socket_call(
        &state(req).join(format!("sockets/claude-pid-{p}.sock")),
        &json!({"op":"identify"}),
    )
}
pub fn handle(req: &Value) -> Result<Value> {
    let op = str_at(req, "op").unwrap_or("");
    if op.starts_with("setup_") {
        return setup(req);
    }
    if matches!(op, "resolve_caller" | "verify") {
        if let Err(e) = explicit_pid(req) {
            return Ok(error("INVALID_INPUT", e));
        }
    }
    match op {
        "discover_sessions" => {
            let mut sessions = vec![];
            if let Ok(entries) = fs::read_dir(state(req).join("sockets")) {
                for e in entries.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if let Some(p) = n
                        .strip_prefix("claude-pid-")
                        .and_then(|s| s.strip_suffix(".sock"))
                        .and_then(|s| s.parse::<u32>().ok())
                    {
                        let mut r = req.clone();
                        r["endpoint_pid"] = json!(p);
                        if let Ok(v) = bridge(&r) {
                            if v["ok"] == true {
                                sessions.push(caller(
                                    "claude",
                                    str_at(&v, "session_id").unwrap_or(""),
                                    str_at(&v, "cwd").unwrap_or(""),
                                    Some(p),
                                    str_at(&v, "session_name"),
                                ));
                            }
                        }
                    }
                }
            }
            Ok(json!({"ok":true,"sessions":sessions}))
        }
        "resolve_caller" | "verify" => {
            let hint = match metadata_session(req) {
                Ok(hint) => hint,
                Err(e) => return Ok(error("INVALID_INPUT", e)),
            };
            let response = bridge(req).ok();
            let endpoint_responded = response.is_some();
            let live = response.filter(|v| v["ok"] == true);
            if live.is_none() && endpoint_responded && req.get("cached_caller").is_some() {
                return Ok(error(
                    "SESSION_NOT_OPEN",
                    "Claude bridge did not identify an open session",
                ));
            }
            if live.is_none()
                && req["require_open"] == false
                && req["cached_caller"].is_object()
                && !req["cli_cmd"].is_string()
            {
                let resolved = match resolve(req) {
                    Ok(s) => Some(s),
                    Err(e) if e.starts_with("Conflicting") => return Ok(error("INVALID_INPUT", e)),
                    Err(_) => hint.clone(),
                };
                match cached_caller(req, "claude", resolved.as_deref(), matches_process) {
                    Ok(Some(c)) => return Ok(json!({"ok":true,"caller":c})),
                    Err(e) => return Ok(e),
                    Ok(None) => {}
                }
            }
            if let (Some(expected), Some(actual)) = (
                hint.as_deref(),
                live.as_ref().and_then(|v| str_at(v, "session_id")),
            ) {
                if expected != actual {
                    return Ok(error("INVALID_INPUT", "Conflicting caller metadata"));
                }
            }
            let sid = match live
                .as_ref()
                .and_then(|v| str_at(v, "session_id"))
                .map(str::to_string)
                .map(Ok)
                .unwrap_or_else(|| resolve(req))
            {
                Ok(s) => s,
                Err(e) => {
                    return Ok(error(
                        if e.starts_with("Conflicting") {
                            "INVALID_INPUT"
                        } else {
                            "BINDING_REQUIRED"
                        },
                        e,
                    ))
                }
            };
            let cwd = live
                .as_ref()
                .and_then(|v| str_at(v, "cwd"))
                .or_else(|| str_at(req, "cwd"))
                .unwrap_or("");
            let t = title(req, &sid, cwd);
            let c = caller("claude", &sid, cwd, pid(req), t.as_deref());
            if let Err(e) = verify(&c, req) {
                return Ok(error("INVALID_INPUT", e));
            }
            if req["require_open"] == true && pid(req).map(|p| !alive(p)).unwrap_or(false) {
                return Ok(error("SESSION_NOT_OPEN", "Endpoint process is closed"));
            }
            Ok(json!({"ok":true,"caller":remember_process(c)}))
        }
        "probe" => match bridge(req) {
            Ok(v) if v["ok"] == true => Ok(
                json!({"ok":true,"status":if v["session_id"]==req["session_id"]{"alive"}else{"switched"},"current_session_id":v["session_id"],"reason":null}),
            ),
            _ => Ok(
                json!({"ok":true,"status":if pid(req).map(|p|!alive(p)).unwrap_or(false){"closed_definitive"}else{"transient_error"},"reason":"Endpoint bridge is unavailable"}),
            ),
        },
        "update_badge" => {
            badge(
                req,
                str_at(req, "session_id").ok_or("Session ID is required")?,
            )?;
            Ok(json!({"ok":true}))
        }
        "wake" => {
            let sid = validate_session(str_at(req, "session_id").ok_or("Session ID is required")?)?;
            if let Some(p) = pid(req) {
                if let Ok(v) = bridge(req) {
                    if v["ok"] == true {
                        if v["session_id"] != sid {
                            return Ok(
                                json!({"ok":true,"outcome":"session_switched","reason":null}),
                            );
                        }
                        let r = socket_call(
                            &state(req).join(format!("sockets/claude-pid-{p}.sock")),
                            &json!({"op":"deliver","session_id":sid,"batch_id":req["batch_id"],"content":req["text"],"wake":req["wake"]!=false}),
                        )?;
                        return Ok(
                            json!({"ok":true,"outcome":if r["ok"]==true{"delivered_unconfirmed"}else{"driver_unavailable"},"reason":null}),
                        );
                    }
                }
            }
            append_wake(req, &sid)?;
            Ok(
                json!({"ok":true,"outcome":"delivered_unconfirmed","reason":"Queued for the next hook event"}),
            )
        }
        _ => Ok(error("INVALID_INPUT", "Unknown plugin operation")),
    }
}
pub fn hook_main() {
    if std::env::args().any(|s| s == "--help" || s == "-h") {
        eprintln!("Usage: cito-claude-hook (reads a hook JSON payload on stdin)");
        return;
    }
    let result = (|| -> Result<Value> {
        let mut raw = String::new();
        std::io::stdin()
            .take(4 * 1024 * 1024)
            .read_to_string(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.trim().is_empty() {
            return Ok(json!({}));
        }
        let input: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let event = str_at(&input, "hook_event_name").unwrap_or("");
        if !EVENTS.contains(&event) {
            return Ok(json!({}));
        }
        let mut req = json!({"cwd":input["cwd"],"explicit_session_id":input["session_id"]});
        let p = hook_endpoint(&req);
        let sid = match str_at(&req, "explicit_session_id") {
            Some(s) => validate_session(s).map_err(|_| "Invalid Claude session identifier")?,
            None => match resolve(&req) {
                Ok(s) => s,
                Err(e) if !e.starts_with("Conflicting") => return Ok(json!({})),
                Err(e) => return Err(e),
            },
        };
        req["endpoint_pid"] = json!(p);
        let fallback = format!("claude-{p}");
        if sid != fallback {
            if let Ok(previous) = read_json(&binding_path(&req, p)) {
                if previous["session_id"] == fallback
                    && str_at(&previous, "start").map(str::to_string) == process_start(p)
                    && UnixStream::connect(state(&req).join(format!("sockets/claude-pid-{p}.sock")))
                        .is_ok()
                {
                    let known = alias_for_fallback(&req, &fallback);
                    if known.as_deref().map(|first| first == sid).unwrap_or(true) {
                        atomic_write(
                            &state(&req).join("claude/aliases").join(&sid),
                            format!("{fallback}\n").as_bytes(),
                        )?;
                    }
                }
            }
        }
        record_binding(&req, p, &sid, str_at(&req, "cwd").unwrap_or(""))?;
        let alias = read_optional(&state(&req).join("claude/aliases").join(&sid))?
            .and_then(|s| validate_session(&s).ok())
            .filter(|s| s.starts_with("claude-") && s != &sid);
        let mut wakes = match alias.as_deref() {
            Some(id) => drain_wakes(&req, id)?,
            None => vec![],
        };
        wakes.extend(drain_wakes(&req, &sid)?);
        // Drain order follows recovered staging files, then the active queue.
        let mut seen = HashSet::new();
        wakes.retain(|w| {
            str_at(w, "batch_id")
                .map(|s| seen.insert(s.to_string()))
                .unwrap_or(false)
        });
        if wakes.is_empty() {
            return Ok(json!({}));
        }
        let content = wakes
            .iter()
            .filter_map(|r| r["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        for id in std::iter::once(sid.as_str()).chain(alias.as_deref()) {
            let badge_path = state(&req).join("claude/badges").join(id);
            if let Ok(s) = fs::read_to_string(&badge_path) {
                let current = s.trim_end();
                if let Some(i) = current.rfind(" (") {
                    let count = &current[i + 2..];
                    if count.len() > 1
                        && count.ends_with(')')
                        && count[..count.len() - 1].chars().all(|c| c.is_ascii_digit())
                    {
                        let mut r = req.clone();
                        r["badge"] = json!(&current[..i]);
                        let _ = badge(&r, id);
                    }
                }
            }
        }
        let config_path = env_value(&req, "CITO_CONFIG_PATH")
            .map(|s| expand(&req, &s))
            .unwrap_or_else(|| state(&req).join("config.json"));
        let cfg = read_json(&config_path).unwrap_or(json!({}));
        let port = env_value(&req, "CITO_DAEMON_PORT")
            .and_then(|s| s.parse::<u16>().ok())
            .filter(|p| *p > 0)
            .or_else(|| {
                cfg["daemonPort"]
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .filter(|p| *p > 0)
            })
            .unwrap_or(4290);
        let ack_session = alias.as_deref().unwrap_or(&sid);
        let ack = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"cito_inbox","arguments":{"ack_all":false,"ack_ids":wakes.iter().map(|r|r["batch_id"].clone()).collect::<Vec<_>>()},"_meta":{"claudeSessionId":ack_session,"cwd":req["cwd"]}}});
        let _ = ureq::post(&format!("http://127.0.0.1:{port}/mcp"))
            .timeout(Duration::from_millis(500))
            .send_json(ack);
        if event == "Stop" && wakes.iter().any(|v| v["wake"] != false) {
            Ok(json!({"decision":"block","reason":content}))
        } else {
            Ok(json!({"hookSpecificOutput":{"hookEventName":event,"additionalContext":content}}))
        }
    })();
    match result {
        Ok(v) => emit(&v),
        Err(e) => {
            eprintln!("cito-claude-hook: {e}");
            emit(&json!({}));
        }
    }
}

pub fn main() {
    let mut line = String::new();
    let req = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_str::<Value>(&line).map_err(|e| e.to_string()));
    let req = match req {
        Ok(r) => r,
        Err(e) => {
            emit(&error("INVALID_INPUT", e));
            return;
        }
    };
    if req["op"] != "stdio_sidecar" {
        emit(&handle(&req).unwrap_or_else(|e| error("PLUGIN_EXEC_FAILED", e)));
        return;
    }
    if let Err(e) = sidecar(req) {
        eprintln!("Claude sidecar: {e}");
    }
}
struct SidecarCleanup {
    path: PathBuf,
    inode: (u64, u64),
    badges: std::sync::Mutex<(bool, std::collections::HashMap<PathBuf, (u64, u64)>)>,
}
impl SidecarCleanup {
    fn stop(&self) {
        use std::os::unix::fs::MetadataExt;
        if let Ok(mut guard) = self.badges.lock() {
            if guard.0 {
                return;
            }
            guard.0 = true;
            for (path, inode) in &guard.1 {
                if fs::metadata(path)
                    .map(|m| (m.dev(), m.ino()) == *inode)
                    .unwrap_or(false)
                {
                    let _ = fs::remove_file(path);
                }
            }
            guard.1.clear();
            if fs::metadata(&self.path)
                .map(|m| (m.dev(), m.ino()) == self.inode)
                .unwrap_or(false)
            {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}
static SIDECAR_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
extern "C" fn sidecar_signal(_: libc::c_int) {
    SIDECAR_STOP.store(true, std::sync::atomic::Ordering::Relaxed);
}
fn sidecar_watchdog(cleanup: std::sync::Arc<SidecarCleanup>) {
    let eof = cleanup.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            if line.is_err() {
                break;
            }
        }
        eof.stop();
        std::process::exit(0);
    });
    // SAFETY: the signal handler only sets a lock-free AtomicBool. Filesystem cleanup
    // and process exit run on an ordinary thread, never inside the signal handler.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            sidecar_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            sidecar_signal as *const () as libc::sighandler_t,
        );
    }
    std::thread::spawn(move || {
        while !SIDECAR_STOP.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(2));
        }
        cleanup.stop();
        std::process::exit(0);
    });
}
fn alias_for_fallback(req: &Value, fallback: &str) -> Option<String> {
    fs::read_dir(state(req).join("claude/aliases"))
        .ok()?
        .flatten()
        .find_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            if fs::read_to_string(entry.path()).ok()?.trim() == fallback {
                validate_session(&name).ok()
            } else {
                None
            }
        })
}
fn latest_transcript(req: &Value, cwd: &str) -> Option<(String, u64)> {
    fs::read_dir(transcript_dir(req, cwd))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry
                .file_name()
                .to_str()?
                .strip_suffix(".jsonl")?
                .to_string();
            if name.len() != 36 || !name.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
                return None;
            }
            let time = entry
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            Some((name, time))
        })
        .max_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)))
}
fn sidecar(req: Value) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    use std::sync::{Arc, Mutex};
    let p = pid(&req).ok_or("Endpoint PID is required")?;
    let cwd = str_at(&req, "cwd").unwrap_or("");
    let path = state(&req).join(format!("sockets/claude-pid-{p}.sock"));
    let parent = path.parent().ok_or("Invalid socket path")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            match UnixStream::connect(&path) {
                Ok(_) => return Err("A live bridge already owns the socket".into()),
                Err(probe)
                    if matches!(
                        probe.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) || probe.raw_os_error() == Some(libc::ENOTSOCK) => {}
                Err(_) => return Err("An existing endpoint already owns the socket".into()),
            }
            fs::remove_file(&path).map_err(|e| e.to_string())?;
            UnixListener::bind(&path).map_err(|e| e.to_string())?
        }
        Err(e) => return Err(e.to_string()),
    };
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
    let cleanup = Arc::new(SidecarCleanup {
        path: path.clone(),
        inode: (metadata.dev(), metadata.ino()),
        badges: Mutex::new((false, std::collections::HashMap::new())),
    });
    let explicit = metadata_session(&req)?;
    let initial = latest_transcript(&req, cwd);
    let fallback = format!("claude-{p}");
    let mut alias_active = explicit.is_none() && initial.is_none();
    let mut first_transcript: Option<String> = None;
    let mut current = explicit
        .clone()
        .or_else(|| initial.as_ref().map(|r| r.0.clone()))
        .unwrap_or_else(|| fallback.clone());
    let mut last_active = now_ms();
    let mut delivered = std::collections::VecDeque::<String>::new();
    sidecar_watchdog(cleanup.clone());
    emit(&json!({"op":"ready","capabilities_patch":{"experimental":{"claude/channel":{}}}}));
    // Channel notifications are hints. Allow a healthy writer to submit the hint
    // before replying, but cap that handoff at 10 ms when stdout is blocked.
    // The durable queue remains authoritative; excess hints may be dropped.
    let (notify_tx, notify_rx) =
        std::sync::mpsc::sync_channel::<(Value, std::sync::mpsc::SyncSender<()>)>(64);
    std::thread::spawn(move || {
        for (message, submitted) in notify_rx {
            emit(&message);
            let _ = submitted.try_send(());
        }
    });
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let mut line = String::new();
        let result = (|| -> Result<Value> {
            BufReader::new(&mut stream)
                .take(4 * 1024 * 1024)
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            if !line.ends_with('\n') {
                return Err("Socket closed before newline".into());
            }
            let v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if v["op"] == "ping" {
                return Ok(json!({"ok":true}));
            }
            let latest = latest_transcript(&req, cwd);
            if alias_active && first_transcript.is_none() {
                first_transcript = alias_for_fallback(&req, &fallback);
            }
            let mut next = explicit.clone();
            // A hook-recorded endpoint identity takes precedence over workspace scanning.
            if next.is_none() {
                if let Ok(binding) = read_json(&binding_path(&req, p)) {
                    if binding["source"] != "channel"
                        && binding["cwd"] == cwd
                        && str_at(&binding, "start").map(str::to_string) == process_start(p)
                    {
                        next = str_at(&binding, "session_id").map(str::to_string);
                    }
                }
            }
            if next.is_none() {
                let other_endpoint = fs::read_dir(parent)
                    .ok()
                    .into_iter()
                    .flatten()
                    .flatten()
                    .any(|entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let Some(other) = name
                            .strip_prefix("claude-pid-")
                            .and_then(|s| s.strip_suffix(".sock"))
                            .and_then(|s| s.parse::<u32>().ok())
                        else {
                            return false;
                        };
                        if other == p {
                            return false;
                        }
                        let Ok(binding) = read_json(&binding_path(&req, other)) else {
                            return false;
                        };
                        binding["cwd"] == cwd && UnixStream::connect(entry.path()).is_ok()
                    });
                if other_endpoint {
                    next = Some(current.clone());
                } else if let Some((sid, _)) = &latest {
                    if alias_active {
                        if first_transcript
                            .as_ref()
                            .map(|first| first == sid)
                            .unwrap_or(true)
                        {
                            first_transcript = Some(sid.clone());
                            atomic_write(
                                &state(&req).join("claude/aliases").join(sid),
                                format!("{fallback}\n").as_bytes(),
                            )?;
                            next = Some(fallback.clone());
                        } else {
                            alias_active = false;
                            next = Some(sid.clone());
                        }
                    } else {
                        next = Some(sid.clone());
                    }
                }
            }
            if alias_active && explicit.is_none() {
                if let Some(sid) = next.as_ref().filter(|sid| *sid != &fallback) {
                    if first_transcript
                        .as_ref()
                        .map(|first| first == sid)
                        .unwrap_or(true)
                    {
                        first_transcript = Some(sid.clone());
                        atomic_write(
                            &state(&req).join("claude/aliases").join(sid),
                            format!("{fallback}\n").as_bytes(),
                        )?;
                        next = Some(fallback.clone());
                    } else {
                        alias_active = false;
                    }
                }
            }
            let next = next.unwrap_or_else(|| fallback.clone());
            if next != current {
                last_active = now_ms();
                current = next;
            }
            let mut channel_req = req.clone();
            channel_req["_binding_source"] = json!("channel");
            if read_json(&binding_path(&req, p))
                .map(|b| b["session_id"] != current)
                .unwrap_or(true)
            {
                record_binding(&channel_req, p, &current, cwd)?;
            }
            let transcript = if alias_active {
                first_transcript.as_deref().unwrap_or(&current)
            } else {
                &current
            };
            if v["op"] == "identify" {
                return Ok(
                    json!({"ok":true,"endpoint_pid":p,"session_id":current,"cwd":cwd,"session_name":title(&req,transcript,cwd).unwrap_or_else(||Path::new(cwd).file_name().and_then(|s|s.to_str()).unwrap_or("session").into()),"updated_at_ms":last_active.max(latest.as_ref().map(|r|r.1).unwrap_or(0))}),
                );
            }
            if v["session_id"] != current {
                return Ok(json!({"ok":false,"error":"SESSION_MISMATCH"}));
            }
            if v["op"] == "status" {
                if !v["badge"].is_null() && !v["badge"].is_string() {
                    return Ok(json!({"ok":false,"error":"INVALID_REQUEST"}));
                }
                let mut guard = cleanup
                    .badges
                    .lock()
                    .map_err(|_| "Badge state lock poisoned")?;
                if guard.0 {
                    return Ok(json!({"ok":false,"error":"BRIDGE_CLOSED"}));
                }
                let mut status = req.clone();
                status["badge"] = v["badge"].clone();
                badge(&status, &current)?;
                let badge_path = state(&req).join("claude/badges").join(&current);
                if let Ok(m) = fs::metadata(&badge_path) {
                    guard.1.insert(badge_path, (m.dev(), m.ino()));
                } else {
                    guard.1.remove(&badge_path);
                }
                return Ok(json!({"ok":true}));
            }
            let Some(batch) = str_at(&v, "batch_id") else {
                return Ok(json!({"ok":false,"error":"INVALID_REQUEST"}));
            };
            if v["op"] != "deliver" || !v["content"].is_string() {
                return Ok(json!({"ok":false,"error":"INVALID_REQUEST"}));
            }
            let deduplicated = delivered.iter().any(|id| id == batch);
            if !deduplicated {
                let mut wake = req.clone();
                wake["batch_id"] = v["batch_id"].clone();
                wake["text"] = v["content"].clone();
                wake["wake"] = v["wake"].clone();
                append_wake(&wake, &current)?;
                delivered.push_back(batch.into());
                if delivered.len() > 200 {
                    delivered.pop_front();
                }
                last_active = now_ms();
                let (submitted_tx, submitted_rx) = std::sync::mpsc::sync_channel(1);
                if notify_tx.try_send((json!({"jsonrpc":"2.0","method":"notifications/claude/channel","params":{"content":v["content"],"meta":{"origin":"cito","batch_id":batch}}}),submitted_tx)).is_ok() {
                    let _=submitted_rx.recv_timeout(Duration::from_millis(10));
                }
            }
            Ok(json!({"ok":true,"delivered":true,"deduplicated":deduplicated}))
        })();
        let mut response =
            result.unwrap_or_else(|e| json!({"ok":false,"error":"DELIVERY_FAILED","message":e}));
        if let Some(id) = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|v| v.get("id").cloned())
        {
            response["id"] = id;
        }
        let _ = writeln!(stream, "{response}");
    }
    cleanup.stop();
    Ok(())
}
