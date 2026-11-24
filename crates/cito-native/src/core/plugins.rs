pub use super::config::*;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::env;
use std::ffi::CStr;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(test)]
use std::thread;
use std::time::{Duration, Instant};

pub const CITO_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug)]
pub struct PluginDescriptor {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub tier: String,
    pub order: i64,
    pub sf_symbol: String,
    pub tint: String,
    pub cli_flag: bool,
    pub pid_bound: bool,
    pub auto_listen: AutoListen,
    pub plugin_dir: PathBuf,
    pub is_builtin: bool,
    pub command: Vec<String>,
    pub channel_env: Option<String>,
    pub fields: Vec<Value>,
    pub post_setup_hint: Option<String>,
    pub post_disconnect_hint: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct AutoListen {
    pub mode: AutoListenMode,
    pub recent_window_ms: Option<u64>,
    pub idle_timeout_ms: Option<u64>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AutoListenMode {
    #[default]
    Endpoint,
    Activity,
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
        let user = match hubs.get(hub).and_then(|h| h.get("user")) {
            None | Some(Value::Null) => Some(global_user.as_str()),
            Some(user) => user.as_str(),
        };
        user.filter(|user| validate_user(user))
            .map(str::to_owned)
            .ok_or_else(|| format!("Invalid user for hub {hub}"))
    }
}

pub use crate::support::now_ms;

pub use crate::support::{random_hex, random_uuid_v4, try_random_hex};

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
    let expanded = crate::support::expand_home_path(value, Path::new(&home));
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

pub use validate_harness_id as validate_hub_name;

pub fn validate_nats_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed == "local" || trimmed == "local://" {
        return Ok("local://".to_string());
    }
    let invalid = "INVALID_INPUT: Hub URL must use local://, http:// or https:// without credentials, query or fragment";
    let url = url::Url::parse(trimmed).map_err(|_| invalid.to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
        || trimmed.chars().any(char::is_whitespace)
        || trimmed.contains('\\')
    {
        return Err(invalid.into());
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

/// Validate OIDC issuer or resource URLs without changing their canonical bytes.
pub fn validate_oidc_url(raw: &str, issuer: bool) -> Result<url::Url, String> {
    let url = url::Url::parse(raw).map_err(|_| "Invalid OIDC URL")?;
    let loopback = url.host_str() == Some("127.0.0.1");
    if raw != raw.trim()
        || url.host().is_none()
        || !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || issuer && (url.query().is_some() || raw.contains(['#', '|']))
    {
        return Err("OIDC URL must use HTTPS (HTTP 127.0.0.1 is allowed for tests), without credentials or fragment; issuers cannot contain a query or a vertical bar".into());
    }
    Ok(url)
}

pub fn validate_https_issuer(raw: &str) -> Result<String, String> {
    validate_oidc_url(raw, true)?;
    Ok(raw.to_owned())
}

/// Apply a process-local endpoint override only to the selected Hub.
pub fn apply_hub_url_override(name: &str, selected: &str, hub: &mut Value) -> Result<(), String> {
    if name == selected {
        if let Ok(raw) = env::var("CITO_HUB_URL").or_else(|_| env::var("CITO_NATS_URL")) {
            hub["url"] = json!(validate_nats_url(&raw)?);
        }
    }
    Ok(())
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
    builtin_plugin_roots(None, env::current_exe().ok().as_deref())
        .into_iter()
        .find(|path| path.is_dir())
}
pub(super) fn bundled_plugin_roots(root: &Path) -> [PathBuf; 4] {
    [
        root.join("Resources/plugins"),
        root.join("plugins"),
        root.join("share/cito/plugins"),
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins")),
    ]
}
fn builtin_plugin_roots(state_dir: Option<&Path>, executable: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(dir) = env::var("CITO_BUILTIN_PLUGINS_DIR") {
        if let Ok(path) = expand_home_dir(
            dir.trim(),
            "CITO_BUILTIN_PLUGINS_DIR",
            env::var("HOME").ok().as_deref(),
        ) {
            roots.push(path);
        }
    }
    if let Some(state_dir) = state_dir {
        roots.push(staged_plugins_dir(state_dir));
    }
    if let Some(dir) = executable.and_then(Path::parent) {
        roots.extend(bundled_plugin_roots(&dir.join("..")).into_iter().take(3));
    }
    roots.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins"));
    roots
}

pub fn parse_manifest_dir(dir: &Path, is_builtin: bool) -> Result<PluginDescriptor, String> {
    let raw = fs::read_to_string(dir.join("plugin.json")).map_err(|e| e.to_string())?;
    parse_manifest(
        dir,
        is_builtin,
        &serde_json::from_str(&raw).map_err(|e| e.to_string())?,
    )
}

pub(super) fn reserved_cli_flag(flag: &str) -> bool {
    matches!(
        flag,
        "hub"
            | "team"
            | "scope"
            | "tag"
            | "title"
            | "all"
            | "json"
            | "harness"
            | "session"
            | "help"
            | "version"
            | "setup"
            | "to"
            | "message"
            | "reply-to"
            | "in-reply-to"
            | "idempotency-key"
            | "wake"
            | "user"
            | "host"
            | "repo"
            | "query"
            | "limit"
            | "unread"
            | "peek"
            | "no-ack"
            | "ack-all"
            | "ack"
            | "about"
    )
}

pub(super) fn parse_manifest(
    dir: &Path,
    is_builtin: bool,
    manifest: &Value,
) -> Result<PluginDescriptor, String> {
    let dir_name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Invalid plugin dir")?;
    let Value::Object(map) = manifest else {
        return Err("Manifest is not an object".to_string());
    };
    let id = map
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| validate_harness_id(s))
        .ok_or_else(|| "Invalid plugin id".to_string())?
        .to_string();
    if id == "human" {
        return Err("PLUGIN_LOAD_ERROR: human is an internal companion".into());
    }
    if id != dir_name {
        return Err("Plugin id does not match directory name".to_string());
    }
    let display_name = map
        .get("displayName")
        .and_then(|v| v.as_str())
        .filter(|s| s.len() <= 64 && !s.chars().any(char::is_control))
        .map(str::trim)
        .filter(|s| !s.is_empty())
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
    let mut cli_flag = map
        .get("cliFlag")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if cli_flag && reserved_cli_flag(&id) {
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
        order: map.get("order").and_then(Value::as_i64).unwrap_or(i64::MAX),
        tier,
        sf_symbol,
        tint,
        cli_flag,
        pid_bound: map.get("pidBound").and_then(Value::as_bool).unwrap_or(true),
        auto_listen: {
            let value = map.get("autoListen");
            if value.is_some_and(|v| !v.is_object()) {
                return Err("PLUGIN_LOAD_ERROR: autoListen must be an object".into());
            }
            let mode = match value.and_then(|v| v.get("mode")) {
                None => AutoListenMode::Endpoint,
                Some(Value::String(mode)) if mode == "endpoint" => AutoListenMode::Endpoint,
                Some(Value::String(mode)) if mode == "activity" => AutoListenMode::Activity,
                _ => return Err("PLUGIN_LOAD_ERROR: Invalid autoListen mode".into()),
            };
            for key in ["recentWindowMs", "idleTimeoutMs"] {
                if value
                    .and_then(|v| v.get(key))
                    .is_some_and(|v| !v.is_null() && v.as_u64().is_none())
                {
                    return Err(format!("PLUGIN_LOAD_ERROR: Invalid autoListen {key}"));
                }
            }
            AutoListen {
                mode,
                recent_window_ms: value.and_then(|v| v["recentWindowMs"].as_u64()),
                idle_timeout_ms: value.and_then(|v| v["idleTimeoutMs"].as_u64()),
            }
        },
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

pub fn staged_plugins_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("runtime").join("plugins")
}

pub fn selected_plugin_dir(state_dir: &Path, plugin: &PluginDescriptor) -> PathBuf {
    if plugin.is_builtin {
        staged_plugins_dir(state_dir).join(&plugin.id)
    } else {
        plugin.plugin_dir.clone()
    }
}

pub(super) fn is_staged_plugins_root(root: &Path, state_dir: &Path) -> bool {
    [staged_plugins_dir(state_dir), state_dir.join("plugins")]
        .iter()
        .any(|staged| {
            root == staged
                || fs::canonicalize(root)
                    .ok()
                    .zip(fs::canonicalize(staged).ok())
                    .is_some_and(|(builtin, staged)| builtin == staged)
        })
}

/// Skip legacy copies and aliases even when discovery is read-only.
fn remove_legacy_staged_plugin(sub: &Path, cleanup: bool) -> bool {
    let read_manifest = |dir: &Path| {
        serde_json::from_slice::<serde_json::Map<String, Value>>(
            &fs::read(dir.join("plugin.json")).ok()?,
        )
        .ok()
    };
    let Some(mut manifest) = read_manifest(sub) else {
        return false;
    };
    let Some(id) = manifest.get("id").and_then(Value::as_str) else {
        return false;
    };
    if !validate_harness_id(id) || sub.file_name().and_then(|s| s.to_str()) != Some(id) {
        return false;
    }
    let bundled = super::setup::resolve_bundled_plugin_dir(id);
    let Some(mut shipped) = read_manifest(&bundled) else {
        return false;
    };
    let alias = same_plugin_root(sub, &bundled);
    if !alias && (manifest.remove("version") == shipped.remove("version") || manifest != shipped) {
        return false;
    }
    if cleanup {
        if fs::symlink_metadata(sub).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            // Unlink the entry itself, never the plugin directory it points to.
            let _ = fs::remove_file(sub);
        } else if !alias {
            let _ = fs::remove_dir_all(sub);
        }
    }
    true
}

pub(super) fn same_plugin_root(left: &Path, right: &Path) -> bool {
    left == right
        || fs::canonicalize(left)
            .ok()
            .zip(fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

pub fn cleanup_retired_companion(state_dir: &Path) -> Result<(), String> {
    for path in [
        state_dir.join("plugins/human"),
        state_dir.join("runtime/plugins/human"),
    ] {
        match fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "Cannot remove retired companion {}: {e}",
                    path.display()
                ))
            }
        }
    }
    Ok(())
}

