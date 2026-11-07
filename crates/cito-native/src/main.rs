use serde_json::{json, Map, Value};
use std::collections::{HashSet, VecDeque};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CITO_VERSION: &str = "0.15.0";
const DRAIN_SUFFIX: &str = ".draining-";
const TITLE_TAIL_BYTES: u64 = 128 * 1024;
const TITLE_MAX_CHARS: usize = 200;
const TITLE_CACHE_ENTRIES: usize = 32;
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_TRACKED_BATCHES: usize = 200;

const HOOK_USAGE: &str = "Usage: cito-claude-hook (reads a Claude Code hook payload on stdin)\n\
Install on Stop, SessionStart and UserPromptSubmit to deliver queued Cito wakes.\n";

#[derive(Clone, Debug)]
struct NativeConfig {
    state_dir: PathBuf,
    daemon_port: u16,
    claude_home: PathBuf,
    enable_claude_channel: bool,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn random_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut f) = File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    } else {
        let t = now_ms().to_le_bytes();
        let p = std::process::id().to_le_bytes();
        bytes[0..8].copy_from_slice(&t);
        bytes[8..12].copy_from_slice(&p);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

fn system_home_dir() -> String {
    if let Ok(h) = env::var("HOME") {
        let t = h.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_dir.is_null() {
            if let Ok(s) = std::ffi::CStr::from_ptr((*pw).pw_dir).to_str() {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    "/tmp".to_string()
}

fn expand_home_dir(value: &str, name: &str, base_home: Option<&str>) -> Result<PathBuf, String> {
    if value.trim().is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    let home = match base_home.map(str::trim).filter(|s| !s.is_empty()) {
        Some(h) => h.to_string(),
        None => system_home_dir(),
    };
    let expanded = if value == "~" {
        PathBuf::from(&home)
    } else if let Some(rest) = value.strip_prefix("~/") {
        PathBuf::from(&home).join(rest)
    } else {
        PathBuf::from(value)
    };
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Ok(env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(expanded))
    }
}

fn is_checked_cito_env_key(key: &str) -> bool {
    if !key.starts_with("CITO_") {
        return false;
    }
    let rest = &key["CITO_".len()..];
    if matches!(rest, "STATE_DIR" | "CONFIG_PATH" | "OPEN_BIN") {
        return true;
    }
    if let Some(prefix) = rest.strip_suffix("_HOME").or_else(|| rest.strip_suffix("_BIN")) {
        return !prefix.is_empty()
            && prefix
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    }
    false
}

fn validate_hub_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    if !(bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()) {
        return false;
    }
    bytes
        .iter()
        .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn validate_nats_url(raw: &str) -> Result<(), String> {
    let trimmed = raw.trim();
    if trimmed == "local" || trimmed == "local://" {
        return Ok(());
    }
    let Some((scheme, rest)) = raw.split_once("://") else {
        return Err("CITO_NATS_URL must be a valid Hub, NATS, or WebSocket URL".to_string());
    };
    if !matches!(scheme, "http" | "https" | "nats" | "ws" | "wss") {
        return Err(
            "CITO_NATS_URL must use local://, http://, https://, nats://, ws:// or wss:// without credentials"
                .to_string(),
        );
    }
    if rest.is_empty() || rest.contains('@') || rest.contains('?') || rest.contains('#') {
        return Err(
            "CITO_NATS_URL must use local://, http://, https://, nats://, ws:// or wss:// without credentials"
                .to_string(),
        );
    }
    let host_port = rest.split('/').next().unwrap_or("");
    if host_port.is_empty() {
        return Err("CITO_NATS_URL must be a valid Hub, NATS, or WebSocket URL".to_string());
    }
    if let Some((_, port_str)) = host_port.rsplit_once(':') {
        if !port_str.is_empty() && !port_str.ends_with(']') {
            let p = port_str
                .parse::<u32>()
                .map_err(|_| "CITO_NATS_URL must be a valid Hub, NATS, or WebSocket URL".to_string())?;
            if !(1..=65535).contains(&p) {
                return Err("CITO_NATS_URL must be a valid Hub, NATS, or WebSocket URL".to_string());
            }
        }
    }
    Ok(())
}

fn validate_https_issuer(raw: &str) -> Result<(), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("CITO_OIDC_ISSUER is required in oidc mode".to_string());
    }
    let is_https = trimmed.starts_with("https://");
    let is_loopback_http =
        trimmed.starts_with("http://127.0.0.1:") || trimmed == "http://127.0.0.1" || trimmed.starts_with("http://127.0.0.1/");
    if (!is_https && !is_loopback_http) || trimmed.contains('?') || trimmed.contains('#') || trimmed.contains('@') {
        return Err(
            "CITO_OIDC_ISSUER must use HTTPS (http://127.0.0.1 is allowed for tests), without credentials, query or fragment"
                .to_string(),
        );
    }
    Ok(())
}

fn load_native_config() -> Result<NativeConfig, String> {
    for (k, v) in env::vars() {
        if is_checked_cito_env_key(&k) && v.trim().is_empty() {
            return Err(format!("{k} must not be empty"));
        }
    }
    if let Ok(redir) = env::var("CITO_OIDC_REDIRECT_PORT") {
        let p = redir
            .trim()
            .parse::<u32>()
            .map_err(|_| "CITO_OIDC_REDIRECT_PORT must be an integer in 1..65535".to_string())?;
        if !(1..=65535).contains(&p) {
            return Err("CITO_OIDC_REDIRECT_PORT must be an integer in 1..65535".to_string());
        }
    }
    if let Ok(user) = env::var("CITO_USER") {
        if user.is_empty()
            || user.len() > 64
            || !user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err("CITO_USER must contain 1-64 letters, numbers, dots, underscores or hyphens".to_string());
        }
    }
    if let Ok(hub_env) = env::var("CITO_HUB") {
        let h = hub_env.trim();
        if !h.is_empty() && !validate_hub_name(h) {
            return Err("default_hub must be a valid hub name".to_string());
        }
    }
    if let Ok(url) = env::var("CITO_HUB_URL").or_else(|_| env::var("CITO_NATS_URL")) {
        validate_nats_url(&url)?;
    }
    let auth_mode = env::var("CITO_AUTH_MODE").unwrap_or_else(|_| "none".to_string());
    if !matches!(auth_mode.as_str(), "none" | "token" | "oidc") {
        return Err("CITO_AUTH_MODE must be none, token, or oidc".to_string());
    }
    if auth_mode == "token" {
        let tok = env::var("CITO_HUB_TOKEN").unwrap_or_default();
        if tok.trim().is_empty() {
            return Err("CITO_HUB_TOKEN is required in token mode".to_string());
        }
    }
    let issuer_env = env::var("CITO_OIDC_ISSUER").ok();
    let client_env = env::var("CITO_OIDC_CLIENT_ID").ok();
    if let Some(ref iss) = issuer_env {
        validate_https_issuer(iss)?;
    }
    if auth_mode == "oidc" && issuer_env.as_deref().unwrap_or("").trim().is_empty() {
        return Err("CITO_OIDC_ISSUER is required in oidc mode".to_string());
    }
    if (auth_mode == "oidc" || issuer_env.is_some()) && client_env.as_deref().unwrap_or("").trim().is_empty() {
        return Err("CITO_OIDC_CLIENT_ID is required with an OIDC issuer".to_string());
    }
    if let Some(ref cid) = client_env {
        if cid.is_empty() || cid != cid.trim() {
            return Err("CITO_OIDC_CLIENT_ID must be a non-empty trimmed string".to_string());
        }
    }

    let home_env = env::var("HOME").ok();
    let raw_state = env::var("CITO_STATE_DIR").unwrap_or_else(|_| "~/.cito".to_string());
    let state_dir = expand_home_dir(&raw_state, "CITO_STATE_DIR", home_env.as_deref())?;
    let explicit_config = env::var("CITO_CONFIG_PATH").ok();
    let config_path = match explicit_config {
        Some(ref p) => expand_home_dir(p, "CITO_CONFIG_PATH", home_env.as_deref())?,
        None => state_dir.join("config.json"),
    };

    let mut cfg_daemon_port: Option<u16> = None;
    let mut cfg_claude_home: Option<String> = None;
    let mut cfg_enable_channel: Option<bool> = None;

    if explicit_config.is_some() || config_path.exists() {
        let raw = fs::read_to_string(&config_path)
            .map_err(|_| format!("Cannot load hub configuration: {}", config_path.display()))?;
        let Value::Object(map) = serde_json::from_str::<Value>(&raw)
            .map_err(|_| format!("Cannot load hub configuration: {}", config_path.display()))?
        else {
            return Err(format!("Cannot load hub configuration: {}", config_path.display()));
        };
        let Some(Value::Object(hubs_map)) = map.get("hubs") else {
            return Err(format!("Cannot load hub configuration: {}", config_path.display()));
        };
        if hubs_map.is_empty() {
            return Err(format!("Cannot load hub configuration: {}", config_path.display()));
        }
        if let Some(p) = map.get("daemonPort").and_then(|v| v.as_u64()) {
            if (1..=65535).contains(&p) {
                cfg_daemon_port = Some(p as u16);
            }
        }
        if let Some(plugins) = map.get("plugins").and_then(|v| v.as_object()) {
            if let Some(claude) = plugins.get("claude").and_then(|v| v.as_object()) {
                if let Some(ch) = claude.get("claudeHome").and_then(|v| v.as_str()) {
                    cfg_claude_home = Some(ch.to_string());
                }
                if let Some(en) = claude.get("enableClaudeChannel").and_then(|v| v.as_bool()) {
                    cfg_enable_channel = Some(en);
                }
            }
        }
    }

    let daemon_port = match env::var("CITO_DAEMON_PORT") {
        Ok(val) => {
            let parsed = val
                .trim()
                .parse::<u32>()
                .map_err(|_| "CITO_DAEMON_PORT must be an integer in 1..65535".to_string())?;
            if !(1..=65535).contains(&parsed) {
                return Err("CITO_DAEMON_PORT must be an integer in 1..65535".to_string());
            }
            parsed as u16
        }
        Err(_) => cfg_daemon_port.unwrap_or(4290),
    };

    let raw_claude_home = cfg_claude_home
        .or_else(|| env::var("CITO_CLAUDE_HOME").ok())
        .unwrap_or_else(|| "~/.claude".to_string());
    let claude_home = expand_home_dir(&raw_claude_home, "CITO_CLAUDE_HOME", home_env.as_deref())?;

    let enable_claude_channel = cfg_enable_channel.unwrap_or_else(|| {
        env::var("CITO_CLAUDE_CHANNEL")
            .ok()
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false)
    });

    Ok(NativeConfig {
        state_dir,
        daemon_port,
        claude_home,
        enable_claude_channel,
    })
}

