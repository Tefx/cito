mod daemon;
mod hub;
mod plugins;
mod setup;

use plugins::{
    ensure_daemon, http_post_mcp, load_native_config, now_ms, random_uuid_v4, LoadedPlugins,
    NativeConfig, CITO_VERSION,
};
use serde_json::{json, Map, Value};
use std::collections::{HashSet, VecDeque};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

const DRAIN_SUFFIX: &str = ".draining-";
const TITLE_TAIL_BYTES: u64 = 128 * 1024;
const TITLE_MAX_CHARS: usize = 200;
const TITLE_CACHE_ENTRIES: usize = 32;
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_TRACKED_BATCHES: usize = 200;

pub fn is_valid_transcript_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn validate_session_id(raw: &str, display_name: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let bytes = trimmed.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return Err(format!("Invalid {display_name} session identifier"));
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return Err(format!("Invalid {display_name} session identifier"));
    }
    if !bytes
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(format!("Invalid {display_name} session identifier"));
    }
    Ok(trimmed.to_string())
}

fn transcript_project_dir(cwd: &str, transcript_home: &Path) -> PathBuf {
    let slug: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    transcript_home.join("projects").join(slug)
}

fn file_mtime_ms(meta: &fs::Metadata) -> f64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

pub fn resolve_transcript_session_id(cwd: &str, transcript_home: &Path) -> Option<String> {
    let dir = transcript_project_dir(cwd, transcript_home);
    let entries = fs::read_dir(dir).ok()?;
    let mut newest: Option<(String, f64)> = None;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = match file_name.to_str() {
            Some(s) => s,
            None => continue,
        };
        let id = match name.strip_suffix(".jsonl") {
            Some(s) if is_valid_transcript_uuid(s) => s,
            _ => continue,
        };
        if let Ok(meta) = fs::metadata(entry.path()) {
            if meta.is_file() {
                let mtime = file_mtime_ms(&meta);
                if newest.as_ref().map(|(_, best)| mtime > *best).unwrap_or(true) {
                    newest = Some((id.to_string(), mtime));
                }
            }
        }
    }
    newest.map(|(id, _)| id)
}

type TitleCache = VecDeque<(PathBuf, u64, u64, Option<String>)>;

fn resolve_transcript_session_title(
    cwd: &str,
    session_id: &str,
    transcript_home: &Path,
    cache: &mut TitleCache,
) -> Option<String> {
    if !is_valid_transcript_uuid(session_id) {
        return None;
    }
    let path = transcript_project_dir(cwd, transcript_home).join(format!("{session_id}.jsonl"));
    let meta = fs::metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let size = meta.len();
    let mtime_ms = file_mtime_ms(&meta).round() as u64;

    if let Some((_, c_size, c_mtime, c_title)) = cache.iter().find(|(p, _, _, _)| p == &path) {
        if *c_size == size && *c_mtime == mtime_ms {
            return c_title.clone();
        }
    }

    let start = size.saturating_sub(TITLE_TAIL_BYTES);
    let mut file = File::open(&path).ok()?;
    if start > 0 {
        file.seek(SeekFrom::Start(start)).ok()?;
    }
    let to_read = (size - start) as usize;
    let mut buf = vec![0u8; to_read];
    let n = file.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }

    let mut title: Option<String> = None;
    for line in lines.iter().rev() {
        if !line.contains("\"custom-title\"") {
            continue;
        }
        if let Ok(Value::Object(row)) = serde_json::from_str::<Value>(line) {
            if row.get("type").and_then(|v| v.as_str()) == Some("custom-title") {
                if let Some(raw_title) = row.get("customTitle").and_then(|v| v.as_str()) {
                    if !raw_title.trim().is_empty() {
                        let cleaned_spaces: String = raw_title
                            .chars()
                            .map(|c| {
                                let code = c as u32;
                                if code <= 0x1f || code == 0x7f {
                                    ' '
                                } else {
                                    c
                                }
                            })
                            .collect();
                        let trimmed = cleaned_spaces.trim();
                        if !trimmed.is_empty() {
                            title = Some(trimmed.chars().take(TITLE_MAX_CHARS).collect());
                            break;
                        }
                    }
                }
            }
        }
    }

    if title.is_none() && start > 0 {
        if let Some((_, c_size, _, c_title)) = cache.iter().find(|(p, _, _, _)| p == &path) {
            if size > *c_size {
                title = c_title.clone();
            }
        }
    }

    if let Some(pos) = cache.iter().position(|(p, _, _, _)| p == &path) {
        cache.remove(pos);
    } else if cache.len() >= TITLE_CACHE_ENTRIES {
        cache.pop_front();
    }
    cache.push_back((path, size, mtime_ms, title.clone()));
    title
}

