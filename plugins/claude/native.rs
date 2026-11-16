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
fn interactive_candidate(req: &Value, p: u32) -> bool {
    let Ok(command) = process_command(p) else {
        return false;
    };
    if has_harness_ancestor(p, |ancestor, command| {
        is_agent_harness(command) || live_bridge(req, ancestor)
    }) {
        return false;
    }
    let args: Vec<_> = process_arguments(&command).collect();
    !args
        .iter()
        .any(|arg| matches!(*arg, "-p" | "--print" | "--output-format=stream-json"))
        && !args
            .windows(2)
            .any(|w| w == ["--output-format", "stream-json"])
        && process_has_tty(p)
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
    let hook = str_at(req, "plugin_dir")
        .map(|s| expand(req, s))
        .unwrap_or_else(|| state(req).join("runtime/plugins/claude"))
        .join("bin/cito-claude-hook");
    let desired = json!({"command":command,"args":[],"env":{"CITO_CLAUDE_CHANNEL":"1"}});
    if op == "setup_inspect" {
        let v = read_json(&path)?;
        let hooks_ok = EVENTS.iter().all(|e| {
            v["hooks"][e]
                .as_array()
                .map(|groups| {
                    groups.iter().any(|g| {
                        g["hooks"]
                            .as_array()
                            .map(|h| {
                                h.iter().any(|entry| {
                                    entry["type"] == "command"
                                        && entry["command"].as_str() == hook.to_str()
                                })
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        });
        return Ok(
            json!({"ok":true,"installed":dir.exists(),"configured":v["mcpServers"]["cito"]==desired&&hooks_ok,"paths":[path]}),
        );
    }
    let remove = op == "setup_remove";
    let action = json_mutate(req, "claude", &path, |v| {
        check_mcp_command(
            req,
            str_at(&v["mcpServers"]["cito"], "command"),
            command.to_str().unwrap_or(""),
        )?;
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
    if remove && req["dry_run"] != true {
        let _ = fs::remove_dir_all(state(req).join("claude"));
        if let Ok(entries) = fs::read_dir(state(req).join("sockets")) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("claude-pid-")
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
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
    let row = json!({"batch_id":req["batch_id"],"content":req["text"],"wake":req["wake"]!=false,"created_at_ms":now_ms(),"expected_ack_count":req["signals"].as_array().map(|signals|signals.len().max(1)).unwrap_or(1)});
    writeln!(f, "{row}").map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())
}
fn drain_wakes(req: &Value, sessions: &[&str]) -> Result<Vec<Value>> {
    // Snapshot under both identity locks; output and ACK continuation retire rows later.
    let mut lock_sessions = sessions.to_vec();
    lock_sessions.sort_unstable();
    lock_sessions.dedup();
    let _locks = lock_sessions
        .iter()
        .map(|sid| queue_lock(req, sid))
        .collect::<Result<Vec<_>>>()?;
    let mut paths = vec![];
    for sid in sessions {
        let path = queue(req, sid)?;
        let prefix = format!(
            "{}.draining-",
            path.file_name()
                .and_then(|s| s.to_str())
                .ok_or("Invalid queue path")?
        );
        let mut staged = vec![];
        if let Some(dir) = path.parent() {
            for e in fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
                if e.file_name().to_string_lossy().starts_with(&prefix) {
                    staged.push(e.path());
                }
            }
        }
        staged.sort();
        paths.extend(staged.into_iter().map(|path| ((*sid).to_owned(), path)));
        paths.push(((*sid).to_owned(), path));
    }
    let mut rows = vec![];
    let mut seen = HashSet::new();
    for (source_session, p) in paths {
        let Some(s) = read_optional(&p)? else {
            continue;
        };
        for line in s.lines() {
            if let Ok(mut v) = serde_json::from_str::<Value>(line) {
                if let (Some(id), Some(_)) = (str_at(&v, "batch_id"), v["content"].as_str()) {
                    if seen.insert((source_session.clone(), id.to_string())) {
                        v["ack_session"] = json!(source_session);
                        rows.push(v)
                    }
                }
            }
        }
    }
    Ok(rows)
}
fn settle_wakes(req: &Value, sessions: &[String], wakes: &[Value], retire: bool) -> Result<()> {
    let ids: HashSet<_> = wakes
        .iter()
        .filter_map(|w| Some((str_at(w, "ack_session")?, str_at(w, "batch_id")?)))
        .collect();
    let mut sessions = sessions.to_vec();
    sessions.sort();
    sessions.dedup();
    let _locks = sessions
        .iter()
        .map(|sid| queue_lock(req, sid))
        .collect::<Result<Vec<_>>>()?;
    for sid in sessions {
        let path = queue(req, &sid)?;
        let prefix = format!(
            "{}.draining-",
            path.file_name()
                .ok_or("Invalid queue path")?
                .to_string_lossy()
        );
        let mut paths = vec![path.clone()];
        if let Some(parent) = path.parent() {
            paths.extend(
                fs::read_dir(parent)
                    .map_err(|e| e.to_string())?
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
                    .map(|e| e.path()),
            );
        }
        for path in paths {
            let Some(content) = read_optional(&path)? else {
                continue;
            };
            let mut retained = Vec::new();
            for line in content.lines() {
                if let Ok(mut value) = serde_json::from_str::<Value>(line) {
                    if str_at(&value, "batch_id")
                        .is_some_and(|id| ids.contains(&(sid.as_str(), id)))
                    {
                        if !retire {
                            value["output_delivered"] = json!(true);
                            retained.push(value.to_string());
                        }
                        continue;
                    }
                }
                retained.push(line.to_owned());
            }
            if retained.is_empty() {
                fs::remove_file(&path).map_err(|e| e.to_string())?;
            } else {
                atomic_write(&path, format!("{}\n", retained.join("\n")).as_bytes())?;
            }
        }
    }
    Ok(())
}
fn persist_hook_acks(req: &Value, sessions: &[String], wakes: &[Value]) -> Result<()> {
    let local = crate::core::state::LocalState::open(&state(req))?;
    for wake in wakes {
        let rows = local.db.query("SELECT s.* FROM local_signals s JOIN local_listeners l ON l.hub=s.hub AND l.listener_id=s.listener_id WHERE s.batch_id=? AND l.harness='claude'", &[wake["batch_id"].clone()])?;
        if rows.is_empty() {
            return Err("ACK continuation has no matching local batch".into());
        }
        for row in rows {
            let listener = local
                .listener(
                    str_at(&row, "hub").unwrap_or(""),
                    str_at(&row, "listener_id").unwrap_or(""),
                )?
                .ok_or("ACK listener missing")?;
            if !sessions.iter().any(|sid| listener["session_id"] == *sid)
                || listener["session_id"] != wake["ack_session"]
            {
                return Err("ACK session mismatch".into());
            }
            local.acknowledge(
                str_at(&row, "hub").unwrap_or(""),
                str_at(&row, "listener_id").unwrap_or(""),
                &[row["signal_id"].clone()],
                &format!("{}:claude", str_at(&listener, "host_id").unwrap_or("")),
            )?;
            let acked = local.db.query(
                "SELECT acknowledged_at_ms FROM local_signals WHERE hub=? AND signal_id=?",
                &[row["hub"].clone(), row["signal_id"].clone()],
            )?;
            if acked
                .first()
                .is_none_or(|signal| signal["acknowledged_at_ms"].is_null())
            {
                return Err("ACK signal authority changed".into());
            }
        }
    }
    Ok(())
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
fn live_bridge(req: &Value, pid: u32) -> bool {
    let path = state(req).join(format!("sockets/claude-pid-{pid}.sock"));
    path.exists()
        && socket_call(&path, &json!({"op":"ping"})).is_ok_and(|reply| reply["ok"] == true)
}
fn hook_endpoint(req: &Value) -> u32 {
    if let Some(p) = env_value(req, "CITO_ENDPOINT_PID").and_then(|s| s.parse().ok()) {
        return enclosing_endpoint_pid(Some(p), unsafe { libc::getppid() as u32 });
    }
    // SAFETY: getppid has no pointer arguments or ownership effects.
    let first = unsafe { libc::getppid() as u32 };

    let mut p = first;
    for _ in 0..16 {
        if process_command(p).is_ok_and(|command| matches_process(&command)) || live_bridge(req, p)
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
fn hook_parent_matches(p: u32) -> bool {
    // SAFETY: getppid only reads the current process's parent ID.
    let parent = unsafe { libc::getppid() as u32 };
    parent == p
        || (process_parent(parent) == Some(p)
            && process_command(parent).is_ok_and(|command| {
                process_entrypoint(&command).is_some_and(|(program, _)| {
                    matches!(
                        Path::new(program)
                            .file_name()
                            .and_then(|name| name.to_str()),
                        Some("sh" | "bash" | "zsh" | "dash" | "fish")
                    )
                })
            }))
}
fn binding_snapshot(path: &Path) -> Option<Value> {
    read_json(path)
        .ok()
        .map(|v| json!([v["session_id"], v["cwd"], v["start"], v["source"]]))
}
fn remove_rejected_binding(path: &Path, snapshot: Option<Value>) -> Result<()> {
    let _lock = FileLock::acquire(&path.with_extension("lock"))?;
    if snapshot.is_some() && binding_snapshot(path) == snapshot {
        let _ = fs::remove_file(path);
    }
    Ok(())
}
fn record_binding(req: &Value, p: u32, sid: &str, cwd: &str) -> Result<Option<Value>> {
    let channel = str_at(req, "_binding_source") == Some("channel");
    // Endpoint file locks are leaves: ancestry and bridge I/O must finish first.
    if !channel
        && (!hook_parent_matches(p)
            || hook_endpoint(req) != p
            || has_harness_ancestor(p, |ancestor, command| {
                is_agent_harness(command) || live_bridge(req, ancestor)
            }))
    {
        return Err("SESSION_MISMATCH: A descendant hook cannot claim a parent endpoint".into());
    }
    let identity = process_identity(p).ok();
    let start = identity.as_ref().and_then(|v| str_at(v, "started_at"));
    let path = binding_path(req, p);
    let _lock = FileLock::acquire(&path.with_extension("lock"))?;
    let previous = read_json(&path).ok();
    if channel {
        if let Some(previous) = &previous {
            // Canonical hooks own the identity across workspace changes.
            if start.is_some()
                && str_at(previous, "start") == start
                && (previous["source"] != "channel"
                    || (previous["cwd"] == cwd && previous["session_id"] == sid))
            {
                return Ok(Some(previous.clone()));
            }
        }
    }
    let v = json!({"session_id":validate_session(sid)?,"cwd":cwd,"start":identity.as_ref().and_then(|v|str_at(v,"started_at")),"process_identity":identity,"source":str_at(req,"_binding_source").unwrap_or("hook")});
    atomic_write(&path, v.to_string().as_bytes())?;
    Ok(previous)
}
fn metadata_session(req: &Value) -> Result<Option<String>> {
    let mut explicit = vec![];
    for s in [
        str_at(req, "explicit_session_id"),
        str_at(&req["headers"], "x-claude-session-id"),
    ]
    .into_iter()
    .flatten()
    {
        explicit.push(validate_session(s)?)
    }
    for source in [&req["meta"], &req["_meta"]] {
        for key in ["claudeSessionId", "sessionId", "claude_session_id"] {
            if let Some(s) = str_at(source, key) {
                explicit.push(validate_session(s)?);
            }
        }
        if str_at(source, "expected_harness") == Some("claude") {
            if let Some(s) = str_at(source, "expected_session_id") {
                explicit.push(validate_session(s)?);
            }
        }
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
        if let Some(p) = pid(req) {
            if (caller_supplied_pid(req)?.is_some()
                || (explicit_pid(req)?.is_some() && !req["cli_cmd"].is_string()))
                && !process_command(p).is_ok_and(|command| matches_process(&command))
            {
                return Err("A verified endpoint process is required for caller metadata".into());
            }
        }
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
    let value = cached_file_projection(req, "claude/transcript-cache", &p, || {
        let mut f = fs::File::open(&p).map_err(|e| e.to_string())?;
        let size = f.metadata().map_err(|e| e.to_string())?.len();
        f.seek(SeekFrom::Start(size.saturating_sub(128 * 1024)))
            .map_err(|e| e.to_string())?;
        let mut s = String::new();
        f.read_to_string(&mut s).map_err(|e| e.to_string())?;
        let title: Option<String> = s
            .lines()
            .rev()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find_map(|v| {
                (v["type"] == "custom-title")
                    .then(|| {
                        str_at(&v, "customTitle").map(|s| {
                            s.chars()
                                .map(|c| if c.is_control() { ' ' } else { c })
                                .take(200)
                                .collect()
                        })
                    })
                    .flatten()
            });
        Ok(json!(title))
    })
    .ok()?;
    value.as_str().map(str::to_owned)
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
    let mut p = pid(req).ok_or("Endpoint PID is required")?;
    let mut visited = HashSet::new();
    while p > 1 && visited.insert(p) {
        if process_command(p)
            .is_ok_and(|command| is_agent_harness(&command) && !matches_process(&command))
        {
            break;
        }
        if let Ok(response) = socket_call(
            &state(req).join(format!("sockets/claude-pid-{p}.sock")),
            &json!({"op":"identify"}),
        ) {
            // A live responder's rejection is authoritative, unlike no response.
            let mut response = response;
            response["endpoint_pid"] = json!(p);
            return Ok(response);
        }
        if process_command(p).is_ok_and(|command| matches_process(&command)) {
            break;
        }
        if !matches!(str_at(req, "op"), Some("resolve_caller" | "verify")) {
            break;
        }
        let Some(parent) = process_parent(p) else {
            break;
        };
        p = parent;
    }
    Err("Endpoint bridge is unavailable".into())
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
            let request = req.clone();
            let sessions = inspect_socket_endpoints(req, "claude-pid-", move |p, path| {
                if !interactive_candidate(&request, p) {
                    return None;
                }
                let mut r = request.clone();
                r["endpoint_pid"] = json!(p);
                let binding_before = binding_snapshot(&binding_path(&r, p));
                let response = match socket_call(path, &json!({"op":"identify"})) {
                    Ok(v) if v["ok"] != true => {
                        remove_rejected_binding(&binding_path(&r, p), binding_before).ok()?;
                        return None;
                    }
                    response => response,
                }
                .or_else(|error| {
                    if error != "Socket request timed out" || !alive(p) {
                        return Err(error);
                    }
                    let binding = read_json(&binding_path(&r, p))?;
                    let identity = process_identity(p)?;
                    if str_at(&binding, "start").is_none()
                        || str_at(&binding, "start").map(str::to_owned) != process_start(p)
                        || !matches_process(str_at(&identity, "command").unwrap_or(""))
                        || (binding["process_identity"].is_object()
                            && binding["process_identity"] != identity)
                        || str_at(&binding, "session_id").is_none()
                        || str_at(&binding, "cwd").is_none()
                    {
                        return Err(error);
                    }
                    let mut session = binding;
                    session["ok"] = json!(true);
                    Ok(session)
                });
                let v = response.ok().filter(|v| v["ok"] == true)?;
                let binding_path = binding_path(&r, p);
                let _lock = FileLock::acquire(&binding_path.with_extension("lock")).ok()?;
                if let Ok(mut binding) = read_json(&binding_path) {
                    if binding["session_id"] == v["session_id"]
                        && (binding["updated_at_ms"] != v["updated_at_ms"]
                            || binding["session_name"] != v["session_name"])
                    {
                        binding["updated_at_ms"] = v["updated_at_ms"].clone();
                        binding["session_name"] = v["session_name"].clone();
                        let _ = atomic_write(&binding_path, binding.to_string().as_bytes());
                    }
                }
                let mut session = caller(
                    &r,
                    "claude",
                    str_at(&v, "session_id").unwrap_or(""),
                    str_at(&v, "cwd").unwrap_or(""),
                    Some(p),
                    str_at(&v, "session_name"),
                );
                session["updated_at_ms"] = v["updated_at_ms"].clone();
                Some(session)
            })?
            .into_iter()
            .map(|(_, session)| session)
            .collect::<Vec<_>>();
            Ok(json!({"ok":true,"sessions":sessions}))
        }
        "resolve_caller" | "verify" => {
            let hint = match metadata_session(req) {
                Ok(hint) => hint,
                Err(e) => return Ok(error("INVALID_INPUT", e)),
            };
            let response = bridge(req).ok();
            let endpoint_responded = response.is_some();
            let rejection = response
                .as_ref()
                .filter(|v| v["ok"] != true)
                .map(|v| response_diagnostic(v, "Claude bridge rejected the request"));
            let live = response.filter(|v| v["ok"] == true);
            if live.is_none() && endpoint_responded {
                return Ok(error(
                    "SESSION_NOT_OPEN",
                    rejection
                        .unwrap_or_else(|| "Claude bridge did not identify an open session".into()),
                ));
            }
            if live.is_none()
                && req["require_open"] == false
                && req["cached_caller"].is_object()
                && !req["cli_cmd"].is_string()
            {
                match cached_caller(req, "claude", hint.as_deref(), matches_process, |c| {
                    if let Some(p) = pid(req) {
                        if let Ok(binding) = read_json(&binding_path(req, p)) {
                            if str_at(&binding, "start").is_some()
                                && str_at(&binding, "start").map(str::to_owned) == process_start(p)
                                && str_at(&binding, "session_id")
                                    .is_some_and(|sid| c["session_id"] != sid)
                            {
                                return Err(error(
                                    "BINDING_REQUIRED",
                                    "Endpoint session binding changed",
                                ));
                            }
                        }
                    }
                    Ok(())
                }) {
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
            let endpoint = live
                .as_ref()
                .and_then(|v| v["endpoint_pid"].as_u64())
                .and_then(|p| u32::try_from(p).ok())
                .or_else(|| {
                    pid(req).filter(|p| {
                        process_command(*p).is_ok_and(|command| matches_process(&command))
                    })
                });
            let c = caller(req, "claude", &sid, cwd, endpoint, t.as_deref());
            if live.is_none() && hint.is_none() && has_generic_session_mismatch(req, &sid) {
                return Ok(error("BINDING_REQUIRED", "Caller session mismatch"));
            }
            if let Err(e) = verify(&c, req) {
                return Ok(error("INVALID_INPUT", e));
            }
            if req["require_open"] == true && pid(req).map(|p| !alive(p)).unwrap_or(false) {
                return Ok(error("SESSION_NOT_OPEN", "Endpoint process is closed"));
            }
            Ok(json!({"ok":true,"caller":remember_process(c)}))
        }
        "probe" => {
            if let Some(p) = pid(req) {
                if let Ok(binding) = read_json(&binding_path(req, p)) {
                    if let Ok(current) = process_identity(p) {
                        if !matches_process(str_at(&current, "command").unwrap_or(""))
                            || (binding["process_identity"].is_object()
                                && binding["process_identity"] != current)
                            || str_at(&binding, "start")
                                .is_some_and(|start| process_start(p).as_deref() != Some(start))
                        {
                            return Ok(
                                json!({"ok":true,"status":"closed_definitive","reason":"Endpoint process identity changed"}),
                            );
                        }
                    }
                }
            }
            let binding_before = pid(req).and_then(|p| binding_snapshot(&binding_path(req, p)));
            match bridge(req) {
                Ok(v) if v["ok"] == true => Ok(
                    json!({"ok":true,"status":if v["session_id"]==req["session_id"]{"alive"}else{"switched"},"current_session_id":v["session_id"],"reason":null}),
                ),
                Ok(v) => {
                    if let Some(p) = pid(req) {
                        remove_rejected_binding(&binding_path(req, p), binding_before)?;
                    }
                    Ok(
                        json!({"ok":true,"status":"transient_error","reason":response_diagnostic(&v,"Claude bridge rejected the request")}),
                    )
                }
                Err(_) if pid(req).is_some_and(|p| !alive(p)) => Ok(
                    json!({"ok":true,"status":"closed_definitive","reason":"Endpoint process is closed"}),
                ),
                Err(_) => match resolve(req) {
                    Ok(current) => Ok(
                        json!({"ok":true,"status":if Some(current.as_str()) == str_at(req,"session_id") {"alive"} else {"switched"},"current_session_id":current,"reason":null}),
                    ),
                    Err(reason) => {
                        Ok(json!({"ok":true,"status":"transient_error","reason":reason}))
                    }
                },
            }
        }
        "update_badge" => {
            let sid = validate_session(str_at(req, "session_id").ok_or("Session ID is required")?)?;
            let p = pid(req).ok_or("Endpoint PID is required")?;
            let path = state(req).join(format!("sockets/claude-pid-{p}.sock"));
            if req["badge"].is_null() && !path.exists() {
                return Ok(json!({"ok":true}));
            }
            match socket_call(
                &path,
                &json!({"op":"status","session_id":sid,"badge":req["badge"]}),
            ) {
                Ok(reply) if req["badge"].is_null() && reply["error"] == "SESSION_MISMATCH" => {
                    Ok(json!({"ok":true}))
                }
                Err(_)
                    if req["badge"].is_null()
                        && !process_command(p).is_ok_and(|command| matches_process(&command)) =>
                {
                    let _ = fs::remove_file(&path);
                    Ok(json!({"ok":true}))
                }
                reply => reply,
            }
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
                            json!({"ok":true,"outcome":if r["ok"]==true{"delivered_unconfirmed"}else{"driver_unavailable"},"reason":if r["ok"]==true {Value::Null} else {json!(response_diagnostic(&r,"Claude bridge rejected delivery"))}}),
                        );
                    }
                    return Ok(
                        json!({"ok":true,"outcome":"driver_unavailable","reason":response_diagnostic(&v,"Claude bridge rejected the request")}),
                    );
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
fn hook_ack_confirmed(response: &Value, expected: usize) -> bool {
    response.get("error").is_none()
        && response["result"]["isError"] != true
        && response["result"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|item| {
                item["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .is_some_and(|value| {
                        value["ok"] == true
                            && value["acknowledged_count"]
                                .as_u64()
                                .is_some_and(|count| count >= expected as u64)
                    })
            })
}

pub fn hook_main() {
    if std::env::args().any(|s| s == "--help" || s == "-h") {
        eprintln!("Usage: cito-claude-hook (reads a hook JSON payload on stdin)");
        return;
    }
    let mut continuation = None;
    let mut delivery_locks = Vec::new();
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
        let previous = record_binding(&req, p, &sid, str_at(&req, "cwd").unwrap_or(""))?;
        let fallback = format!("claude-{p}");
        if sid != fallback {
            if let Some(previous) = previous {
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
        let alias = read_optional(&state(&req).join("claude/aliases").join(&sid))?
            .and_then(|s| validate_session(&s).ok())
            .filter(|s| s.starts_with("claude-") && s != &sid);
        let sessions: Vec<_> = alias
            .as_deref()
            .into_iter()
            .chain(std::iter::once(sid.as_str()))
            .collect();
        let mut ordered = sessions.clone();
        ordered.sort();
        ordered.dedup();
        for session in ordered {
            delivery_locks.push(FileLock::acquire(
                &queue(&req, session)?.with_extension("delivery.lock"),
            )?);
        }
        let mut wakes = drain_wakes(&req, &sessions)?;
        // Drain order follows recovered staging files, then the active queue.
        let mut seen = HashSet::new();
        wakes.retain(|w| seen.insert((w["ack_session"].clone(), w["batch_id"].clone())));
        if wakes.is_empty() {
            return Ok(json!({}));
        }
        let content = wakes
            .iter()
            .filter(|r| r["output_delivered"] != true)
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
            .and_then(|s| s.trim().parse::<u16>().ok())
            .filter(|p| *p > 0)
            .or_else(|| {
                cfg["daemonPort"]
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .filter(|p| *p > 0)
            })
            .unwrap_or(4290);
        let acks: Vec<_> = sessions.iter().filter_map(|session| {
            let ids: Vec<_> = wakes.iter().filter(|wake| wake["ack_session"] == *session).map(|wake| wake["batch_id"].clone()).collect();
            if ids.is_empty() { return None; }
            let expected = wakes.iter().filter(|wake| wake["ack_session"] == *session)
                .map(|wake| wake["expected_ack_count"].as_u64().unwrap_or(1).max(1) as usize)
                .fold(0usize, usize::saturating_add);
            Some((expected, json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"cito_inbox","arguments":{"ack_all":false,"ack_ids":ids},"_meta":{"claudeSessionId":session,"cwd":req["cwd"]}}})))
        }).collect();
        continuation = Some((
            req.clone(),
            sessions.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            wakes.clone(),
            port,
            acks,
        ));
        if content.is_empty() {
            Ok(json!({}))
        } else if event == "Stop"
            && wakes
                .iter()
                .any(|v| v["wake"] != false && v["output_delivered"] != true)
        {
            Ok(json!({"decision":"block","reason":content}))
        } else {
            Ok(json!({"hookSpecificOutput":{"hookEventName":event,"additionalContext":content}}))
        }
    })();
    match result {
        Ok(v) => {
            let stdout = std::io::stdout();
            let mut output = stdout.lock();
            if writeln!(output, "{v}")
                .and_then(|_| output.flush())
                .is_err()
            {
                return;
            }
            if let Some((req, sessions, wakes, port, acks)) = continuation {
                if let Err(error) = settle_wakes(&req, &sessions, &wakes, false) {
                    eprintln!("cito-claude-hook: {error}");
                    return;
                }
                let mut confirmed = true;
                for (expected, ack) in acks {
                    let response = ureq::post(&format!("http://127.0.0.1:{port}/mcp"))
                        .timeout(Duration::from_millis(500))
                        .send_json(ack)
                        .ok()
                        .and_then(|r| r.into_json::<Value>().ok());
                    confirmed &= response
                        .as_ref()
                        .is_some_and(|r| hook_ack_confirmed(r, expected));
                }
                if confirmed || persist_hook_acks(&req, &sessions, &wakes).is_ok() {
                    if let Err(error) = settle_wakes(&req, &sessions, &wakes, true) {
                        eprintln!("cito-claude-hook: {error}");
                    }
                }
            }
        }
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
type BadgeInodes = std::collections::HashMap<PathBuf, (u64, u64)>;

struct SidecarCleanup {
    path: PathBuf,
    inode: (u64, u64),
    badges: std::sync::Mutex<(bool, BadgeInodes)>,
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
static SIDECAR_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
static SIDECAR_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
extern "C" fn sidecar_signal(_: libc::c_int) {
    SIDECAR_STOP.store(true, std::sync::atomic::Ordering::Relaxed);
    let fd = SIDECAR_PIPE.load(std::sync::atomic::Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: only an async-signal-safe write touches the installed nonblocking pipe.
        unsafe {
            libc::write(fd, [1u8].as_ptr().cast(), 1);
        }
    }
}
fn sidecar_watchdog(cleanup: std::sync::Arc<SidecarCleanup>) {
    let wake = match WakePipe::new() {
        Ok(wake) => wake,
        Err(error) => {
            eprintln!("Cannot create sidecar shutdown pipe: {error}");
            cleanup.stop();
            std::process::exit(1);
        }
    };
    SIDECAR_PIPE.store(wake.write_fd(), std::sync::atomic::Ordering::Relaxed);
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
    // SAFETY: the signal handler sets a lock-free atomic and writes a wake byte. Filesystem cleanup
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
        if !SIDECAR_STOP.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = poll_until(
                &mut [libc::pollfd {
                    fd: wake.read_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                }],
                None,
            );
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
    if !interactive_candidate(&req, p) {
        return Ok(());
    }
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
                        && str_at(&binding, "start").is_some()
                        && str_at(&binding, "start").map(str::to_string) == process_start(p)
                    {
                        next = str_at(&binding, "session_id").map(str::to_string);
                        alias_active = next.as_deref().is_some_and(|sid| {
                            alias_for_fallback(&req, &fallback).as_deref() == Some(sid)
                        });
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
                        binding["cwd"] == cwd
                            && alive(other)
                            && str_at(&binding, "start").map(str::to_owned) == process_start(other)
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
            let mut next = next.unwrap_or_else(|| fallback.clone());
            let mut channel_req = req.clone();
            channel_req["_binding_source"] = json!("channel");
            if let Some(binding) = record_binding(&channel_req, p, &next, cwd)? {
                if binding["source"] != "channel"
                    && str_at(&binding, "start").is_some()
                    && str_at(&binding, "start").map(str::to_owned) == process_start(p)
                {
                    if let Some(sid) = str_at(&binding, "session_id") {
                        // A hook can commit while this request waits for the leaf lock.
                        // Keep an established fallback alias, otherwise adopt its identity.
                        alias_active = alias_for_fallback(&req, &fallback).as_deref() == Some(sid);
                        let selected = if alias_active { &fallback } else { sid };
                        next = selected.to_string();
                    }
                }
            }
            if next != current {
                current = next;
                last_active = now_ms();
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