fn is_valid_transcript_uuid(s: &str) -> bool {
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

fn validate_claude_session_id(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let bytes = trimmed.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return Err("Invalid Claude session identifier".to_string());
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return Err("Invalid Claude session identifier".to_string());
    }
    if !bytes
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("Invalid Claude session identifier".to_string());
    }
    Ok(trimmed.to_string())
}

fn claude_project_dir(cwd: &str, claude_home: &Path) -> PathBuf {
    let slug: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    claude_home.join("projects").join(slug)
}

fn file_mtime_ms(meta: &fs::Metadata) -> f64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

fn resolve_claude_session_id(cwd: &str, claude_home: &Path) -> Option<String> {
    let dir = claude_project_dir(cwd, claude_home);
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

fn resolve_claude_session_title(
    cwd: &str,
    session_id: &str,
    claude_home: &Path,
    cache: &mut TitleCache,
) -> Option<String> {
    if !is_valid_transcript_uuid(session_id) {
        return None;
    }
    let path = claude_project_dir(cwd, claude_home).join(format!("{session_id}.jsonl"));
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

fn resolve_claude_transcript_mtime_ms(cwd: &str, session_id: &str, claude_home: &Path) -> u64 {
    if !is_valid_transcript_uuid(session_id) {
        return 0;
    }
    let path = claude_project_dir(cwd, claude_home).join(format!("{session_id}.jsonl"));
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => file_mtime_ms(&meta).round() as u64,
        _ => 0,
    }
}

#[derive(Clone, Debug)]
struct ClaudeWake {
    batch_id: String,
    content: String,
    wake: bool,
}

fn claude_wakes_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("claude").join("wakes")
}

