use crate::support::*;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
fn matches_process(command: &str) -> bool {
    crate::support::matches_process(command, |entry, name, script| {
        if !script {
            return name == "claude";
        }
        matches!(name, "claude" | "claude.js" | "claude.mjs")
            || (entry.contains("/claude-code/") && matches!(name, "cli.js" | "cli.mjs"))
    })
}
fn interactive_candidate(req: &Value, p: u32) -> bool {
    let Ok(command) = process_command(p) else {
        return false;
    };
    if has_harness_ancestor(p, |ancestor, command| {
        is_agent_harness(command) || live_bridge(req, "claude", ancestor)
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
fn clean_hooks(v: &mut Value) {
    if let Some(events) = v.get_mut("hooks").and_then(Value::as_object_mut) {
        events.retain(|_, groups| {
            if let Some(a) = groups.as_array_mut() {
                a.retain_mut(|g| {
                    if let Some(hooks) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                        hooks.retain(|h| {
                            h["type"] != "command"
                                || !str_at(h, "command").is_some_and(|command| {
                                    let path = command.trim_matches(['\'', '"']);
                                    path == "cito-claude-hook"
                                        || path.ends_with("/cito-claude-hook")
                                })
                        });
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
    let hook = hook.to_str().ok_or("Hook path must be valid UTF-8")?;
    let hook_command = if hook
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/_-.@%+=:,".contains(&b))
    {
        hook.to_owned()
    } else {
        format!("'{}'", hook.replace('\'', "'\\''"))
    };
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
                                    entry["type"] == "command" && entry["command"] == hook_command
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
                groups.push(json!({"hooks":[{"type":"command","command":hook_command}]}));
            }
        }
        Ok(())
    })?;
    remove_setup_runtime(req, "claude", Some("claude-pid-"))?;
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
fn wake_queue_paths(req: &Value, sid: &str) -> Result<Vec<PathBuf>> {
    let path = queue(req, sid)?;
    let prefix = format!(
        "{}.draining-",
        path.file_name()
            .ok_or("Invalid queue path")?
            .to_string_lossy()
    );
    let mut paths = Vec::new();
    for entry in
        fs::read_dir(path.parent().ok_or("Invalid queue path")?).map_err(|e| e.to_string())?
    {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            paths.push(entry.path());
        }
    }
    paths.sort();
    paths.push(path);
    Ok(paths)
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
        paths.extend(
            wake_queue_paths(req, sid)?
                .into_iter()
                .map(|path| ((*sid).to_owned(), path)),
        );
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
        .filter_map(|w| Some((raw_str_at(w, "ack_session")?, str_at(w, "batch_id")?)))
        .collect();
    let mut sessions = sessions.to_vec();
    sessions.sort();
    sessions.dedup();
    let _locks = sessions
        .iter()
        .map(|sid| queue_lock(req, sid))
        .collect::<Result<Vec<_>>>()?;
    for sid in sessions {
        for path in wake_queue_paths(req, &sid)? {
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
fn hook_endpoint(req: &Value) -> u32 {
    let hint = env_value(req, "CITO_ENDPOINT_PID").and_then(|s| s.parse().ok());
    // SAFETY: getppid only reads the parent process ID.
    let parent = unsafe { libc::getppid() as u32 };
    if hint.is_some() {
        return enclosing_endpoint_pid(hint, parent);
    }
    enclosing_endpoint_pid_with(None, parent, |pid| {
        process_command(pid).is_ok_and(|cmd| matches_process(&cmd))
            || live_bridge(req, "claude", pid)
    })
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
fn canonical_hook_identity(binding: &Value, pid: u32) -> Option<(&str, Option<&str>)> {
    if binding["source"] == "channel"
        || process_start(pid).as_deref() != Some(str_at(binding, "start")?)
    {
        return None;
    }
    Some((raw_str_at(binding, "session_id")?, str_at(binding, "cwd")))
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
                is_agent_harness(command) || live_bridge(req, "claude", ancestor)
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
fn metadata_session(req: &Value, binding: Option<&(Value, String)>) -> Result<Option<String>> {
    let mut explicit = vec![];
    for s in [
        raw_str_at(req, "explicit_session_id"),
        raw_str_at(&req["headers"], "x-claude-session-id"),
    ]
    .into_iter()
    .flatten()
    {
        explicit.push(validate_session(s)?)
    }
    for source in [&req["meta"], &req["_meta"]] {
        for key in ["claudeSessionId", "sessionId", "claude_session_id"] {
            if let Some(s) = raw_str_at(source, key) {
                explicit.push(validate_session(s)?);
            }
        }
        if str_at(source, "expected_harness") == Some("claude") {
            if let Some(s) = raw_str_at(source, "expected_session_id") {
                explicit.push(validate_session(s)?);
            }
        }
    }
    if let Some(s) = env_value(req, "CLAUDE_SESSION_ID") {
        if !s.trim().is_empty() {
            explicit.push(validate_session(&s)?);
        }
    }
    if let Some((record, published)) = binding {
        for id in &mut explicit {
            if record["session_id"] == *id {
                id.clone_from(published);
            }
        }
    }
    explicit.sort();
    explicit.dedup();
    if explicit.len() > 1 {
        return Err("Conflicting caller metadata".into());
    }
    Ok(explicit.pop())
}
// A workspace transcript has no PID. A peer without a bridge can own it too.
fn workspace_ambiguous(cwd: &str, endpoint: Option<u32>) -> Result<bool> {
    let canonical = fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
    let mut peers = user_processes()?.into_iter().filter(|&p| {
        Some(p) != endpoint
            && process_command(p).is_ok_and(|cmd| matches_process(&cmd))
            && process_cwd(p).is_ok_and(|dir| Path::new(&dir) == canonical)
    });
    if endpoint.is_none() {
        peers.next(); // One process can own the transcript; a second makes it ambiguous.
    }
    Ok(peers.next().is_some())
}
fn session_alias(req: &Value, sid: &str) -> Result<Option<String>> {
    let path = state(req)
        .join("claude/aliases")
        .join(validate_session(sid)?);
    Ok(read_optional(&path)?
        .and_then(|s| validate_session(s.strip_suffix('\n').unwrap_or(&s)).ok())
        .filter(|s| s.starts_with("claude-") && s != sid))
}
// Endpoint files retain the hook's transcript ID. Publish the identity used by the live bridge.
fn published_session(req: &Value, binding: &Value) -> Result<String> {
    let sid = raw_str_at(binding, "session_id").ok_or("Missing endpoint session")?;
    Ok(session_alias(req, sid)?.unwrap_or_else(|| sid.to_owned()))
}
fn bound_session(
    req: &Value,
    endpoint: Option<u32>,
    cwd: Option<&str>,
) -> Result<Option<(Value, String)>> {
    let Some(pid) = endpoint else { return Ok(None) };
    let Ok(record) = read_json(&binding_path(req, pid)) else {
        return Ok(None);
    };
    if cwd.is_some_and(|cwd| record["cwd"] != cwd)
        || raw_str_at(&record, "session_id").is_none()
        || str_at(&record, "start").is_none_or(|start| process_start(pid).as_deref() != Some(start))
    {
        return Ok(None);
    }
    let published = published_session(req, &record)?;
    Ok(Some((record, published)))
}
fn resolve(req: &Value) -> Result<String> {
    let endpoint = pid(req);
    let binding = bound_session(req, endpoint, Some(str_at(req, "cwd").unwrap_or("")))?;
    resolve_session(
        req,
        endpoint,
        metadata_session(req, binding.as_ref())?.as_deref(),
        binding.as_ref(),
    )
}
fn resolve_session(
    req: &Value,
    endpoint: Option<u32>,
    explicit: Option<&str>,
    binding: Option<&(Value, String)>,
) -> Result<String> {
    let cwd = str_at(req, "cwd").unwrap_or("");
    if let Some((record, published)) = binding {
        if explicit.is_some_and(|id| id != published) {
            return Err("Conflicting caller metadata for the endpoint".into());
        }
        let current = process_identity(endpoint.ok_or("Endpoint PID is required")?)?;
        if !matches_process(str_at(&current, "command").ok_or("Endpoint command is unavailable")?) {
            return Err("Endpoint process command does not match the plugin".into());
        }
        if record["process_identity"].is_object() && record["process_identity"] != current {
            return Err("Endpoint process command or start time changed".into());
        }
        return Ok(published.clone());
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
        return Ok(s.to_owned());
    }
    let mut transcripts = vec![];
    if let Ok(entries) = fs::read_dir(transcript_dir(req, cwd)) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if let Some(s) = n.strip_suffix(".jsonl") {
                if crate::is_valid_transcript_uuid(s) {
                    transcripts.push(s.to_string());
                }
            }
        }
    }
    if transcripts.len() == 1
        && (pid(req).is_none() || req["cli_cmd"].is_string())
        && !workspace_ambiguous(
            cwd,
            pid(req).filter(|p| process_command(*p).is_ok_and(|cmd| matches_process(&cmd))),
        )?
    {
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
        let start = size.saturating_sub(128 * 1024);
        f.seek(SeekFrom::Start(start.saturating_sub(1)))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        f.take(size - start + u64::from(start > 0))
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let lines = if start == 0 {
            &bytes[..]
        } else {
            bytes
                .iter()
                .position(|b| *b == b'\n')
                .map_or(&[][..], |i| &bytes[i + 1..])
        };
        let title: Option<String> = String::from_utf8_lossy(lines)
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
#[cfg(test)]
#[test]
fn transcript_tail_only_projects_complete_records() {
    let root = std::env::temp_dir().join(format!("cito-tail-{}", crate::plugins::random_hex(12)));
    let req = json!({"state_dir":root, "env":{"CITO_CLAUDE_HOME":root}});
    let project = transcript_dir(&req, "/repo");
    fs::create_dir_all(&project).unwrap();
    for (sid, preceding_byte, expected) in
        [("complete", b'\n', Some("Work")), ("fragment", b'x', None)]
    {
        let mut data = vec![preceding_byte];
        data.extend_from_slice(b"{\"type\":\"custom-title\",\"customTitle\":\"Work\"}\n");
        data.resize(1 + 128 * 1024, b' ');
        fs::write(project.join(format!("{sid}.jsonl")), data).unwrap();
        assert_eq!(title(&req, sid, "/repo").as_deref(), expected);
    }
    fs::remove_dir_all(root).unwrap();
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
    let resolve = matches!(str_at(req, "op"), Some("resolve_caller" | "verify"));
    for p in endpoint_pid_walk(
        vec![p],
        if resolve { usize::MAX } else { 0 },
        matches_process,
    ) {
        if let Ok(response) = socket_call(
            &state(req).join(format!("sockets/claude-pid-{p}.sock")),
            &json!({"op":"identify"}),
        ) {
            // A live responder's rejection is authoritative, unlike no response.
            let mut response = response;
            response["endpoint_pid"] = json!(p);
            return Ok(response);
        }
    }
    Err("Endpoint bridge is unavailable".into())
}
pub fn handle(req: &Value) -> Result<Value> {
    let op = str_at(req, "op").unwrap_or("");
    if matches!(op, "setup_inspect" | "setup_apply" | "setup_remove") {
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
                };
                let response = socket_response_or_cached(response, p, || {
                    let binding = read_json(&binding_path(&r, p))?;
                    let identity = process_identity(p)?;
                    if str_at(&binding, "start").is_none()
                        || str_at(&binding, "start").map(str::to_owned) != process_start(p)
                        || !matches_process(str_at(&identity, "command").unwrap_or(""))
                        || (binding["process_identity"].is_object()
                            && binding["process_identity"] != identity)
                        || str_at(&binding, "cwd").is_none()
                    {
                        return Err("Endpoint identity changed".into());
                    }
                    let mut session = binding;
                    session["session_id"] = json!(published_session(&r, &session)?);
                    session["ok"] = json!(true);
                    Ok(session)
                });
                let v = response.ok().filter(|v| v["ok"] == true)?;
                let binding_path = binding_path(&r, p);
                let _lock = FileLock::acquire(&binding_path.with_extension("lock")).ok()?;
                if let Ok(mut binding) = read_json(&binding_path) {
                    if published_session(&r, &binding).is_ok_and(|sid| v["session_id"] == sid)
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
                    raw_str_at(&v, "session_id").unwrap_or(""),
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
            let live_endpoint = live
                .as_ref()
                .and_then(|v| v["endpoint_pid"].as_u64())
                .and_then(|p| u32::try_from(p).ok());
            let live_sid = live.as_ref().and_then(|v| raw_str_at(v, "session_id"));
            let cached = live.is_none()
                && req["require_open"] == false
                && req["cached_caller"].is_object()
                && !req["cli_cmd"].is_string();
            let cwd_fence =
                (live_sid.is_none() && !cached).then(|| str_at(req, "cwd").unwrap_or(""));
            let endpoint = live_endpoint.or_else(|| pid(req));
            let binding = match bound_session(req, endpoint, cwd_fence) {
                Ok(binding) => binding,
                Err(_) if live_sid.is_some() => None, // A valid live response stands on its own.
                Err(e) => return Ok(error("BINDING_REQUIRED", e)),
            };
            let source = binding
                .as_ref()
                .filter(|(_, published)| live_sid.is_none_or(|sid| sid == published));
            let hint = match metadata_session(req, source) {
                Ok(hint) => hint,
                Err(e) => return Ok(error("INVALID_INPUT", e)),
            };
            let hook_id = source.and_then(|(record, _)| raw_str_at(record, "session_id"));
            if cached {
                match cached_caller(
                    req,
                    "claude",
                    hint.as_deref(),
                    hook_id,
                    matches_process,
                    |c| {
                        if binding
                            .as_ref()
                            .is_some_and(|(_, published)| c["session_id"] != *published)
                        {
                            return Err(error(
                                "BINDING_REQUIRED",
                                "Endpoint session binding changed",
                            ));
                        }
                        Ok(())
                    },
                ) {
                    Ok(Some(c)) => return Ok(json!({"ok":true,"caller":c})),
                    Err(e) => return Ok(e),
                    Ok(None) => {}
                }
            }
            if let (Some(expected), Some(actual)) = (hint.as_deref(), live_sid) {
                if expected != actual {
                    return Ok(error("INVALID_INPUT", "Conflicting caller metadata"));
                }
            }
            let sid = match live_sid
                .map(str::to_string)
                .map(Ok)
                .unwrap_or_else(|| resolve_session(req, endpoint, hint.as_deref(), source))
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
            let t = live
                .as_ref()
                .and_then(|v| str_at(v, "session_name"))
                .map(str::to_owned)
                .or_else(|| title(req, hook_id.unwrap_or(&sid), cwd));
            let endpoint = live_endpoint.or_else(|| {
                pid(req)
                    .filter(|p| process_command(*p).is_ok_and(|command| matches_process(&command)))
            });
            let c = caller(req, "claude", &sid, cwd, endpoint, t.as_deref());
            if live.is_none() && hint.is_none() && has_generic_session_mismatch(req, &sid, hook_id)
            {
                return Ok(error("BINDING_REQUIRED", "Caller session mismatch"));
            }
            if let Err(e) = verify(&c, req, hook_id) {
                return Ok(error("INVALID_INPUT", e));
            }
            if req["require_open"] == true && pid(req).map(|p| !alive(p)).unwrap_or(false) {
                return Ok(error("SESSION_NOT_OPEN", "Endpoint process is closed"));
            }
            Ok(json!({"ok":true,"caller":remember_process(c)}))
        }
        "probe" => {
            if let Some(p) = pid(req) {
                if let Ok(current) = process_identity(p) {
                    if endpoint_identity_changed(req, &current, matches_process) {
                        return Ok(
                            json!({"ok":true,"status":"closed_definitive","reason":"Endpoint process identity changed"}),
                        );
                    }
                    if let Ok(binding) = read_json(&binding_path(req, p)) {
                        if (binding["process_identity"].is_object()
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
                        json!({"ok":true,"status":if Some(current.as_str()) == raw_str_at(req,"session_id") {"alive"} else {"switched"},"current_session_id":current,"reason":null}),
                    ),
                    Err(reason) => {
                        Ok(json!({"ok":true,"status":"transient_error","reason":reason}))
                    }
                },
            }
        }
        "update_badge" => {
            let sid =
                validate_session(raw_str_at(req, "session_id").ok_or("Session ID is required")?)?;
            let p = pid(req).ok_or("Endpoint PID is required")?;
            let path = state(req).join(format!("sockets/claude-pid-{p}.sock"));
            update_badge(req, &path, p, &sid, matches_process)
        }
        "wake" => {
            let sid =
                validate_session(raw_str_at(req, "session_id").ok_or("Session ID is required")?)?;
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
                            &json!({"op":"deliver","session_id":sid,"batch_id":req["batch_id"],"content":req["text"],"wake":req["wake"]!=false,"signals":req["signals"]}),
                        );
                        match r {
                            Ok(reply) if reply["error"] == "SESSION_MISMATCH" => {
                                return Ok(
                                    json!({"ok":true,"outcome":"session_switched","reason":null}),
                                );
                            }
                            Ok(reply) => {
                                return Ok(
                                    json!({"ok":true,"outcome":if reply["ok"]==true{"delivered_unconfirmed"}else{"driver_unavailable"},"reason":if reply["ok"]==true {Value::Null} else {json!(response_diagnostic(&reply,"Claude bridge rejected delivery"))}}),
                                )
                            }
                            Err(_) => {
                                append_wake(req, &sid)?;
                                return Ok(
                                    json!({"ok":true,"outcome":"delivered_unconfirmed","reason":"Queued for the next hook event"}),
                                );
                            }
                        }
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
        let sid = match raw_str_at(&req, "explicit_session_id") {
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
        let alias = session_alias(&req, &sid)?;
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
        let wakes = drain_wakes(&req, &sessions)?;
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
        // Hook ACKs historically skip a malformed source and try the next one.
        let port = env_value(&req, "CITO_DAEMON_PORT")
            .and_then(|env| resolve_daemon_port(Some(&env), None).ok())
            .unwrap_or_else(|| {
                resolve_daemon_port(None, cfg.get("daemonPort")).unwrap_or(DEFAULT_DAEMON_PORT)
            });
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
                    let response = crate::plugins::http_post_mcp(
                        port,
                        &ack.to_string(),
                        Duration::from_millis(500),
                    )
                    .ok()
                    .filter(|r| r.status == 200)
                    .and_then(|r| serde_json::from_str::<Value>(&r.body).ok());
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
                if fs::symlink_metadata(path).is_ok_and(|m| (m.dev(), m.ino()) == *inode) {
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
            let value = fs::read_to_string(entry.path()).ok()?;
            if value.strip_suffix('\n').unwrap_or(&value) == fallback {
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
            if !crate::is_valid_transcript_uuid(&name) {
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
    let explicit = metadata_session(&req, None)?;
    // An unhooked process cannot attribute a workspace transcript to itself.
    let fallback = format!("claude-{p}");
    let mut alias_active = explicit.is_none();
    let mut first_transcript: Option<String> = None;
    let mut current = explicit.clone().unwrap_or_else(|| fallback.clone());
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
            if alias_active && first_transcript.is_none() {
                first_transcript = alias_for_fallback(&req, &fallback);
            }
            let mut next = explicit.clone();
            let mut cwd = cwd;
            let initial;
            if next.is_none() {
                initial = read_json(&binding_path(&req, p)).ok();
                if let Some((sid, workspace)) =
                    initial.as_ref().and_then(|v| canonical_hook_identity(v, p))
                {
                    next = Some(sid.to_owned());
                    cwd = workspace.unwrap_or(cwd);
                    alias_active = alias_for_fallback(&req, &fallback).as_deref() == Some(sid);
                }
            }
            let mut latest = latest_transcript(&req, cwd);
            if next.is_none() {
                if latest.is_some() && workspace_ambiguous(cwd, Some(p))? {
                    latest = None;
                    next = Some(fallback.clone());
                    alias_active = false;
                    first_transcript = None;
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
            let binding = record_binding(&channel_req, p, &next, cwd)?;
            if let Some((sid, workspace)) =
                binding.as_ref().and_then(|v| canonical_hook_identity(v, p))
            {
                // A hook can commit while this request waits for the leaf lock.
                alias_active = alias_for_fallback(&req, &fallback).as_deref() == Some(sid);
                next = if alias_active {
                    first_transcript = Some(sid.into());
                    fallback.clone()
                } else {
                    sid.into()
                };
                if let Some(workspace) = workspace.filter(|&path| path != cwd) {
                    cwd = workspace;
                    latest = latest_transcript(&req, cwd);
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
                wake["signals"] = v["signals"].clone();
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