pub fn load_plugins(state_dir: &Path) -> LoadedPlugins {
    load_plugins_with_cleanup(state_dir, true)
}

pub fn load_plugins_readonly(state_dir: &Path) -> LoadedPlugins {
    load_plugins_with_cleanup(state_dir, false)
}

pub fn plugin_roots_for_executable(
    state_dir: &Path,
    executable: Option<&Path>,
) -> Vec<(PathBuf, bool)> {
    let mut roots = Vec::new();
    if let Ok(extra) = env::var("CITO_PLUGIN_PATH") {
        for part in extra.split(':').filter(|part| !part.trim().is_empty()) {
            if let Ok(path) = expand_home_dir(
                part.trim(),
                "CITO_PLUGIN_PATH",
                env::var("HOME").ok().as_deref(),
            ) {
                roots.push((path, false));
            }
        }
    }
    if let Ok(dir) = env::var("CITO_PLUGIN_DIR") {
        roots.push((PathBuf::from(dir), false));
    }
    roots.push((state_dir.join("plugins"), false));
    roots.extend(
        builtin_plugin_roots(Some(state_dir), executable)
            .into_iter()
            .map(|path| (path, true)),
    );
    roots
}

fn load_plugins_with_cleanup(state_dir: &Path, cleanup_legacy: bool) -> LoadedPlugins {
    if cleanup_legacy {
        if let Err(error) = cleanup_retired_companion(state_dir) {
            eprintln!("{error}");
        }
    }
    let (builtin_roots, external_roots): (Vec<_>, Vec<_>) =
        plugin_roots_for_executable(state_dir, env::current_exe().ok().as_deref())
            .into_iter()
            .partition(|(_, builtin)| *builtin);
    let builtin_roots: Vec<_> = builtin_roots.into_iter().map(|(path, _)| path).collect();
    let external_roots: Vec<_> = external_roots.into_iter().map(|(path, _)| path).collect();
    load_plugins_from_roots(
        &external_roots,
        &builtin_roots,
        Some(&state_dir.join("plugins")),
        cleanup_legacy,
    )
}