fn claude_wake_queue_path(state_dir: &Path, session_id: &str) -> Result<PathBuf, String> {
    let valid = validate_claude_session_id(session_id)?;
    Ok(claude_wakes_dir(state_dir).join(format!("{valid}.jsonl")))
}

fn claude_badge_path(state_dir: &Path, session_id: &str) -> Result<PathBuf, String> {
    let valid = validate_claude_session_id(session_id)?;
    Ok(state_dir.join("claude").join("badges").join(valid))
}

fn claude_alias_path(state_dir: &Path, session_id: &str) -> Result<PathBuf, String> {
    let valid = validate_claude_session_id(session_id)?;
    Ok(state_dir.join("claude").join("aliases").join(valid))
}

fn write_claude_session_alias(state_dir: &Path, session_id: &str, target_session_id: &str) -> Result<(), String> {
    let path = claude_alias_path(state_dir, session_id)?;
    let target = validate_claude_session_id(target_session_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    fs::write(&path, format!("{target}\n")).map_err(|e| e.to_string())?;
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    Ok(())
}

fn read_claude_session_alias(state_dir: &Path, session_id: &str) -> Option<String> {
    let path = claude_alias_path(state_dir, session_id).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    let target = validate_claude_session_id(&raw).ok()?;
    if target.starts_with("claude-") && target != session_id {
        Some(target)
    } else {
        None
    }
}

fn append_claude_wake(
    state_dir: &Path,
    session_id: &str,
    batch_id: &str,
    content: &str,
    wake: bool,
    created_at_ms: u64,
) -> Result<(), String> {
    let path = claude_wake_queue_path(state_dir, session_id)?;
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

fn parse_wakes(text: &str) -> Vec<ClaudeWake> {
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
            out.push(ClaudeWake {
                batch_id,
                content,
                wake,
            });
        }
    }
    out
}