fn resolve_transcript_mtime_ms(cwd: &str, session_id: &str, transcript_home: &Path) -> u64 {
    if !is_valid_transcript_uuid(session_id) {
        return 0;
    }
    let path = transcript_project_dir(cwd, transcript_home).join(format!("{session_id}.jsonl"));
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => file_mtime_ms(&meta).round() as u64,
        _ => 0,
    }
}

#[derive(Clone, Debug)]
struct QueuedWake {
    batch_id: String,
    content: String,
    wake: bool,
}

fn wakes_dir(state_dir: &Path, plugin_id: &str) -> PathBuf {
    state_dir.join(plugin_id).join("wakes")
}

fn wake_queue_path(state_dir: &Path, plugin_id: &str, display_name: &str, session_id: &str) -> Result<PathBuf, String> {
    let valid = validate_session_id(session_id, display_name)?;
    Ok(wakes_dir(state_dir, plugin_id).join(format!("{valid}.jsonl")))
}

fn badge_path(state_dir: &Path, plugin_id: &str, display_name: &str, session_id: &str) -> Result<PathBuf, String> {
    let valid = validate_session_id(session_id, display_name)?;
    Ok(state_dir.join(plugin_id).join("badges").join(valid))
}

fn alias_path(state_dir: &Path, plugin_id: &str, display_name: &str, session_id: &str) -> Result<PathBuf, String> {
    let valid = validate_session_id(session_id, display_name)?;
    Ok(state_dir.join(plugin_id).join("aliases").join(valid))
}