pub(super) fn load_plugins_from_roots(
    external_roots: &[PathBuf],
    builtin_roots: &[PathBuf],
    user_root: Option<&Path>,
    cleanup_legacy: bool,
) -> LoadedPlugins {
    let mut plugins = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut rejected_ids = HashSet::new();
    for (root, is_builtin) in external_roots
        .iter()
        .map(|root| (root, false))
        .chain(builtin_roots.iter().map(|root| (root, true)))
    {
        let Ok(rd) = fs::read_dir(root) else { continue };
        let mut subdirs: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        subdirs.sort();
        let is_user_root =
            !is_builtin && user_root.is_some_and(|user| same_plugin_root(root, user));
        for sub in subdirs {
            if is_user_root && remove_legacy_staged_plugin(&sub, cleanup_legacy) {
                continue;
            }
            if sub.file_name().is_some_and(|name| name == "human")
                || fs::read(sub.join("plugin.json"))
                    .ok()
                    .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
                    .is_some_and(|manifest| manifest["id"].as_str() == Some("human"))
            {
                continue;
            }
            match parse_manifest_dir(&sub, is_builtin) {
                Ok(desc) => {
                    if seen_ids.insert(desc.id.clone()) {
                        plugins.push(desc);
                    }
                }
                Err(_) if !is_builtin => {
                    let id = sub.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    if !seen_ids.contains(id) {
                        rejected_ids.insert(id.to_string());
                    }
                }
                Err(_) => {}
            }
        }
    }
    // An invalid higher-priority directory must not poison a valid fallback.
    rejected_ids.retain(|id| !seen_ids.contains(id));
    plugins.sort_by_key(|p| {
        (
            if p.kind == "agent-harness" { 0 } else { 1 },
            p.order,
            match p.tier.as_str() {
                "stable" => 0,
                "experimental" => 1,
                _ => 2,
            },
            p.display_name.clone(),
        )
    });
    LoadedPlugins {
        plugins,
        rejected_ids,
    }
}