fn drain_claude_wakes(state_dir: &Path, session_id: &str) -> Result<Vec<ClaudeWake>, String> {
    let valid_id = validate_claude_session_id(session_id)?;
    let alias = read_claude_session_alias(state_dir, &valid_id);
    let sessions = match alias {
        Some(a) => vec![a, valid_id],
        None => vec![valid_id],
    };
    let dir = claude_wakes_dir(state_dir);
    for id in &sessions {
        let path = claude_wake_queue_path(state_dir, id)?;
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

fn write_claude_badge(state_dir: &Path, session_id: &str, badge: Option<&str>) -> Result<(), String> {
    let path = claude_badge_path(state_dir, session_id)?;
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

fn strip_badge_unread_count(state_dir: &Path, session_id: &str) {
    let path = match claude_badge_path(state_dir, session_id) {
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
                    let _ = write_claude_badge(state_dir, session_id, Some(stripped));
                }
            }
        }
    }
}

struct HttpResponse {
    status: u16,
    body: String,
}

fn read_http_response(stream: &mut TcpStream) -> Result<HttpResponse, String> {
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    let n = reader.read_line(&mut status_line).map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("Empty HTTP response".to_string());
    }
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("Invalid HTTP status: {}", status_line.trim()))?;

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_ascii_lowercase();
            if key == "content-length" {
                content_length = val.parse::<usize>().ok();
            } else if key == "transfer-encoding" && val.contains("chunked") {
                chunked = true;
            }
        }
    }

    if status == 202 || status == 204 {
        return Ok(HttpResponse {
            status,
            body: String::new(),
        });
    }

    let mut body_bytes = Vec::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            let r = reader.read_line(&mut size_line).map_err(|e| e.to_string())?;
            if r == 0 {
                break;
            }
            let size_hex = size_line.trim().split(';').next().unwrap_or("");
            let chunk_size = usize::from_str_radix(size_hex, 16).unwrap_or(0);
            if chunk_size == 0 {
                let mut trailer = String::new();
                let _ = reader.read_line(&mut trailer);
                break;
            }
            let mut chunk = vec![0u8; chunk_size];
            reader.read_exact(&mut chunk).map_err(|e| e.to_string())?;
            body_bytes.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            let _ = reader.read_exact(&mut crlf);
        }
    } else if let Some(len) = content_length {
        body_bytes.resize(len, 0);
        reader.read_exact(&mut body_bytes).map_err(|e| e.to_string())?;
    } else {
        let _ = reader.read_to_end(&mut body_bytes);
    }

    Ok(HttpResponse {
        status,
        body: String::from_utf8_lossy(&body_bytes).to_string(),
    })
}

