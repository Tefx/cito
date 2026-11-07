use serde_json::{Map, Value};
use std::collections::HashSet;
use std::env;
use std::ffi::{CStr, CString};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const CITO_VERSION: &str = "0.17.0";

#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open_v2(
        filename: *const libc::c_char,
        pp_db: *mut *mut libc::c_void,
        flags: libc::c_int,
        z_vfs: *const libc::c_char,
    ) -> libc::c_int;
    fn sqlite3_close(db: *mut libc::c_void) -> libc::c_int;
    fn sqlite3_exec(
        db: *mut libc::c_void,
        sql: *const libc::c_char,
        callback: Option<
            extern "C" fn(
                *mut libc::c_void,
                libc::c_int,
                *mut *mut libc::c_char,
                *mut *mut libc::c_char,
            ) -> libc::c_int,
        >,
        arg: *mut libc::c_void,
        errmsg: *mut *mut libc::c_char,
    ) -> libc::c_int;
    fn sqlite3_free(ptr: *mut libc::c_void);
}

const SQLITE_OPEN_READONLY: libc::c_int = 0x00000001;

#[derive(Clone, Debug)]
pub struct PluginDescriptor {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub tier: String,
    pub sf_symbol: String,
    pub tint: String,
    pub cli_flag: bool,
    pub protocol: String,
    pub driver: Option<String>,
    pub native: Map<String, Value>,
    pub plugin_dir: PathBuf,
    pub is_builtin: bool,
    pub has_script_entry: bool,
}

impl PluginDescriptor {
    pub fn native_str(&self, key: &str) -> Option<&str> {
        self.native.get(key).and_then(|v| v.as_str())
    }
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

    pub fn find_by_protocol(&self, protocol: &str) -> Option<&PluginDescriptor> {
        self.plugins.iter().find(|p| p.protocol == protocol)
    }