pub fn exit_cli_error(command: &str, error: &str) -> ! {
    eprintln!("{command}: {}", super::cli::presentation_error(error));
    std::process::exit(1)
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
    let mut cfg_daemon_port = None;
    let mut default_hub = env::var("CITO_HUB")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "local".to_string());

    if explicit_config.is_some() || config_path.exists() {
        let raw = fs::read_to_string(&config_path)
            .map_err(|_| format!("Cannot load hub configuration: {}", config_path.display()))?;
        let Value::Object(mut map) = serde_json::from_str::<Value>(&raw)
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
        cfg_daemon_port = map.remove("daemonPort");
    }

    let daemon_port = crate::support::resolve_daemon_port(
        env::var("CITO_DAEMON_PORT").ok().as_deref(),
        cfg_daemon_port.as_ref(),
    )?;

    let config = NativeConfig {
        state_dir,
        config_path,
        home_dir,
        daemon_port,
        user,
        default_hub,
    };
    let raw = read_config_text(&config.config_path)?;
    let (_, hubs, _) = try_normalize_existing_config(raw.as_deref())?;
    for hub in hubs.keys() {
        config.effective_user(hub)?;
    }
    let plugins = load_plugins(&config.state_dir);
    Ok((config, plugins))
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
    // Session identity belongs to each request, never to the daemon's launcher.
    for (key, _) in env::vars_os() {
        let name = key.to_string_lossy();
        if name.ends_with("_SESSION_ID")
            || name.ends_with("_SESSION_FILE")
            || name.ends_with("_SESSION_PATH")
            || name.ends_with("_THREAD_ID")
            || name == "CITO_ENDPOINT_PID"
        {
            command.env_remove(key);
        }
    }
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
        let mut request = request.clone();
        if request.is_object() && request.get("plugin_dir").is_none_or(Value::is_null) {
            let plugin_dir = if plugin.is_builtin {
                let state_dir = match request["state_dir"].as_str() {
                    Some(path) => PathBuf::from(path),
                    None => super::setup::resolve_state_and_config_paths()?.0,
                };
                selected_plugin_dir(&state_dir, plugin)
            } else {
                plugin.plugin_dir.clone()
            };
            request["plugin_dir"] = json!(plugin_dir);
        }
        let mut command = plugin_command(plugin)?;
        if let Some(env) = request["env"].as_object() {
            for (key, value) in env {
                if let Some(value) = value.as_str() {
                    command.env(key, value);
                }
            }
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut input = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        input.push(b'\n');
        let output = crate::support::run_piped_command(
            command,
            Some(&input),
            timeout,
            [1_048_576, 65_536],
            Some(&mut progress),
        )?
        .ok_or("Plugin timed out")?;
        if !output.status.success() {
            return Err(format!(
                "Plugin exited {}: {}",
                output.status.code().unwrap_or(-1),
                crate::support::bounded_diagnostic(
                    &String::from_utf8_lossy(&output.stderr),
                    "Plugin process failed"
                )
            ));
        }
        let output = std::str::from_utf8(&output.stdout).map_err(|e| e.to_string())?;
        let mut lines = output.lines();
        let line = lines.next().ok_or("Plugin produced no response")?;
        if lines.any(|line| !line.trim().is_empty()) {
            return Err("Plugin produced more than one response line".into());
        }
        let response: Value =
            serde_json::from_str(line).map_err(|_| "Invalid plugin JSON".to_owned())?;
        if !response.is_object() {
            return Err("Plugin response must be a JSON object".into());
        }
        Ok(response)
    };
    execute().map_err(|e| format!("PLUGIN_EXEC_FAILED: {}: {e}", plugin.id))
}

