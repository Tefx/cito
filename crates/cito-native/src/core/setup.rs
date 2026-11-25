#[cfg(test)]
use crate::plugins::random_hex;
use crate::plugins::CITO_VERSION;
use crate::plugins::{
    expand_home_dir, selected_plugin_dir, staged_plugins_dir, try_random_hex, validate_harness_id,
    validate_hub_name, validate_nats_url, LoadedPlugins, PluginDescriptor,
};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct SetupAction {
    pub target: String,
    pub path: String,
    pub status: String,
    pub backup_path: Option<String>,
    pub detail: String,
}

impl SetupAction {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("target".to_string(), json!(self.target));
        m.insert("path".to_string(), json!(self.path));
        m.insert("status".to_string(), json!(self.status));
        if let Some(ref b) = self.backup_path {
            m.insert("backupPath".to_string(), json!(b));
        }
        m.insert("detail".to_string(), json!(self.detail));
        Value::Object(m)
    }
}

pub fn resolve_stable_package_root(raw: &str) -> String {
    if let Some(cellar_idx) = raw.find("/Cellar/") {
        let prefix = &raw[..cellar_idx];
        let after_cellar = &raw[cellar_idx + "/Cellar/".len()..];
        let mut parts = after_cellar.splitn(3, '/');
        if let (Some(name), Some(_ver)) = (parts.next(), parts.next()) {
            if !name.is_empty() {
                let tail = parts.next().map(|t| format!("/{t}")).unwrap_or_default();
                return format!("{prefix}/opt/{name}{tail}");
            }
        }
    }
    raw.to_string()
}