    pub fn agent_plugins(&self) -> Vec<&PluginDescriptor> {
        self.plugins.iter().filter(|p| p.kind == "agent").collect()
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
    pub transcript_home: PathBuf,
    pub enable_transcript_channel: bool,
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

pub fn random_hex(bytes_len: usize) -> String {
    let mut bytes = vec![0u8; bytes_len];
    if let Ok(mut f) = File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    } else {
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = ((now_ms() as usize).wrapping_add(i * 31)) as u8;
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn random_base32_12() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut bytes = [0u8; 12];
    if let Ok(mut f) = File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    }
    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
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

pub fn expand_home_dir(value: &str, name: &str, base_home: Option<&str>) -> Result<PathBuf, String> {
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

pub fn validate_harness_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
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
            let p = port_str
                .parse::<u32>()
                .map_err(|_| "CITO_NATS_URL must be a valid Hub, NATS, or WebSocket URL".to_string())?;
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
    if let Some(prefix) = rest.strip_suffix("_HOME").or_else(|| rest.strip_suffix("_BIN")) {
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
        let p = PathBuf::from(explicit.trim());
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for rel in ["../Resources/plugins", "../plugins", "../share/cito/plugins"] {
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

fn parse_manifest_dir(dir: &Path, is_builtin: bool) -> Result<PluginDescriptor, String> {
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
        .filter(|s| !s.is_empty() && s.len() <= 64 && !s.chars().any(|c| (c as u32) <= 0x1f || (c as u32) == 0x7f))
        .ok_or_else(|| "Invalid displayName".to_string())?
        .to_string();
    let kind = map
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("agent")
        .to_string();
    if !matches!(kind.as_str(), "agent" | "human") {
        return Err("Invalid plugin kind".to_string());
    }
    let tier = if is_builtin {
        map.get("tier")
            .and_then(|v| v.as_str())
            .unwrap_or("stable")
            .to_string()
    } else {
        "custom".to_string()
    };
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
    let mut cli_flag = map.get("cliFlag").and_then(|v| v.as_bool()).unwrap_or(false);
    if cli_flag && reserved_flags.contains(&id.as_str()) {
        cli_flag = false;
    }
    let protocol = map
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or(if kind == "human" { "desktop-ui" } else { "socket-bridge" })
        .to_string();
    let driver = map.get("driver").and_then(|v| v.as_str()).map(String::from);
    let native = map
        .get("native")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let has_script_entry = dir.join("index.js").is_file() || dir.join("index.ts").is_file();

    Ok(PluginDescriptor {
        id,
        kind,
        display_name,
        tier,
        sf_symbol,
        tint,
        cli_flag,
        protocol,
        driver,
        native,
        plugin_dir: dir.to_path_buf(),
        is_builtin,
        has_script_entry,
    })
}

pub fn load_plugins(state_dir: &Path) -> LoadedPlugins {
    let mut plugins = Vec::new();
    let mut builtin_ids = HashSet::new();
    let mut rejected_ids = HashSet::new();

    if let Some(builtin_root) = resolve_builtin_plugins_dir() {
        if let Ok(rd) = fs::read_dir(&builtin_root) {
            let mut subdirs: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            subdirs.sort();
            for sub in subdirs {
                if let Ok(desc) = parse_manifest_dir(&sub, true) {
                    builtin_ids.insert(desc.id.clone());
                    plugins.push(desc);
                }
            }
        }
    }

    plugins.sort_by_key(|p| match (p.kind.as_str(), p.tier.as_str()) {
        ("agent", "stable") => (0, p.display_name.clone()),
        ("agent", "experimental") => (1, p.display_name.clone()),
        ("agent", _) => (2, p.display_name.clone()),
        _ => (3, p.display_name.clone()),
    });

    let mut ext_roots = vec![state_dir.join("plugins")];
    if let Ok(extra) = env::var("CITO_PLUGIN_PATH") {
        for part in extra.split(':').filter(|s| !s.trim().is_empty()) {
            ext_roots.push(PathBuf::from(part.trim()));
        }
    }

    let mut seen_ext = HashSet::new();
    for root in ext_roots {
        let Ok(rd) = fs::read_dir(&root) else { continue };
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
    let user = match env::var("CITO_USER") {
        Ok(u) => {
            if u.is_empty()
                || u.len() > 64
                || !u
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            {
                return Err("CITO_USER must contain 1-64 letters, numbers, dots, underscores or hyphens".to_string());
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
    let home_dir = expand_home_dir("~", "HOME", home_env.as_deref())?;
    let raw_state = env::var("CITO_STATE_DIR").unwrap_or_else(|_| "~/.cito".to_string());
    let state_dir = expand_home_dir(&raw_state, "CITO_STATE_DIR", home_env.as_deref())?;
    let explicit_config = env::var("CITO_CONFIG_PATH").ok();
    let config_path = match explicit_config {
        Some(ref p) => expand_home_dir(p, "CITO_CONFIG_PATH", home_env.as_deref())?,
        None => state_dir.join("config.json"),
    };

    let plugins = load_plugins(&state_dir);
    let transcript_plugin = plugins.find_by_protocol("transcript-hook");
    let home_env_key = transcript_plugin
        .and_then(|p| p.native_str("homeEnv"))
        .unwrap_or("CITO_TRANSCRIPT_HOME");
    let default_transcript_home = transcript_plugin
        .and_then(|p| p.native_str("defaultHome"))
        .unwrap_or("~/.cito/transcripts");
    let channel_env_key = transcript_plugin
        .and_then(|p| p.native_str("channelEnv"))
        .unwrap_or("CITO_TRANSCRIPT_CHANNEL");
    let config_home_key = transcript_plugin
        .and_then(|p| p.native_str("configHomeKey"))
        .unwrap_or("transcriptHome");
    let config_channel_key = transcript_plugin
        .and_then(|p| p.native_str("configChannelKey"))
        .unwrap_or("enableChannel");
    let transcript_id = transcript_plugin.map(|p| p.id.as_str()).unwrap_or("");

    let mut cfg_daemon_port: Option<u16> = None;
    let mut cfg_transcript_home: Option<String> = None;
    let mut cfg_enable_channel: Option<bool> = None;
    let mut default_hub = env::var("CITO_HUB")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "local".to_string());

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
        if env::var("CITO_HUB").is_err() {
            if let Some(dh) = map.get("default_hub").and_then(|v| v.as_str()) {
                default_hub = dh.to_string();
            } else if let Some(first) = hubs_map.keys().next() {
                default_hub = first.clone();
            }
        }
        if let Some(p) = map.get("daemonPort").and_then(|v| v.as_u64()) {
            if (1..=65535).contains(&p) {
                cfg_daemon_port = Some(p as u16);
            }
        }
        if let Some(plugins_obj) = map.get("plugins").and_then(|v| v.as_object()) {
            if let Some(tp_cfg) = plugins_obj.get(transcript_id).and_then(|v| v.as_object()) {
                if let Some(ch) = tp_cfg.get(config_home_key).and_then(|v| v.as_str()) {
                    cfg_transcript_home = Some(ch.to_string());
                }
                if let Some(en) = tp_cfg.get(config_channel_key).and_then(|v| v.as_bool()) {
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

    let raw_transcript_home = cfg_transcript_home
        .or_else(|| env::var(home_env_key).ok())
        .unwrap_or_else(|| default_transcript_home.to_string());
    let transcript_home = expand_home_dir(&raw_transcript_home, home_env_key, home_env.as_deref())?;

    let enable_transcript_channel = cfg_enable_channel.unwrap_or_else(|| {
        env::var(channel_env_key)
            .ok()
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false)
    });

    Ok((
        NativeConfig {
            state_dir,
            config_path,
            home_dir,
            daemon_port,
            user,
            default_hub,
            transcript_home,
            enable_transcript_channel,
        },
        plugins,
    ))
}

pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub fn read_http_response(stream: &mut TcpStream) -> Result<HttpResponse, String> {
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

pub fn http_post_mcp(port: u16, body: &str, timeout: Duration) -> Result<HttpResponse, String> {
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

pub fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

pub fn ensure_daemon(config: &NativeConfig) -> Result<(), String> {
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

#[derive(Clone, Debug)]
pub struct SqliteThreadRow {
    pub id: String,
    pub cwd: String,
    pub archived: bool,
}

extern "C" fn collect_thread_rows_cb(
    arg: *mut libc::c_void,
    argc: libc::c_int,
    argv: *mut *mut libc::c_char,
    _col_names: *mut *mut libc::c_char,
) -> libc::c_int {
    if arg.is_null() || argv.is_null() || argc < 3 {
        return 0;
    }
    unsafe {
        let rows = &mut *(arg as *mut Vec<SqliteThreadRow>);
        let get_str = |idx: isize| -> String {
            let ptr = *argv.offset(idx);
            if ptr.is_null() {
                String::new()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().to_string()
            }
        };
        let id = get_str(0);
        let cwd = get_str(1);
        let archived_raw = get_str(2);
        let archived = archived_raw == "1" || archived_raw.eq_ignore_ascii_case("true");
        if !id.is_empty() {
            rows.push(SqliteThreadRow { id, cwd, archived });
        }
    }
    0
}

pub fn query_sqlite_threads(db_path: &Path) -> Vec<SqliteThreadRow> {
    if !db_path.is_file() {
        return Vec::new();
    }
    let Ok(c_path) = CString::new(db_path.to_string_lossy().as_bytes()) else {
        return Vec::new();
    };
    let mut rows: Vec<SqliteThreadRow> = Vec::new();
    unsafe {
        let mut db: *mut libc::c_void = std::ptr::null_mut();
        if sqlite3_open_v2(c_path.as_ptr(), &mut db, SQLITE_OPEN_READONLY, std::ptr::null()) == 0 && !db.is_null() {
            let sql = CString::new("SELECT id, cwd, archived FROM threads ORDER BY updated_at DESC").unwrap();
            let mut errmsg: *mut libc::c_char = std::ptr::null_mut();
            let _ = sqlite3_exec(
                db,
                sql.as_ptr(),
                Some(collect_thread_rows_cb),
                &mut rows as *mut Vec<SqliteThreadRow> as *mut libc::c_void,
                &mut errmsg,
            );
            if !errmsg.is_null() {
                sqlite3_free(errmsg as *mut libc::c_void);
            }
            let _ = sqlite3_close(db);
        } else if !db.is_null() {
            let _ = sqlite3_close(db);
        }
    }
    rows
}