fn http_post_mcp(port: u16, body: &str, timeout: Duration) -> Result<HttpResponse, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let req = format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n\
         Connection: close\r\n\
         Content-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;
    read_http_response(&mut stream)
}

#[derive(Debug)]
struct DaemonHealth {
    pid: i32,
    version: Option<String>,
}

fn probe_daemon_health(config: &NativeConfig) -> Option<DaemonHealth> {
    let addr = SocketAddr::from(([127, 0, 0, 1], config.daemon_port));
    let timeout = Duration::from_millis(500);
    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let req = format!(
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        config.daemon_port
    );
    stream.write_all(req.as_bytes()).ok()?;
    let resp = read_http_response(&mut stream).ok()?;
    if resp.status != 200 {
        return None;
    }
    let val: Value = serde_json::from_str(&resp.body).ok()?;
    let obj = val.as_object()?;
    if obj.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let state_dir = obj.get("stateDir").and_then(|v| v.as_str())?;
    if Path::new(state_dir) != config.state_dir {
        return None;
    }
    let pid = obj.get("pid").and_then(|v| v.as_i64())?;
    if pid <= 0 {
        return None;
    }
    let version = obj.get("version").and_then(|v| v.as_str()).map(String::from);
    Some(DaemonHealth {
        pid: pid as i32,
        version,
    })
}

fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

fn resolve_node_bin() -> String {
    if let Ok(b) = env::var("CITO_NODE_BIN") {
        let t = b.trim();
        if !t.is_empty() && Path::new(t).exists() {
            return t.to_string();
        }
    }
    if let Ok(path_var) = env::var("PATH") {
        for dir in path_var.split(':') {
            if dir.is_empty() {
                continue;
            }
            let cand = Path::new(dir).join("node");
            if cand.is_file() {
                return cand.to_string_lossy().to_string();
            }
        }
    }
    for cand in ["/opt/homebrew/bin/node", "/usr/local/bin/node"] {
        if Path::new(cand).is_file() {
            return cand.to_string();
        }
    }
    "node".to_string()
}

fn resolve_cito_mcp_entry() -> Result<PathBuf, String> {
    if let Ok(entry) = env::var("CITO_MCP_ENTRY") {
        let t = entry.trim();
        if !t.is_empty() && Path::new(t).exists() {
            return Ok(PathBuf::from(t));
        }
    }
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let exe_dir = exe.parent().ok_or_else(|| "Cannot resolve binary parent dir".to_string())?;
    let sibling = exe_dir.join("cito-mcp");
    if sibling.exists() {
        return Ok(sibling);
    }
    Err("Cannot locate bin/cito-mcp for daemon startup".to_string())
}