fn write_session_alias(
    state_dir: &Path,
    plugin_id: &str,
    display_name: &str,
    session_id: &str,
    target_session_id: &str,
) -> Result<(), String> {
    let path = alias_path(state_dir, plugin_id, display_name, session_id)?;
    let target = validate_session_id(target_session_id, display_name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    fs::write(&path, format!("{target}\n")).map_err(|e| e.to_string())?;
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    Ok(())
}

fn read_session_alias(state_dir: &Path, plugin_id: &str, display_name: &str, session_id: &str) -> Option<String> {
    let path = alias_path(state_dir, plugin_id, display_name, session_id).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    let target = validate_session_id(&raw, display_name).ok()?;
    let prefix = format!("{plugin_id}-");
    if target.starts_with(&prefix) && target != session_id {
        Some(target)
    } else {
        None
    }
}

fn append_wake(
    state_dir: &Path,
    plugin_id: &str,
    display_name: &str,
    session_id: &str,
    batch_id: &str,
    content: &str,
    wake: bool,
    created_at_ms: u64,
) -> Result<(), String> {
    let path = wake_queue_path(state_dir, plugin_id, display_name, session_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    let line = serde_json::to_string(&json!({
        "batch_id": batch_id,
        "content": content,
        "wake": wake,
        "created_at_ms": created_at_ms,
    }))
    .map_err(|e| e.to_string())?;
    file.write_all(format!("{line}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}

fn parse_wakes(text: &str) -> Vec<QueuedWake> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(Value::Object(row)) = serde_json::from_str::<Value>(line) {
            let batch_id = match row.get("batch_id").and_then(|v| v.as_str()) {
                Some(s) if !s.trim().is_empty() => s.to_string(),
                _ => continue,
            };
            let content = match row.get("content").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let wake = row.get("wake").and_then(|v| v.as_bool()) != Some(false);
            out.push(QueuedWake {
                batch_id,
                content,
                wake,
            });
        }
    }
    out
}

fn drain_wakes(
    state_dir: &Path,
    plugin_id: &str,
    display_name: &str,
    session_id: &str,
) -> Result<Vec<QueuedWake>, String> {
    let valid_id = validate_session_id(session_id, display_name)?;
    let alias = read_session_alias(state_dir, plugin_id, display_name, &valid_id);
    let sessions = match alias {
        Some(a) => vec![a, valid_id],
        None => vec![valid_id],
    };
    let dir = wakes_dir(state_dir, plugin_id);
    for id in &sessions {
        let path = wake_queue_path(state_dir, plugin_id, display_name, id)?;
        let staged = PathBuf::from(format!("{}{}{}", path.display(), DRAIN_SUFFIX, random_uuid_v4()));
        match fs::rename(&path, &staged) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let entries = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let prefixes: Vec<String> = sessions
        .iter()
        .map(|id| format!("{id}.jsonl{DRAIN_SUFFIX}"))
        .collect();
    let mut staged_files: Vec<(PathBuf, f64, String)> = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = match file_name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if !prefixes.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let file_path = entry.path();
        if let (Ok(meta), Ok(text)) = (fs::metadata(&file_path), fs::read_to_string(&file_path)) {
            staged_files.push((file_path, file_mtime_ms(&meta), text));
        }
    }
    staged_files.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut wakes = Vec::new();
    let mut seen = HashSet::new();
    for (file_path, _, text) in &staged_files {
        for wake in parse_wakes(text) {
            if seen.insert(wake.batch_id.clone()) {
                wakes.push(wake);
            }
        }
        let _ = fs::remove_file(file_path);
    }
    Ok(wakes)
}

fn write_badge(
    state_dir: &Path,
    plugin_id: &str,
    display_name: &str,
    session_id: &str,
    badge: Option<&str>,
) -> Result<(), String> {
    let path = badge_path(state_dir, plugin_id, display_name, session_id)?;
    match badge {
        None => {
            let _ = fs::remove_file(path);
            Ok(())
        }
        Some(text) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
            fs::write(&path, format!("{text}\n")).map_err(|e| e.to_string())?;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            Ok(())
        }
    }
}

fn strip_badge_unread_count(state_dir: &Path, plugin_id: &str, display_name: &str, session_id: &str) {
    let path = match badge_path(state_dir, plugin_id, display_name, session_id) {
        Ok(p) => p,
        Err(_) => return,
    };
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let current = raw.trim_end_matches(&['\r', '\n'][..]);
    if let Some(open_paren) = current.rfind(" (") {
        if current.ends_with(')') {
            let digits = &current[open_paren + 2..current.len() - 1];
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                let stripped = &current[..open_paren];
                if !stripped.is_empty() && stripped != current {
                    let _ = write_badge(state_dir, plugin_id, display_name, session_id, Some(stripped));
                }
            }
        }
    }
}

fn run_transcript_hook(args: &[String]) {
    let (cfg_res, hook_cmd, plugin_id, display_name, session_env, meta_key) = match load_native_config() {
        Ok((cfg, plugins)) => {
            let tp = plugins.find_by_protocol("transcript-hook");
            let hc = tp
                .and_then(|p| p.native_str("hookCommand"))
                .unwrap_or("cito-hook")
                .to_string();
            let pid = tp.map(|p| p.id.clone()).unwrap_or_else(|| "transcript".to_string());
            let dname = tp
                .map(|p| p.display_name.clone())
                .unwrap_or_else(|| "Session".to_string());
            let senv = tp
                .and_then(|p| p.native_str("sessionEnv"))
                .unwrap_or("SESSION_ID")
                .to_string();
            let mkey = tp
                .and_then(|p| p.native_str("metaKey"))
                .unwrap_or("sessionId")
                .to_string();
            (Ok(cfg), hc, pid, dname, senv, mkey)
        }
        Err(e) => {
            let plugins = plugins::load_plugins(Path::new("/tmp"));
            let tp = plugins.find_by_protocol("transcript-hook");
            let hc = tp
                .and_then(|p| p.native_str("hookCommand"))
                .unwrap_or("cito-hook")
                .to_string();
            let pid = tp.map(|p| p.id.clone()).unwrap_or_else(|| "transcript".to_string());
            let dname = tp
                .map(|p| p.display_name.clone())
                .unwrap_or_else(|| "Session".to_string());
            let senv = tp
                .and_then(|p| p.native_str("sessionEnv"))
                .unwrap_or("SESSION_ID")
                .to_string();
            let mkey = tp
                .and_then(|p| p.native_str("metaKey"))
                .unwrap_or("sessionId")
                .to_string();
            (Err(e), hc, pid, dname, senv, mkey)
        }
    };

    if args.iter().any(|a| a == "--help" || a == "-h") {
        let usage = format!(
            "Usage: {hook_cmd} (reads a {display_name} Code hook payload on stdin)\n\
             Install on Stop, SessionStart and UserPromptSubmit to deliver queued Cito wakes.\n"
        );
        let _ = std::io::stderr().write_all(usage.as_bytes());
        return;
    }

    let emit_empty = || {
        let _ = std::io::stdout().write_all(b"{}\n");
        let _ = std::io::stdout().flush();
    };

    let result: Result<Option<Value>, String> = (|| {
        let mut raw_stdin = String::new();
        let is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
        if !is_tty {
            std::io::stdin()
                .read_to_string(&mut raw_stdin)
                .map_err(|e| e.to_string())?;
        }

        let input: Map<String, Value> = if raw_stdin.trim().is_empty() {
            Map::new()
        } else {
            match serde_json::from_str::<Value>(&raw_stdin).map_err(|e| e.to_string())? {
                Value::Object(map) => map,
                _ => return Err("Expected a hook payload object".to_string()),
            }
        };

        let event = input
            .get("hook_event_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| args.first().map(|s| s.trim().to_string()))
            .unwrap_or_default();

        if !matches!(event.as_str(), "Stop" | "SessionStart" | "UserPromptSubmit") {
            return Ok(None);
        }

        let config = cfg_res?;
        let cwd = input
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| {
                env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .to_string_lossy()
                    .to_string()
            });

        let explicit_raw = input
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(String::from)
            .or_else(|| {
                env::var(&session_env)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            });

        let explicit_session = match explicit_raw {
            Some(s) => Some(validate_session_id(&s, &display_name)?),
            None => None,
        };

        let w_dir = wakes_dir(&config.state_dir, &plugin_id);
        if !w_dir.exists() {
            return Ok(None);
        }

        let session = match explicit_session {
            Some(s) => s,
            None => match resolve_transcript_session_id(&cwd, &config.transcript_home) {
                Some(s) => s,
                None => return Ok(None),
            },
        };

        let wakes = drain_wakes(&config.state_dir, &plugin_id, &display_name, &session)?;
        if wakes.is_empty() {
            return Ok(None);
        }

        let alias = read_session_alias(&config.state_dir, &plugin_id, &display_name, &session);
        let ack_session = alias.as_deref().unwrap_or(&session);
        strip_badge_unread_count(&config.state_dir, &plugin_id, &display_name, &session);
        if let Some(ref a) = alias {
            strip_badge_unread_count(&config.state_dir, &plugin_id, &display_name, a);
        }

        let ack_ids: Vec<String> = wakes.iter().map(|w| w.batch_id.clone()).collect();
        let rpc_body = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "cito_inbox",
                "arguments": {
                    "ack_all": false,
                    "ack_ids": ack_ids,
                },
                "_meta": {
                    meta_key: ack_session,
                    "cwd": cwd,
                }
            }
        }))
        .unwrap_or_default();
        let _ = http_post_mcp(config.daemon_port, &rpc_body, Duration::from_millis(500));

        let combined = wakes
            .iter()
            .map(|w| w.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let any_wake = wakes.iter().any(|w| w.wake);
        let out = if event == "Stop" && any_wake {
            json!({
                "decision": "block",
                "reason": combined,
            })
        } else {
            json!({
                "hookSpecificOutput": {
                    "hookEventName": event,
                    "additionalContext": combined,
                }
            })
        };
        Ok(Some(out))
    })();

    match result {
        Ok(Some(val)) => {
            if let Ok(s) = serde_json::to_string(&val) {
                let _ = std::io::stdout().write_all(format!("{s}\n").as_bytes());
                let _ = std::io::stdout().flush();
            } else {
                emit_empty();
            }
        }
        Ok(None) => emit_empty(),
        Err(err) => {
            let _ = std::io::stderr().write_all(format!("{hook_cmd}: {err}\n").as_bytes());
            emit_empty();
        }
    }
}