pub fn peek_plugin_exit(pid: u32) -> Result<Option<(bool, i32)>, String> {
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

/// Socket reads share one budget, including reads inside BufReader and read_exact.
fn read_http_until(stream: &mut TcpStream, deadline: Instant) -> Result<HttpResponse, String> {
    struct SocketReader<'a>(&'a mut TcpStream, Instant);
    impl Read for SocketReader<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let remaining = self.1.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            self.0.set_read_timeout(Some(remaining))?;
            self.0.read(bytes)
        }
    }
    let response = read_http_response(&mut SocketReader(stream, deadline))?;
    if Instant::now() >= deadline {
        return Err("HTTP response timed out".into());
    }
    Ok(response)
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
    let deadline = Instant::now() + timeout;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
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
    read_http_until(&mut stream, deadline)
}

#[derive(Debug)]
pub struct DaemonHealth {
    pub pid: i32,
    pub version: Option<String>,
}

const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

pub fn probe_daemon_health(config: &NativeConfig) -> Option<DaemonHealth> {
    probe_daemon_health_until(config, Instant::now() + HEALTH_PROBE_TIMEOUT)
}

fn probe_daemon_health_until(config: &NativeConfig, deadline: Instant) -> Option<DaemonHealth> {
    let addr = SocketAddr::from(([127, 0, 0, 1], config.daemon_port));
    let mut stream =
        TcpStream::connect_timeout(&addr, deadline.checked_duration_since(Instant::now())?).ok()?;
    stream
        .set_write_timeout(Some(deadline.checked_duration_since(Instant::now())?))
        .ok()?;
    let req = format!(
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        config.daemon_port
    );
    stream.write_all(req.as_bytes()).ok()?;
    let resp = read_http_until(&mut stream, deadline).ok()?;
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

#[cfg(test)]
#[test]
fn pid_alive_rejects_non_process_ids() {
    assert!(!pid_alive(0));
    assert!(!pid_alive(u32::MAX as i32));
    assert!(pid_alive(std::process::id() as i32));
}

pub fn pid_alive(pid: i32) -> bool {
    crate::support::alive(pid as u32)
}

pub fn stop_daemon(config: &NativeConfig, health: &DaemonHealth) -> Result<(), String> {
    let current = probe_daemon_health(config).ok_or("Daemon identity could not be revalidated")?;
    if current.pid != health.pid || current.version != health.version {
        return Err("Daemon identity changed before shutdown".to_string());
    }
    let exited = crate::support::child_exit_fd(health.pid as u32)?;
    // SAFETY: PID was obtained twice from the matching state directory's health endpoint.
    if unsafe { libc::kill(health.pid, libc::SIGTERM) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(format!("Cannot stop daemon: {e}"));
        }
    }
    let ready = crate::support::poll_until(
        &mut [libc::pollfd {
            fd: exited.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }],
        Some(Instant::now() + Duration::from_secs(5)),
    )?;
    if !ready {
        return Err("Daemon did not stop within 5000ms".into());
    }
    // Only the Child owner reaps. A health response does not convey waitpid ownership.
    Ok(())
}

/// Direct daemon entry and spawning clients use the same stale-version handover.
/// Returns true when a current daemon is already running.
pub fn handover_stale_daemon(config: &NativeConfig) -> Result<bool, String> {
    if let Some(existing) = probe_daemon_health(config) {
        if existing.version.as_deref() == Some(CITO_VERSION) {
            return Ok(true);
        }
        if let Err(error) = stop_daemon(config, &existing) {
            // A peer can complete the handover between either health probe and SIGTERM.
            // Never signal a replacement. An empty endpoint follows the cold-start path;
            // the daemon owner lock still arbitrates who gets to start.
            return match probe_daemon_health(config) {
                Some(h) if h.version.as_deref() == Some(CITO_VERSION) => Ok(true),
                None => Ok(false),
                _ => Err(error),
            };
        }
    }
    Ok(false)
}