pub fn resolve_package_root() -> PathBuf {
    if let Ok(exe) = env::current_exe() {
        // Cargo executables live under a profile directory, not the package bin/.
        // OUT_DIR also covers an explicitly configured Cargo target directory.
        let checkout = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        if let Some(profile) = Path::new(env!("OUT_DIR")).ancestors().nth(3) {
            if exe.starts_with(profile) && checkout.join("Cargo.toml").is_file() {
                return checkout;
            }
        }
        if let Some(exe_dir) = exe.parent() {
            let parent = exe_dir.parent().unwrap_or(exe_dir);
            let raw = parent.to_string_lossy().to_string();
            let stable = PathBuf::from(resolve_stable_package_root(&raw));
            if stable.exists() {
                return stable;
            }
            return parent.to_path_buf();
        }
    }
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

pub fn resolve_bundled_plugin_dir(id: &str) -> PathBuf {
    bundled_plugin_dir(&resolve_package_root(), id)
}

use super::plugins::bundled_plugin_roots;

pub(super) fn resolve_bundled_plugins_root() -> Option<PathBuf> {
    bundled_plugin_roots(&resolve_package_root())
        .into_iter()
        .find(|dir| dir.is_dir())
}

fn bundled_plugin_dir(root: &Path, id: &str) -> PathBuf {
    bundled_plugin_roots(root)
        .into_iter()
        .map(|dir| dir.join(id))
        .find(|dir| dir.join("plugin.json").is_file())
        .unwrap_or_else(|| {
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins")).join(id)
        })
}

pub fn is_staged_plugin_intact(state_dir: &Path, plugin: &PluginDescriptor) -> bool {
    is_plugin_dir_intact(&selected_plugin_dir(state_dir, plugin), plugin)
}

pub(super) fn is_plugin_dir_intact(dir: &Path, plugin: &PluginDescriptor) -> bool {
    if !dir.join("plugin.json").is_file() {
        return false;
    }
    if plugin
        .command
        .first()
        .is_some_and(|cmd| !valid_plugin_command(dir, cmd))
    {
        return false;
    }
    !plugin.is_builtin
        || (fs::read(dir.join("plugin.json"))
            .ok()
            .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
            .is_some_and(|manifest| manifest["version"].as_str() == Some(CITO_VERSION))
            && contains_plugin_files(&resolve_bundled_plugin_dir(&plugin.id), dir))
}

fn valid_plugin_command(dir: &Path, cmd: &str) -> bool {
    !(cmd.starts_with("./") || cmd.starts_with("../"))
        || fs::metadata(dir.join(cmd))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn contains_plugin_files(source: &Path, target: &Path) -> bool {
    fn equal_bytes(a: &Path, b: &Path) -> std::io::Result<bool> {
        use std::io::Read;
        let (mut a, mut b) = (fs::File::open(a)?, fs::File::open(b)?);
        let (source, staged) = (a.metadata()?, b.metadata()?);
        if source.len() != staged.len()
            || (source.permissions().mode() & 0o111 != 0
                && staged.permissions().mode() & 0o111 == 0)
        {
            return Ok(false);
        }
        let (mut left, mut right) = ([0; 16 * 1024], [0; 16 * 1024]);
        loop {
            let n = a.read(&mut left)?;
            if n == 0 {
                return Ok(b.read(&mut right)? == 0);
            }
            b.read_exact(&mut right[..n])?;
            if left[..n] != right[..n] {
                return Ok(false);
            }
        }
    }
    if super::plugins::same_plugin_root(source, target) {
        return true;
    }
    let (Ok(entries), Ok(staged_entries)) = (fs::read_dir(source), fs::read_dir(target)) else {
        return false;
    };
    let mut count = 0;
    for entry in entries {
        count += 1;
        let Ok(entry) = entry else { return false };
        let staged = target.join(entry.file_name());
        let (Ok(kind), Ok(meta)) = (entry.file_type(), fs::symlink_metadata(&staged)) else {
            return false;
        };
        let path = entry.path();
        if kind != meta.file_type() {
            return false;
        }
        let equal = if kind.is_dir() {
            contains_plugin_files(&path, &staged)
        } else if kind.is_symlink() {
            fs::read_link(path)
                .is_ok_and(|link| fs::read_link(staged).is_ok_and(|other| link == other))
        } else {
            kind.is_file() && equal_bytes(&path, &staged).unwrap_or(false)
        };
        if !equal {
            return false;
        }
    }
    staged_entries.count() == count
}

pub fn resolve_state_and_config_paths() -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let home_env = env::var("HOME").ok();
    let home_dir = expand_home_dir("~", "HOME", home_env.as_deref())?;
    let raw_state = env::var("CITO_STATE_DIR").unwrap_or_else(|_| "~/.cito".to_string());
    let state_dir = expand_home_dir(&raw_state, "CITO_STATE_DIR", home_env.as_deref())?;
    let config_path = match env::var("CITO_CONFIG_PATH") {
        Ok(p) => expand_home_dir(&p, "CITO_CONFIG_PATH", home_env.as_deref())?,
        Err(_) => state_dir.join("config.json"),
    };
    let resolve = |p: &Path| crate::support::profile_target(p, |_| {});
    let target = resolve(&config_path)?;
    let mut writer = target.as_os_str().to_owned();
    writer.push(".lock");
    let writer = resolve(Path::new(&writer))?;
    let state = resolve(&state_dir)?;
    let overlaps = |lock: &Path| {
        [&target, &writer]
            .iter()
            .any(|p| p.starts_with(lock) || lock.starts_with(p))
    };
    for name in ["daemon.lock", "runtime/staging.lock"] {
        let lock = state.join(name);
        if overlaps(&lock) || resolve(&lock).is_ok_and(|p| overlaps(&p)) {
            return Err("Config path conflicts with an ownership lock".into());
        }
    }
    Ok((state_dir, config_path, home_dir))
}

pub fn ensure_cli_shims_native(
    state_dir: &Path,
    plugins: &LoadedPlugins,
) -> Result<Vec<PathBuf>, String> {
    ensure_plugins_staged(state_dir, plugins.plugins.iter().filter(|p| p.is_builtin))
}

/// Repair one app without changing any other app's staged installation.
pub fn ensure_single_plugin_staged(
    state_dir: &Path,
    plugin: &PluginDescriptor,
) -> Result<Vec<PathBuf>, String> {
    ensure_plugins_staged(state_dir, std::iter::once(plugin).filter(|p| p.is_builtin))
}

fn ensure_plugins_staged<'a>(
    state_dir: &Path,
    plugins: impl Iterator<Item = &'a PluginDescriptor>,
) -> Result<Vec<PathBuf>, String> {
    let runtime = state_dir.join("runtime");
    fs::create_dir_all(&runtime).map_err(|e| e.to_string())?;
    let _lock = crate::support::FileLock::acquire(&runtime.join("staging.lock"))?;
    super::plugins::cleanup_retired_companion(state_dir)?;
    let candidate = runtime.join(format!(".staging-{}", try_random_hex(12)?));
    fs::create_dir(&candidate).map_err(|e| e.to_string())?;
    let _cleanup = Candidate(candidate.clone());
    let bin_dir = candidate.join("bin");
    if state_dir.join("bin").exists() {
        copy_dir_recursive(&state_dir.join("bin"), &bin_dir)?;
    }
    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let exe_str = resolve_stable_package_root(&exe.to_string_lossy());
    let plugins_dir = staged_plugins_dir(state_dir);
    let mut written = Vec::new();
    let mut publications = Vec::new();
    for plugin in plugins {
        let target = plugins_dir.join(&plugin.id);
        if is_plugin_dir_intact(&target, plugin) {
            continue;
        }
        let bundled = resolve_bundled_plugin_dir(&plugin.id);
        let source = if bundled.join("plugin.json").is_file()
            && !super::plugins::same_plugin_root(&bundled, &target)
        {
            &bundled
        } else if plugin.plugin_dir.is_dir() {
            &plugin.plugin_dir
        } else {
            // No packaged or staged source remains to reconstruct this plugin.
            continue;
        };
        if super::plugins::same_plugin_root(source, &target) {
            continue;
        }
        let staged = candidate.join(&plugin.id);
        copy_dir_recursive(source, &staged)?;
        let parsed = super::plugins::parse_manifest_dir(&staged, true)?;
        if !valid_plugin_command(&staged, &parsed.command[0]) {
            return Err(format!(
                "PLUGIN_LOAD_ERROR: Invalid staged executable for {}",
                plugin.id
            ));
        }
        publications.push((staged, target.clone()));
        written.push(target);
    }
    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
    for bin_name in ["cito", "cito-mcp"] {
        let shim_path = bin_dir.join(bin_name);
        let entry = if bin_name == "cito-mcp" {
            format!("export CITO_ENDPOINT_PID=\"\u{24}{{CITO_ENDPOINT_PID:-\u{24}PPID}}\"\nif [ \"\u{24}#\" -eq 0 ]; then\n  exec {} mcp-stdio\nfi\n", shell_quote(&exe_str))
        } else {
            String::new()
        };
        let script = format!(
            "#!/bin/sh\nexport CITO_BUILTIN_PLUGINS_DIR={}\n{entry}exec {} \"\u{24}@\"\n",
            shell_quote(&plugins_dir.to_string_lossy()),
            shell_quote(&exe_str)
        );
        // Generated shims replace copied entries; resource symlinks stay untouched.
        match fs::symlink_metadata(&shim_path) {
            Ok(_) => fs::remove_file(&shim_path).map_err(|e| e.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        fs::write(&shim_path, script).map_err(|e| e.to_string())?;
        fs::set_permissions(&shim_path, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        written.push(state_dir.join("bin").join(bin_name));
    }
    // Validate every candidate and shim before changing any published generation.
    for (staged, target) in publications {
        publish_directory(&staged, &target)?;
    }
    publish_directory(&bin_dir, &state_dir.join("bin"))?;
    Ok(written)
}

struct Candidate(PathBuf);
impl Drop for Candidate {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&path).map_err(|e| e.to_string())?, &target)
                .map_err(|e| e.to_string())?;
        } else if metadata.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            fs::copy(&path, &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Publish a validated directory generation without exposing a missing target.
fn publish_directory(candidate: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if !target.exists() {
        return fs::rename(candidate, target).map_err(|e| e.to_string());
    }
    use std::os::unix::ffi::OsStrExt;
    let from =
        std::ffi::CString::new(candidate.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let to = std::ffi::CString::new(target.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: both paths are live NUL-terminated strings on the same filesystem.
    // Exchange preserves the old generation at candidate until cleanup on return.
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result = -1;
    if result != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

pub fn launch_agent_plist_path(home_dir: &Path) -> PathBuf {
    home_dir
        .join("Library")
        .join("LaunchAgents")
        .join("com.tefx.cito.daemon.plist")
}

pub fn inspect_launch_at_login(home_dir: &Path) -> bool {
    let plist = launch_agent_plist_path(home_dir);
    if let Ok(raw) = fs::read_to_string(plist) {
        raw.contains("com.tefx.cito.daemon") && raw.contains("<true/>")
    } else {
        false
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn set_launch_at_login(
    enabled: bool,
    state_dir: &Path,
    home_dir: &Path,
    plugins: &LoadedPlugins,
) -> Result<PathBuf, String> {
    let plist_path = launch_agent_plist_path(home_dir);
    if !enabled {
        match fs::remove_file(&plist_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        return Ok(plist_path);
    }
    ensure_cli_shims_native(state_dir, plugins)?;
    let bin_dir = state_dir.join("bin");
    let cito_bin = bin_dir.join("cito");
    let path_val = format!(
        "{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        bin_dir.display()
    );
    let config_path = match env::var("CITO_CONFIG_PATH") {
        Ok(raw) => expand_home_dir(&raw, "CITO_CONFIG_PATH", Some(&home_dir.to_string_lossy()))?,
        Err(_) => state_dir.join("config.json"),
    };
    let config_env = if config_path != state_dir.join("config.json") {
        format!(
            "<key>CITO_CONFIG_PATH</key><string>{}</string>",
            escape_xml(&config_path.to_string_lossy())
        )
    } else {
        String::new()
    };
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
           <key>Label</key>\n\
           <string>com.tefx.cito.daemon</string>\n\
           <key>ProgramArguments</key>\n\
           <array>\n\
             <string>{}</string>\n\
             <string>daemon</string>\n\
           </array>\n\
           <key>RunAtLoad</key>\n\
           <true/>\n\
           <key>KeepAlive</key>\n\
           <false/>\n\
           <key>EnvironmentVariables</key>\n\
           <dict>\n\
             <key>HOME</key>\n\
             <string>{}</string>\n\
             <key>PATH</key>\n\
             <string>{}</string>\n\
             <key>CITO_STATE_DIR</key>\n\
             <string>{}</string>\n\
             {config_env}\n\
           </dict>\n\
         </dict>\n\
         </plist>\n",
        escape_xml(&cito_bin.to_string_lossy()),
        escape_xml(&home_dir.to_string_lossy()),
        escape_xml(&path_val),
        escape_xml(&state_dir.to_string_lossy()),
    );
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    crate::support::atomic_write_with_mode(&plist_path, xml.as_bytes(), Some(0o644))?;
    Ok(plist_path)
}

pub(crate) fn mutate_file<F>(
    target: &str,
    path: &Path,
    dry_run: bool,
    mode: Option<u32>,
    transform: F,
) -> Result<SetupAction, String>
where
    F: FnOnce(Option<&str>) -> Result<(String, Option<String>, String), String>,
{
    // Validate before acquiring the shared mutation lock or creating directories.
    crate::plugins::read_config_text(path)?;
    let mut detail = String::new();
    let mut conflict = false;
    let canonical = crate::support::publication_target(path)?;
    // Share the daemon's canonical .lock while retaining plugin mutation's
    // private backups and .cito.lock protocol inside this exclusion boundary.
    let action = crate::plugins::with_config_lock_mode(&canonical, dry_run, || {
        crate::support::mutate_with_mode(
            &json!({"op":"setup_apply","dry_run":dry_run}),
            target,
            &canonical,
            mode,
            |old| {
                let (status, next, description) = transform(old)?;
                detail = description;
                conflict = status == "conflict";
                Ok(if status == "unchanged" || conflict {
                    old.map(str::to_owned)
                } else {
                    next
                })
            },
        )
    })?;
    Ok(SetupAction {
        target: target.into(),
        path: action["path"].as_str().unwrap_or("").into(),
        status: if conflict {
            "conflict"
        } else {
            action["status"].as_str().unwrap_or("unchanged")
        }
        .into(),
        backup_path: action["backupPath"].as_str().map(str::to_owned),
        detail,
    })
}

fn setup_request(
    plugin: &PluginDescriptor,
    op: &str,
    force: bool,
    dry_run: bool,
) -> Result<Value, String> {
    let (state_dir, _, _) = resolve_state_and_config_paths()?;
    let plugin_dir = selected_plugin_dir(&state_dir, plugin);
    crate::plugins::plugin_request(
        plugin,
        &json!({
            "op": op, "force": force, "dry_run": dry_run,
            "state_dir": state_dir, "plugin_dir": plugin_dir,
        }),
    )
}

pub fn try_inspect_plugin_setup(
    plugin: &PluginDescriptor,
) -> Result<(bool, bool, Vec<String>), String> {
    match setup_request(plugin, "setup_inspect", false, true) {
        Ok(response) if response["ok"] == true => Ok((
            response["installed"]
                .as_bool()
                .ok_or("Plugin inspection requires boolean installed")?,
            response["configured"]
                .as_bool()
                .ok_or("Plugin inspection requires boolean configured")?,
            response["paths"]
                .as_array()
                .ok_or("Plugin inspection requires paths array")?
                .iter()
                .map(|p| {
                    p.as_str()
                        .map(str::to_owned)
                        .ok_or("Plugin inspection requires string paths")
                })
                .collect::<Result<_, _>>()?,
        )),
        Ok(response) => Err(crate::support::response_diagnostic(
            &response,
            "Plugin inspection failed",
        )),
        Err(error) => Err(error),
    }
}

fn plugin_setup_actions(
    plugin: &PluginDescriptor,
    op: &str,
    force: bool,
    dry_run: bool,
) -> Result<Vec<SetupAction>, String> {
    let response = setup_request(plugin, op, force, dry_run)?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(crate::support::response_diagnostic(
            &response,
            "Plugin setup failed",
        ));
    }
    let actions = response
        .get("actions")
        .and_then(Value::as_array)
        .ok_or("Plugin setup returned no actions")?;
    actions
        .iter()
        .map(|action| {
            let text = |key: &str| {
                action
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("Plugin action missing {key}"))
            };
            let status = text("status")?;
            if !matches!(
                status.as_str(),
                "created" | "updated" | "unchanged" | "removed" | "missing" | "conflict"
            ) {
                return Err("Unknown plugin setup action status".into());
            }
            if op == "setup_remove" && status == "conflict" && !dry_run {
                return Err(format!(
                    "Plugin removal conflict in {}: {}",
                    text("path")?,
                    text("detail")?
                ));
            }
            Ok(SetupAction {
                target: text("target")?,
                path: text("path")?,
                status,
                backup_path: action
                    .get("backupPath")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                detail: text("detail")?,
            })
        })
        .collect()
}

pub fn apply_plugin_setup(
    plugin: &PluginDescriptor,
    force: bool,
    dry_run: bool,
) -> Result<Vec<SetupAction>, String> {
    plugin_setup_actions(plugin, "setup_apply", force, dry_run)
}

pub fn remove_plugin_setup(plugin: &PluginDescriptor) -> Result<Vec<SetupAction>, String> {
    plugin_setup_actions(plugin, "setup_remove", false, false)
}

// Discover conflicts across all integrations before any driver cleans its runtime.
fn remove_integrations(plugins: &LoadedPlugins) -> Result<Vec<SetupAction>, String> {
    let agents = plugins.agent_plugins();
    for plugin in &agents {
        for action in plugin_setup_actions(plugin, "setup_remove", false, true)? {
            if action.status == "conflict" {
                return Err(format!(
                    "Plugin removal conflict in {}: {}",
                    action.path, action.detail
                ));
            }
        }
    }
    let mut actions = Vec::new();
    for plugin in agents {
        actions.extend(remove_plugin_setup(plugin)?);
    }
    Ok(actions)
}

pub fn inspect_setup_json(
    state_dir: &Path,
    config_path: &Path,
    home_dir: &Path,
    plugins: &LoadedPlugins,
) -> Value {
    let config_exists = config_path.exists();
    let (default_hub, hubs_obj, rest_obj) = match read_normalized_config(config_path) {
        Ok(config) => config,
        Err(error) => return json!({"ok":false,"error":error,"configPath":config_path}),
    };
    let mut hubs_out = Map::new();
    for (k, v) in &hubs_obj {
        if let Some(m) = v.as_object() {
            let url = m
                .get("url")
                .or_else(|| m.get("natsUrl"))
                .and_then(|x| x.as_str())
                .unwrap_or("local://");
            let auth = m.get("authMode").and_then(|x| x.as_str()).unwrap_or("none");
            let enabled = m.get("enabled").and_then(|x| x.as_bool()) != Some(false);
            let mut h = Map::new();
            h.insert("url".to_string(), json!(url));
            h.insert("natsUrl".to_string(), json!(url));
            h.insert("authMode".to_string(), json!(auth));
            h.insert("enabled".to_string(), json!(enabled));
            if let Some(ds) = m.get("defaultScope").and_then(|x| x.as_str()) {
                h.insert("defaultScope".to_string(), json!(ds));
            }
            hubs_out.insert(k.clone(), Value::Object(h));
        }
    }
    let mut harnesses = Vec::new();
    for p in plugins.agent_plugins() {
        let (installed, configured, paths) = match try_inspect_plugin_setup(p) {
            Ok(inspection) => inspection,
            Err(error) => {
                harnesses.push(json!({"id":p.id,"displayName":p.display_name,"tier":p.tier,
                    "installed":null,"configured":null,"paths":[],"status":"error","error":crate::core::cli::presentation_error(&error)}));
                continue;
            }
        };
        if paths.is_empty() && !p.is_builtin {
            continue;
        }
        harnesses.push(json!({
            "id": p.id,
            "displayName": p.display_name,
            "tier": p.tier,
            "installed": installed,
            "configured": configured,
            "paths": paths,
        }));
    }
    let host_hub = rest_obj
        .get("host_hub")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_else(|| {
            let mut m = Map::new();
            m.insert("enabled".to_string(), json!(false));
            m.insert("port".to_string(), json!(4291));
            m.insert("token".to_string(), json!(""));
            m
        });
    let _ = state_dir;
    json!({
        "configPath": config_path.to_string_lossy(),
        "configExists": config_exists,
        "defaultHub": default_hub,
        "hubs": hubs_out,
        "harnesses": harnesses,
        "hostHub": host_hub,
        "launchAtLogin": inspect_launch_at_login(home_dir),
    })
}

use super::config::ConfigSnapshot;
use crate::plugins::try_normalize_existing_config;

mod app_bundle;
#[cfg(test)]
use app_bundle::build_mac_app_bundle_from_root;
pub use app_bundle::build_mac_app_bundle_native;
fn resolve_app_bundle_path(dest_dir: Option<&str>, state_dir: &Path) -> Result<PathBuf, String> {
    let base_dest = match dest_dir {
        Some(dest) => expand_home_dir(dest, "destDir", None)?,
        None => state_dir.join("Applications"),
    };
    Ok(if base_dest.to_string_lossy().ends_with(".app") {
        base_dest
    } else {
        base_dest.join("Cito.app")
    })
}

fn read_normalized_config(path: &Path) -> Result<super::config::NormalizedConfig, String> {
    crate::plugins::read_config_text(path)
        .and_then(|raw| try_normalize_existing_config(raw.as_deref()))
}

fn daemon_control_config(
    state_dir: &Path,
    config_path: &Path,
    home_dir: &Path,
    recovery: Option<&ConfigSnapshot>,
) -> Result<crate::plugins::NativeConfig, String> {
    let (default_hub, _, rest) = match recovery {
        Some(before) => before
            .normalized()
            .unwrap_or_else(|_| ("local".into(), Map::new(), before.recovery_object())),
        None => read_normalized_config(config_path)?,
    };
    // Setup can repair a profile with an invalid saved port using an explicit control endpoint.
    let endpoint = env::var("CITO_DAEMON_PORT").ok();
    let endpoint = endpoint.as_deref().filter(|s| !s.trim().is_empty());
    let daemon_port = match crate::support::resolve_daemon_port(
        endpoint,
        rest.get("daemonPort").filter(|_| endpoint.is_none()),
    ) {
        Err(_) if recovery.is_some() && endpoint.is_none() => crate::support::DEFAULT_DAEMON_PORT,
        port => port?,
    };
    Ok(crate::plugins::NativeConfig {
        state_dir: state_dir.to_path_buf(),
        config_path: config_path.to_path_buf(),
        home_dir: home_dir.to_path_buf(),
        daemon_port,
        user: String::new(),
        default_hub,
    })
}

// A serving daemon and offline cleanup must exclude each other through settlement.
pub(super) fn acquire_daemon_owner(state_dir: &Path) -> Result<fs::File, String> {
    fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    let owner = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(state_dir.join("daemon.lock"))
        .map_err(|e| e.to_string())?;
    // SAFETY: the returned descriptor owns the lock until drop.
    if unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Daemon owner lock is held".into());
    }
    Ok(owner)
}

pub(super) fn purge_disconnected_data(
    dir: &Path,
    state: Option<&super::state::LocalState>,
) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("oidc-token-")
        {
            fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    match fs::remove_dir_all(dir.join("activity")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("STATE_ERROR: Cannot remove session activity metadata".into()),
    }
    let Some(state) = state else {
        return Ok(());
    };
    state.db.transaction(|db| {
        db.execute("DELETE FROM local_sent_idempotency; DELETE FROM pending_hub_acks; DELETE FROM pending_hub_detaches; DELETE FROM wake_retries; DELETE FROM local_signals; DELETE FROM local_listeners; DELETE FROM session_custom_titles;", &[])?;
        Ok(())
    })
}

fn disconnect_offline(
    config: &crate::plugins::NativeConfig,
    before: &ConfigSnapshot,
    purge: bool,
) -> Result<Value, String> {
    let dir = &config.state_dir;
    let _owner = acquire_daemon_owner(dir)?;
    // An absent matching health response can also mean a foreign or unhealthy listener.
    let _endpoint = std::net::TcpListener::bind(("127.0.0.1", config.daemon_port))
        .map_err(|e| format!("Cannot reserve daemon endpoint for disconnect: {e}"))?;
    let target = crate::support::publication_target(&config.config_path)?;
    crate::plugins::with_config_lock(&target, || {
        before.check_current(&config.config_path)?;
        let state = dir
            .join("state.sqlite")
            .try_exists()
            .map_err(|e| e.to_string())?
            .then(|| super::state::LocalState::open(dir))
            .transpose()?;
        let plugins = crate::plugins::load_plugins_readonly(dir);
        remove_integrations(&plugins)?;
        let mut receipt = json!({"ok":true,"committed":true});
        if purge {
            set_launch_at_login(false, dir, &config.home_dir, &plugins)?;
            before.reset_hubs_locked(&config.config_path, &target)?;
            let clear = || {
                super::hub::HubEngine::reset_offline(dir)?;
                purge_disconnected_data(dir, state.as_ref())
            };
            clear().map_err(|e| {
                format!(
                    "RESET_INCOMPLETE: Configuration reset committed; local data purge failed: {e}"
                )
            })?;
        } else if let Some(state) = state {
            for row in state
                .active()?
                .into_iter()
                .filter(|row| row["harness"] != "human")
            {
                // Only the persisted binding can identify a remote cleanup target.
                if row["hub_url"].is_string() {
                    state.db.execute("INSERT OR REPLACE INTO pending_hub_detaches(hub,listener_id,hub_url) VALUES(?,?,?)", &[row["hub"].clone(), row["listener_id"].clone(), row["hub_url"].clone()])?;
                }
                state.pause_listener_for(&row)?;
                receipt["warning"] =
                    json!("Local bindings stopped; Hub detach could not be confirmed offline");
            }
        }
        Ok(receipt)
    })
}

pub fn uninstall_setup_native(
    state_dir: &Path,
    config_path: &Path,
    home_dir: &Path,
    plugins: &LoadedPlugins,
    purge_data: bool,
) -> Result<(String, Vec<SetupAction>), String> {
    uninstall_setup_with_owner(state_dir, config_path, home_dir, plugins, purge_data, None)
}
pub(crate) fn uninstall_setup_with_owner(
    state_dir: &Path,
    config_path: &Path,
    home_dir: &Path,
    plugins: &LoadedPlugins,
    purge_data: bool,
    owner: Option<&fs::File>,
) -> Result<(String, Vec<SetupAction>), String> {
    let config = daemon_control_config(
        state_dir,
        config_path,
        home_dir,
        Some(&ConfigSnapshot::read(config_path)?),
    )?;
    let daemon_port = config.daemon_port;
    let default_hub = config.default_hub.clone();
    if let Some(health) = owner
        .is_none()
        .then(|| crate::plugins::probe_daemon_health(&config))
        .flatten()
    {
        let response = crate::plugins::http_post_json(
            daemon_port,
            "/control",
            r#"{"op":"unlisten_all"}"#,
            Duration::from_millis(5000),
        )?;
        if response.status != 200
            || serde_json::from_str::<Value>(&response.body)
                .ok()
                .and_then(|v| v.get("ok").and_then(Value::as_bool))
                != Some(true)
        {
            return Err("Daemon refused unlisten_all; uninstall aborted".to_string());
        }
        crate::plugins::stop_daemon(&config, &health)?;
    }

    let _owner = owner
        .is_none()
        .then(|| acquire_daemon_owner(state_dir))
        .transpose()?;
    let state = fs::canonicalize(state_dir).map_err(|e| e.to_string())?;
    let resolved_config = crate::support::publication_target(config_path)?;
    let mut config_targets = vec![resolved_config.clone()];
    if purge_data {
        let default = crate::support::publication_target(&state.join("config.json"))?;
        if default.starts_with(&state) {
            config_targets.push(default);
        }
        config_targets.sort();
        config_targets.dedup();
    }
    let staging = state.join("runtime/staging.lock");
    let mut keep = vec![state.join("daemon.lock"), staging.clone()];
    for target in &config_targets {
        let mut name = target.as_os_str().to_owned();
        name.push(".lock");
        let name = PathBuf::from(name);
        if keep.contains(&name) || keep.contains(target) {
            return Err("Config path conflicts with an ownership lock".into());
        }
        keep.push(name);
    }
    let remove = || {
        let _staging = crate::support::FileLock::acquire(&staging)?;
        let mut keep = keep;
        if !purge_data {
            keep.extend(crate::support::profile_keep(config_path, &state)?);
            keep.push(resolved_config.clone());
        }
        let mut actions = remove_integrations(plugins)?;

        let plist_path = launch_agent_plist_path(home_dir);
        // Keep paths use physical parent directories, but retain symlink leaves.
        let physical_leaf = |p: &Path| -> Result<PathBuf, String> {
            Ok(
                fs::canonicalize(p.parent().ok_or("Missing parent directory")?)
                    .map_err(|e| e.to_string())?
                    .join(p.file_name().ok_or("Missing filename")?),
            )
        };
        if plist_path.exists() && !keep.contains(&physical_leaf(&plist_path)?) {
            fs::remove_file(&plist_path).map_err(|e| e.to_string())?;
            actions.push(SetupAction {
                target: "cito".to_string(),
                path: plist_path.to_string_lossy().to_string(),
                status: "updated".to_string(),
                backup_path: None,
                detail: "Removed LaunchAgent plist".to_string(),
            });
        }

        for (sub, label) in [
            ("bin", "Removed CLI shims directory"),
            ("runtime", "Removed staged runtime files"),
            ("sockets", "Removed local sockets directory"),
        ] {
            let dir = state.join(sub);
            if dir.exists() && remove_except(&dir, &keep).map_err(|e| e.to_string())? {
                actions.push(SetupAction {
                    target: "cito".to_string(),
                    path: dir.to_string_lossy().to_string(),
                    status: "updated".to_string(),
                    backup_path: None,
                    detail: label.to_string(),
                });
            }
        }
        for p in &plugins.plugins {
            if matches!(p.id.as_str(), "plugins" | "bin" | "runtime" | "sockets") {
                continue;
            }
            let dir = state.join(&p.id);
            if dir.exists() && remove_except(&dir, &keep).map_err(|e| e.to_string())? {
                actions.push(SetupAction {
                    target: p.id.clone(),
                    path: dir.to_string_lossy().into_owned(),
                    status: "updated".into(),
                    backup_path: None,
                    detail: "Removed plugin runtime data".into(),
                });
            }
        }

        let app_targets = [
            resolve_app_bundle_path(None, state_dir)?,
            resolve_app_bundle_path(None, home_dir)?,
        ];
        for (index, app_target) in app_targets.iter().enumerate() {
            if app_targets[..index].contains(app_target) {
                continue;
            }
            let is_symlink = fs::symlink_metadata(app_target)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            if is_symlink {
                if keep.contains(&physical_leaf(app_target)?) {
                    continue;
                }
                fs::remove_file(app_target).map_err(|e| e.to_string())?;
                actions.push(SetupAction {
                    target: "cito".to_string(),
                    path: app_target.to_string_lossy().to_string(),
                    status: "updated".to_string(),
                    backup_path: None,
                    detail: format!("Removed {} symlink", app_target.display()),
                });
            } else if purge_data && app_target.exists() {
                let path = physical_leaf(app_target)?;
                if !remove_except(&path, &keep).map_err(|e| e.to_string())? {
                    continue;
                }
                actions.push(SetupAction {
                    target: "cito".to_string(),
                    path: app_target.to_string_lossy().to_string(),
                    status: "updated".to_string(),
                    backup_path: None,
                    detail: format!("Removed {}", app_target.display()),
                });
            }
        }
        if purge_data {
            let config_path = resolved_config.as_path();
            if let Some(parent) = config_path.parent() {
                if let Some(cname) = config_path.file_name().and_then(|s| s.to_str()) {
                    let bak_prefix = format!("{cname}.bak.");
                    if parent.exists() {
                        for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
                            let entry = entry.map_err(|e| e.to_string())?;
                            let fname = entry.file_name().to_string_lossy().into_owned();
                            if fname == format!("{cname}.cito.lock")
                                || fname.starts_with(&bak_prefix)
                            {
                                fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
                            }
                        }
                    }
                }
            }
            if config_path.exists() && !config_path.starts_with(&state) {
                fs::remove_file(config_path).map_err(|e| e.to_string())?;
            }
            if state_dir.exists() {
                remove_except(&state, &keep).map_err(|e| e.to_string())?;
                actions.push(SetupAction {
                    target: "cito".to_string(),
                    path: state_dir.to_string_lossy().to_string(),
                    status: "updated".to_string(),
                    backup_path: None,
                    detail: "Removed local data while retaining ownership and writer locks"
                        .to_string(),
                });
            }
        }

        Ok((default_hub, actions))
    };
    crate::plugins::with_config_lock_mode(&config_targets[0], false, || {
        match config_targets.get(1) {
            Some(other) => crate::plugins::with_config_lock_mode(other, false, remove),
            None => remove(),
        }
    })
}

use crate::support::remove_except;

fn removal_action(target: &str, path: &Path, result: &Value, fallback: &str) -> SetupAction {
    let mut details = vec![result["notice"].as_str().unwrap_or(fallback).to_owned()];
    if let Some(status) = result["detach_status"].as_str() {
        details.push(format!("Detach status: {status}"));
    }
    let warnings: Vec<_> = result["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let summary = warnings.join("; ");
    let mut degraded = result["detach_status"] == "pending";
    for warning in warnings
        .into_iter()
        .chain(result["warning"].as_str().filter(|s| *s != summary))
    {
        degraded = true;
        let message = super::cli::presentation_error(warning);
        if !details.contains(&message) {
            details.push(message);
        }
    }
    SetupAction {
        target: target.into(),
        path: path.display().to_string(),
        status: if degraded { "warning" } else { "updated" }.into(),
        backup_path: None,
        detail: details.join("; "),
    }
}

fn disconnect_plugin(plugin: &PluginDescriptor, dry_run: bool) -> Result<Vec<SetupAction>, String> {
    if dry_run {
        return plugin_setup_actions(plugin, "setup_remove", false, true);
    }
    let (config, _) = crate::plugins::load_native_config()?;
    crate::plugins::ensure_daemon(&config)?;
    let result = super::cli::control_request(
        &config,
        &json!({"op":"setup_remove_app","harness":plugin.id}),
    )?;
    Ok(vec![removal_action(
        &plugin.id,
        &config.state_dir,
        &result,
        "Disconnected integration",
    )])
}
pub fn run_setup_cli(args: &[String]) {
    let run = || -> Result<bool, String> {
        let (state_dir, config_path, home_dir) = resolve_state_and_config_paths()?;

        let mut status_only = false;
        let mut dry_run = false;
        let mut force = false;
        let mut json_output = false;
        let mut uninstall = false;
        let mut invite: Option<String> = None;
        let mut print_invite = false;
        let mut disconnect_all = false;
        let mut purge_data = false;
        let mut set_default = false;
        let mut launch_at_login: Option<bool> = None;
        let mut build_app = false;
        let mut build_app_dest: Option<String> = None;
        let mut share_hub: Option<bool> = None;
        let mut share_port: Option<i64> = None;
        let mut hub_name: Option<String> = None;
        let mut hub_url: Option<String> = None;
        let mut hub_auth: Option<String> = None;
        let mut hub_user: Option<String> = None;
        let mut hub_token: Option<String> = None;
        let mut hub_issuer: Option<String> = None;
        let mut hub_client_id: Option<String> = None;
        let mut hub_redirect_port: Option<Value> = None;
        let mut enable_hubs: Vec<String> = Vec::new();
        let mut disable_hubs: Vec<String> = Vec::new();
        let mut remove_hubs: Vec<String> = Vec::new();
        let mut enable_harnesses: Vec<String> = Vec::new();
        let mut disable_harnesses: Vec<String> = Vec::new();

        let read_val = |arg: &str, idx: usize, flag: &str| -> Result<(String, usize), String> {
            let mut next = idx;
            let allow_empty = matches!(flag, "--issuer" | "--client-id" | "--redirect-port");
            let value = super::cli_value::read(args, &mut next, flag, allow_empty)?;
            let value = if arg.contains('=') && !matches!(flag, "--client-id" | "--issuer") {
                value.trim()
            } else {
                value
            };
            if value.is_empty() && !allow_empty {
                return Err(format!("Missing value for {flag}"));
            }
            Ok((value.to_owned(), next))
        };

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == "--status" {
                status_only = true;
            } else if arg == "--dry-run" {
                dry_run = true;
            } else if arg == "--force" {
                force = true;
            } else if arg == "--json" {
                json_output = true;
            } else if arg == "--uninstall" {
                uninstall = true;
            } else if arg == "--invite" || arg.starts_with("--invite=") {
                let (v, next_i) = read_val(arg, i, "--invite")?;
                invite = Some(v);
                i = next_i;
            } else if arg == "--print-invite" {
                print_invite = true;
            } else if arg == "--disconnect-all" {
                disconnect_all = true;
            } else if arg == "--purge-data" {
                purge_data = true;
            } else if arg == "--default" {
                set_default = true;
            } else if arg == "--login-item" {
                launch_at_login = Some(true);
            } else if arg == "--no-login-item" {
                launch_at_login = Some(false);
            } else if arg == "--share-hub" {
                share_hub = Some(true);
            } else if arg == "--no-share-hub" {
                share_hub = Some(false);
            } else if arg == "--share-port" || arg.starts_with("--share-port=") {
                let (v, next_i) = read_val(arg, i, "--share-port")?;
                share_port = Some(v.parse::<i64>().unwrap_or(-1));
                i = next_i;
            } else if arg == "--build-app" || arg.starts_with("--build-app=") {
                build_app = true;
                if let Some(rest) = arg.strip_prefix("--build-app=") {
                    if !rest.trim().is_empty() {
                        build_app_dest = Some(rest.trim().to_string());
                    }
                } else if let Some(next) = args.get(i + 1) {
                    if !next.starts_with('-') {
                        build_app_dest = Some(next.clone());
                        i += 1;
                    }
                }
            } else if arg == "--hub" || arg.starts_with("--hub=") {
                let (v, next_i) = read_val(arg, i, "--hub")?;
                hub_name = Some(v);
                i = next_i;
            } else if arg == "--url" || arg.starts_with("--url=") {
                let (v, next_i) = read_val(arg, i, "--url")?;
                hub_url = Some(v);
                i = next_i;
            } else if arg == "--user" || arg.starts_with("--user=") {
                let (v, next_i) = read_val(arg, i, "--user")?;
                if !crate::plugins::validate_user(&v) {
                    return Err("INVALID_INPUT: Invalid user".into());
                }
                hub_user = Some(v);
                i = next_i;
            } else if arg == "--auth" || arg.starts_with("--auth=") {
                let (v, next_i) = read_val(arg, i, "--auth")?;
                hub_auth = Some(crate::plugins::canonical_auth_mode(&v)?);
                i = next_i;
            } else if arg == "--token" || arg.starts_with("--token=") {
                let (v, next_i) = read_val(arg, i, "--token")?;
                hub_token = Some(v);
                i = next_i;
            } else if arg == "--issuer" || arg.starts_with("--issuer=") {
                let (v, next_i) = read_val(arg, i, "--issuer")?;
                hub_issuer = Some(v);
                i = next_i;
            } else if arg == "--client-id" || arg.starts_with("--client-id=") {
                let (v, next_i) = read_val(arg, i, "--client-id")?;
                hub_client_id = Some(v);
                i = next_i;
            } else if arg == "--redirect-port" || arg.starts_with("--redirect-port=") {
                let (v, next_i) = read_val(arg, i, "--redirect-port")?;
                hub_redirect_port = Some(if v.is_empty() {
                    Value::Null
                } else {
                    json!(v.parse::<u16>().map_err(|_| {
                        "INVALID_INPUT: redirect port must be an integer from 0 to 65535"
                    })?)
                });
                i = next_i;
            } else if arg == "--enable-hub" || arg.starts_with("--enable-hub=") {
                let (v, next_i) = read_val(arg, i, "--enable-hub")?;
                enable_hubs.push(v);
                i = next_i;
            } else if arg == "--disable-hub" || arg.starts_with("--disable-hub=") {
                let (v, next_i) = read_val(arg, i, "--disable-hub")?;
                disable_hubs.push(v);
                i = next_i;
            } else if arg == "--remove-hub" || arg.starts_with("--remove-hub=") {
                let (v, next_i) = read_val(arg, i, "--remove-hub")?;
                remove_hubs.push(v);
                i = next_i;
            } else if arg == "--enable" || arg.starts_with("--enable=") {
                let (v, next_i) = read_val(arg, i, "--enable")?;
                enable_harnesses.push(v);
                i = next_i;
            } else if arg == "--disable" || arg.starts_with("--disable=") {
                let (v, next_i) = read_val(arg, i, "--disable")?;
                disable_harnesses.push(v);
                i = next_i;
            } else if arg.starts_with('-') {
                return Err(format!("Unknown flag: {arg}"));
            } else {
                return Err(format!("Unexpected argument: {arg}"));
            }
            i += 1;
        }

        if purge_data && !(uninstall || disconnect_all) {
            return Err("--purge-data requires --uninstall or --disconnect-all".into());
        }
        for (present, flag) in [
            (hub_user.is_some(), "--user"),
            (set_default, "--default"),
            (hub_auth.is_some(), "--auth"),
            (hub_token.is_some(), "--token"),
            (hub_issuer.is_some(), "--issuer"),
            (hub_client_id.is_some(), "--client-id"),
            (hub_redirect_port.is_some(), "--redirect-port"),
        ] {
            if present && hub_url.is_none() {
                return Err(format!("{flag} requires --url"));
            }
        }
        if hub_name.is_some() && hub_url.is_none() {
            return Err("--hub requires --url".to_string());
        }

        if invite.is_some() || print_invite || disconnect_all {
            if usize::from(invite.is_some())
                + usize::from(print_invite)
                + usize::from(disconnect_all)
                != 1
                || uninstall
                || build_app
                || status_only
                || hub_url.is_some()
                || share_hub.is_some()
                || share_port.is_some()
                || !enable_hubs.is_empty()
                || !disable_hubs.is_empty()
                || !remove_hubs.is_empty()
                || !enable_harnesses.is_empty()
                || !disable_harnesses.is_empty()
                || launch_at_login.is_some()
                || set_default
                || (purge_data && !disconnect_all)
            {
                return Err("Invite and disconnect operations must be used separately from other setup operations".into());
            }
            let action = if let Some(invite) = invite {
                json!({"op":"paste_hub_invite","invite":invite})
            } else if print_invite {
                json!({"op":"get_snapshot"})
            } else {
                json!({"op":"setup_disconnect_all","purge_data":purge_data})
            };
            if dry_run {
                println!("{}", json!({"ok":true,"dry_run":true,"op":action["op"]}));
                return Ok(false);
            }
            let config = match crate::plugins::load_native_config() {
                Ok((config, _)) => {
                    crate::plugins::ensure_daemon(&config)?;
                    config
                }
                Err(error) if disconnect_all => {
                    let before = ConfigSnapshot::read(&config_path)?;
                    if !purge_data
                        && before
                            .normalized()
                            .and_then(|(_, hubs, rest)| {
                                crate::plugins::validate_saved_profile(
                                    &hubs,
                                    &before.recovery_object(),
                                )?;
                                crate::support::resolve_daemon_port(None, rest.get("daemonPort"))
                            })
                            .is_ok()
                    {
                        return Err(error);
                    }
                    let config =
                        daemon_control_config(&state_dir, &config_path, &home_dir, Some(&before))?;
                    // A replacement cannot start against a damaged profile. Prefer the
                    // running owner and its registrations, even across versions.
                    if crate::plugins::probe_daemon_health(&config).is_none() {
                        println!("{}", disconnect_offline(&config, &before, purge_data)?);
                        return Ok(false);
                    }
                    config
                }
                Err(error) => return Err(error),
            };
            let result = super::cli::control_request(&config, &action)?;
            if print_invite {
                if result["settings"]["host_hub"]["enabled"] != true {
                    return Err("Hub sharing is disabled".into());
                }
                let command = result["settings"]["host_hub"]["invite_command"]
                    .as_str()
                    .ok_or("Invite unavailable")?;
                if json_output {
                    println!("{}", json!({"ok":true,"invite_command":command}));
                } else {
                    println!("{command}");
                }
            } else {
                println!("{result}");
            }
            return Ok(false);
        }
        let plugins = crate::plugins::load_plugins_readonly(&state_dir);
        for h in enable_harnesses.iter().chain(&disable_harnesses) {
            if plugins.rejected_ids.contains(h) {
                return Err(format!(
                    "PLUGIN_LOAD_ERROR: Invalid plugin manifest for {h}"
                ));
            }
            if !validate_harness_id(h) || !plugins.agent_plugins().iter().any(|p| &p.id == h) {
                return Err(format!("Unknown harness: {h}"));
            }
        }
        if dry_run && (build_app || uninstall) {
            let before = uninstall
                .then(|| ConfigSnapshot::read(&config_path))
                .transpose()?;
            let current_default =
                daemon_control_config(&state_dir, &config_path, &home_dir, before.as_ref())?
                    .default_hub;
            let action = SetupAction {
                target: "cito".to_string(),
                path: if build_app {
                    resolve_app_bundle_path(build_app_dest.as_deref(), &state_dir)?
                        .to_string_lossy()
                        .into_owned()
                } else {
                    state_dir.to_string_lossy().into_owned()
                },
                status: "planned".to_string(),
                backup_path: None,
                detail: if build_app {
                    "Build application bundle".to_string()
                } else if purge_data {
                    "Unlisten all, stop daemon, remove integrations and purge local data"
                        .to_string()
                } else {
                    "Unlisten all, stop daemon and remove integrations".to_string()
                },
            };
            emit_setup_result(&config_path, &current_default, true, &[action], json_output);
            return Ok(false);
        }

        if status_only {
            let inspection = inspect_setup_json(&state_dir, &config_path, &home_dir, &plugins);
            if json_output {
                println!("{}", serde_json::to_string_pretty(&inspection).unwrap());
            } else {
                if inspection["ok"] == false {
                    return Err(inspection["error"]
                        .as_str()
                        .unwrap_or("Configuration inspection failed")
                        .into());
                }
                let exists_str = if config_path.exists() {
                    "exists"
                } else {
                    "default"
                };
                println!(
                    "Cito Configuration: {} ({exists_str})",
                    config_path.display()
                );
                println!(
                    "Default Hub: {}",
                    inspection
                        .get("defaultHub")
                        .and_then(|v| v.as_str())
                        .unwrap_or("local")
                );
                if let Some(hubs) = inspection.get("hubs").and_then(|v| v.as_object()) {
                    for (name, h) in hubs {
                        let u = h.get("url").and_then(|v| v.as_str()).unwrap_or("local://");
                        let a = h.get("authMode").and_then(|v| v.as_str()).unwrap_or("none");
                        let e = h.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                        println!("  - hub {name}: {u} (auth={a}, enabled={e})");
                    }
                }
                if let Some(hs) = inspection.get("harnesses").and_then(|v| v.as_array()) {
                    for h in hs {
                        let id = h.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let tier = h.get("tier").and_then(|v| v.as_str()).unwrap_or("stable");
                        if let Some(error) = h["error"].as_str() {
                            println!("  - harness {id} [{tier}]: status=unknown, error={error}");
                            continue;
                        }
                        let inst = h
                            .get("installed")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let cfg = h
                            .get("configured")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        println!("  - harness {id} [{tier}]: installed={inst}, configured={cfg}");
                    }
                }
            }
            if inspection["ok"] == false {
                std::process::exit(1);
            }
            return Ok(false);
        }

        if uninstall && !build_app {
            let (default_hub, actions) =
                uninstall_setup_native(&state_dir, &config_path, &home_dir, &plugins, purge_data)
                    .inspect_err(|e| {
                    if json_output {
                        println!(
                            "{}",
                            json!({"ok":false,"error":super::cli::presentation_error(e)})
                        );
                    }
                })?;
            emit_setup_result(&config_path, &default_hub, false, &actions, json_output);
            return Ok(false);
        }
        let raw_config = crate::plugins::read_config_text(&config_path)?;
        let (default_hub, existing_hubs, rest) =
            try_normalize_existing_config(raw_config.as_deref())?;
        let host_edit = share_hub.is_some() || share_port.is_some();
        let refresh_apps =
            !host_edit || !enable_harnesses.is_empty() || !disable_harnesses.is_empty();
        let reconcile = |built_app: Option<&Path>| -> Result<(), String> {
            let config = daemon_control_config(&state_dir, &config_path, &home_dir, None)?;
            if let Some(existing) = crate::plugins::probe_daemon_health(&config) {
                if let Some(app) = built_app {
                    crate::plugins::stop_daemon(&config, &existing)?;
                    // status runs ensure_daemon from the published helper, even if setup
                    // itself was invoked from a checkout or a different installation.
                    let status = Command::new(app.join("Contents/Helpers/cito-native"))
                        .arg("status")
                        .stdout(Stdio::null())
                        .status()
                        .map_err(|e| e.to_string())?;
                    if !status.success() {
                        return Err(format!("Rebuilt application status failed: {status}"));
                    }
                } else {
                    crate::plugins::ensure_daemon(&config)?;
                }
                for op in [
                    host_edit.then_some("set_host_hub"),
                    refresh_apps.then_some("refresh_available"),
                ]
                .into_iter()
                .flatten()
                {
                    let result = super::cli::control_request(&config, &json!({"op":op}))?;
                    if let Some(warning) = result["warning"]
                        .as_str()
                        .or_else(|| result["maintenance_warning"].as_str())
                    {
                        return Err(warning.to_owned());
                    }
                }
            }
            Ok(())
        };
        if build_app {
            let (app_path, bytes) =
                build_mac_app_bundle_native(build_app_dest.as_deref(), &state_dir, &plugins)?;
            reconcile(Some(&app_path))
                .map_err(|e| format!("Application built; daemon update incomplete: {e}"))?;
            if json_output {
                let out = json!({
                    "appPath": app_path.to_string_lossy(),
                    "bytes": bytes,
                });
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
            } else {
                println!("Built {} ({} bytes)", app_path.display(), bytes);
            }
            return Ok(true);
        }

        let only_disable = !disable_harnesses.is_empty()
            && enable_harnesses.is_empty()
            && hub_url.is_none()
            && enable_hubs.is_empty()
            && disable_hubs.is_empty()
            && remove_hubs.is_empty()
            && launch_at_login.is_none()
            && share_hub.is_none()
            && share_port.is_none();

        if only_disable {
            let mut actions = Vec::new();
            for h in &disable_harnesses {
                if let Some(p) = plugins.find_by_id(h) {
                    actions.extend(disconnect_plugin(p, dry_run)?);
                }
            }
            if !dry_run {
                reconcile(None)?;
            }
            emit_setup_result(&config_path, &default_hub, dry_run, &actions, json_output);
            return Ok(!dry_run);
        }

        for n in enable_hubs
            .iter()
            .chain(disable_hubs.iter())
            .chain(remove_hubs.iter())
        {
            if !validate_hub_name(n) {
                return Err(format!("Invalid hub name: {n}"));
            }
        }
        for n in &remove_hubs {
            if n == "local" {
                return Err("Cannot remove reserved 'local' hub; disable it instead".to_string());
            }
        }

        let mut validated_hub: Option<(String, Value)> = None;
        if let Some(url_raw) = &hub_url {
            if let Some(name) = hub_name.as_deref().filter(|s| !validate_hub_name(s)) {
                return Err(format!("Invalid hub name: {name}"));
            }
            let url = validate_nats_url(url_raw)?;
            if hub_name.as_deref() == Some("local") && url != "local://" {
                return Err("INVALID_INPUT: The local hub must use local://".into());
            }
            let def_scope = if url == "local://" {
                "workspace"
            } else {
                "self"
            };
            let mut entry = Map::new();
            entry.insert("url".to_string(), json!(url));
            if let Some(ref user) = hub_user {
                entry.insert("user".into(), json!(user));
            }
            if let Some(ref auth) = hub_auth {
                entry.insert("authMode".into(), json!(auth));
            }
            if let Some(ref t) = hub_token {
                entry.insert("token".to_string(), json!(t.trim()));
            }
            if let Some(ref iss) = hub_issuer {
                entry.insert(
                    "issuer".to_string(),
                    if iss.is_empty() {
                        Value::Null
                    } else {
                        json!(iss)
                    },
                );
            }
            if let Some(ref cid) = hub_client_id {
                entry.insert(
                    "clientId".to_string(),
                    if cid.is_empty() {
                        Value::Null
                    } else {
                        json!(cid)
                    },
                );
            }
            entry.insert("enabled".to_string(), json!(true));
            entry.insert("defaultScope".to_string(), json!(def_scope));
            if let Some(port) = hub_redirect_port {
                entry.insert("redirectPort".into(), port);
            }
            let health = super::config::probe_hub_health(&url, true);
            let name = hub_name.clone().unwrap_or_else(|| {
                super::config::available_hub_name(
                    super::config::advertised_name(&health).map(str::to_owned),
                    Some(&existing_hubs),
                    &entry["url"],
                )
            });
            let base = existing_hubs.get(&name).unwrap_or(&Value::Null);
            let entry = super::config::merge_hub_config(base, &Value::Object(entry));
            validated_hub = Some((name, super::config::apply_hub_auth(entry, &health)?));
        }

        let editing_endpoints = validated_hub.is_some()
            || !enable_hubs.is_empty()
            || !disable_hubs.is_empty()
            || !remove_hubs.is_empty()
            || host_edit;
        // An existing ephemeral listener has no usable port in the file. Ask
        // its owner before taking the config lock; never start a daemon here.
        let live_port = if editing_endpoints
            && share_hub != Some(false)
            && share_port.is_none()
            && rest
                .get("host_hub")
                .is_some_and(|host| host["enabled"] == true && host["port"] == 0)
        {
            let control = daemon_control_config(&state_dir, &config_path, &home_dir, None)?;
            if crate::plugins::probe_daemon_health(&control).is_some() {
                let snapshot =
                    super::cli::control_request(&control, &json!({"op":"get_snapshot"}))?;
                let host = &snapshot["settings"]["host_hub"];
                host["port"]
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .filter(|_| host["running"] == true)
            } else {
                None
            }
        } else {
            None
        };
        let mut resolved_default_hub = "local".to_string();
        let config_action = mutate_file("cito", &config_path, dry_run, Some(0o600), |existing| {
            let (mut def_hub, mut hubs, mut rest) = try_normalize_existing_config(existing)?;
            let had_remote_before = hubs.iter().any(|(k, v)| {
                k != "local"
                    && v.get("url")
                        .or_else(|| v.get("natsUrl"))
                        .and_then(|x| x.as_str())
                        != Some("local://")
            });

            if (validated_hub.is_some() || live_port.is_some()) && existing != raw_config.as_deref()
            {
                return Err("INVALID_INPUT: Configuration changed during discovery; retry".into());
            }
            if let Some((ref hname, ref hentry)) = validated_hub {
                hubs.insert(hname.clone(), hentry.clone());
                if set_default || (!had_remote_before && def_hub == "local" && hname != "local") {
                    def_hub = hname.clone();
                }
            }
            for n in &enable_hubs {
                let Some(Value::Object(ref mut m)) = hubs.get_mut(n) else {
                    return Err(format!("Unknown hub: {n}"));
                };
                m.remove("disabled");
                m.insert("enabled".to_string(), json!(true));
            }
            for n in &disable_hubs {
                let Some(Value::Object(ref mut m)) = hubs.get_mut(n) else {
                    return Err(format!("Unknown hub: {n}"));
                };
                m.remove("disabled");
                m.insert("enabled".to_string(), json!(false));
            }
            for n in &remove_hubs {
                if hubs.remove(n).is_none() {
                    return Err(format!("Unknown hub: {n}"));
                }
            }

            if share_hub.is_some() || share_port.is_some() {
                let mut patch = json!({});
                if let Some(enabled) = share_hub {
                    patch["enabled"] = json!(enabled);
                }
                if let Some(port) = share_port {
                    patch["port"] = json!(port);
                }
                let host = super::config::apply_host_hub_patch(
                    rest.get("host_hub").unwrap_or(&Value::Null),
                    &patch,
                )?;
                rest.insert("host_hub".into(), host);
            }

            let mut next_obj = Map::new();
            next_obj.insert("default_hub".to_string(), json!(def_hub));
            next_obj.insert("hubs".to_string(), Value::Object(hubs));
            for (k, v) in rest {
                next_obj.insert(k, v);
            }
            let mut next = Value::Object(next_obj);
            super::config::repair_default_hub(&mut next);
            if editing_endpoints {
                super::daemon::host_hub::validate_hub_edit(
                    &next,
                    live_port,
                    validated_hub
                        .as_ref()
                        .map(|(name, _)| name.as_str())
                        .or_else(|| enable_hubs.first().map(String::as_str)),
                )?;
            }
            resolved_default_hub = next["default_hub"].as_str().unwrap_or("local").to_owned();
            let next_content = format!("{}\n", serde_json::to_string_pretty(&next).unwrap());
            match existing {
                None => Ok((
                    "created".to_string(),
                    Some(next_content),
                    format!(
                        "Created {} (default_hub: {resolved_default_hub})",
                        config_path.display()
                    ),
                )),
                Some(ex) if ex == next_content => Ok((
                    "unchanged".to_string(),
                    None,
                    format!("{} already up to date", config_path.display()),
                )),
                Some(_) => Ok((
                    "updated".to_string(),
                    Some(next_content),
                    format!(
                        "Updated {} (default_hub: {resolved_default_hub})",
                        config_path.display()
                    ),
                )),
            }
        })?;

        let committed = !dry_run && matches!(config_action.status.as_str(), "created" | "updated");
        let setup_error = |error: String| {
            if committed {
                format!(
                    "Configuration saved; setup incomplete: {}",
                    super::cli::presentation_error(&error)
                )
            } else {
                error
            }
        };
        let saved_error = |error: String| {
            format!(
                "Configuration saved; daemon update incomplete: {}",
                super::cli::presentation_error(&error)
            )
        };

        let mut actions = vec![config_action];
        if !dry_run && !remove_hubs.is_empty() {
            let (config, _) = crate::plugins::load_native_config().map_err(&saved_error)?;
            crate::plugins::ensure_daemon(&config).map_err(&saved_error)?;
            for hub in &remove_hubs {
                let result = super::cli::control_request(
                    &config,
                    &json!({"op":"setup_remove_hub","hub":hub}),
                )
                .map_err(&saved_error)?;
                actions.push(removal_action(hub, &config_path, &result, "Removed hub"));
            }
        }

        let bare_setup = enable_harnesses.is_empty()
            && disable_harnesses.is_empty()
            && hub_url.is_none()
            && enable_hubs.is_empty()
            && disable_hubs.is_empty()
            && remove_hubs.is_empty()
            && launch_at_login.is_none()
            && share_hub.is_none()
            && share_port.is_none();
        let plugins = if dry_run {
            plugins
        } else {
            ensure_cli_shims_native(&state_dir, &plugins).map_err(setup_error)?;
            crate::plugins::load_plugins_readonly(&state_dir)
        };
        let enabled_set: HashSet<String> = enable_harnesses.into_iter().collect();
        let disabled_set: HashSet<String> = disable_harnesses.into_iter().collect();

        for plugin in plugins.agent_plugins() {
            if disabled_set.contains(&plugin.id) {
                actions.extend(disconnect_plugin(plugin, dry_run).map_err(setup_error)?);
                continue;
            }
            let should_cfg = should_setup_harness(plugin, &enabled_set, bare_setup);
            if !should_cfg {
                let (paths, inspection_error) = match try_inspect_plugin_setup(plugin) {
                    Ok((_, _, paths)) => (paths, None),
                    Err(error) => (Vec::new(), Some(error)),
                };
                if paths.is_empty() && !plugin.is_builtin && inspection_error.is_none() {
                    continue;
                }
                actions.push(SetupAction {
                    target: plugin.id.clone(),
                    path: paths.first().cloned().unwrap_or_else(|| plugin.id.clone()),
                    status: if inspection_error.is_some() {
                        "warning"
                    } else {
                        "skipped"
                    }
                    .into(),
                    backup_path: None,
                    detail: inspection_error
                        .map(|error| {
                            format!(
                                "Plugin inspection failed: {}",
                                super::cli::presentation_error(&error)
                            )
                        })
                        .unwrap_or_else(|| {
                            format!(
                                "Skipped {} harness '{}' (pass --enable {} to configure)",
                                plugin.tier, plugin.id, plugin.id
                            )
                        }),
                });
                continue;
            }
            actions.extend(apply_plugin_setup(plugin, force, dry_run).map_err(setup_error)?);
        }

        if !dry_run {
            if let Some(en) = launch_at_login {
                set_launch_at_login(en, &state_dir, &home_dir, &plugins).map_err(setup_error)?;
            }
            reconcile(None).map_err(setup_error)?;
        }

        emit_setup_result(
            &config_path,
            &resolved_default_hub,
            dry_run,
            &actions,
            json_output,
        );
        Ok(!dry_run)
    };

    match run() {
        Ok(true) => {
            // Cleanup also requires the resulting native configuration to be valid.
            let _ = crate::plugins::load_native_config();
        }
        Ok(false) => {}
        Err(err) => crate::plugins::exit_cli_error("cito setup", &err),
    }
}

/// Only the bare CLI setup path opts into all stable harnesses. UI/config callers pass false.
pub fn should_setup_harness(
    plugin: &PluginDescriptor,
    enabled: &HashSet<String>,
    bare_cli: bool,
) -> bool {
    enabled.contains(&plugin.id) || (bare_cli && enabled.is_empty() && plugin.tier == "stable")
}

fn emit_setup_result(
    config_path: &Path,
    default_hub: &str,
    dry_run: bool,
    actions: &[SetupAction],
    json_output: bool,
) {
    let ok = !actions.iter().any(|a| a.status == "conflict");
    if json_output {
        let out = json!({
            "ok": ok,
            "dryRun": dry_run,
            "configPath": config_path.to_string_lossy(),
            "defaultHub": default_hub,
            "actions": actions.iter().map(|a| a.to_json()).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        for a in actions {
            println!("[{}] {}: {} ({})", a.status, a.target, a.path, a.detail);
        }
    }
    if !ok {
        for conflict in actions.iter().filter(|a| a.status == "conflict") {
            eprintln!(
                "cito setup: conflict in {}: {}",
                conflict.path, conflict.detail
            );
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, LoadedPlugins) {
        let root = env::temp_dir().join(format!("cito-setup-test-{}", random_hex(8)));
        let plugin_dir = root.join("plugins/sample");
        fs::create_dir_all(plugin_dir.join("bin")).unwrap();
        fs::write(plugin_dir.join("plugin.json"), r#"{"id":"sample","kind":"agent-harness","displayName":"Sample","command":["./bin/sample"]}"#).unwrap();
        fs::write(
            plugin_dir.join("bin/sample"),
            "#!/bin/sh\nprintf '{\"ok\":true,\"actions\":[]}\\n'\n",
        )
        .unwrap();
        fs::set_permissions(
            plugin_dir.join("bin/sample"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let plugin = crate::plugins::load_plugins(&root)
            .find_by_id("sample")
            .unwrap()
            .clone();
        (
            root,
            LoadedPlugins {
                plugins: vec![plugin],
                rejected_ids: HashSet::new(),
            },
        )
    }

    #[test]
    fn setup_status_reports_inspection_errors_instead_of_false_absence() {
        let (root, plugins) = fixture();
        fs::write(plugins.plugins[0].plugin_dir.join("bin/sample"), "#!/bin/sh\nprintf '%s\\n' '{\"ok\":false,\"error_code\":\"PLUGIN_EXEC_FAILED\",\"message\":\"Inspection timed out (os error 60)\"}'\n").unwrap();
        let result = cli_child(&root, &root, &["--status"]);
        let output = String::from_utf8_lossy(&result.stdout);
        assert!(result.status.success(), "{:?}", result);
        assert!(
            output.contains("status=unknown, error=Inspection timed out"),
            "{output}"
        );
        assert!(
            !output.lines().any(|line| line.contains("harness sample")
                && line.contains("installed=false, configured=false")),
            "{output}"
        );
        assert!(!output.contains("os error"), "{output}");
        let plugin = &plugins.plugins[0];
        let driver = plugin.plugin_dir.join("bin/sample");
        for (key, value) in [
            ("installed", json!(0)),
            ("configured", json!("no")),
            ("paths", json!(null)),
            ("paths", json!(["settings", 2])),
        ] {
            let mut response = json!({"ok":true,"installed":true,"configured":false,"paths":[]});
            response[key] = value;
            fs::write(&driver, format!("#!/bin/sh\necho '{response}'\n")).unwrap();
            assert!(try_inspect_plugin_setup(plugin).unwrap_err().contains(key));
        }
        fs::write(&driver,"#!/bin/sh\necho '{\"ok\":true,\"actions\":[{\"target\":\"sample\",\"status\":\"warning\",\"path\":\"settings\",\"detail\":\"fixture\"}]}'\n").unwrap();
        assert!(remove_plugin_setup(plugin).unwrap_err().contains("status"));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn validated_plugin_and_shim_generations_preserve_working_files_on_failure() {
        let (root, mut plugins) = fixture();
        plugins.plugins[0].is_builtin = true;
        let second = root.join("plugins/second");
        fs::create_dir_all(second.join("bin")).unwrap();
        fs::copy(
            plugins.plugins[0].plugin_dir.join("bin/sample"),
            second.join("bin/second"),
        )
        .unwrap();
        fs::write(second.join("plugin.json"),r#"{"id":"second","kind":"agent-harness","displayName":"Second","command":["./bin/second"]}"#).unwrap();
        let mut descriptor = crate::plugins::load_plugins(&root)
            .find_by_id("second")
            .unwrap()
            .clone();
        descriptor.is_builtin = true;
        plugins.plugins.push(descriptor);
        let state = root.join("installed");
        ensure_cli_shims_native(&state, &plugins).unwrap();
        let staged = staged_plugins_dir(&state).join("sample");
        let previous = fs::read(staged.join("bin/sample")).unwrap();
        let shim = fs::read(state.join("bin/cito")).unwrap();
        fs::write(staged.join("obsolete"), "old generation").unwrap();
        let source = &plugins.plugins[0].plugin_dir;
        fs::write(source.join("bin/sample"), "#!/bin/sh\necho updated\n").unwrap();
        ensure_cli_shims_native(&state, &plugins).unwrap();
        assert!(!staged.join("obsolete").exists());
        assert_ne!(fs::read(staged.join("bin/sample")).unwrap(), previous);
        let valid = fs::read(staged.join("bin/sample")).unwrap();
        fs::write(
            source.join("bin/sample"),
            "#!/bin/sh\necho must-not-publish\n",
        )
        .unwrap();
        fs::remove_file(second.join("bin/second")).unwrap();
        assert!(ensure_cli_shims_native(&state, &plugins).is_err());
        assert_eq!(fs::read(staged.join("bin/sample")).unwrap(), valid);
        assert_eq!(fs::read(state.join("bin/cito")).unwrap(), shim);
        plugins.plugins.pop();
        fs::write(
            plugins.plugins[0].plugin_dir.join("plugin.json"),
            r#"{"id":"sample","kind":"agent-harness","displayName":"Sample","command":["sh"]}"#,
        )
        .unwrap();
        ensure_cli_shims_native(&state, &plugins).unwrap();
        assert!(fs::read_dir(state.join("runtime"))
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().starts_with(".staging-")));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn generated_shims_replace_symlinks_without_mutating_active_targets() {
        use std::os::unix::fs::symlink;
        let (root, mut plugins) = fixture();
        plugins.plugins[0].is_builtin = true;
        let source = &plugins.plugins[0].plugin_dir;
        fs::write(source.join("resource"), "resource bytes").unwrap();
        symlink("resource", source.join("resource-link")).unwrap();
        let state = root.join("installed");
        fs::create_dir_all(state.join("bin/cito-mcp")).unwrap();
        let external = root.join("external-shim");
        fs::write(&external, "external bytes").unwrap();
        fs::set_permissions(&external, fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&external, state.join("bin/cito")).unwrap();
        fs::write(state.join("relative-shim"), "relative bytes").unwrap();
        fs::set_permissions(
            state.join("relative-shim"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let snapshot = |path: &Path| {
            (
                fs::read(path).unwrap(),
                fs::metadata(path).unwrap().permissions().mode(),
            )
        };
        let before = snapshot(&external);
        // The second shim is a directory: fail after preparing the first shim.
        assert!(ensure_cli_shims_native(&state, &plugins).is_err());
        assert_eq!(snapshot(&external), before);
        assert_eq!(snapshot(&state.join("bin/cito")), before);
        assert_eq!(fs::read_link(state.join("bin/cito")).unwrap(), external);
        assert!(state.join("bin/cito-mcp").is_dir());
        fs::remove_dir(state.join("bin/cito-mcp")).unwrap();
        symlink("../relative-shim", state.join("bin/cito-mcp")).unwrap();
        symlink("../relative-shim", state.join("bin/resource-link")).unwrap();
        let relative_before = snapshot(&state.join("relative-shim"));
        ensure_cli_shims_native(&state, &plugins).unwrap();
        assert_eq!(snapshot(&external), before);
        assert_eq!(snapshot(&state.join("relative-shim")), relative_before);
        for name in ["cito", "cito-mcp"] {
            let path = state.join("bin").join(name);
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(metadata.is_file());
            assert!(!metadata.file_type().is_symlink());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
            // The generated shim executes this test binary with a harmless listing flag.
            let output = Command::new(path).arg("--list").output().unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        assert_eq!(
            fs::read_link(state.join("bin/resource-link")).unwrap(),
            Path::new("../relative-shim")
        );
        assert_eq!(
            fs::read_link(staged_plugins_dir(&state).join("sample/resource-link")).unwrap(),
            Path::new("resource")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn intact_plugin_requires_executable_local_commands() {
        let (root, mut plugins) = fixture();
        let plugin = &mut plugins.plugins[0];
        let dir = plugin.plugin_dir.clone();
        let binary = dir.join("bin/sample");
        for command in ["./bin/sample", "../sample/bin/sample"] {
            plugin.command[0] = command.into();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o100)).unwrap();
            assert!(is_plugin_dir_intact(&dir, plugin));
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(!is_plugin_dir_intact(&dir, plugin));
        }
        plugin.command[0] = "sh".into();
        assert!(is_plugin_dir_intact(&dir, plugin));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bundled_plugin_resolution_covers_distribution_layouts() {
        let (root, _) = fixture();
        fs::remove_file(root.join("plugins/sample/plugin.json")).unwrap();
        for (package, relative) in [
            (root.join("Cito.app/Contents"), "Resources/plugins/sample"),
            (root.join("prefix"), "share/cito/plugins/sample"),
            (root.clone(), "plugins/sample"),
        ] {
            let bundled = package.join(relative);
            fs::create_dir_all(&bundled).unwrap();
            // A directory left behind without its manifest is not a distribution.
            assert_ne!(bundled_plugin_dir(&package, "sample"), bundled);
            fs::write(bundled.join("plugin.json"), "{}").unwrap();
            assert_eq!(bundled_plugin_dir(&package, "sample"), bundled);
        }
        assert_eq!(
            bundled_plugin_dir(&root.join("missing"), "sample"),
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins/sample"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "isolated CLI subprocess invoked by setup regression tests"]
    fn setup_cli_process_worker() {
        let args: Vec<String> =
            serde_json::from_str(&env::var("CITO_SETUP_TEST_ARGS").expect("worker arguments"))
                .unwrap();
        if args == ["test-private-build-app"] {
            // SAFETY: umask has no pointer arguments; only this isolated worker inherits it.
            unsafe { libc::umask(0o077) };
            run_setup_cli(&["--build-app".into()]);
        } else if args == ["test-codesign-failure"] {
            env::set_var("CITO_CODESIGN_IDENTITY", "nonexistent-identity-cito-test");
            let root = PathBuf::from(env::var("HOME").unwrap());
            let plugins = crate::plugins::load_plugins(&root);
            let error = build_mac_app_bundle_native(None, &root, &plugins).unwrap_err();
            assert!(error.contains("codesign failed"), "{error}");
        } else if env::var("CITO_SETUP_TEST_LAUNCH_CONFIG").is_ok() {
            let root = PathBuf::from(env::var("HOME").unwrap());
            let plugins = crate::plugins::load_plugins(&root);
            use std::os::unix::fs::MetadataExt;
            let path = set_launch_at_login(true, &root.join("installed"), &root, &plugins).unwrap();
            let previous = fs::File::open(&path).unwrap();
            set_launch_at_login(true, &root.join("installed"), &root, &plugins).unwrap();
            let metadata = fs::metadata(&path).unwrap();
            assert_ne!(previous.metadata().unwrap().ino(), metadata.ino());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
        } else if env::var("CITO_SETUP_TEST_LOAD_CONFIG").is_ok() {
            let (config, _) = crate::plugins::load_native_config().unwrap();
            assert_eq!(
                config.config_path,
                PathBuf::from(env::var("HOME").unwrap()).join("custom/config.json")
            );
        } else {
            run_setup_cli(&args);
        }
    }

    fn cli_child(home_dir: &Path, state_dir: &Path, args: &[&str]) -> std::process::Output {
        fs::create_dir_all(home_dir.join("bundled")).unwrap();
        Command::new(env::current_exe().unwrap())
            .args([
                "--ignored",
                "setup_cli_process_worker",
                "--test-threads=1",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", env::var("PATH").unwrap())
            .env("HOME", home_dir)
            .env("CITO_STATE_DIR", state_dir)
            .env("CITO_CONFIG_PATH", state_dir.join("config.json"))
            .env("CITO_BUILTIN_PLUGINS_DIR", home_dir.join("bundled"))
            .env("CITO_DISABLE_UI", "1")
            .env("CITO_UI_HEADLESS", "1")
            .env("CITO_SETUP_TEST_ARGS", serde_json::to_string(args).unwrap())
            .output()
            .unwrap()
    }

    #[test]
    fn app_bundle_permissions_are_public_under_private_umask() {
        let (root, _) = fixture();
        let bundled = root.join("bundled/sample");
        fs::create_dir_all(bundled.join("bin")).unwrap();
        fs::write(bundled.join("package.json"), "{}").unwrap();
        fs::set_permissions(
            bundled.join("package.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::write(bundled.join("bin/hook"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(bundled.join("bin/hook"), fs::Permissions::from_mode(0o700)).unwrap();
        let output = cli_child(&root, &root, &["test-private-build-app"]);
        assert!(output.status.success(), "{output:?}");
        let app = root.join("Applications/Cito.app");
        for (path, mode) in [
            ("", 0o755),
            ("Contents", 0o755),
            ("Contents/Info.plist", 0o644),
            ("Contents/MacOS/Cito", 0o755),
            ("Contents/Helpers/cito-native", 0o755),
            ("Contents/Helpers/cito-ui", 0o755),
            ("Contents/Resources/AppIcon.icns", 0o644),
            ("Contents/Resources/plugins/sample", 0o755),
            ("Contents/Resources/plugins/sample/package.json", 0o644),
            ("Contents/Resources/plugins/sample/bin/hook", 0o755),
            ("Contents/_CodeSignature/CodeResources", 0o644),
        ] {
            assert_eq!(
                fs::metadata(app.join(path)).unwrap().permissions().mode() & 0o7777,
                mode,
                "{path}"
            );
        }
        assert_eq!(
            fs::metadata(root.join("bin")).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn app_bundle_rejects_invalid_signing_identity() {
        let (root, _) = fixture();
        let result = cli_child(&root, &root, &["test-codesign-failure"]);
        fs::remove_dir_all(root).unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn app_bundle_rejects_ui_copy_failure() {
        let (root, plugins) = fixture();
        fs::create_dir_all(root.join("bin")).unwrap();
        let ui = root.join("bin/cito-ui-darwin-arm64");
        fs::write(&ui, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&ui, fs::Permissions::from_mode(0o000)).unwrap();
        let result = build_mac_app_bundle_from_root(None, &root, &plugins, &root);
        assert!(!root.join("Applications/Cito.app").exists());
        assert_eq!(fs::read_dir(root.join("Applications")).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
        let error = result.unwrap_err();
        assert!(error.contains("os error 13"), "{error}");
    }

    #[test]
    fn app_bundle_rejects_plugin_copy_failure() {
        let (root, _) = fixture();
        let bundled = root.join("bundled");
        fs::create_dir_all(&bundled).unwrap();
        let unreadable = bundled.join("unreadable");
        fs::write(&unreadable, "plugin resource").unwrap();
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
        let result = cli_child(&root, &root, &["--build-app"]);
        assert!(!root.join("Applications/Cito.app").exists());
        assert_eq!(fs::read_dir(root.join("Applications")).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
        assert!(!result.status.success());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains("Permission denied"), "{error}");
        assert!(!error.contains("os error"), "{error}");
    }

    #[test]
    fn cli_dry_run_guards_build_uninstall_and_disable() {
        let (root, _) = fixture();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/sentinel"), "keep").unwrap();
        fs::write(
            root.join("config.json"),
            "{\"hubs\":{\"local\":{\"url\":\"local://\"}}}",
        )
        .unwrap();
        fs::create_dir_all(root.join("bundled")).unwrap();
        fn snapshot(path: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
            let mut files = std::collections::BTreeMap::new();
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    files.insert(path.clone(), Vec::new());
                    files.extend(snapshot(&path));
                } else {
                    files.insert(path.clone(), fs::read(&path).unwrap());
                }
            }
            files
        }
        let before = snapshot(&root);
        for args in [
            vec!["--dry-run", "--build-app"],
            vec!["--dry-run", "--uninstall", "--purge-data"],
            vec!["--dry-run", "--disable", "sample"],
        ] {
            let result = cli_child(&root, &root, &args);
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(snapshot(&root), before);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_build_app_matches_dry_run_with_separate_home_and_state() {
        let (root, _) = fixture();
        let home = root.join("home");
        let state = root.join("state");
        let result_json = |args: &[&str]| -> Value {
            let output = cli_child(&home, &state, args);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8(output.stdout).unwrap();
            // The isolated worker also prints the Rust test harness summary.
            serde_json::from_str(&stdout[stdout.find('{').unwrap()..=stdout.rfind('}').unwrap()])
                .unwrap()
        };
        for (destination, expected) in [
            (None, state.join("Applications/Cito.app")),
            (
                Some(root.join("directory").display().to_string()),
                root.join("directory/Cito.app"),
            ),
            (
                Some(root.join("Custom.app").display().to_string()),
                root.join("Custom.app"),
            ),
            (
                Some("~/directory".to_string()),
                home.join("directory/Cito.app"),
            ),
            (Some("~/Custom.app".to_string()), home.join("Custom.app")),
        ] {
            let mut args = vec!["--json", "--build-app"];
            if let Some(dest) = destination.as_deref() {
                args.push(dest);
            }
            args.push("--dry-run");
            let preview = result_json(&args);
            assert_eq!(preview["actions"][0]["path"], json!(expected));
            assert!(!expected.exists(), "Dry-run must not create the bundle");
            assert!(!state.exists(), "Dry-run must not create local state");
            args.pop();
            let built = result_json(&args);
            assert_eq!(built["appPath"], preview["actions"][0]["path"]);
            assert!(expected.join("Contents/Info.plist").is_file());
            assert!(expected.join("Contents/Helpers/cito-native").is_file());
            fs::remove_dir_all(&state).unwrap();
        }
        assert!(!home.join("Applications/Cito.app").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_uninstall_covers_state_and_home_bundles_without_duplicate_actions() {
        for shared_home in [false, true] {
            for symlink in [false, true] {
                for purge in [false, true] {
                    let (root, _) = fixture();
                    let home = root.join("home");
                    let state = if shared_home {
                        home.clone()
                    } else {
                        root.join("state")
                    };
                    fs::create_dir_all(&state).unwrap();
                    let external_config = root.join("settings/config.json");
                    fs::create_dir_all(external_config.parent().unwrap()).unwrap();
                    fs::write(&external_config, "{}").unwrap();
                    let backup = external_config.with_file_name("config.json.bak.fixture");
                    let lock = external_config.with_file_name("config.json.lock");
                    fs::write(&backup, "private backup").unwrap();
                    fs::write(&lock, "").unwrap();
                    std::os::unix::fs::symlink(&external_config, state.join("config.json"))
                        .unwrap();
                    let outside = root.join("Applications/Cito.app");
                    fs::create_dir_all(&outside).unwrap();
                    fs::write(outside.join("sentinel"), "unrelated bundle").unwrap();
                    let mut targets = vec![
                        state.join("Applications/Cito.app"),
                        home.join("Applications/Cito.app"),
                    ];
                    targets.dedup();
                    for target in &targets {
                        fs::create_dir_all(target.parent().unwrap()).unwrap();
                        if symlink {
                            std::os::unix::fs::symlink(&outside, target).unwrap();
                        } else {
                            fs::create_dir_all(target).unwrap();
                            fs::write(target.join("sentinel"), "local bundle").unwrap();
                        }
                    }
                    let mut args = vec!["--json", "--uninstall"];
                    if purge {
                        args.push("--purge-data");
                    }
                    let output = cli_child(&home, &state, &args);
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let stdout = String::from_utf8(output.stdout).unwrap();
                    let result: Value = serde_json::from_str(
                        &stdout[stdout.find('{').unwrap()..=stdout.rfind('}').unwrap()],
                    )
                    .unwrap();
                    let actions = result["actions"].as_array().unwrap();
                    assert_eq!(external_config.exists(), !purge);
                    assert_eq!(backup.exists(), !purge);
                    assert!(lock.exists(), "Keep the config writer inode");
                    for target in &targets {
                        assert_eq!(fs::symlink_metadata(target).is_err(), symlink || purge);
                        assert_eq!(
                            actions
                                .iter()
                                .filter(|a| a["path"] == json!(target))
                                .count(),
                            usize::from(symlink || purge)
                        );
                    }
                    assert_eq!(
                        fs::read_to_string(outside.join("sentinel")).unwrap(),
                        "unrelated bundle"
                    );
                    fs::remove_dir_all(root).unwrap();
                }
            }
        }
    }

    #[test]
    fn setup_url_alone_uses_advertised_name_without_overwriting_other_profiles() {
        let (root, _) = fixture();
        let engine = crate::core::hub::HubEngine::open(&root.join("hub")).unwrap();
        let mut auth = crate::core::auth::HubAuth::new("none", None, None, None).unwrap();
        auth.advertise_name(Some("team")).unwrap();
        let server = engine.start_server("127.0.0.1", 0, auth).unwrap();
        let url = format!("http://{}", server.address);
        let path = root.join("config.json");
        let initial = json!({"hubs":{"team":{"url":"local://"}}});
        for (extra, name) in [(vec![], "team-2"), (vec!["--hub", "custom"], "custom")] {
            fs::write(&path, initial.to_string()).unwrap();
            let mut args = vec!["--url", &url, "--auth", "none", "--default"];
            args.extend(extra);
            let output = cli_child(&root, &root, &args);
            assert!(output.status.success(), "{output:?}");
            let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["default_hub"], name);
            assert_eq!(saved["hubs"][name]["url"], url);
            assert!(saved["hubs"][name].get("discovered_name").is_none());
            assert_eq!(saved["hubs"]["team"]["url"], "local://");
        }
        let before = json!({"hubs":{"team":{"url":url,"authMode":"token","token":"saved-token"}}});
        fs::write(&path, before.to_string()).unwrap();
        let output = cli_child(&root, &root, &["--url", &url]);
        assert!(output.status.success(), "{output:?}");
        let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["hubs"]["team"]["authMode"], "token");
        assert_eq!(saved["hubs"]["team"]["token"], "saved-token");
        server.shutdown();
        engine.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_default_changes_preserve_local_hub_opt_out() {
        let (root, _) = fixture();
        let path = root.join("config.json");
        fs::write(&path, r#"{"defaultHub":"personal","hubs":{"personal":{"url":"https://personal.example.com"}}}"#).unwrap();
        for (name, url, disable_local, expected) in [
            ("work", "https://work.example.com", false, true),
            ("personal", "https://personal.example.com", true, false),
            ("work", "https://work.example.com", false, false),
        ] {
            let mut args = vec!["--hub", name, "--url", url, "--auth", "none", "--default"];
            if disable_local {
                args.extend(["--disable-hub", "local"]);
            }
            let result = cli_child(&root, &root, &args);
            assert!(result.status.success(), "{result:?}");
            let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["default_hub"], name);
            assert_eq!(saved["hubs"]["local"]["url"], "local://");
            assert_eq!(saved["hubs"]["local"]["enabled"], expected);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_share_updates_preserve_host_settings_and_aliases() {
        let (root, _) = fixture();
        for key in ["host_hub", "hostHub"] {
            let mut raw = json!({"hubs":{"local":{"url":"local://"}}});
            raw[key] = json!({"enabled":false,"port":4291,"token":"retained","bind":"127.0.0.1","auth_mode":"none","extension":{"value":7}});
            fs::write(root.join("config.json"), raw.to_string()).unwrap();
            for (args, enabled, port) in [
                (vec!["--share-hub"], true, 4291),
                (vec!["--share-port", "5301"], true, 5301),
                (vec!["--no-share-hub"], false, 5301),
            ] {
                let output = cli_child(&root, &root, &args);
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let config: Value =
                    serde_json::from_str(&fs::read_to_string(root.join("config.json")).unwrap())
                        .unwrap();
                assert_eq!(
                    config["host_hub"],
                    json!({"enabled":enabled,"port":port,"token":"retained","bind":"127.0.0.1","auth_mode":"none","extension":{"value":7}})
                );
                assert!(config.get("hostHub").is_none());
            }
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn setup_uses_discovery_auth_precedence_for_mixed_credentials() {
        let (root, _) = fixture();
        let result = cli_child(
            &root,
            &root,
            &[
                "--hub",
                "work",
                "--url",
                "local://",
                "--token",
                "secret",
                "--issuer",
                "https://id.example.com",
            ],
        );
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let saved: Value =
            serde_json::from_slice(&fs::read(root.join("config.json")).unwrap()).unwrap();
        assert_eq!(saved["hubs"]["work"]["authMode"], "token");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_rejects_malformed_config_before_writes() {
        let (root, _) = fixture();
        fs::write(root.join("config.json"), "{broken").unwrap();
        let result = cli_child(&root, &root, &["--enable", "sample"]);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("Invalid JSON"));
        assert_eq!(
            fs::read_to_string(root.join("config.json")).unwrap(),
            "{broken"
        );
        assert!(!root.join("config.json.lock").exists());
        assert!(!root.join("bin").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_targets_requested_harness_and_hub_only_updates_skip_all_applies() {
        let (root, _) = fixture();
        let script = "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' \"$request\" >> calls.jsonl\nprintf '{\"ok\":true,\"installed\":true,\"configured\":true,\"paths\":[],\"actions\":[]}\\n'\n";
        fs::write(root.join("plugins/sample/bin/sample"), script).unwrap();
        fs::create_dir_all(root.join("plugins/other/bin")).unwrap();
        fs::write(
            root.join("plugins/other/plugin.json"),
            r#"{"id":"other","displayName":"Other","command":["./bin/other"]}"#,
        )
        .unwrap();
        fs::write(root.join("plugins/other/bin/other"), script).unwrap();
        fs::set_permissions(
            root.join("plugins/other/bin/other"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let apply_count = |id: &str| {
            fs::read_to_string(root.join("plugins").join(id).join("calls.jsonl"))
                .unwrap_or_default()
                .lines()
                .filter(|line| serde_json::from_str::<Value>(line).unwrap()["op"] == "setup_apply")
                .count()
        };
        let result = cli_child(&root, &root, &["--enable", "sample"]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!((apply_count("sample"), apply_count("other")), (1, 0));
        let result = cli_child(
            &root,
            &root,
            &[
                "--hub",
                "cloud",
                "--url",
                "https://example.com",
                "--auth",
                "NoNe",
            ],
        );
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!((apply_count("sample"), apply_count("other")), (1, 0));
        let config: Value =
            serde_json::from_str(&fs::read_to_string(root.join("config.json")).unwrap()).unwrap();
        assert_eq!(config["hubs"]["cloud"]["authMode"], "none");
        let result = cli_child(&root, &root, &[]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!((apply_count("sample"), apply_count("other")), (2, 1));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tilde_config_path_is_expanded_by_setup_and_config_loader() {
        let (root, _) = fixture();
        fs::create_dir_all(root.join("custom")).unwrap();
        fs::write(
            root.join("custom/config.json"),
            r#"{"hubs":{"local":{"url":"local://"}}}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("bundled")).unwrap();
        for loader in [false, true] {
            let mut cmd = Command::new(env::current_exe().unwrap());
            cmd.args([
                "--ignored",
                "setup_cli_process_worker",
                "--test-threads=1",
                "--nocapture",
            ])
            .env("HOME", &root)
            .env("CITO_STATE_DIR", &root)
            .env("CITO_CONFIG_PATH", "~/custom/config.json")
            .env("CITO_BUILTIN_PLUGINS_DIR", root.join("bundled"))
            .env_remove("CITO_PLUGIN_PATH")
            .env("CITO_SETUP_TEST_ARGS", "[\"--status\",\"--json\"]");
            if loader {
                cmd.env("CITO_SETUP_TEST_LOAD_CONFIG", "1");
            }
            let result = cmd.output().unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            if !loader {
                assert!(String::from_utf8_lossy(&result.stdout).contains(
                    &root
                        .join("custom/config.json")
                        .to_string_lossy()
                        .to_string()
                ));
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shim_install_stages_only_builtins_and_preserves_external_sources() {
        let (root, mut plugins) = fixture();
        let external = plugins.plugins[0].clone();
        let builtin = crate::plugins::load_plugins(&root)
            .plugins
            .into_iter()
            .find(|p| p.is_builtin && p.command[0].starts_with("./"))
            .unwrap();
        plugins.plugins.push(builtin.clone());
        let installed = root.join("installed");
        ensure_cli_shims_native(&installed, &plugins).unwrap();
        let staged = staged_plugins_dir(&installed).join(&builtin.id);
        let binary = staged.join(&builtin.command[0]);
        assert!(binary.is_file());
        assert_eq!(
            fs::metadata(binary).unwrap().permissions().mode() & 0o111,
            0o111
        );
        assert!(!installed.join("plugins").exists());
        assert!(is_staged_plugin_intact(&installed, &external));
        assert_eq!(
            fs::read(external.plugin_dir.join("bin/sample")).unwrap(),
            b"#!/bin/sh\nprintf '{\"ok\":true,\"actions\":[]}\\n'\n"
        );
        let mut names = fs::read_dir(installed.join("bin"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["cito", "cito-mcp"]);
        for name in names {
            assert!(fs::read_to_string(installed.join("bin").join(name))
                .unwrap()
                .contains(&format!(
                    "export CITO_BUILTIN_PLUGINS_DIR='{}'",
                    staged_plugins_dir(&installed).display()
                )));
        }
        // Reinstalling a staged descriptor keeps the package as its repair source.
        let manifest = staged.join("plugin.json");
        let mut stale: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        stale["version"] = json!("0.0.0");
        fs::write(&manifest, stale.to_string()).unwrap();
        assert!(!is_staged_plugin_intact(&installed, &builtin));
        plugins.plugins[1] = crate::plugins::parse_manifest_dir(&staged, true).unwrap();
        ensure_cli_shims_native(&installed, &plugins).unwrap();
        assert!(is_staged_plugin_intact(&installed, &builtin));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn setup_selection_only_applies_explicit_targets_or_bare_stable_defaults() {
        let (root, plugins) = fixture();
        let sample = &plugins.plugins[0];
        let empty = HashSet::new();
        assert!(!should_setup_harness(sample, &empty, false));
        assert!(should_setup_harness(sample, &empty, true));
        assert!(!should_setup_harness(
            sample,
            &HashSet::from(["other".to_string()]),
            false
        ));
        assert!(should_setup_harness(
            sample,
            &HashSet::from(["sample".to_string()]),
            false
        ));
        let mut experimental = sample.clone();
        experimental.tier = "experimental".to_string();
        assert!(!should_setup_harness(&experimental, &empty, true));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn credential_save_restricts_existing_external_config_mode() {
        let (root, _) = fixture();
        let path = root.join("external/config.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"user":"alice"}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let next = r#"{"user":"alice","token":"secret"}"#;
        let action = mutate_file("cito", &path, false, Some(0o600), |_| {
            Ok((
                "updated".into(),
                Some(next.into()),
                "Save credentials".into(),
            ))
        })
        .unwrap();
        assert_eq!(action.status, "updated");
        assert_eq!(fs::read_to_string(&path).unwrap(), next);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let backup = PathBuf::from(action.backup_path.unwrap());
        assert_eq!(fs::read_to_string(&backup).unwrap(), r#"{"user":"alice"}"#);
        assert_eq!(
            fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_mutation_preserves_original_bytes_and_dry_run_creates_nothing() {
        let (root, _) = fixture();
        let path = root.join("config.json");
        fs::write(&path, "{broken").unwrap();
        assert!(mutate_file("cito", &path, false, None, |_| Ok((
            "updated".to_string(),
            Some("{}".to_string()),
            "Replace".to_string()
        )))
        .is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{broken");
        assert!(!root.join("config.json.lock").exists());
        let absent = root.join("absent/config.json");
        let result = mutate_file("cito", &absent, true, None, |_| {
            Ok((
                "created".to_string(),
                Some("{}".to_string()),
                "Create".to_string(),
            ))
        })
        .unwrap();
        assert_eq!(result.status, "created");
        assert!(!absent.parent().unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_uses_configured_port_unlistens_and_waits_before_removal() {
        // Keep process-global environment immutable while other unit tests run.
        if env::var_os("CITO_UNINSTALL_TEST_WORKER").is_none() {
            for mode in ["unset", "padded", "blank"] {
                let output = Command::new(env::current_exe().unwrap())
                    .args(["core::setup::tests::uninstall_uses_configured_port_unlistens_and_waits_before_removal", "--exact", "--nocapture"])
                    .env("CITO_UNINSTALL_TEST_WORKER", mode)
                    .env_remove("CITO_DAEMON_PORT")
                    .output().unwrap();
                assert!(
                    output.status.success(),
                    "{mode}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            }
            return;
        }
        use std::io::{Read, Write};
        let (root, mut plugins) = fixture();
        let state = root.join("installed");
        let user_dir = state.join("plugins/plugins");
        fs::create_dir_all(&user_dir).unwrap();
        let user_manifest = json!({"id":"plugins","kind":"human","displayName":"User plugin","command":["./driver"]}).to_string();
        fs::write(user_dir.join("plugin.json"), &user_manifest).unwrap();
        plugins
            .plugins
            .push(crate::plugins::parse_manifest_dir(&user_dir, false).unwrap());
        for sub in ["bin", "runtime/plugins", "plugins", "sockets", "sample"] {
            fs::create_dir_all(state.join(sub)).unwrap();
            fs::write(state.join(sub).join("sentinel"), "present until shutdown").unwrap();
        }
        fs::write(state.join("daemon.lock"), "lock").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        match env::var("CITO_UNINSTALL_TEST_WORKER").unwrap().as_str() {
            "padded" => env::set_var("CITO_DAEMON_PORT", format!(" {port} ")),
            "blank" => env::set_var("CITO_DAEMON_PORT", " \t "),
            _ => {}
        }
        let config_path = root.join("config.json");
        fs::write(
            &config_path,
            json!({"daemonPort":port,"hubs":{"local":{"url":"local://"}}}).to_string(),
        )
        .unwrap();
        let mut child = Command::new("/bin/sleep").arg("20").spawn().unwrap();
        let pid = child.id();
        let server_state = state.clone();
        let server = std::thread::spawn(move || {
            let mut operations = Vec::new();
            for connection in listener.incoming().take(3) {
                let mut stream = connection.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut byte = [0u8; 1];
                while !bytes.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                let headers = String::from_utf8(bytes).unwrap();
                assert!(server_state.join("bin/sentinel").exists());
                let body = if headers.starts_with("GET /health ") {
                    operations.push("health");
                    json!({"ok":true,"pid":pid,"stateDir":server_state,"version":CITO_VERSION})
                        .to_string()
                } else {
                    assert!(headers.starts_with("POST /control "));
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("Content-Length: "))
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    let mut input = vec![0; length];
                    stream.read_exact(&mut input).unwrap();
                    assert_eq!(
                        serde_json::from_slice::<Value>(&input).unwrap()["op"],
                        "unlisten_all"
                    );
                    operations.push("unlisten_all");
                    json!({"ok":true}).to_string()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
            operations
        });
        let result = uninstall_setup_native(&state, &config_path, &root, &plugins, false);
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        result.unwrap();
        assert_eq!(server.join().unwrap(), ["health", "unlisten_all", "health"]);
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            child.try_wait().unwrap().and_then(|s| s.signal()),
            Some(libc::SIGTERM)
        );
        assert!(!crate::support::alive(pid));
        assert!(state.join("runtime/staging.lock").exists());
        for sub in ["bin", "runtime/plugins", "sockets", "sample"] {
            assert!(
                !state.join(sub).exists(),
                "{sub} should be removed on uninstall"
            );
        }
        assert_eq!(
            fs::read_to_string(state.join("plugins/sentinel")).unwrap(),
            "present until shutdown"
        );
        assert_eq!(
            fs::read_to_string(user_dir.join("plugin.json")).unwrap(),
            user_manifest
        );
        assert!(
            state.join("daemon.lock").is_file(),
            "Keep the stable lock inode across cleanup"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn launch_agent_preserves_nondefault_config_path() {
        let (root, _) = fixture();
        let state = root.join("installed");
        let expected = root.join("custom/config.json");
        let output = Command::new(env::current_exe().unwrap())
            .args([
                "--ignored",
                "setup_cli_process_worker",
                "--test-threads=1",
                "--nocapture",
            ])
            .env("HOME", &root)
            .env("CITO_CONFIG_PATH", &expected)
            .env("CITO_SETUP_TEST_LAUNCH_CONFIG", "1")
            .env("CITO_SETUP_TEST_ARGS", "[]")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let xml = fs::read_to_string(launch_agent_plist_path(&root)).unwrap();
        let expected = expected.to_string_lossy();
        assert!(xml.contains(&format!(
            "<key>CITO_CONFIG_PATH</key><string>{}</string>",
            escape_xml(&expected)
        )));
        assert!(xml.contains(&escape_xml(&state.join("bin/cito").to_string_lossy())));
        fs::remove_dir_all(root).unwrap();
    }
}