struct BridgeCleanupState {
    socket_path: Option<PathBuf>,
    state_dir: PathBuf,
    plugin_id: String,
    display_name: String,
    badged_sessions: HashSet<String>,
}

static mut GLOBAL_CLEANUP: *const Mutex<BridgeCleanupState> = std::ptr::null();

fn perform_bridge_cleanup(cleanup: &Mutex<BridgeCleanupState>) {
    if let Ok(mut guard) = cleanup.lock() {
        if let Some(sock) = guard.socket_path.take() {
            let _ = fs::remove_file(sock);
        }
        let sessions: Vec<String> = guard.badged_sessions.drain().collect();
        let pid = guard.plugin_id.clone();
        let dname = guard.display_name.clone();
        for id in sessions {
            let _ = write_badge(&guard.state_dir, &pid, &dname, &id, None);
        }
    }
}

extern "C" fn handle_term_signal(_sig: libc::c_int) {
    unsafe {
        if !GLOBAL_CLEANUP.is_null() {
            perform_bridge_cleanup(&*GLOBAL_CLEANUP);
        }
        libc::_exit(0);
    }
}

fn probe_live_bridge(path: &Path) -> bool {
    let mut stream = match UnixStream::connect(path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let timeout = Duration::from_millis(500);
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    if stream.write_all(b"{\"op\":\"identify\"}\n").is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return false;
    }
    if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&line) {
        return obj.get("ok").and_then(|v| v.as_bool()) == Some(true);
    }
    false
}