fn ensure_daemon(config: &NativeConfig) -> Result<(), String> {
    if let Some(existing) = probe_daemon_health(config) {
        if existing.version.as_deref() == Some(CITO_VERSION) {
            return Ok(());
        }
        unsafe {
            libc::kill(existing.pid, libc::SIGTERM);
        }
        let stop_deadline = Instant::now() + Duration::from_millis(5000);
        while pid_alive(existing.pid) && Instant::now() < stop_deadline {
            thread::sleep(Duration::from_millis(50));
        }
    }

    let node_bin = resolve_node_bin();
    let entry = resolve_cito_mcp_entry()?;
    let mut cmd = Command::new(node_bin);
    cmd.arg(entry)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| format!("Failed to spawn daemon: {e}"))?;

    let deadline = Instant::now() + Duration::from_millis(5000);
    while Instant::now() < deadline {
        if let Some(h) = probe_daemon_health(config) {
            if h.version.as_deref() == Some(CITO_VERSION) {
                return Ok(());
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "Daemon did not become healthy on 127.0.0.1:{} for {} within 5000ms",
        config.daemon_port,
        config.state_dir.display()
    ))
}

fn run_claude_hook(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        let _ = std::io::stderr().write_all(HOOK_USAGE.as_bytes());
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

        let config = load_native_config()?;
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
                env::var("CLAUDE_SESSION_ID")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            });

        let explicit_session = match explicit_raw {
            Some(s) => Some(validate_claude_session_id(&s)?),
            None => None,
        };

        let wakes_dir = claude_wakes_dir(&config.state_dir);
        if !wakes_dir.exists() {
            return Ok(None);
        }

        let session = match explicit_session {
            Some(s) => s,
            None => match resolve_claude_session_id(&cwd, &config.claude_home) {
                Some(s) => s,
                None => return Ok(None),
            },
        };

        let wakes = drain_claude_wakes(&config.state_dir, &session)?;
        if wakes.is_empty() {
            return Ok(None);
        }

        let alias = read_claude_session_alias(&config.state_dir, &session);
        let ack_session = alias.as_deref().unwrap_or(&session);
        strip_badge_unread_count(&config.state_dir, &session);
        if let Some(ref a) = alias {
            strip_badge_unread_count(&config.state_dir, a);
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
                    "claudeSessionId": ack_session,
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
            let _ = std::io::stderr().write_all(format!("cito-claude-hook: {err}\n").as_bytes());
            emit_empty();
        }
    }
}

struct BridgeCleanupState {
    socket_path: Option<PathBuf>,
    state_dir: PathBuf,
    badged_sessions: HashSet<String>,
}

static mut GLOBAL_CLEANUP: *const Mutex<BridgeCleanupState> = std::ptr::null();

