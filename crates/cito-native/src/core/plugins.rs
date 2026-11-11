pub use super::config::*;
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
use std::collections::HashSet;
use std::env;
use std::ffi::CStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const CITO_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug)]
pub struct PluginDescriptor {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub tier: String,
    pub sf_symbol: String,
    pub tint: String,
    pub cli_flag: bool,
    pub plugin_dir: PathBuf,
    pub is_builtin: bool,
    pub command: Vec<String>,
    pub channel_env: Option<String>,
    pub fields: Vec<Value>,
    pub post_setup_hint: Option<String>,
    pub post_disconnect_hint: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LoadedPlugins {
    pub plugins: Vec<PluginDescriptor>,
    pub rejected_ids: HashSet<String>,
}

impl LoadedPlugins {
    pub fn find_by_id(&self, id: &str) -> Option<&PluginDescriptor> {
        self.plugins.iter().find(|p| p.id == id)
    }

    pub fn agent_plugins(&self) -> Vec<&PluginDescriptor> {
        self.plugins
            .iter()
            .filter(|p| matches!(p.kind.as_str(), "agent" | "agent-harness"))
            .collect()
    }

    #[cfg(test)]
    pub fn human_plugins(&self) -> Vec<&PluginDescriptor> {
        self.plugins
            .iter()
            .filter(|p| matches!(p.kind.as_str(), "human" | "human-channel"))
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct NativeConfig {
    pub state_dir: PathBuf,
    pub config_path: PathBuf,
    pub home_dir: PathBuf,
    pub daemon_port: u16,
    pub user: String,
    pub default_hub: String,
}

impl NativeConfig {
    /// Reload the per-hub user so identity changes take effect without restarting the process.
    pub fn effective_user(&self, hub: &str) -> Result<String, String> {
        let raw = read_config_text(&self.config_path)?;
        let (_, hubs, rest) = try_normalize_existing_config(raw.as_deref())?;
        let global_user = env::var("CITO_USER")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| rest.get("user").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| self.user.clone());
        let user = hubs
            .get(hub)
            .and_then(|h| h.get("user"))
            .and_then(Value::as_str)
            .unwrap_or(&global_user);
        if !validate_user(user) {
            return Err(format!("Invalid user for hub {hub}"));
        }
        Ok(user.to_string())
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn random_uuid_v4() -> String {
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

/// Generates cryptographic tokens without masking an entropy-source failure.
pub fn try_random_hex(bytes_len: usize) -> Result<String, String> {
    let mut bytes = vec![0u8; bytes_len];
    getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Best-effort identifiers for temporary paths; use try_random_hex for secrets.
pub fn random_hex(bytes_len: usize) -> String {
    try_random_hex(bytes_len).unwrap_or_else(|_| {
        (0..bytes_len)
            .map(|i| format!("{:02x}", ((now_ms() as usize).wrapping_add(i * 31)) as u8))
            .collect()
    })
}

pub fn system_home_dir() -> String {
    if let Ok(h) = env::var("HOME") {
        let t = h.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_dir.is_null() {
            if let Ok(s) = CStr::from_ptr((*pw).pw_dir).to_str() {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    "/tmp".to_string()
}

pub fn expand_home_dir(
    value: &str,
    name: &str,
    base_home: Option<&str>,
) -> Result<PathBuf, String> {
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
        Ok(env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(expanded))
    }
}

pub fn validate_harness_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

pub fn validate_hub_name(name: &str) -> bool {
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

pub fn validate_nats_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed == "local" || trimmed == "local://" {
        return Ok("local://".to_string());
    }
    let Some((scheme, rest)) = trimmed.split_once("://") else {
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
            let p = port_str.parse::<u32>().map_err(|_| {
                "CITO_NATS_URL must be a valid Hub, NATS, or WebSocket URL".to_string()
            })?;
            if !(1..=65535).contains(&p) {
                return Err("CITO_NATS_URL must be a valid Hub, NATS, or WebSocket URL".to_string());
            }
        }
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

pub fn validate_https_issuer(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("CITO_OIDC_ISSUER is required in oidc mode".to_string());
    }
    let is_https = trimmed.starts_with("https://");
    let is_loopback_http = trimmed.starts_with("http://127.0.0.1:")
        || trimmed == "http://127.0.0.1"
        || trimmed.starts_with("http://127.0.0.1/");
    if (!is_https && !is_loopback_http)
        || trimmed.contains('?')
        || trimmed.contains('#')
        || trimmed.contains('@')
    {
        return Err(
            "CITO_OIDC_ISSUER must use HTTPS (http://127.0.0.1 is allowed for tests), without credentials, query or fragment"
                .to_string(),
        );
    }
    let after_scheme = trimmed.split_once("://").map(|(_, r)| r).unwrap_or("");
    let host_part = after_scheme.split('/').next().unwrap_or("");
    if host_part.is_empty() || (!is_loopback_http && !host_part.contains('.')) {
        return Err(
            "CITO_OIDC_ISSUER must use HTTPS (http://127.0.0.1 is allowed for tests), without credentials, query or fragment"
                .to_string(),
        );
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

fn is_checked_cito_env_key(key: &str) -> bool {
    if !key.starts_with("CITO_") {
        return false;
    }
    let rest = &key["CITO_".len()..];
    if matches!(rest, "STATE_DIR" | "CONFIG_PATH" | "OPEN_BIN") {
        return true;
    }
    if let Some(prefix) = rest
        .strip_suffix("_HOME")
        .or_else(|| rest.strip_suffix("_BIN"))
    {
        if prefix == "NODE" {
            return false;
        }
        return !prefix.is_empty()
            && prefix
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    }
    false
}

pub fn resolve_builtin_plugins_dir() -> Option<PathBuf> {
    if let Ok(explicit) = env::var("CITO_BUILTIN_PLUGINS_DIR") {
        let p = expand_home_dir(
            explicit.trim(),
            "CITO_BUILTIN_PLUGINS_DIR",
            env::var("HOME").ok().as_deref(),
        )
        .ok()?;
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for rel in [
                "../Resources/plugins",
                "../plugins",
                "../share/cito/plugins",
            ] {
                let cand = exe_dir.join(rel);
                if cand.is_dir() {
                    return Some(cand);
                }
            }
        }
    }
    let compile_fallback = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins"));
    if compile_fallback.is_dir() {
        return Some(compile_fallback);
    }
    None
}

pub fn parse_manifest_dir(dir: &Path, is_builtin: bool) -> Result<PluginDescriptor, String> {
    let dir_name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "Invalid plugin dir".to_string())?;
    let manifest_path = dir.join("plugin.json");
    let raw = fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
    let Value::Object(map) = serde_json::from_str::<Value>(&raw).map_err(|e| e.to_string())? else {
        return Err("Manifest is not an object".to_string());
    };
    let id = map
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| validate_harness_id(s))
        .ok_or_else(|| "Invalid plugin id".to_string())?
        .to_string();
    if id != dir_name {
        return Err("Plugin id does not match directory name".to_string());
    }
    let display_name = map
        .get("displayName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && !s.chars().any(|c| (c as u32) <= 0x1f || (c as u32) == 0x7f)
        })
        .ok_or_else(|| "Invalid displayName".to_string())?
        .to_string();
    let kind = match map
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("agent-harness")
    {
        "agent" | "agent-harness" => "agent-harness",
        "human" | "human-channel" => "human-channel",
        _ => return Err("Invalid plugin kind".to_string()),
    }
    .to_string();
    let tier = map
        .get("tier")
        .and_then(Value::as_str)
        .unwrap_or("stable")
        .to_string();
    if !matches!(tier.as_str(), "stable" | "experimental") {
        return Err("Invalid plugin tier".to_string());
    }
    let scope = map
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("hub")
        .to_string();
    if !matches!(scope.as_str(), "user" | "hub") {
        return Err("Invalid plugin scope".to_string());
    }
    let sf_symbol = map
        .get("sfSymbol")
        .and_then(|v| v.as_str())
        .unwrap_or("terminal")
        .to_string();
    let tint = map
        .get("tint")
        .and_then(|v| v.as_str())
        .unwrap_or("indigo")
        .to_string();
    let reserved_flags = [
        "hub", "team", "scope", "tag", "harness", "session", "help", "version", "setup",
    ];
    let mut cli_flag = map
        .get("cliFlag")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if cli_flag && reserved_flags.contains(&id.as_str()) {
        cli_flag = false;
    }
    let command = match map.get("command") {
        Some(Value::Array(parts)) if !parts.is_empty() => parts
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| "Invalid plugin command".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
        None if !is_builtin && dir.join("index.js").is_file() => vec![
            "node".to_string(),
            dir.join("index.js").to_string_lossy().into_owned(),
        ],
        None if !is_builtin && dir.join("index.ts").is_file() => vec![
            "node".to_string(),
            "--experimental-strip-types".to_string(),
            dir.join("index.ts").to_string_lossy().into_owned(),
        ],
        _ => {
            return Err(
                "PLUGIN_LOAD_ERROR: manifest needs command or a script entrypoint".to_string(),
            )
        }
    };
    let channel_env = map
        .get("channelEnv")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let fields = map
        .get("fields")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(PluginDescriptor {
        id,
        kind,
        display_name,
        tier,
        sf_symbol,
        tint,
        cli_flag,
        plugin_dir: dir.to_path_buf(),
        is_builtin,
        command,
        channel_env,
        fields,
        post_setup_hint: map
            .get("postSetupHint")
            .and_then(Value::as_str)
            .map(str::to_owned),
        post_disconnect_hint: map
            .get("postDisconnectHint")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

pub fn load_plugins(state_dir: &Path) -> LoadedPlugins {
    let mut plugins = Vec::new();
    let mut builtin_ids = HashSet::new();
    let mut rejected_ids = HashSet::new();

    let staged_root = state_dir.join("plugins");
    let mut builtin_roots: Vec<_> = resolve_builtin_plugins_dir().into_iter().collect();
    let using_staged_root = builtin_roots.first().is_some_and(|root| {
        root == &staged_root
            || fs::canonicalize(root)
                .ok()
                .zip(fs::canonicalize(&staged_root).ok())
                .is_some_and(|(builtin, staged)| builtin == staged)
    });
    if using_staged_root {
        // Installed shims point at staged files; recover deleted descriptors from
        // the distribution so a new process can still offer Repair.
        builtin_roots.extend(super::setup::resolve_bundled_plugins_root());
    }
    for builtin_root in builtin_roots {
        if let Ok(rd) = fs::read_dir(&builtin_root) {
            let mut subdirs: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            subdirs.sort();
            for sub in subdirs {
                // The staged directory also hosts custom plugins. Only bundled
                // IDs belong here; leave the rest for external discovery below.
                if using_staged_root
                    && !sub.file_name().and_then(|s| s.to_str()).is_some_and(|id| {
                        super::setup::resolve_bundled_plugin_dir(id)
                            .join("plugin.json")
                            .is_file()
                    })
                {
                    continue;
                }
                if let Ok(desc) = parse_manifest_dir(&sub, true) {
                    if builtin_ids.insert(desc.id.clone()) {
                        plugins.push(desc);
                    }
                }
            }
        }
    }

    plugins.sort_by_key(|p| match (p.kind.as_str(), p.tier.as_str()) {
        ("agent-harness", "stable") => (0, p.display_name.clone()),
        ("agent-harness", "experimental") => (1, p.display_name.clone()),
        ("agent-harness", _) => (2, p.display_name.clone()),
        _ => (3, p.display_name.clone()),
    });

    let mut ext_roots = vec![state_dir.join("plugins")];
    if let Ok(extra) = env::var("CITO_PLUGIN_PATH") {
        for part in extra.split(':').filter(|s| !s.trim().is_empty()) {
            if let Ok(path) = expand_home_dir(
                part.trim(),
                "CITO_PLUGIN_PATH",
                env::var("HOME").ok().as_deref(),
            ) {
                ext_roots.push(path);
            }
        }
    }

    let mut seen_ext = HashSet::new();
    for root in ext_roots {
        let Ok(rd) = fs::read_dir(&root) else {
            continue;
        };
        let mut subdirs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        subdirs.sort();
        for sub in subdirs {
            let dir_name = sub
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            match parse_manifest_dir(&sub, false) {
                Ok(desc) => {
                    if builtin_ids.contains(&desc.id) || seen_ext.contains(&desc.id) {
                        continue;
                    }
                    seen_ext.insert(desc.id.clone());
                    plugins.push(desc);
                }
                Err(_) => {
                    if !builtin_ids.contains(&dir_name) {
                        rejected_ids.insert(dir_name);
                    }
                }
            }
        }
    }

    LoadedPlugins {
        plugins,
        rejected_ids,
    }
}

pub fn load_native_config() -> Result<(NativeConfig, LoadedPlugins), String> {
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
    let mut user = match env::var("CITO_USER") {
        Ok(u) => {
            if !validate_user(&u) {
                return Err(
                    "CITO_USER must contain 1-64 letters, numbers, dots, underscores or hyphens"
                        .to_string(),
                );
            }
            u
        }
        Err(_) => env::var("USER").unwrap_or_else(|_| "user".to_string()),
    };
    if let Ok(hub_env) = env::var("CITO_HUB") {
        let h = hub_env.trim();
        if !h.is_empty() && !validate_hub_name(h) {
            return Err("default_hub must be a valid hub name".to_string());
        }
    }
    if let Ok(url) = env::var("CITO_HUB_URL").or_else(|_| env::var("CITO_NATS_URL")) {
        validate_nats_url(&url)?;
    }
    let auth_mode =
        canonical_auth_mode(&env::var("CITO_AUTH_MODE").unwrap_or_else(|_| "none".to_string()))?;
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
    if (auth_mode == "oidc" || issuer_env.is_some())
        && client_env.as_deref().unwrap_or("").trim().is_empty()
    {
        return Err("CITO_OIDC_CLIENT_ID is required with an OIDC issuer".to_string());
    }
    if let Some(ref cid) = client_env {
        if cid.is_empty() || cid != cid.trim() {
            return Err("CITO_OIDC_CLIENT_ID must be a non-empty trimmed string".to_string());
        }
    }

    let home_env = env::var("HOME").ok();
    let home_dir = expand_home_dir("~", "HOME", home_env.as_deref())?;
    let raw_state = env::var("CITO_STATE_DIR").unwrap_or_else(|_| "~/.cito".to_string());
    let state_dir = expand_home_dir(&raw_state, "CITO_STATE_DIR", home_env.as_deref())?;
    let explicit_config = env::var("CITO_CONFIG_PATH").ok();
    let config_path = match explicit_config {
        Some(ref p) => expand_home_dir(p, "CITO_CONFIG_PATH", home_env.as_deref())?,
        None => state_dir.join("config.json"),
    };

    if let Ok(id) = fs::read_to_string(state_dir.join("host-id")) {
        if !crate::is_valid_transcript_uuid(id.trim()) {
            return Err("Invalid host-id UUID".into());
        }
    }
    let plugins = load_plugins(&state_dir);
    let mut cfg_daemon_port: Option<u16> = None;
    let mut default_hub = env::var("CITO_HUB")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "local".to_string());

    if explicit_config.is_some() || config_path.exists() {
        let raw = fs::read_to_string(&config_path)
            .map_err(|_| format!("Cannot load hub configuration: {}", config_path.display()))?;
        let Value::Object(map) = serde_json::from_str::<Value>(&raw)
            .map_err(|e| format!("Invalid JSON in {}: {e}", config_path.display()))?
        else {
            return Err(format!(
                "Cannot load hub configuration: {}",
                config_path.display()
            ));
        };
        let (configured_default, hubs_map, _) = try_normalize_existing_config(Some(&raw))?;
        for hub in hubs_map.values() {
            validate_nats_url(hub["url"].as_str().unwrap_or(""))?;
            if hub["authMode"] == "oidc" {
                super::login::Settings::new(hub)?;
            }
        }
        if env::var("CITO_HUB").is_err() {
            default_hub = configured_default;
        }
        if !hubs_map.contains_key(&default_hub) {
            return Err("Invalid defaultHub".into());
        }
        if map
            .get("defaultHub")
            .or_else(|| map.get("default_hub"))
            .and_then(Value::as_str)
            .is_some_and(|h| !hubs_map.contains_key(h))
        {
            return Err("Invalid defaultHub".into());
        }
        if env::var("CITO_USER").is_err() {
            if let Some(config_user) = map.get("user").and_then(Value::as_str) {
                if !validate_user(config_user) {
                    return Err("Invalid user in config.json".to_string());
                }
                user = config_user.to_string();
            }
        }
        if let Some(value) = map.get("daemonPort") {
            let p = value
                .as_u64()
                .filter(|p| (1..=65535).contains(p))
                .ok_or("daemonPort must be an integer in 1..65535")?;
            cfg_daemon_port = Some(p as u16);
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

    Ok((
        NativeConfig {
            state_dir,
            config_path,
            home_dir,
            daemon_port,
            user,
            default_hub,
        },
        plugins,
    ))
}

/// Build a subprocess without involving a shell. Relative executable paths belong to the plugin.
pub fn plugin_command(plugin: &PluginDescriptor) -> Result<Command, String> {
    let executable = plugin
        .command
        .first()
        .ok_or("PLUGIN_LOAD_ERROR: empty command")?;
    let relative = executable.starts_with("./") || executable.starts_with("../");
    let plugin_dir =
        if plugin.is_builtin && !super::setup::is_plugin_dir_intact(&plugin.plugin_dir, plugin) {
            super::setup::resolve_bundled_plugin_dir(&plugin.id)
        } else {
            plugin.plugin_dir.clone()
        };
    let executable = if relative {
        plugin_dir.join(executable)
    } else if executable.starts_with('~') {
        expand_home_dir(
            executable,
            "plugin command",
            env::var("HOME").ok().as_deref(),
        )?
    } else {
        PathBuf::from(executable)
    };
    let mut command = Command::new(executable);
    command.args(&plugin.command[1..]).current_dir(plugin_dir);
    Ok(command)
}

pub fn plugin_request(plugin: &PluginDescriptor, request: &Value) -> Result<Value, String> {
    plugin_request_with_timeout(plugin, request, Duration::from_millis(10_000))
}

pub fn plugin_request_with_timeout(
    plugin: &PluginDescriptor,
    request: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    plugin_request_with_progress(plugin, request, timeout, |_| Ok(()))
}

/// Run caller-owned progress checks while polling the child. An error aborts and reaps
/// the plugin process group, allowing the receiver to fail closed on lease loss.
pub fn plugin_request_with_progress(
    plugin: &PluginDescriptor,
    request: &Value,
    timeout: Duration,
    mut progress: impl FnMut(Duration) -> Result<(), String>,
) -> Result<Value, String> {
    let mut execute = || -> Result<Value, String> {
        let mut command = plugin_command(plugin)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let result = (|| {
            let mut stdin = child.stdin.take();
            let mut stdout = child.stdout.take().ok_or("Missing plugin stdout")?;
            let mut stderr = child.stderr.take().ok_or("Missing plugin stderr")?;
            for fd in [
                stdin.as_ref().ok_or("Missing plugin stdin")?.as_raw_fd(),
                stdout.as_raw_fd(),
                stderr.as_raw_fd(),
            ] {
                // SAFETY: each fd is an owned pipe that remains open for this request.
                unsafe {
                    let flags = libc::fcntl(fd, libc::F_GETFL);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                }
            }
            let mut input = serde_json::to_vec(request).map_err(|e| e.to_string())?;
            input.push(b'\n');
            let mut written = 0;
            let mut output = Vec::new();
            let mut errors = Vec::new();
            let mut out_eof = false;
            let mut err_eof = false;
            let started = Instant::now();
            let deadline = started + timeout;
            loop {
                if Instant::now() >= deadline {
                    return Err("Plugin timed out".to_string());
                }
                progress(started.elapsed())?;
                if let Some(pipe) = stdin.as_mut() {
                    match pipe.write(&input[written..]) {
                        Ok(0) => {
                            return Err("Plugin closed stdin before request completed".to_string())
                        }
                        Ok(n) => written += n,
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(e) => return Err(e.to_string()),
                    }
                    if written == input.len() {
                        stdin.take();
                    }
                }
                for (pipe, buffer, eof, limit) in [
                    (
                        &mut stdout as &mut dyn Read,
                        &mut output,
                        &mut out_eof,
                        1_048_576usize,
                    ),
                    (
                        &mut stderr as &mut dyn Read,
                        &mut errors,
                        &mut err_eof,
                        65_536usize,
                    ),
                ] {
                    let mut bytes = [0u8; 8192];
                    loop {
                        match pipe.read(&mut bytes) {
                            Ok(0) => {
                                *eof = true;
                                break;
                            }
                            Ok(n) => {
                                if buffer.len() + n > limit {
                                    return Err("Plugin output exceeds limit".to_string());
                                }
                                buffer.extend_from_slice(&bytes[..n]);
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(e) => return Err(e.to_string()),
                        }
                    }
                }
                if let Some((success, code)) = peek_plugin_exit(child.id())? {
                    if out_eof && err_eof {
                        if !success {
                            return Err(format!(
                                "Plugin exited {code}: {}",
                                String::from_utf8_lossy(&errors)
                            ));
                        }
                        let output = std::str::from_utf8(&output).map_err(|e| e.to_string())?;
                        let mut lines = output.lines();
                        let line = lines.next().ok_or("Plugin produced no response")?;
                        if lines.any(|line| !line.trim().is_empty()) {
                            return Err("Plugin produced more than one response line".to_string());
                        }
                        let response: Value = serde_json::from_str(line)
                            .map_err(|e| format!("Invalid plugin JSON: {e}"))?;
                        if !response.is_object() {
                            return Err("Plugin response must be a JSON object".to_string());
                        }
                        return Ok(response);
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
        })();
        if result.is_err() {
            // SAFETY: WNOWAIT keeps the child PID reserved until the final wait below.
            // Its process-group ID therefore cannot be reused during graceful cleanup.
            let group = -(child.id() as i32);
            unsafe {
                libc::kill(group, libc::SIGTERM);
            }
            let grace = Instant::now() + Duration::from_millis(50);
            while Instant::now() < grace {
                // SAFETY: signal zero observes our still-reserved process group.
                if unsafe { libc::kill(group, 0) } != 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            // SAFETY: the child remains unreaped, preserving group ownership on escalation.
            unsafe {
                libc::kill(group, libc::SIGKILL);
            }
        }
        let waited = child.wait().map_err(|error| error.to_string());
        match result {
            Ok(value) => waited.map(|_| value),
            Err(error) => Err(error),
        }
    };
    execute().map_err(|e| format!("PLUGIN_EXEC_FAILED: {}: {e}", plugin.id))
}

fn peek_plugin_exit(pid: u32) -> Result<Option<(bool, i32)>, String> {
    // SAFETY: siginfo is writable, pid belongs to an unreaped Child, and WNOWAIT
    // observes completion without releasing its PID or process-group ownership.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(None);
        }
        return Err(error.to_string());
    }
    if info.si_signo == 0 {
        return Ok(None);
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let status = unsafe { info.si_status() };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let status = info.si_status;
    Ok(Some((
        info.si_code == libc::CLD_EXITED && status == 0,
        status,
    )))
}

#[cfg(test)]
pub fn plugin_call(plugin: &PluginDescriptor, request: &Value) -> Value {
    plugin_request(plugin, request).unwrap_or_else(
        |message| json!({"ok":false,"error_code":"PLUGIN_EXEC_FAILED","message":message}),
    )
}

pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub fn read_http_response(stream: &mut impl Read) -> Result<HttpResponse, String> {
    const MAX_BYTES: usize = 8 * 1024 * 1024;
    fn line(reader: &mut impl BufRead, total: &mut usize) -> Result<String, String> {
        let mut bytes = Vec::new();
        reader
            .take((MAX_BYTES - *total + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .map_err(|e| e.to_string())?;
        *total += bytes.len();
        if *total > MAX_BYTES || !bytes.ends_with(b"\n") {
            return Err("Truncated or oversized HTTP headers".into());
        }
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }
    let mut reader = BufReader::new(stream);
    let mut header_bytes = 0;
    let status_line = line(&mut reader, &mut header_bytes)?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("Invalid HTTP status: {}", status_line.trim()))?;
    let mut content_length = None;
    let mut chunked = false;
    loop {
        let header = line(&mut reader, &mut header_bytes)?;
        if header == "\r\n" || header == "\n" {
            break;
        }
        let (key, value) = header.split_once(':').ok_or("Invalid HTTP header")?;
        if key.eq_ignore_ascii_case("content-length") {
            let length = value
                .trim()
                .parse::<usize>()
                .map_err(|_| "Invalid Content-Length")?;
            if length > MAX_BYTES || content_length.replace(length).is_some() {
                return Err("Oversized or duplicate Content-Length".into());
            }
        } else if key.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.trim().eq_ignore_ascii_case("chunked");
        }
    }
    if status == 202 || status == 204 {
        return Ok(HttpResponse {
            status,
            body: String::new(),
        });
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let size = line(&mut reader, &mut header_bytes)?;
            let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|_| "Invalid HTTP chunk size")?;
            if size == 0 {
                while !matches!(
                    line(&mut reader, &mut header_bytes)?.as_str(),
                    "\r\n" | "\n"
                ) {}
                break;
            }
            if size > MAX_BYTES - body.len() {
                return Err("HTTP body exceeds 8 MiB".into());
            }
            let offset = body.len();
            body.resize(offset + size, 0);
            reader
                .read_exact(&mut body[offset..])
                .map_err(|e| e.to_string())?;
            let mut crlf = [0; 2];
            reader.read_exact(&mut crlf).map_err(|e| e.to_string())?;
            if &crlf != b"\r\n" {
                return Err("Invalid HTTP chunk terminator".into());
            }
        }
    } else if let Some(length) = content_length {
        body.resize(length, 0);
        reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    } else {
        reader
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut body)
            .map_err(|e| e.to_string())?;
        if body.len() > MAX_BYTES {
            return Err("HTTP body exceeds 8 MiB".into());
        }
    }
    Ok(HttpResponse {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

pub fn http_post_mcp(port: u16, body: &str, timeout: Duration) -> Result<HttpResponse, String> {
    http_post_json(port, "/mcp", body, timeout)
}

pub fn http_post_json(
    port: u16,
    path: &str,
    body: &str,
    timeout: Duration,
) -> Result<HttpResponse, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let req = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n\
         Connection: close\r\n\
         Content-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;
    read_http_response(&mut stream)
}

#[derive(Debug)]
pub struct DaemonHealth {
    pub pid: i32,
    pub version: Option<String>,
}

pub fn probe_daemon_health(config: &NativeConfig) -> Option<DaemonHealth> {
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
    daemon_health_from_json(config, &val)
}

fn daemon_health_from_json(config: &NativeConfig, val: &Value) -> Option<DaemonHealth> {
    let obj = val.as_object()?;
    if obj.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let state_dir = obj.get("stateDir").and_then(|v| v.as_str())?;
    if Path::new(state_dir) != config.state_dir {
        return None;
    }
    let pid = obj.get("pid").and_then(|v| v.as_i64())?;
    if pid <= 1 || pid > i32::MAX as i64 || pid == std::process::id() as i64 {
        return None;
    }
    let version = obj
        .get("version")
        .and_then(|v| v.as_str())
        .map(String::from);
    Some(DaemonHealth {
        pid: pid as i32,
        version,
    })
}

pub fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal zero checks existence without signaling the process.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

pub fn stop_daemon(config: &NativeConfig, health: &DaemonHealth) -> Result<(), String> {
    let current = probe_daemon_health(config).ok_or("Daemon identity could not be revalidated")?;
    if current.pid != health.pid || current.version != health.version {
        return Err("Daemon identity changed before shutdown".to_string());
    }
    // SAFETY: PID was obtained twice from the matching state directory's health endpoint.
    if unsafe { libc::kill(health.pid, libc::SIGTERM) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(format!("Cannot stop daemon: {e}"));
        }
    }
    let deadline = Instant::now() + Duration::from_millis(5000);
    while pid_alive(health.pid) && Instant::now() < deadline {
        // A daemon launched by this process may have exited as an unreaped child.
        // SAFETY: waitpid targets only the verified daemon and never blocks.
        if unsafe { libc::waitpid(health.pid, std::ptr::null_mut(), libc::WNOHANG) } == health.pid {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    if pid_alive(health.pid) {
        return Err("Daemon did not stop within 5000ms".to_string());
    }
    Ok(())
}

/// Direct daemon entry and spawning clients use the same stale-version handover.
/// Returns true when a current daemon is already running.
pub fn handover_stale_daemon(config: &NativeConfig) -> Result<bool, String> {
    if let Some(existing) = probe_daemon_health(config) {
        if existing.version.as_deref() == Some(CITO_VERSION) {
            return Ok(true);
        }
        stop_daemon(config, &existing)?;
    }
    Ok(false)
}

pub fn ensure_daemon(config: &NativeConfig) -> Result<(), String> {
    if let Some(existing) = probe_daemon_health(config) {
        if existing.version.as_deref() == Some(CITO_VERSION) {
            return Ok(());
        }
        stop_daemon(config, &existing)?;
    }

    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()
        .map_err(|e| format!("Failed to spawn daemon: {e}"))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture(script: &str) -> (PathBuf, PluginDescriptor) {
        let root = PathBuf::from(env::var("HOME").expect("isolated HOME"))
            .join(format!("cito-plugin-test-{}", random_hex(8)));
        let dir = root.join("sample");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("plugin.json"), r#"{"id":"sample","displayName":"Sample","kind":"agent-harness","command":["./run.sh"]}"#).unwrap();
        fs::write(dir.join("run.sh"), format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(dir.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let descriptor = parse_manifest_dir(&dir, false).unwrap();
        (root, descriptor)
    }

    #[test]
    fn documented_subprocess_manifests_load() {
        let html = include_str!("../../../../site/docs/plugins.html");
        let examples: Vec<_> = html
            .split("<pre><code data-plugin-manifest>")
            .skip(1)
            .map(|part| {
                serde_json::from_str::<Value>(part.split("</code>").next().unwrap()).unwrap()
            })
            .collect();
        assert_eq!(examples.len(), 2);
        let root = env::temp_dir().join(format!("cito-doc-manifests-{}", random_hex(8)));
        for (example, (id, kind)) in examples.iter().zip([
            ("sample-agent", "agent-harness"),
            ("sample-chat", "human-channel"),
        ]) {
            let dir = root.join(id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("plugin.json"), example.to_string()).unwrap();
            let descriptor = parse_manifest_dir(&dir, false).unwrap();
            assert_eq!(descriptor.id, id);
            assert_eq!(descriptor.kind, kind);
            assert_eq!(descriptor.tier, "experimental");
            assert_eq!(descriptor.plugin_dir, dir);
            assert_eq!(descriptor.command, vec![format!("./bin/cito-plugin-{id}")]);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ndjson_request_round_trip_and_relative_command() {
        let (root, plugin) = fixture("/bin/cat");
        let request = json!({"op":"setup_inspect","payload":"line\\nquoted \"value\""});
        assert_eq!(plugin_request(&plugin, &request).unwrap(), request);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subprocess_failures_are_structured_and_timeout_kills_child() {
        for script in [
            "printf 'invalid\\n'",
            "printf '{}\\n'; exit 4",
            "printf '{}\\n{}\\n'",
        ] {
            let (root, plugin) = fixture(script);
            let result = plugin_call(&plugin, &json!({"op":"probe"}));
            assert_eq!(result["ok"], false);
            assert_eq!(result["error_code"], "PLUGIN_EXEC_FAILED");
            fs::remove_dir_all(root).unwrap();
        }
        let (root, plugin) = fixture("echo $$ > child.pid; exec /bin/sleep 10");
        let started = Instant::now();
        let result = plugin_request_with_timeout(
            &plugin,
            &json!({"op":"probe"}),
            Duration::from_millis(1000),
        );
        assert!(result.unwrap_err().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(3));
        let pid: i32 = fs::read_to_string(plugin.plugin_dir.join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!pid_alive(pid));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn progress_failure_aborts_and_reaps_plugin() {
        let (root, plugin) = fixture("echo $$ > child.pid; exec /bin/sleep 10");
        let pid_path = plugin.plugin_dir.join("child.pid");
        let result = plugin_request_with_progress(
            &plugin,
            &json!({"op":"wake"}),
            Duration::from_secs(3),
            |_| {
                if fs::read_to_string(&pid_path)
                    .ok()
                    .and_then(|s| s.trim().parse::<i32>().ok())
                    .is_some()
                {
                    Err("Wake lease renewal failed".to_string())
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.unwrap_err().contains("Wake lease renewal failed"));
        let pid: i32 = fs::read_to_string(pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!pid_alive(pid));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_kind_aliases_and_script_fallback() {
        let (root, plugin) = fixture("/bin/cat");
        for (kind, expected) in [
            ("agent", "agent-harness"),
            ("agent-harness", "agent-harness"),
            ("human", "human-channel"),
            ("human-channel", "human-channel"),
        ] {
            fs::write(
                plugin.plugin_dir.join("plugin.json"),
                json!({"id":"sample","displayName":"Sample","kind":kind,"command":["./run.sh"]})
                    .to_string(),
            )
            .unwrap();
            let parsed = parse_manifest_dir(&plugin.plugin_dir, false).unwrap();
            assert_eq!(parsed.kind, expected);
            let loaded = LoadedPlugins {
                plugins: vec![parsed],
                rejected_ids: HashSet::new(),
            };
            assert_eq!(
                loaded.agent_plugins().len(),
                usize::from(expected == "agent-harness")
            );
            assert_eq!(
                loaded.human_plugins().len(),
                usize::from(expected == "human-channel")
            );
        }
        fs::write(
            plugin.plugin_dir.join("plugin.json"),
            r#"{"id":"sample","displayName":"Sample"}"#,
        )
        .unwrap();
        assert!(parse_manifest_dir(&plugin.plugin_dir, false)
            .unwrap_err()
            .contains("PLUGIN_LOAD_ERROR"));
        fs::write(plugin.plugin_dir.join("index.ts"), "").unwrap();
        let fallback = parse_manifest_dir(&plugin.plugin_dir, false).unwrap();
        assert_eq!(fallback.command[0], "node");
        assert_eq!(fallback.command[1], "--experimental-strip-types");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn handover_preserves_current_and_reaps_stale_or_legacy_daemons() {
        use std::net::TcpListener;
        for version in [Some(CITO_VERSION), Some("0.0.0"), None] {
            let (root, _) = fixture("/bin/cat");
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            #[expect(
                clippy::zombie_processes,
                reason = "Stale handover reaps the verified PID with waitpid; the current-version branch waits below."
            )]
            let mut child = Command::new("/bin/sleep").arg("20").spawn().unwrap();
            let pid = child.id();
            let config = NativeConfig {
                state_dir: root.clone(),
                config_path: root.join("config.json"),
                home_dir: root.clone(),
                daemon_port: port,
                user: "alice".into(),
                default_hub: "local".into(),
            };
            let state = root.clone();
            let version = version.map(str::to_owned);
            let expected_current = version.as_deref() == Some(CITO_VERSION);
            let requests = if expected_current { 1 } else { 2 };
            let server = thread::spawn(move || {
                for connection in listener.incoming().take(requests) {
                    let mut stream = connection.unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut headers = Vec::new();
                    let mut byte = [0u8; 1];
                    while !headers.ends_with(b"\r\n\r\n") {
                        stream.read_exact(&mut byte).unwrap();
                        headers.push(byte[0]);
                    }
                    let mut response = json!({"ok":true,"pid":pid,"stateDir":state});
                    if let Some(version) = &version {
                        response["version"] = json!(version);
                    }
                    let response = response.to_string();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        response.len(),
                        response
                    )
                    .unwrap();
                }
            });
            let result = handover_stale_daemon(&config);
            if result.is_err() {
                let _ = child.kill();
                let _ = child.wait();
            }
            assert_eq!(result.unwrap(), expected_current);
            server.join().unwrap();
            assert_eq!(pid_alive(pid as i32), expected_current);
            if expected_current {
                child.kill().unwrap();
                child.wait().unwrap();
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn http_response_bounds_and_malformed_framing_are_errors() {
        for response in [
            "HTTP/1.1 200 OK\r\nContent-Length: 8388609\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: invalid\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nx",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nnope\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n800001\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx",
        ] {
            assert!(
                read_http_response(&mut response.as_bytes()).is_err(),
                "{response}"
            );
        }
        let mut oversized_header = vec![b'x'; 8 * 1024 * 1024 + 1];
        assert!(read_http_response(&mut oversized_header.as_slice()).is_err());
        oversized_header.clear();
        let mut unframed = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
        unframed.resize(unframed.len() + 8 * 1024 * 1024 + 1, b'x');
        assert!(read_http_response(&mut unframed.as_slice()).is_err());
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n";
        assert_eq!(
            read_http_response(&mut chunked.as_slice()).unwrap().body,
            "hi"
        );
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("body failed"))
            }
        }
        let mut reader = std::io::Read::chain(b"HTTP/1.1 200 OK\r\n\r\n".as_slice(), Broken);
        assert!(read_http_response(&mut reader).is_err());
    }
    #[test]
    fn daemon_health_requires_matching_state_and_safe_pid() {
        let (root, _) = fixture("/bin/cat");
        let config = NativeConfig {
            state_dir: root.clone(),
            config_path: root.join("config.json"),
            home_dir: root.clone(),
            daemon_port: 4290,
            user: "alice".to_string(),
            default_hub: "local".to_string(),
        };
        for pid in [0i64, 1, -5, i32::MAX as i64 + 1, std::process::id() as i64] {
            assert!(daemon_health_from_json(
                &config,
                &json!({"ok":true,"pid":pid,"stateDir":root})
            )
            .is_none());
        }
        assert!(daemon_health_from_json(
            &config,
            &json!({"ok":true,"pid":123,"stateDir":root.join("other")})
        )
        .is_none());
        let health =
            daemon_health_from_json(&config, &json!({"ok":true,"pid":123,"stateDir":root}))
                .unwrap();
        assert_eq!(health.pid, 123);
        assert!(health.version.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn effective_user_observes_edits_and_rejects_malformed_json() {
        let (root, _) = fixture("/bin/cat");
        let config = NativeConfig {
            state_dir: root.clone(),
            config_path: root.join("config.json"),
            home_dir: root.clone(),
            daemon_port: 4290,
            user: "alice".to_string(),
            default_hub: "local".to_string(),
        };
        let environment_user = env::var("CITO_USER")
            .ok()
            .filter(|value| !value.trim().is_empty());
        assert_eq!(
            config.effective_user("local").unwrap(),
            environment_user.as_deref().unwrap_or("alice")
        );
        fs::write(
            &config.config_path,
            r#"{"user":"carol","hubs":{"local":{}}}"#,
        )
        .unwrap();
        assert_eq!(
            config.effective_user("local").unwrap(),
            environment_user.as_deref().unwrap_or("carol")
        );
        fs::write(&config.config_path, r#"{"hubs":{"local":{"user":"bob"}}}"#).unwrap();
        assert_eq!(config.effective_user("local").unwrap(), "bob");
        fs::write(&config.config_path, "{").unwrap();
        assert!(config.effective_user("local").is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