struct ChannelRuntime {
    config: NativeConfig,
    plugin_id: String,
    display_name: String,
    notif_method: String,
    endpoint_pid: u32,
    cwd: String,
    explicit_env_id: Option<String>,
    fallback_id: String,
    use_fallback_alias: bool,
    first_transcript: Option<String>,
    last_session_id: Option<String>,
    last_active_ms: u64,
    delivered: VecDeque<String>,
    title_cache: TitleCache,
    stdout_lock: Arc<Mutex<()>>,
    cleanup: Arc<Mutex<BridgeCleanupState>>,
}

impl ChannelRuntime {
    fn note_session(&mut self, next_id: String) -> String {
        if self.last_session_id.is_some() && self.last_session_id.as_deref() != Some(&next_id) {
            self.last_active_ms = now_ms();
        }
        self.last_session_id = Some(next_id.clone());
        next_id
    }

    fn current_session_id(&mut self) -> Result<String, String> {
        if let Some(ref exp) = self.explicit_env_id.clone() {
            let valid = validate_session_id(exp, &self.display_name)?;
            return Ok(self.note_session(valid));
        }
        let resolved = resolve_transcript_session_id(&self.cwd, &self.config.transcript_home);
        let Some(resolved_id) = resolved else {
            return Ok(self.note_session(self.fallback_id.clone()));
        };
        if self.use_fallback_alias {
            if self.first_transcript.is_none() || self.first_transcript.as_deref() == Some(&resolved_id) {
                if self.first_transcript.is_none() {
                    self.last_active_ms = now_ms();
                }
                self.first_transcript = Some(resolved_id.clone());
                let _ = write_session_alias(
                    &self.config.state_dir,
                    &self.plugin_id,
                    &self.display_name,
                    &resolved_id,
                    &self.fallback_id,
                );
                return Ok(self.note_session(self.fallback_id.clone()));
            }
            self.use_fallback_alias = false;
        }
        Ok(self.note_session(resolved_id))
    }

    fn handle_request(&mut self, req: &Map<String, Value>) -> Value {
        let current = match self.current_session_id() {
            Ok(id) => id,
            Err(_) => return json!({ "ok": false, "error": "DELIVERY_FAILED" }),
        };
        let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("");
        if op == "identify" {
            let alias_active = self.use_fallback_alias && self.first_transcript.is_some();
            let transcript_id = if alias_active {
                self.first_transcript.clone().unwrap()
            } else {
                current.clone()
            };
            let title = resolve_transcript_session_title(
                &self.cwd,
                &transcript_id,
                &self.config.transcript_home,
                &mut self.title_cache,
            );
            let mtime_ms = resolve_transcript_mtime_ms(&self.cwd, &transcript_id, &self.config.transcript_home);
            let fallback_name = Path::new(&self.cwd)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("session")
                .to_string();
            let session_name = title.unwrap_or(fallback_name);
            let updated_at_ms = std::cmp::max(self.last_active_ms, mtime_ms);
            return json!({
                "ok": true,
                "endpoint_pid": self.endpoint_pid,
                "session_id": current,
                "session_name": session_name,
                "cwd": self.cwd,
                "updated_at_ms": updated_at_ms,
            });
        }

        if req.get("session_id").and_then(|v| v.as_str()) != Some(&current) {
            return json!({ "ok": false, "error": "SESSION_MISMATCH" });
        }

        if op == "status" {
            let badge_val = req.get("badge");
            let badge_str: Option<&str> = match badge_val {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.as_str()),
                _ => return json!({ "ok": false, "error": "INVALID_REQUEST" }),
            };
            if let Ok(mut guard) = self.cleanup.lock() {
                if badge_str.is_some() {
                    guard.badged_sessions.insert(current.clone());
                } else {
                    guard.badged_sessions.remove(&current);
                }
            }
            if write_badge(
                &self.config.state_dir,
                &self.plugin_id,
                &self.display_name,
                &current,
                badge_str,
            )
            .is_err()
            {
                return json!({ "ok": false, "error": "DELIVERY_FAILED" });
            }
            return json!({ "ok": true });
        }