pub fn ensure_daemon(config: &NativeConfig) -> Result<(), String> {
    if handover_stale_daemon(config)? {
        return Ok(());
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
    // Reserve a waiter before spawning so a thread allocation failure cannot orphan a child.
    let (handoff, receiver) = std::sync::mpsc::channel::<std::process::Child>();
    std::thread::Builder::new()
        .name("daemon-reaper".into())
        .spawn(move || {
            if let Ok(mut child) = receiver.recv() {
                let _ = child.wait();
            }
        })
        .map_err(|e| format!("Cannot start daemon waiter: {e}"))?;
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn daemon: {e}"))?;
    let result = (|| {
        let mut retry = crate::support::RetryBackoff::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let probe_deadline = deadline.min(Instant::now() + HEALTH_PROBE_TIMEOUT);
            if probe_daemon_health_until(config, probe_deadline)
                .is_some_and(|h| h.version.as_deref() == Some(CITO_VERSION))
            {
                return Ok(());
            }
            // Our child can lose the owner lock before the winner serves /health.
            // Leave a final probe window rather than sleeping out the readiness budget.
            let remaining = deadline.saturating_duration_since(Instant::now());
            let delay = (remaining / 2)
                .max(Duration::from_millis(10))
                .min(remaining);
            std::thread::sleep(retry.failed().min(delay));
        }
        Err(format!(
            "Daemon did not become healthy on 127.0.0.1:{} for {} within 5000ms",
            config.daemon_port,
            config.state_dir.display()
        ))
    })();
    let error = match result {
        Err(error) => error,
        Ok(()) => match handoff.send(child) {
            Ok(()) => return Ok(()),
            Err(e) => {
                child = e.0;
                "Daemon waiter is unavailable".into()
            }
        },
    };
    let _ = child.kill();
    let _ = child.wait();
    Err(error)
}