fn perform_bridge_cleanup(cleanup: &Mutex<BridgeCleanupState>) {
    if let Ok(mut guard) = cleanup.lock() {
        if let Some(sock) = guard.socket_path.take() {
            let _ = fs::remove_file(sock);
        }
        let sessions: Vec<String> = guard.badged_sessions.drain().collect();
        for id in sessions {
            let _ = write_claude_badge(&guard.state_dir, &id, None);
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
            let valid = validate_claude_session_id(exp)?;
            return Ok(self.note_session(valid));
        }
        let resolved = resolve_claude_session_id(&self.cwd, &self.config.claude_home);
        let Some(resolved_id) = resolved else {
            return Ok(self.note_session(self.fallback_id.clone()));
        };
        if self.use_fallback_alias {
            if self.first_transcript.is_none() || self.first_transcript.as_deref() == Some(&resolved_id) {
                if self.first_transcript.is_none() {
                    self.last_active_ms = now_ms();
                }
                self.first_transcript = Some(resolved_id.clone());
                let _ = write_claude_session_alias(&self.config.state_dir, &resolved_id, &self.fallback_id);
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
            let title = resolve_claude_session_title(
                &self.cwd,
                &transcript_id,
                &self.config.claude_home,
                &mut self.title_cache,
            );
            let mtime_ms = resolve_claude_transcript_mtime_ms(&self.cwd, &transcript_id, &self.config.claude_home);
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
            if write_claude_badge(&self.config.state_dir, &current, badge_str).is_err() {
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
            if append_claude_wake(
                &self.config.state_dir,
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
                "method": "notifications/claude/channel",
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

fn start_claude_channel_listener(
    config: &NativeConfig,
    endpoint_pid: u32,
    stdout_lock: Arc<Mutex<()>>,
    cleanup: Arc<Mutex<BridgeCleanupState>>,
) -> bool {
    if !config.enable_claude_channel {
        return false;
    }
    let sockets_dir = config.state_dir.join("sockets");
    if fs::create_dir_all(&sockets_dir).is_err() {
        return false;
    }
    let _ = fs::set_permissions(&sockets_dir, fs::Permissions::from_mode(0o700));
    let socket_path = sockets_dir.join(format!("claude-pid-{endpoint_pid}.sock"));

    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if probe_live_bridge(&socket_path) {
                let _ = std::io::stderr().write_all(
                    format!("cito-native: socket {} already in use by live bridge\n", socket_path.display()).as_bytes(),
                );
                return false;
            }
            let _ = fs::remove_file(&socket_path);
            match UnixListener::bind(&socket_path) {
                Ok(l) => l,
                Err(_) => return false,
            }
        }
        Err(_) => return false,
    };
    let _ = fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600));
    if let Ok(mut guard) = cleanup.lock() {
        guard.socket_path = Some(socket_path);
    }

    let cwd = env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .to_string_lossy()
        .to_string();
    let explicit_env_id = env::var("CLAUDE_SESSION_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let use_fallback_alias =
        explicit_env_id.is_none() && resolve_claude_session_id(&cwd, &config.claude_home).is_none();

    let mut runtime = ChannelRuntime {
        config: config.clone(),
        endpoint_pid,
        cwd,
        explicit_env_id,
        fallback_id: format!("claude-{endpoint_pid}"),
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

    true
}

fn enrich_initialize_result(mut msg: Value, channel_enabled: bool) -> Value {
    if !channel_enabled {
        return msg;
    }
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
                        exp_map.insert("claude/channel".to_string(), json!({}));
                    }
                }
            }
        }
    }
    msg
}

fn extract_jsonrpc_from_http_body(body: &str) -> Option<Value> {
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

    let config = match load_native_config() {
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
        badged_sessions: HashSet::new(),
    }));

    unsafe {
        GLOBAL_CLEANUP = Arc::as_ptr(&cleanup);
        libc::signal(libc::SIGINT, handle_term_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle_term_signal as *const () as libc::sighandler_t);
    }

    let channel_active =
        start_claude_channel_listener(&config, endpoint_pid, stdout_lock.clone(), cleanup.clone());

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
                        let out_val = enrich_initialize_result(rpc_val, channel_active);
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
            Err(err) => {
                if let Some(id_val) = req_id {
                    let err_val = json!({
                        "jsonrpc": "2.0",
                        "id": id_val,
                        "error": {
                            "code": -32603,
                            "message": err,
                        }
                    });
                    if let Ok(out_str) = serde_json::to_string(&err_val) {
                        if let Ok(_guard) = stdout_lock.lock() {
                            let _ = std::io::stdout().write_all(format!("{out_str}\n").as_bytes());
                            let _ = std::io::stdout().flush();
                        }
                    }
                } else {
                    let _ = std::io::stderr().write_all(format!("cito-mcp: {err}\n").as_bytes());
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

    if argv0_name.ends_with("cito-claude-hook") {
        run_claude_hook(&args[1..]);
        return;
    }

    match args.get(1).map(String::as_str) {
        Some("claude-hook") => run_claude_hook(&args[2..]),
        Some("mcp-stdio") => run_mcp_stdio(),
        Some("--version" | "-v") => {
            let _ = std::io::stdout().write_all(format!("cito v{CITO_VERSION}\n").as_bytes());
        }
        _ => run_mcp_stdio(),
    }
}