        if op != "deliver" {
            return json!({ "ok": false, "error": "INVALID_REQUEST" });
        }

        let batch_id = match req.get("batch_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => return json!({ "ok": false, "error": "INVALID_REQUEST" }),
        };
        let content = match req.get("content").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return json!({ "ok": false, "error": "INVALID_REQUEST" }),
        };
        let wake = req.get("wake").and_then(|v| v.as_bool()) != Some(false);
        let deduplicated = self.delivered.iter().any(|b| b == batch_id);
        if !deduplicated {
            if append_wake(
                &self.config.state_dir,
                &self.plugin_id,
                &self.display_name,
                &current,
                batch_id,
                content,
                wake,
                now_ms(),
            )
            .is_err()
            {
                return json!({ "ok": false, "error": "DELIVERY_FAILED" });
            }
            self.delivered.push_back(batch_id.to_string());
            if self.delivered.len() > MAX_TRACKED_BATCHES {
                self.delivered.pop_front();
            }
            self.last_active_ms = now_ms();
            let notif = json!({
                "jsonrpc": "2.0",
                "method": self.notif_method,
                "params": {
                    "content": content,
                    "meta": {
                        "origin": "cito",
                        "batch_id": batch_id,
                    }
                }
            });
            if let Ok(line) = serde_json::to_string(&notif) {
                if let Ok(_guard) = self.stdout_lock.lock() {
                    let _ = std::io::stdout().write_all(format!("{line}\n").as_bytes());
                    let _ = std::io::stdout().flush();
                }
            }
        }
        json!({
            "ok": true,
            "delivered": true,
            "deduplicated": deduplicated,
        })
    }
}

fn start_transcript_channel_listener(
    config: &NativeConfig,
    plugins: &LoadedPlugins,
    endpoint_pid: u32,
    stdout_lock: Arc<Mutex<()>>,
    cleanup: Arc<Mutex<BridgeCleanupState>>,
) -> Option<String> {
    if !config.enable_transcript_channel {
        return None;
    }
    let tp = plugins.find_by_protocol("transcript-hook")?;
    let plugin_id = tp.id.clone();
    let display_name = tp.display_name.clone();
    let session_env = tp.native_str("sessionEnv").unwrap_or("SESSION_ID");
    let cap_key = tp.native_str("channelCapability").unwrap_or("channel").to_string();
    let notif_method = tp
        .native_str("channelNotificationMethod")
        .unwrap_or("notifications/channel")
        .to_string();

    let sockets_dir = config.state_dir.join("sockets");
    if fs::create_dir_all(&sockets_dir).is_err() {
        return None;
    }
    let _ = fs::set_permissions(&sockets_dir, fs::Permissions::from_mode(0o700));
    let socket_path = sockets_dir.join(format!("{plugin_id}-pid-{endpoint_pid}.sock"));

    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if probe_live_bridge(&socket_path) {
                let _ = std::io::stderr().write_all(
                    format!(
                        "cito-native: socket {} already in use by live bridge\n",
                        socket_path.display()
                    )
                    .as_bytes(),
                );
                return None;
            }
            let _ = fs::remove_file(&socket_path);
            match UnixListener::bind(&socket_path) {
                Ok(l) => l,
                Err(_) => return None,
            }
        }
        Err(_) => return None,
    };
    let _ = fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600));
    if let Ok(mut guard) = cleanup.lock() {
        guard.socket_path = Some(socket_path);
        guard.plugin_id = plugin_id.clone();
        guard.display_name = display_name.clone();
    }

    let cwd = env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .to_string_lossy()
        .to_string();
    let explicit_env_id = env::var(session_env)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let use_fallback_alias =
        explicit_env_id.is_none() && resolve_transcript_session_id(&cwd, &config.transcript_home).is_none();

    let mut runtime = ChannelRuntime {
        config: config.clone(),
        plugin_id: plugin_id.clone(),
        display_name,
        notif_method,
        endpoint_pid,
        cwd,
        explicit_env_id,
        fallback_id: format!("{plugin_id}-{endpoint_pid}"),
        use_fallback_alias,
        first_transcript: None,
        last_session_id: None,
        last_active_ms: now_ms(),
        delivered: VecDeque::new(),
        title_cache: VecDeque::new(),
        stdout_lock,
        cleanup,
    };

    thread::spawn(move || {
        for stream_res in listener.incoming() {
            let mut stream = match stream_res {
                Ok(s) => s,
                Err(_) => continue,
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(3000)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(3000)));
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            let mut overflow = false;
            while buf.len() <= MAX_REQUEST_BYTES {
                match stream.read(&mut byte) {
                    Ok(0) => break,
                    Ok(_) => {
                        if byte[0] == b'\n' {
                            break;
                        }
                        buf.push(byte[0]);
                    }
                    Err(_) => break,
                }
                if buf.len() > MAX_REQUEST_BYTES {
                    overflow = true;
                    break;
                }
            }
            if overflow || buf.is_empty() {
                continue;
            }
            let reply = match serde_json::from_slice::<Value>(&buf) {
                Ok(Value::Object(req)) => runtime.handle_request(&req),
                _ => json!({ "ok": false, "error": "DELIVERY_FAILED" }),
            };
            if let Ok(s) = serde_json::to_string(&reply) {
                let _ = stream.write_all(format!("{s}\n").as_bytes());
                let _ = stream.flush();
            }
        }
    });

    Some(cap_key)
}