#[cfg(test)]
mod tests {
    #[test]
    fn builtin_cli_flags_cannot_be_claimed_by_plugin_manifests() {
        for id in [
            "hub",
            "team",
            "scope",
            "tag",
            "title",
            "all",
            "json",
            "harness",
            "session",
            "help",
            "version",
            "setup",
            "to",
            "message",
            "reply-to",
            "in-reply-to",
            "idempotency-key",
            "wake",
            "user",
            "host",
            "repo",
            "query",
            "limit",
            "unread",
            "peek",
            "no-ack",
            "ack-all",
            "ack",
            "about",
            "fixture",
        ] {
            let manifest = serde_json::json!({"id":id,"kind":"agent","displayName":"Fixture","cliFlag":true,"command":["fixture"]});
            let plugin = super::parse_manifest(std::path::Path::new(id), false, &manifest).unwrap();
            assert_eq!(plugin.cli_flag, id == "fixture", "{id}");
        }
    }
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn oidc_urls_preserve_queries_and_issuer_identity() {
        for issuer in [
            "https://identity",
            "https://[::1]/tenant",
            "https://id.example.com/tenant@team/",
            "http://127.0.0.1:8400",
        ] {
            assert_eq!(validate_https_issuer(issuer).unwrap(), issuer);
        }
        for endpoint in [
            "https://id.example.com/authorize?tenant=work&prompt=login",
            "https://identity/keys?version=2",
        ] {
            assert!(validate_oidc_url(endpoint, false).is_ok());
            assert!(validate_oidc_url(endpoint, true).is_err());
        }
        for issuer in ["https://identity/tenant|user", "http://127.0.0.1/path|user"] {
            assert!(validate_oidc_url(issuer, false).is_ok());
            assert!(validate_oidc_url(issuer, true).is_err());
        }
        for bad in [
            "https://alice@id.example.com",
            "https://id.example.com/#keys",
            "http://id.example.com",
            "http://localhost:9000",
            "http://127.0.0.2:9000",
        ] {
            assert!(validate_oidc_url(bad, false).is_err());
            assert!(validate_oidc_url(bad, true).is_err());
        }
    }

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
    fn external_priority_and_removal_preserve_builtin_fallback() {
        let root = env::temp_dir().join(format!("cito-plugin-priority-{}", random_hex(8)));
        let external: Vec<_> = ["first", "second", "user"]
            .iter()
            .map(|name| root.join(name))
            .collect();
        let builtin = root.join("bundled");
        for dir in external.iter().chain(std::iter::once(&builtin)) {
            let plugin = dir.join("sample");
            fs::create_dir_all(&plugin).unwrap();
            let name = dir.file_name().unwrap().to_str().unwrap();
            fs::write(
                plugin.join("plugin.json"),
                json!({"id":"sample","kind":"agent","tier":"stable",
                "displayName":name,"command":[format!("./{name}.sh")]})
                .to_string(),
            )
            .unwrap();
        }
        for (index, name) in ["first", "second", "user", "bundled"]
            .into_iter()
            .enumerate()
        {
            let loaded =
                load_plugins_from_roots(&external, std::slice::from_ref(&builtin), None, false);
            assert_eq!(loaded.plugins.len(), 1);
            let plugin = loaded.find_by_id("sample").unwrap();
            assert_eq!(plugin.display_name, name);
            assert_eq!(plugin.command, [format!("./{name}.sh")]);
            assert_eq!(plugin.is_builtin, index == 3);
            assert!(loaded.rejected_ids.is_empty());
            if index < 3 {
                fs::remove_dir_all(external[index].join("sample")).unwrap();
            }
        }
        // Invalid high-priority copies do not reject a valid lower-priority descriptor.
        let invalid = external[0].join("sample");
        fs::create_dir_all(&invalid).unwrap();
        fs::write(invalid.join("plugin.json"), "{").unwrap();
        let loaded =
            load_plugins_from_roots(&external, std::slice::from_ref(&builtin), None, false);
        assert!(loaded.find_by_id("sample").unwrap().is_builtin);
        assert!(!loaded.rejected_ids.contains("sample"));
        fs::remove_dir_all(builtin.join("sample")).unwrap();
        assert!(load_plugins_from_roots(&external, &[builtin], None, false)
            .rejected_ids
            .contains("sample"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upgrades_remove_only_unmodified_builtin_manifests() {
        let state = env::temp_dir().join(format!("cito-plugin-upgrade-{}", random_hex(8)));
        let root = state.join("plugins");
        let dir = root.join("pi");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plugin.json");
        let mut legacy: Value =
            serde_json::from_str(include_str!("../../../../plugins/pi/plugin.json")).unwrap();
        legacy["version"] = json!("0.0.0");
        for (field, value) in [
            ("command", json!(["./bin/cito-custom"])),
            ("command", json!(["./bin/cito-plugin-pi", "custom"])),
            ("postSetupHint", json!("Custom hint")),
            ("extra", json!(true)),
        ] {
            let mut custom = legacy.clone();
            custom[field] = value;
            fs::write(&path, custom.to_string()).unwrap();
            let loaded = load_plugins(&state);
            let plugin = loaded.find_by_id("pi").unwrap();
            assert!(!plugin.is_builtin);
            assert_eq!(plugin.plugin_dir, dir);
            assert_eq!(fs::read_to_string(&path).unwrap(), custom.to_string());
        }
        fs::write(&path, legacy.to_string()).unwrap();
        assert!(
            load_plugins_readonly(&state)
                .find_by_id("pi")
                .unwrap()
                .is_builtin
        );
        assert!(path.is_file());
        assert!(load_plugins(&state).find_by_id("pi").unwrap().is_builtin);
        assert!(!dir.exists());
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn internal_companion_ignores_external_directory_and_manifest_ids() {
        let root = env::temp_dir().join(format!("cito-companion-priority-{}", random_hex(8)));
        let external = root.join("external");
        let builtin = root.join("bundled");
        for (dir, id, display) in [
            (external.join("human"), "human", "External companion"),
            (external.join("alias"), "human", "Aliased companion"),
            (builtin.join("human"), "human", "Bundled companion"),
        ] {
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("plugin.json"),
                json!({"id":id,"kind":"human",
                "displayName":display,"command":["./run.sh"]})
                .to_string(),
            )
            .unwrap();
        }
        let loaded = load_plugins_from_roots(
            std::slice::from_ref(&external),
            std::slice::from_ref(&builtin),
            None,
            false,
        );
        assert!(loaded.find_by_id("human").is_none());
        assert!(external.join("human/plugin.json").is_file());
        let state = root.join("state");
        for path in [
            state.join("plugins/human"),
            state.join("runtime/plugins/human"),
        ] {
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("orphan"), "no manifest").unwrap();
        }
        cleanup_retired_companion(&state).unwrap();
        assert!(!state.join("plugins/human").exists());
        assert!(!state.join("runtime/plugins/human").exists());
        fs::remove_dir_all(root).unwrap();
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
        let state_dir = root.join("state");
        assert_eq!(selected_plugin_dir(&state_dir, &plugin), plugin.plugin_dir);
        let mut builtin = plugin.clone();
        builtin.is_builtin = true;
        assert_eq!(
            selected_plugin_dir(&state_dir, &builtin),
            state_dir.join("runtime/plugins/sample")
        );
        for provided in [None, Some(Value::Null), Some(json!(root.join("explicit")))] {
            let mut request = json!({"op":"setup_inspect","payload":"line\\nquoted \"value\""});
            if let Some(value) = &provided {
                request["plugin_dir"] = value.clone();
            }
            let mut expected = request.clone();
            if provided.as_ref().is_none_or(Value::is_null) {
                expected["plugin_dir"] = json!(plugin.plugin_dir);
            }
            assert_eq!(plugin_request(&plugin, &request).unwrap(), expected);
            assert_eq!(
                request.get("plugin_dir"),
                provided.as_ref(),
                "Caller payload must not change"
            );
        }
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
            assert!(plugin_request(&plugin, &json!({"op":"probe"})).is_err());
            fs::remove_dir_all(root).unwrap();
        }
        let (root, plugin) = fixture("printf '%06000d\\n' 1 >&2; exit 4");
        let error = plugin_request(&plugin, &json!({"op":"probe"})).unwrap_err();
        assert!(
            error.chars().count() < 600,
            "Subprocess stderr must stay bounded"
        );
        fs::remove_dir_all(root).unwrap();
        let (root, plugin) =
            fixture("printf '{\"ok\":false,\"error_code\":\"OFFLINE\",\"message\":\"Retry\"}\\n'");
        let envelope = plugin_request(&plugin, &json!({"op":"probe"})).unwrap();
        assert_eq!(
            envelope["ok"], false,
            "MCP still owns negative envelope routing"
        );
        assert_eq!(envelope["error_code"], "OFFLINE");
        fs::remove_dir_all(root).unwrap();
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
    fn manifest_auto_listen_is_optional_typed_and_independent_of_pid_binding() {
        let (root, plugin) = fixture("/bin/cat");
        assert_eq!(plugin.auto_listen.mode, AutoListenMode::Endpoint);
        assert_eq!(plugin.auto_listen.idle_timeout_ms, None);
        let path = plugin.plugin_dir.join("plugin.json");
        let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        value["pidBound"] = json!(false);
        value["autoListen"] =
            json!({"mode":"activity","recentWindowMs":900000,"idleTimeoutMs":3600000});
        fs::write(&path, value.to_string()).unwrap();
        let parsed = parse_manifest_dir(&plugin.plugin_dir, false).unwrap();
        assert!(!parsed.pid_bound);
        assert_eq!(parsed.auto_listen.mode, AutoListenMode::Activity);
        assert_eq!(parsed.auto_listen.idle_timeout_ms, Some(3_600_000));
        for invalid in [
            json!({"mode":"invalid"}),
            json!({"mode":false}),
            json!({"mode":null}),
            json!({"mode":1}),
            json!({"mode":[]}),
            json!({"mode":{}}),
            json!({"mode":"endpoint","idleTimeoutMs":-1}),
            json!("activity"),
        ] {
            value["autoListen"] = invalid;
            fs::write(&path, value.to_string()).unwrap();
            assert!(parse_manifest_dir(&plugin.plugin_dir, false).is_err());
        }
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
    fn handover_preserves_current_and_stops_stale_or_legacy_daemons() {
        use std::net::TcpListener;
        for version in [Some(CITO_VERSION), Some("0.0.0"), None] {
            let (root, _) = fixture("/bin/cat");
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
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
            if expected_current {
                assert!(child.try_wait().unwrap().is_none());
                child.kill().unwrap();
            }
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(
                child.wait().unwrap().signal(),
                Some(if expected_current {
                    libc::SIGKILL
                } else {
                    libc::SIGTERM
                })
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn http_response_deadline_applies_to_progressing_headers_and_body() {
        for (prefix, suffix) in [
            (
                "HTTP/1.1 200 OK\r\nX-Slow: ",
                "abcdefghi\r\nContent-Length: 0\r\n\r\n",
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n",
                "abcdefghijkl",
            ),
            (
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nc\r\n",
                "abcdefghijkl\r\n0\r\n\r\n",
            ),
        ] {
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (mut server, _) = listener.accept().unwrap();
            server.write_all(prefix.as_bytes()).unwrap();
            let writer = thread::spawn(move || {
                for b in suffix.bytes() {
                    if server.write_all(&[b]).is_err() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            });
            assert!(
                read_http_until(&mut client, Instant::now() + Duration::from_millis(45)).is_err()
            );
            drop(client);
            writer.join().unwrap();
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
