use crate::support::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
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
fn desktop_running(req: &Value) -> Result<bool> {
    if let Some(p) = pid(req) {
        let identity = process_identity(p)?;
        let command =
            str_at(&identity, "command").ok_or("Desktop process command is unavailable")?;
        return Ok(desktop_command(command) || matches_process(command));
    }
    let output = Command::new("/bin/ps")
        .args(["-axo", "uid=,command="])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("Desktop process inspection failed".into());
    }
    // SAFETY: geteuid only reads this process's effective user ID.
    let uid = unsafe { libc::geteuid() }.to_string();
    Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        line.trim_start()
            .split_once(char::is_whitespace)
            .is_some_and(|(owner, command)| {
                owner == uid
                    && (desktop_command(command.trim_start())
                        || matches_process(command.trim_start()))
            })
    }))
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
    let old = read_optional(&path)?;
    let doc = parse(old.as_deref())?;
    if !remove {
        if let Some(cmd) = doc
            .get("mcp_servers")
            .and_then(|v| v.get("cito"))
            .and_then(|v| v.get("command"))
            .and_then(Item::as_str)
        {
            if cmd != command
                && cmd != "cito-mcp"
                && !cmd.ends_with("/cito-mcp")
                && req["force"] != true
            {
                return Ok(
                    json!({"ok":true,"actions":[{"target":"codex","path":path,"status":"conflict","backupPath":null,"detail":"Conflicting MCP command; pass --force to replace"}]}),
                );
            }
        }
    }
    let action = mutate(req, "codex", &path, |old| {
        let mut d = parse(old)?;
        if !remove && req["force"] != true {
            if let Some(cmd) = d
                .get("mcp_servers")
                .and_then(|v| v.get("cito"))
                .and_then(|v| v.get("command"))
                .and_then(Item::as_str)
            {
                if cmd != command && cmd != "cito-mcp" && !cmd.ends_with("/cito-mcp") {
                    return Err("Conflicting MCP command; pass --force to replace".into());
                }
            }
        }
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
fn row_for_id(req: &Value, id: &str) -> Result<Option<Value>> {
    let path = db(req);
    if !path.try_exists().map_err(|e| e.to_string())? {
        return Ok(None);
    }
    if sqlite_rows(&path, "PRAGMA table_info(threads)")?.is_empty() {
        return Ok(None);
    }
    Ok(sqlite_rows(
        &path,
        &format!(
            "SELECT * FROM threads WHERE id = '{}'",
            id.replace('\'', "''")
        ),
    )?
    .into_iter()
    .next())
}
fn try_rows(req: &Value) -> Result<Vec<Value>> {
    let path = db(req);
    if !path.try_exists().map_err(|e| e.to_string())? {
        return Ok(vec![]);
    }
    let columns = sqlite_rows(&path, "PRAGMA table_info(threads)")?;
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
    ]
    .into_iter()
    .filter(|column| has(column))
    .collect::<Vec<_>>()
    .join(", ");
    let mut predicates = vec!["id IS NOT NULL", "id != ''", "cwd IS NOT NULL", "cwd != ''"];
    if has("archived") && req["cli_cmd"] != "unlisten" {
        predicates.push("archived = 0");
    }
    if has("thread_source") {
        predicates.push("(thread_source IS NULL OR thread_source = '' OR thread_source = 'user')");
    }
    if has("source") {
        predicates.push("(source IS NULL OR source NOT LIKE '{\"subagent\"%')");
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
    Ok(sqlite_rows(
        &path,
        &format!(
            "SELECT {projection} FROM threads WHERE {} ORDER BY {}{}",
            predicates.join(" AND "),
            order.join(", "),
            if req["op"] == "discover_sessions" {
                " LIMIT 32"
            } else {
                ""
            }
        ),
    )?
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
    if let Some(s) = str_at(req, "explicit_session_id") {
        ids.push(s.into())
    }
    let public = json!({"meta":req["meta"],"_meta":req["_meta"]});
    if explicit_pid(&public)?.is_none() && req["meta"]["expected_harness"] == "codex" {
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
    caller("codex", str_at(r, "id").unwrap_or(""), cwd, pid(req), title)
}
fn command_exit(mut command: Command, deadline: Instant) -> Result<Option<bool>> {
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status.success())),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(5));
    }
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
                    .take(8)
                    .map(|r| {
                        let mut c = row_caller(req, r);
                        c["updated_at_ms"] = json!(str_at(r, "recency_at_ms")
                            .or_else(|| str_at(r, "updated_at"))
                            .and_then(|s| s.parse::<u64>().ok())
                            .map(|t| if t < 100_000_000_000 { t * 1000 } else { t }));
                        c
                    })
                    .collect::<Vec<_>>()
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
            let public = json!({"meta":req["meta"],"_meta":req["_meta"]});
            if explicit_pid(&public)?.is_some() {
                return Ok(error(
                    if explicit.is_some() {
                        "INVALID_INPUT"
                    } else {
                        "BINDING_REQUIRED"
                    },
                    "Conflicting caller metadata: explicit bridge PID cannot claim a Codex thread",
                ));
            }
            let all = try_rows(req)?;
            let known_row = explicit
                .as_deref()
                .map(|id| row_for_id(req, id))
                .transpose()?
                .flatten();
            let needs_cached_pid = (explicit.is_some() && known_row.is_none())
                || (explicit.is_none() && (str_at(req, "cli_cmd").is_none() || all.is_empty()));
            if needs_cached_pid {
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
            if let Err(e) = verify(&c, req) {
                return Ok(error("INVALID_INPUT", e));
            }
            Ok(json!({"ok":true,"caller":remember_process(c)}))
        }
        "probe" => {
            let id = str_at(req, "session_id").unwrap_or("");
            let archived = sqlite_rows(
                &db(req),
                &format!(
                    "SELECT archived FROM threads WHERE id = '{}'",
                    id.replace('\'', "''")
                ),
            )
            .ok()
            .and_then(|r| r.first().cloned())
            .and_then(|r| r["archived"].as_str().map(|s| s != "0"));
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
                command_exit(command, Instant::now() + timeout)
            };
            let already_queued = req["queue_written"] == true;
            let queue_confirmed = if already_queued {
                req["queue_confirmed"] == true
            } else {
                let Some(bin) = binary(req) else {
                    return Ok(
                        json!({"ok":true,"outcome":"driver_unavailable","queue_written":false,"reason":"Codex binary was not found"}),
                    );
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
            if !already_queued && req["checkpoint_only"] == true {
                // Return the insertion checkpoint before requesting a separately
                // fenced open. The host rechecks unreadness and binding ownership.
                return Ok(
                    json!({"ok":true,"outcome":"delivered_unconfirmed","queue_written":true,"queue_confirmed":queue_confirmed,"insertion_unknown":!queue_confirmed,"open_confirmed":false,"continue_wake":true}),
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
                json!({"ok":true,"outcome":if !already_queued && queue_confirmed && opened {"delivered"}else{"delivered_unconfirmed"},"queue_written":true,"queue_confirmed":queue_confirmed,"insertion_unknown":!queue_confirmed,"open_confirmed":opened,"reason":if opened{None}else{Some("Codex open effect was not confirmed")}}),
            )
        }
        _ => Ok(error("INVALID_INPUT", "Unknown plugin operation")),
    }
}