fn enrich_initialize_result(mut msg: Value, channel_cap: Option<&str>) -> Value {
    let Some(cap_key) = channel_cap else {
        return msg;
    };
    if let Value::Object(ref mut root) = msg {
        if let Some(Value::Object(ref mut result)) = root.get_mut("result") {
            if result.contains_key("protocolVersion") {
                let caps = result
                    .entry("capabilities")
                    .or_insert_with(|| Value::Object(Map::new()));
                if !caps.is_object() {
                    *caps = Value::Object(Map::new());
                }
                if let Value::Object(ref mut caps_map) = caps {
                    let exp = caps_map
                        .entry("experimental")
                        .or_insert_with(|| Value::Object(Map::new()));
                    if !exp.is_object() {
                        *exp = Value::Object(Map::new());
                    }
                    if let Value::Object(ref mut exp_map) = exp {
                        exp_map.insert(cap_key.to_string(), json!({}));
                    }
                }
            }
        }
    }
    msg
}

pub fn extract_jsonrpc_from_http_body(body: &str) -> Option<Value> {
    let mut last_data: Option<&str> = None;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                last_data = Some(trimmed);
            }
        }
    }
    if let Some(json_str) = last_data {
        if let Ok(v) = serde_json::from_str::<Value>(json_str) {
            return Some(v);
        }
    }
    let trimmed = body.trim();
    if trimmed.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            return Some(v);
        }
    }
    None
}

