use crate::support::*;
use serde_json::{json, Value};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use toml_edit::{value, DocumentMut, Item, Table};
fn matches_process(command: &str) -> bool {
    let Some((program, script)) = process_entrypoint(command) else {
        return false;
    };
    let entry = script.unwrap_or(program);
    let name = Path::new(entry)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if script.is_some() {
        return matches!(name, "codex" | "codex.js" | "codex.mjs");
    }
    name == "codex"
        || (name == "Codex" && program.contains("/Codex.app/Contents/MacOS/"))
        || (name == "ChatGPT" && program.contains("/ChatGPT.app/Contents/MacOS/"))
}
fn desktop_command(command: &str) -> bool {
    process_entrypoint(command).is_some_and(|(program, script)| {
        let entry = script.unwrap_or(program);
        match Path::new(entry).file_name().and_then(|n| n.to_str()) {
            Some("Codex") => entry.contains("/Codex.app/Contents/MacOS/"),
            Some("ChatGPT") => entry.contains("/ChatGPT.app/Contents/MacOS/"),
            _ => false,
        }
    })
}
fn interactive_process(p: u32, command: &str) -> bool {
    if has_harness_ancestor(p, |_, command| is_agent_harness(command)) {
        return false;
    }
    if desktop_command(command) {
        return true;
    }
    matches_process(command)
        && process_has_tty(p)
        && !command.split_whitespace().any(|arg| {
            matches!(
                arg,
                "exec"
                    | "exec-server"
                    | "app-server"
                    | "mcp-server"
                    | "cloud"
                    | "--help"
                    | "--version"
            )
        })
}
fn desktop_running(req: &Value) -> Result<bool> {
    if let Some(p) = pid(req) {
        let identity = process_identity(p)?;
        let command =
            str_at(&identity, "command").ok_or("Desktop process command is unavailable")?;
        return Ok(interactive_process(p, command));
    }
    Ok(user_processes()?
        .into_iter()
        .any(|pid| process_command(pid).is_ok_and(|command| interactive_process(pid, &command))))
}
fn db(req: &Value) -> PathBuf {
    config_home(req, "CITO_CODEX_HOME", "~/.codex").join("state_5.sqlite")
}
fn binary(req: &Value) -> Option<PathBuf> {
    let mut candidates = vec![];
    if let Some(p) = env_value(req, "CITO_CODEX_BIN").filter(|s| !s.trim().is_empty()) {
        let p = expand(req, &p);
        return p
            .metadata()
            .ok()
            .filter(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .map(|_| p);
    }
    candidates.extend([PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex"),PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"),home(req).join("Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex"),home(req).join("Applications/Codex.app/Contents/Resources/codex")]);
    for d in env_value(req, "PATH").unwrap_or_default().split(':') {
        candidates.push(Path::new(d).join("codex"));
    }
    candidates.into_iter().find(|p| {
        p.metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}
fn parse(s: Option<&str>) -> Result<DocumentMut> {
    s.unwrap_or("")
        .parse()
        .map_err(|e| format!("Invalid config.toml: {e}"))
}
fn configured(doc: &DocumentMut, cmd: &str) -> bool {
    doc.get("mcp_servers")
        .and_then(|v| v.get("cito"))
        .is_some_and(|cito| {
            cito.get("enabled").and_then(Item::as_bool) != Some(false)
                && cito
                    .get("command")
                    .and_then(Item::as_str)
                    .is_some_and(|entry| {
                        entry == cmd || entry == "cito-mcp" || entry.ends_with("/cito-mcp")
                    })
        })
}
fn setup(req: &Value) -> Result<Value> {
    let path = config_home(req, "CITO_CODEX_HOME", "~/.codex").join("config.toml");
    let command = state(req)
        .join("bin/cito-mcp")
        .to_string_lossy()
        .into_owned();
    let op = str_at(req, "op").unwrap_or("");
    if op == "setup_inspect" {
        let old = read_optional(&path)?;
        let d = parse(old.as_deref())?;
        return Ok(
            json!({"ok":true,"installed":path.parent().map(Path::exists).unwrap_or(false)||binary(req).is_some(),"configured":configured(&d,&command),"paths":[path]}),
        );
    }
    let remove = op == "setup_remove";
    let action = mutate(req, "codex", &path, |old| {
        let mut d = parse(old)?;
        check_mcp_command(
            req,
            d.get("mcp_servers")
                .and_then(|v| v.get("cito"))
                .and_then(|v| v.get("command"))
                .and_then(Item::as_str),
            &command,
        )?;
        if remove {
            if let Some(t) = d.get_mut("mcp_servers").and_then(Item::as_table_like_mut) {
                t.remove("cito");
                if t.is_empty() {
                    d.remove("mcp_servers");
                }
            }
        } else {
            if !d.contains_key("mcp_servers") {
                d["mcp_servers"] = Item::Table(Table::new());
            }
            if d["mcp_servers"].as_table_like().is_none() {
                return Err("mcp_servers must be a table".into());
            }
            if d["mcp_servers"].get("cito").is_none() {
                d["mcp_servers"]["cito"] = Item::Table(Table::new());
            }
            let t = &mut d["mcp_servers"]["cito"];
            if t.as_table_like().is_none() {
                return Err("mcp_servers.cito must be a table".into());
            }
            t["command"] = value(command);
            t["args"] = value(toml_edit::Array::new());
            t["enabled"] = value(true);
        }
        let next = d.to_string();
        Ok(if old.is_none() && next.is_empty() {
            None
        } else {
            Some(next)
        })
    })?;
    Ok(json!({"ok":true,"actions":[action]}))
}
fn activity_at(row: &Value) -> Option<(u64, u64)> {
    let revision = str_at(row, "recency_at_ms")
        .or_else(|| str_at(row, "updated_at"))?
        .parse::<u64>()
        .ok()?;
    Some((
        revision,
        if revision < 100_000_000_000 {
            revision.saturating_mul(1000)
        } else {
            revision
        },
    ))
}
fn effect_path(req: &Value, sid: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    state(req)
        .join("activity/codex")
        .join(format!("{:x}.json", Sha256::digest(sid.as_bytes())))
}
fn record_effect(req: &Value, sid: &str, text: &str, activity: Option<(u64, u64)>) -> Result<()> {
    use sha2::{Digest, Sha256};
    let path = effect_path(req, sid);
    std::fs::create_dir_all(path.parent().ok_or("Invalid activity path")?)
        .map_err(|_| "Cannot save activity attribution")?;
    let _lock = FileLock::acquire(&path.with_extension("lock"))?;
    let mut ledger = read_json(&path)?;
    let hash = json!(format!("{:x}", Sha256::digest(text.as_bytes())));
    let mut effects = ledger["effects"].as_array().cloned().unwrap_or_default();
    if !effects.contains(&hash) {
        effects.push(hash);
    }
    ledger["effects"] = json!(effects);
    if let Some((revision, at)) = activity {
        ledger["fallback"] = json!({"revision":revision,"at_ms":at});
    }
    atomic_write(&path, ledger.to_string().as_bytes())
}
fn idle_wake_guard(path: &Path) -> Result<Option<std::fs::File>> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| "Cannot inspect in-flight wake")?;
    // SAFETY: the descriptor stays owned by file. Closing it releases this shared
    // lock, including on process exit. Wake uses an exclusive lock on the same path.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err("Cannot inspect in-flight wake".into())
    }
}
fn source_activity(req: &Value, row: &Value) -> Result<Value> {
    use sha2::{Digest, Sha256};
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    let path = effect_path(req, str_at(row, "id").unwrap_or(""));
    std::fs::create_dir_all(path.parent().ok_or("Invalid activity path")?)
        .map_err(|_| "Cannot save session activity")?;
    let _lock = FileLock::acquire(&path.with_extension("lock"))?;
    let ledger = read_json(&path)?;
    let Some((source_revision, recency)) = activity_at(row) else {
        return Ok(Value::Null);
    };
    let cursor_path = path.with_extension("source.json");
    let mut cached = read_json(&cursor_path)?;
    let previous = cached.clone();
    // Retain the maximum observed source integer, never a refresh counter. This
    // also prevents a source timestamp rollback from decreasing the revision.
    let revision = source_revision.max(cached["revision"].as_u64().unwrap_or(0));
    // Do not consume a user-shaped queue record before its attribution commits.
    // Hold the shared guard through checkpointing so a wake cannot start mid-scan.
    // The nonblocking acquisition avoids waiting on a wake that needs our ledger lock.
    let Some(_idle) = idle_wake_guard(&path.with_extension("wake.lock"))? else {
        return Ok(json!({"revision":revision,"at_ms":recency,"kind":"cito_effect"}));
    };
    if let Some(rollout) = str_at(row, "rollout_path") {
        match std::fs::File::open(rollout) {
            Ok(mut file) => {
                let metadata = file
                    .metadata()
                    .map_err(|_| "Cannot inspect session activity")?;
                let same_source = cached["device"] == metadata.dev()
                    && cached["inode"] == metadata.ino()
                    && cached["offset"]
                        .as_u64()
                        .is_some_and(|offset| offset <= metadata.len());
                let mut offset = if same_source {
                    cached["offset"].as_u64().unwrap_or(0)
                } else {
                    0
                };
                let mut latest = if same_source {
                    cached["user_at_ms"].as_u64()
                } else {
                    None
                };
                let mut saw_effect = same_source && cached["saw_effect"] == true;
                file.seek(SeekFrom::Start(offset))
                    .map_err(|_| "Cannot read session activity")?;
                let mut reader = BufReader::new(file);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    let n = reader
                        .read_until(b'\n', &mut line)
                        .map_err(|_| "Cannot read session activity")?;
                    // Never checkpoint a partially appended JSONL record.
                    if n == 0 || !line.ends_with(b"\n") {
                        break;
                    }
                    offset += n as u64;
                    let Ok(event) = serde_json::from_str::<Value>(&String::from_utf8_lossy(&line))
                    else {
                        continue;
                    };
                    let payload = &event["payload"];
                    let text = if payload["type"] == "user_message" {
                        payload["message"].as_str().map(str::to_owned)
                    } else if payload["type"] == "message" && payload["role"] == "user" {
                        payload["content"].as_array().map(|a| {
                            a.iter()
                                .filter_map(|v| v["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                    } else {
                        None
                    };
                    let Some(text) = text else {
                        continue;
                    };
                    let hash = json!(format!("{:x}", Sha256::digest(text.as_bytes())));
                    let effect = ledger["effects"]
                        .as_array()
                        .is_some_and(|effects| effects.contains(&hash));
                    if effect {
                        saw_effect = true;
                    } else if let Some(at) =
                        event["timestamp"].as_str().and_then(activity_timestamp)
                    {
                        latest = Some(latest.unwrap_or(0).max(at));
                    }
                }
                // The cursor bounds repeat work, not the activity revision. Revision
                // remains the durable DB source integer across scans and restarts.
                let observed = json!({"device":metadata.dev(),"inode":metadata.ino(),"offset":offset,"user_at_ms":latest,"saw_effect":saw_effect,"revision":revision});
                if observed != previous {
                    atomic_write(&cursor_path, observed.to_string().as_bytes())?;
                }
                if let Some(at) = latest {
                    return Ok(json!({"revision":revision,"at_ms":at,"kind":"user_turn"}));
                }
                if saw_effect {
                    return Ok(json!({"revision":revision,"at_ms":recency,"kind":"cito_effect"}));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Cannot read session activity".into()),
        }
    }
    cached["revision"] = json!(revision);
    if cached != previous {
        atomic_write(&cursor_path, cached.to_string().as_bytes())?;
    }
    if let (Some(effect_revision), Some(effect_at)) = (
        ledger["fallback"]["revision"].as_u64(),
        ledger["fallback"]["at_ms"].as_u64(),
    ) {
        let kind = if source_revision <= effect_revision || recency <= effect_at {
            "cito_effect"
        } else {
            "user_turn"
        };
        return Ok(json!({"revision":revision,"at_ms":recency,"kind":kind}));
    }
    Ok(json!({"revision":revision,"at_ms":recency,"kind":"unknown"}))
}
fn activity_timestamp(raw: &str) -> Option<u64> {
    // SAFETY: libc::tm has valid zero-initialized fields before strptime fills them.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let input = std::ffi::CString::new(raw).ok()?;
    // SAFETY: both C strings and tm remain live through strptime/timegm.
    let rest = unsafe { libc::strptime(input.as_ptr(), c"%Y-%m-%dT%H:%M:%S".as_ptr(), &mut tm) };
    if rest.is_null() {
        return None;
    }
    let seconds = unsafe { libc::timegm(&mut tm) };
    let fraction = raw
        .get(19..)?
        .strip_prefix('.')
        .map(|s| {
            s.chars()
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect::<String>()
        })
        .unwrap_or_default();
    let ms = format!("{fraction:0<3}").parse::<u64>().ok()?;
    u64::try_from(seconds)
        .ok()?
        .checked_mul(1000)?
        .checked_add(ms)
}
fn row_for_id(req: &Value, id: &str) -> Result<Option<Value>> {
    let path = db(req);
    if !path.try_exists().map_err(|e| e.to_string())? {
        return Ok(None);
    }
    let connection = ReadOnlySqlite::open(&path, Duration::from_millis(1200))?;
    if connection.rows("PRAGMA table_info(threads)")?.is_empty() {
        return Ok(None);
    }
    Ok(connection
        .rows(&format!(
            "SELECT * FROM threads WHERE id = '{}'",
            id.replace('\'', "''")
        ))?
        .into_iter()
        .next())
}
fn try_rows(req: &Value) -> Result<Vec<Value>> {
    let path = db(req);
    if !path.try_exists().map_err(|e| e.to_string())? {
        return Ok(vec![]);
    }
    let connection = ReadOnlySqlite::open(&path, Duration::from_millis(1200))?;
    let columns = connection.rows("PRAGMA table_info(threads)")?;
    let has = |name: &str| columns.iter().any(|c| c["name"] == name);
    if !has("id") || !has("cwd") {
        return Ok(vec![]);
    }
    let projection = [
        "id",
        "cwd",
        "name",
        "title",
        "git_branch",
        "git_origin_url",
        "recency_at_ms",
        "updated_at",
        "archived",
        "thread_source",
        "source",
        "rollout_path",
    ]
    .into_iter()
    .filter(|column| has(column))
    .collect::<Vec<_>>()
    .join(", ");
    let mut predicates = vec![
        "id IS NOT NULL",
        "trim(id,char(9)||char(10)||char(13)||' ') != ''",
        "cwd IS NOT NULL",
        "trim(cwd,char(9)||char(10)||char(13)||' ') != ''",
    ];
    if has("archived") && req["cli_cmd"] != "unlisten" {
        predicates.push("archived = 0");
    }
    if has("thread_source") {
        predicates.push("(thread_source IS NULL OR thread_source = '' OR thread_source = 'user')");
    }
    if has("source") {
        predicates.push("(source IS NULL OR (source NOT IN ('exec','subagent') AND source NOT LIKE '%\"subagent\"%'))");
    }
    let mut order = vec![];
    if has("archived") && req["cli_cmd"] == "unlisten" {
        order.push("archived ASC");
    }
    if has("recency_at_ms") {
        order.push("recency_at_ms DESC");
    }
    if has("updated_at") {
        order.push("updated_at DESC");
    }
    order.push("rowid DESC");
    let predicate = predicates.join(" AND ");
    let order = order.join(", ");
    let window = if req["op"] == "discover_sessions" {
        let recency = match (has("recency_at_ms"), has("updated_at")) {
            (true, true) => "COALESCE(recency_at_ms,updated_at,0)",
            (true, false) => "COALESCE(recency_at_ms,0)",
            (false, true) => "COALESCE(updated_at,0)",
            (false, false) => "0",
        };
        let cutoff = now_ms().saturating_sub(req["recent_window_ms"].as_u64().unwrap_or(900_000));
        let retained = req["retained_session_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        // One coherent snapshot covers display, recent enrollment and restoration.
        format!(" AND (id IN ({retained}) OR rowid IN (SELECT rowid FROM threads WHERE {predicate} ORDER BY {order} LIMIT 8) OR CASE WHEN {recency}<100000000000 THEN {recency}*1000 ELSE {recency} END >= {cutoff})")
    } else {
        String::new()
    };
    Ok(connection
        .rows(&format!(
            "SELECT {projection} FROM threads WHERE {predicate}{window} ORDER BY {order}"
        ))?
        .into_iter()
        // Preserve caller validation for whitespace-only IDs and working directories.
        .filter(|row| str_at(row, "id").is_some() && str_at(row, "cwd").is_some())
        .collect())
}
fn metadata_id(req: &Value) -> Result<Option<String>> {
    let mut ids = vec![];
    for source in [&req["meta"], &req["_meta"]] {
        for key in ["openai/threadId", "threadId", "codexThreadId"] {
            if let Some(s) = str_at(source, key) {
                ids.push(s.to_string())
            }
        }
    }
    for source in [&req["meta"], &req["headers"]] {
        let turn = &source["x-codex-turn-metadata"];
        let fragments = turn
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(std::slice::from_ref(turn));
        for fragment in fragments {
            let parsed = if let Some(s) = fragment.as_str() {
                serde_json::from_str::<Value>(s).map_err(|_| "Invalid turn metadata JSON")?
            } else {
                fragment.clone()
            };
            for key in ["thread_id", "session_id"] {
                if let Some(s) = str_at(&parsed, key) {
                    ids.push(s.into())
                }
            }
        }
    }
    if caller_supplied_pid(req)?.is_none()
        && str_at(&req["meta"], "expected_harness").is_none_or(|id| id == "codex")
    {
        if let Some(s) = str_at(req, "explicit_session_id") {
            ids.push(s.into())
        }
    }
    if caller_supplied_pid(req)?.is_none() && req["meta"]["expected_harness"] == "codex" {
        if let Some(s) = str_at(&req["meta"], "expected_session_id") {
            ids.push(s.into())
        }
    }
    if let Some(s) = env_value(req, "CODEX_THREAD_ID") {
        if !s.trim().is_empty() {
            ids.push(s)
        }
    }
    ids.sort();
    ids.dedup();
    if ids.len() > 1 {
        return Err("Conflicting caller metadata".into());
    }
    Ok(ids.pop())
}
fn row_caller(req: &Value, r: &Value) -> Value {
    let cwd = str_at(r, "cwd")
        .or_else(|| str_at(req, "cwd"))
        .unwrap_or("");
    let title = str_at(r, "name").or_else(|| str_at(r, "title"));
    if r.get("git_branch").is_some() || r.get("git_origin_url").is_some() {
        // SQLite metadata is authoritative, including absent values. Do not
        // launch git once per historical thread during discovery.
        return json!({"harness":"codex","session_id":r["id"],"cwd":cwd,
            "endpoint_pid":pid(req),"branch":r["git_branch"],
            "repo":str_at(r,"git_origin_url").and_then(normalize_repo),
            "title":title.unwrap_or_else(||Path::new(cwd).file_name().and_then(|s|s.to_str()).unwrap_or("session"))});
    }
    caller(
        req,
        "codex",
        str_at(r, "id").unwrap_or(""),
        cwd,
        pid(req),
        title,
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
            let running = desktop_running(req)?;
            let sessions = if running {
                try_rows(req)?
                    .iter()
                    .map(|r| {
                        let mut c = row_caller(req, r);
                        // One unreadable rollout must not discard unrelated threads.
                        // Unknown activity cannot enroll an idle activity-mode session.
                        c["activity"] = source_activity(req, r).unwrap_or(Value::Null);
                        c["updated_at_ms"] = json!(str_at(r, "recency_at_ms")
                            .or_else(|| str_at(r, "updated_at"))
                            .and_then(|s| s.parse::<u64>().ok())
                            .map(|t| if t < 100_000_000_000 { t * 1000 } else { t }));
                        Ok(c)
                    })
                    .collect::<Result<Vec<_>>>()?
            } else {
                vec![]
            };
            Ok(json!({"ok":true,"sessions":sessions}))
        }
        "resolve_caller" | "verify" => {
            let explicit = match metadata_id(req) {
                Ok(v) => v,
                Err(e) => return Ok(error("INVALID_INPUT", e)),
            };
            if caller_supplied_pid(req)?.is_some() {
                return Ok(error(
                    if explicit.is_some() {
                        "INVALID_INPUT"
                    } else {
                        "BINDING_REQUIRED"
                    },
                    "Conflicting caller metadata: explicit bridge PID cannot claim a Codex thread",
                ));
            }
            let transport_pid = explicit_pid(req)?.is_some();
            if explicit.is_none()
                && transport_pid
                && pid(req).is_some_and(|p| {
                    process_command(p)
                        .is_ok_and(|cmd| is_agent_harness(&cmd) && !matches_process(&cmd))
                })
            {
                return Ok(error(
                    "BINDING_REQUIRED",
                    "Transport belongs to another harness",
                ));
            }
            let all = try_rows(req)?;
            let known_row = explicit
                .as_deref()
                .map(|id| row_for_id(req, id))
                .transpose()?
                .flatten();
            let needs_cached_pid = (explicit.is_some() && known_row.is_none())
                || (explicit.is_none()
                    && ((!transport_pid && str_at(req, "cli_cmd").is_none()) || all.is_empty()));
            if needs_cached_pid && (!transport_pid || req["cached_caller"].is_object()) {
                match cached_caller(req, "codex", explicit.as_deref(), matches_process) {
                    Ok(Some(c)) => return Ok(json!({"ok":true,"caller":c})),
                    Err(e) => return Ok(e),
                    Ok(None) => {}
                }
            }
            let row = if let Some(id) = explicit.as_deref() {
                let row = known_row;
                if row
                    .as_ref()
                    .and_then(|r| str_at(r, "archived"))
                    .map(|s| s != "0")
                    .unwrap_or(false)
                    && req["require_open"] != false
                {
                    return Ok(error("SESSION_NOT_OPEN", "Codex thread is archived"));
                }
                row.unwrap_or_else(|| json!({"id":id,"cwd":req["cwd"]}))
            } else if str_at(req, "cli_cmd").is_some()
                || transport_pid
                || (pid(req).is_none()
                    && (req["meta"]["expected_harness"] == "codex"
                        || (req["http_transport"] != true
                            && req["meta"]["http_transport"] != true)))
            {
                match all
                    .iter()
                    .find(|r| r["cwd"] == req["cwd"])
                    .or_else(|| all.first())
                {
                    Some(r) => r.clone(),
                    None => {
                        return Ok(error(
                            "BINDING_REQUIRED",
                            "Codex thread metadata is required",
                        ))
                    }
                }
            } else {
                return Ok(error(
                    "BINDING_REQUIRED",
                    "Codex thread metadata is required",
                ));
            };
            let c = row_caller(req, &row);
            // A generic session fence cannot turn an inferred thread into an
            // authoritative cross-provider conflict. Let other providers resolve it.
            if explicit.is_none()
                && req["meta"]["expected_harness"] != "codex"
                && str_at(&req["meta"], "expected_session_id")
                    .is_some_and(|id| c["session_id"] != id)
            {
                return Ok(error("BINDING_REQUIRED", "Caller session mismatch"));
            }
            if let Err(e) = verify(&c, req) {
                return Ok(error("INVALID_INPUT", e));
            }
            Ok(json!({"ok":true,"caller":remember_process(c)}))
        }
        "probe" => {
            let inspection = setup(
                &json!({"op":"setup_inspect","env":req["env"],"state_dir":req["state_dir"],"plugin_dir":req["plugin_dir"]}),
            )?;
            if inspection["configured"] != true {
                return Ok(
                    json!({"ok":true,"status":"closed_definitive","reason":"Integration is disconnected"}),
                );
            }
            let id = str_at(req, "session_id").unwrap_or("");
            let archived = match row_for_id(req, id) {
                Ok(row) => row.and_then(|r| r["archived"].as_str().map(|s| s != "0")),
                Err(error)
                    if matches!(
                        error.as_str(),
                        "Session database is busy" | "Session database deadline exceeded"
                    ) =>
                {
                    return Ok(json!({"ok":true,"status":"transient_error","reason":error}));
                }
                Err(error) => return Err(error),
            };
            let status = if archived == Some(true) {
                "closed_definitive"
            } else {
                // Thread metadata is authoritative; the MCP endpoint may be a
                // forwarder rather than Desktop. Liveness belongs to the app.
                let mut app_request = req.clone();
                if let Some(request) = app_request.as_object_mut() {
                    request.remove("endpoint_pid");
                }
                match desktop_running(&app_request) {
                    Ok(true) => "alive",
                    Ok(false) => "closed_definitive",
                    Err(_) => "transient_error",
                }
            };
            Ok(json!({"ok":true,"status":status,"current_session_id":id,"reason":null}))
        }
        "update_badge" => Ok(json!({"ok":true})),
        "wake" => {
            let sid = str_at(req, "session_id").ok_or("Session ID is required")?;
            let text = req["text"].as_str().ok_or("Wake text is required")?;
            let run = |mut command: Command, timeout: Duration| {
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                if let Some(env) = req["env"].as_object() {
                    for (key, value) in env {
                        if let Some(value) = value.as_str() {
                            command.env(key, value);
                        }
                    }
                }
                run_command_with_timeout(command, timeout)
                    .map(|output| output.map(|o| o.status.success()))
            };
            let already_queued = req["queue_written"] == true;
            let mut wake_guard = None;
            let queue_confirmed = if already_queued {
                req["queue_confirmed"] == true
            } else {
                let Some(bin) = binary(req) else {
                    return Ok(
                        json!({"ok":true,"outcome":"driver_unavailable","queue_written":false,"reason":"Codex binary was not found"}),
                    );
                };
                // Serialize deliveries and exclude source qualification until the
                // confirmed effect is durable. OS locks need no stale-PID recovery.
                wake_guard = match FileLock::acquire(
                    &effect_path(req, sid).with_extension("wake.lock"),
                ) {
                    Ok(guard) => Some(guard),
                    Err(_) => {
                        return Ok(
                            json!({"ok":true,"outcome":"driver_unavailable","queue_written":false,"reason":"Cannot guard wake attribution"}),
                        )
                    }
                };
                let mut attempt = 0;
                loop {
                    let mut command = Command::new(&bin);
                    command
                        .args(["queue", "--resume", "--thread", sid, "--message", text])
                        .env(
                            "CODEX_HOME",
                            config_home(req, "CITO_CODEX_HOME", "~/.codex"),
                        );
                    let failure = match run(command, Duration::from_secs(10)) {
                        Ok(Some(true)) => break true,
                        // A timed-out insertion may have committed. Never submit it again.
                        Ok(None) => break false,
                        Ok(Some(false)) => "Codex queue command failed".to_string(),
                        Err(error) => error,
                    };
                    attempt += 1;
                    if attempt >= 3 {
                        return Ok(
                            json!({"ok":true,"outcome":"driver_unavailable","queue_written":false,"reason":failure}),
                        );
                    }
                    thread::sleep(Duration::from_secs(attempt));
                }
            };
            // Attribution failure must not erase a confirmed insertion checkpoint:
            // the host would otherwise retry a queue command that already committed.
            let attribution_error = if !already_queued && queue_confirmed {
                row_for_id(req, sid)
                    .and_then(|row| {
                        record_effect(req, sid, text, row.as_ref().and_then(activity_at))
                    })
                    .err()
            } else {
                None
            };
            drop(wake_guard);
            if !already_queued && req["checkpoint_only"] == true {
                // Return the insertion checkpoint before requesting a separately
                // fenced open. The host rechecks unreadness and binding ownership.
                return Ok(
                    json!({"ok":true,"outcome":"delivered_unconfirmed","queue_written":true,"queue_confirmed":queue_confirmed,"insertion_unknown":!queue_confirmed,"open_confirmed":false,"continue_wake":true,"reason":attribution_error}),
                );
            }
            let mut opened = false;
            for attempt in 0..2 {
                let mut command = Command::new(
                    env_value(req, "CITO_OPEN_BIN").unwrap_or_else(|| "/usr/bin/open".into()),
                );
                command.args(["-g", &format!("codex://threads/wake/{sid}")]);
                if matches!(run(command, Duration::from_secs(3)), Ok(Some(true))) {
                    opened = true;
                    break;
                }
                if attempt == 0 {
                    thread::sleep(Duration::from_millis(500));
                }
            }
            Ok(
                json!({"ok":true,"outcome":if queue_confirmed && opened {"delivered"}else{"delivered_unconfirmed"},"queue_written":true,"queue_confirmed":queue_confirmed,"insertion_unknown":!queue_confirmed,"open_confirmed":opened,"reason":if opened{attribution_error}else{Some("Codex open effect was not confirmed".into())}}),
            )
        }
        _ => Ok(error("INVALID_INPUT", "Unknown plugin operation")),
    }
}