fn run_mcp_stdio() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    let (config, plugins) = match load_native_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::io::stderr().write_all(format!("cito-mcp: {e}\n").as_bytes());
            std::process::exit(1);
        }
    };

    if let Err(e) = ensure_daemon(&config) {
        let _ = std::io::stderr().write_all(format!("cito-mcp: {e}\n").as_bytes());
        std::process::exit(1);
    }

    let endpoint_pid = env::var("CITO_ENDPOINT_PID")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&p| p > 0)
        .unwrap_or_else(|| unsafe { libc::getppid() as u32 });

    let stdout_lock = Arc::new(Mutex::new(()));
    let cleanup = Arc::new(Mutex::new(BridgeCleanupState {
        socket_path: None,
        state_dir: config.state_dir.clone(),
        plugin_id: String::new(),
        display_name: String::new(),
        badged_sessions: HashSet::new(),
    }));

    unsafe {
        GLOBAL_CLEANUP = Arc::as_ptr(&cleanup);
        libc::signal(libc::SIGINT, handle_term_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle_term_signal as *const () as libc::sighandler_t);
    }

    let channel_cap = start_transcript_channel_listener(
        &config,
        &plugins,
        endpoint_pid,
        stdout_lock.clone(),
        cleanup.clone(),
    );

    let stdin = std::io::stdin();
    let reader = BufReader::new(stdin.lock());
    for line_res in reader.lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let req_id = parsed.as_object().and_then(|o| o.get("id")).cloned();
        let has_id = req_id.is_some();

        if let Value::Object(ref mut obj) = parsed {
            if obj.get("method").and_then(|v| v.as_str()) == Some("tools/call") {
                if let Some(Value::Object(ref mut params)) = obj.get_mut("params") {
                    let meta = params
                        .entry("_meta")
                        .or_insert_with(|| Value::Object(Map::new()));
                    if !meta.is_object() {
                        *meta = Value::Object(Map::new());
                    }
                    if let Value::Object(ref mut meta_map) = meta {
                        meta_map
                            .entry("_stdio")
                            .or_insert_with(|| json!({ "endpoint_pid": endpoint_pid }));
                    }
                }
            }
        }
        let forward_body = serde_json::to_string(&parsed).unwrap_or_else(|_| trimmed.to_string());

        match http_post_mcp(config.daemon_port, &forward_body, Duration::from_secs(60)) {
            Ok(resp) => {
                if has_id {
                    if let Some(rpc_val) = extract_jsonrpc_from_http_body(&resp.body) {
                        let out_val = enrich_initialize_result(rpc_val, channel_cap.as_deref());
                        if let Ok(out_str) = serde_json::to_string(&out_val) {
                            if let Ok(_guard) = stdout_lock.lock() {
                                let _ = std::io::stdout().write_all(format!("{out_str}\n").as_bytes());
                                let _ = std::io::stdout().flush();
                            }
                        }
                    } else if resp.status >= 400 {
                        let err_val = json!({
                            "jsonrpc": "2.0",
                            "id": req_id.unwrap_or(Value::Null),
                            "error": {
                                "code": -32603,
                                "message": format!("HTTP {}", resp.status),
                            }
                        });
                        if let Ok(out_str) = serde_json::to_string(&err_val) {
                            if let Ok(_guard) = stdout_lock.lock() {
                                let _ = std::io::stdout().write_all(format!("{out_str}\n").as_bytes());
                                let _ = std::io::stdout().flush();
                            }
                        }
                    }
                }
            }
            Err(e) => {
                if has_id {
                    let err_val = json!({
                        "jsonrpc": "2.0",
                        "id": req_id.unwrap_or(Value::Null),
                        "error": {
                            "code": -32603,
                            "message": e,
                        }
                    });
                    if let Ok(out_str) = serde_json::to_string(&err_val) {
                        if let Ok(_guard) = stdout_lock.lock() {
                            let _ = std::io::stdout().write_all(format!("{out_str}\n").as_bytes());
                            let _ = std::io::stdout().flush();
                        }
                    }
                }
            }
        }
    }

    perform_bridge_cleanup(&cleanup);
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let argv0_name = args
        .first()
        .and_then(|s| Path::new(s).file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");

    if argv0_name.ends_with("-hook") {
        run_transcript_hook(&args[1..]);
        return;
    }

    match args.get(1).map(|s| s.as_str()) {
        Some("--version" | "-v") => {
            println!("cito v{CITO_VERSION}");
        }
        Some(sub) if sub == "hook" || sub.ends_with("-hook") => {
            run_transcript_hook(&args[2..]);
        }
        Some("setup") => {
            setup::run_setup_cli(&args[2..]);
        }
        Some("hub") => {
            hub::run_hub_cli(&args[2..]);
        }
        Some("daemon") => {
            let (config, plugins) = match load_native_config() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("cito daemon: {e}");
                    std::process::exit(1);
                }
            };
            daemon::run_daemon(&config, &plugins);
        }
        Some("listen" | "unlisten" | "find" | "send" | "inbox") => {
            let cmd = args[1].as_str();
            let (config, plugins) = match load_native_config() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
            daemon::run_cli_command(cmd, &args[2..], &config, &plugins);
        }
        Some("mcp-stdio") | None => {
            if argv0_name == "Cito" && args.get(1).is_none() {
                let (config, plugins) = match load_native_config() {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("cito daemon: {e}");
                        std::process::exit(1);
                    }
                };
                daemon::run_daemon(&config, &plugins);
                return;
            }
            run_mcp_stdio();
        }
        Some(other) => {
            let _ = std::io::stderr().write_all(format!("cito-native: unknown subcommand '{other}'\n").as_bytes());
            std::process::exit(1);
        }
    }
}
