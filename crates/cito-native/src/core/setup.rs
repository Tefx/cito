use crate::plugins::{
    expand_home_dir, now_ms, random_hex, resolve_builtin_plugins_dir, system_home_dir,
    validate_harness_id, validate_https_issuer, validate_hub_name, validate_nats_url,
    LoadedPlugins, PluginDescriptor, CITO_VERSION,
};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
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

pub fn resolve_state_and_config_paths() -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let home_env = env::var("HOME").ok();
    let home_dir = expand_home_dir("~", "HOME", home_env.as_deref())?;
    let raw_state = env::var("CITO_STATE_DIR").unwrap_or_else(|_| "~/.cito".to_string());
    let state_dir = expand_home_dir(&raw_state, "CITO_STATE_DIR", home_env.as_deref())?;
    let config_path = match env::var("CITO_CONFIG_PATH") {
        Ok(p) => expand_home_dir(&p, "CITO_CONFIG_PATH", home_env.as_deref())?,
        Err(_) => state_dir.join("config.json"),
    };
    Ok((state_dir, config_path, home_dir))
}

pub fn ensure_cli_shims_native(
    state_dir: &Path,
    plugins: &LoadedPlugins,
) -> Result<Vec<PathBuf>, String> {
    let bin_dir = state_dir.join("bin");
    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let exe_str = resolve_stable_package_root(&exe.to_string_lossy());
    let plugins_dir = state_dir.join("plugins");
    let mut written = Vec::new();
    for plugin in &plugins.plugins {
        let target = plugins_dir.join(&plugin.id);
        if fs::canonicalize(&plugin.plugin_dir).ok() != fs::canonicalize(&target).ok()
            || !target.exists()
        {
            copy_dir_recursive(&plugin.plugin_dir, &target)?;
            written.push(target);
        }
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
        fs::write(&shim_path, script).map_err(|e| e.to_string())?;
        fs::set_permissions(&shim_path, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        written.push(shim_path);
    }
    Ok(written)
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
    fs::write(&plist_path, xml).map_err(|e| e.to_string())?;
    let _ = fs::set_permissions(&plist_path, fs::Permissions::from_mode(0o644));
    Ok(plist_path)
}

fn mutate_file<F>(
    target: &str,
    path: &Path,
    dry_run: bool,
    mode: Option<u32>,
    transform: F,
) -> Result<SetupAction, String>
where
    F: FnOnce(Option<&str>) -> Result<(String, Option<String>, String), String>,
{
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let path = resolved.as_path();
    // Reject malformed input before even creating a lockfile, then reread while locked.
    crate::plugins::read_config_text(path)?;
    crate::plugins::with_config_lock_mode(path, dry_run, || {
        let existing = crate::plugins::read_config_text(path)?;
        let existing_mode = fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777);
        let (status, next_content, detail) = transform(existing.as_deref())?;
        if status == "unchanged" || status == "conflict" {
            return Ok(SetupAction {
                target: target.to_string(),
                path: path.to_string_lossy().to_string(),
                status,
                backup_path: None,
                detail,
            });
        }
        if let (Some(ref ex), Some(ref nx)) = (&existing, &next_content) {
            if ex == nx {
                return Ok(SetupAction {
                    target: target.to_string(),
                    path: path.to_string_lossy().to_string(),
                    status: "unchanged".to_string(),
                    backup_path: None,
                    detail,
                });
            }
        }
        if dry_run {
            return Ok(SetupAction {
                target: target.to_string(),
                path: path.to_string_lossy().to_string(),
                status,
                backup_path: None,
                detail,
            });
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let target_mode = mode.or(existing_mode).unwrap_or(0o600);
        let mut backup_path = None;
        if let Some(ref ex) = existing {
            let bak = format!("{}.bak.{}.{}", path.display(), now_ms(), random_hex(4));
            let mut backup = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(target_mode)
                .open(&bak)
                .map_err(|e| e.to_string())?;
            backup.write_all(ex.as_bytes()).map_err(|e| e.to_string())?;
            backup_path = Some(bak);
        }
        if let Some(ref nx) = next_content {
            let temporary = path.with_extension(format!("tmp.{}", random_hex(8)));
            let result = (|| {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(target_mode)
                    .open(&temporary)
                    .map_err(|e| e.to_string())?;
                file.write_all(nx.as_bytes()).map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                fs::rename(&temporary, path).map_err(|e| e.to_string())
            })();
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            result?;
        }
        Ok(SetupAction {
            target: target.to_string(),
            path: path.to_string_lossy().to_string(),
            status,
            backup_path,
            detail,
        })
    })
}

fn setup_request(
    plugin: &PluginDescriptor,
    op: &str,
    force: bool,
    dry_run: bool,
) -> Result<Value, String> {
    let (state_dir, _, _) = resolve_state_and_config_paths()?;
    let plugin_dir = state_dir.join("plugins").join(&plugin.id);
    crate::plugins::plugin_request(
        plugin,
        &json!({
            "op": op, "force": force, "dry_run": dry_run,
            "state_dir": state_dir, "plugin_dir": plugin_dir,
            "env": env::vars().collect::<std::collections::BTreeMap<_, _>>(),
        }),
    )
}

pub fn inspect_plugin_setup(plugin: &PluginDescriptor) -> (bool, bool, Vec<String>) {
    match setup_request(plugin, "setup_inspect", false, true) {
        Ok(response) if response.get("ok").and_then(Value::as_bool) == Some(true) => (
            response
                .get("installed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            response
                .get("configured")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            response
                .get("paths")
                .and_then(Value::as_array)
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        ),
        _ => (false, false, Vec::new()),
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
        return Err(response
            .get("message")
            .or_else(|| response.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("Plugin setup failed")
            .to_string());
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
            Ok(SetupAction {
                target: text("target")?,
                path: text("path")?,
                status: text("status")?,
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
    remove_plugin_setup_dry_run(plugin, false)
}

pub fn remove_plugin_setup_dry_run(
    plugin: &PluginDescriptor,
    dry_run: bool,
) -> Result<Vec<SetupAction>, String> {
    plugin_setup_actions(plugin, "setup_remove", false, dry_run)
}

pub fn inspect_setup_json(
    state_dir: &Path,
    config_path: &Path,
    home_dir: &Path,
    plugins: &LoadedPlugins,
) -> Value {
    let config_exists = config_path.exists();
    let normalized = crate::plugins::read_config_text(config_path)
        .and_then(|raw| try_normalize_existing_config(raw.as_deref()));
    let (default_hub, hubs_obj, rest_obj) = match normalized {
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
        let (installed, configured, paths) = inspect_plugin_setup(p);
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

pub use crate::plugins::{
    normalize_existing_config, try_normalize_existing_config, with_config_lock,
};

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            fs::copy(&path, &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn dir_bytes(path: &Path) -> u64 {
    let Ok(meta) = fs::metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(rd) = fs::read_dir(path) else { return 0 };
    rd.flatten().map(|e| dir_bytes(&e.path())).sum()
}

pub fn build_mac_app_bundle_native(
    dest_dir: Option<&str>,
    state_dir: &Path,
    plugins: &LoadedPlugins,
) -> Result<(PathBuf, u64), String> {
    let home = system_home_dir();
    let base_dest = match dest_dir {
        Some(d) => expand_home_dir(d, "destDir", Some(&home))?,
        None => state_dir.join("Applications"),
    };
    let app_path = if base_dest.to_string_lossy().ends_with(".app") {
        base_dest
    } else {
        base_dest.join("Cito.app")
    };
    let contents_dir = app_path.join("Contents");
    let macos_dir = contents_dir.join("MacOS");
    let helpers_dir = contents_dir.join("Helpers");
    let resources_dir = contents_dir.join("Resources");

    let _ = fs::remove_dir_all(&app_path);
    fs::create_dir_all(&macos_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&helpers_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&resources_dir).map_err(|e| e.to_string())?;

    let info_plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
           <key>CFBundleIdentifier</key>\n\
           <string>one.tefx.cito</string>\n\
           <key>CFBundleName</key>\n\
           <string>Cito</string>\n\
           <key>CFBundleDisplayName</key>\n\
           <string>Cito</string>\n\
           <key>CFBundleExecutable</key>\n\
           <string>Cito</string>\n\
           <key>CFBundleIconFile</key>\n\
           <string>AppIcon</string>\n\
           <key>CFBundleIconName</key>\n\
           <string>AppIcon</string>\n\
           <key>CFBundleShortVersionString</key>\n\
           <string>{CITO_VERSION}</string>\n\
           <key>CFBundleVersion</key>\n\
           <string>{CITO_VERSION}</string>\n\
           <key>CFBundlePackageType</key>\n\
           <string>APPL</string>\n\
           <key>LSUIElement</key>\n\
           <true/>\n\
           <key>LSMinimumSystemVersion</key>\n\
           <string>13.0</string>\n\
         </dict>\n\
         </plist>\n"
    );
    fs::write(contents_dir.join("Info.plist"), info_plist).map_err(|e| e.to_string())?;

    let launcher = "#!/bin/sh\nset -e\nAPP_CONTENTS=\"$(cd \"$(dirname \"$0\")/..\" && pwd)\"\nexport CITO_BUILTIN_PLUGINS_DIR=\"${CITO_BUILTIN_PLUGINS_DIR:-$APP_CONTENTS/Resources/plugins}\"\nif [ \"$#\" -eq 0 ]; then\n  exec \"$APP_CONTENTS/Helpers/cito-native\" daemon\nfi\ncase \"$1\" in\n  -psn_*)\n    shift\n    exec \"$APP_CONTENTS/Helpers/cito-native\" daemon \"$@\"\n    ;;\nesac\nexec \"$APP_CONTENTS/Helpers/cito-native\" \"$@\"\n";
    let launcher_path = macos_dir.join("Cito");
    fs::write(&launcher_path, launcher).map_err(|e| e.to_string())?;
    let _ = fs::set_permissions(&launcher_path, fs::Permissions::from_mode(0o755));

    let pkg_root = resolve_package_root();
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let helper_native = helpers_dir.join("cito-native");
    fs::copy(&exe, &helper_native).map_err(|e| e.to_string())?;
    let _ = fs::set_permissions(&helper_native, fs::Permissions::from_mode(0o755));

    for ui_cand in [
        pkg_root.join("bin/cito-ui-darwin-arm64"),
        pkg_root.join("Helpers/cito-ui"),
        state_dir.join("bin/cito-ui"),
    ] {
        if ui_cand.is_file() {
            let helper_ui = helpers_dir.join("cito-ui");
            let _ = fs::copy(&ui_cand, &helper_ui);
            let _ = fs::set_permissions(&helper_ui, fs::Permissions::from_mode(0o755));
            break;
        }
    }

    for icon_cand in [
        pkg_root.join("bin/AppIcon.icns"),
        pkg_root.join("Resources/AppIcon.icns"),
    ] {
        if icon_cand.is_file() {
            let _ = fs::copy(&icon_cand, resources_dir.join("AppIcon.icns"));
            break;
        }
    }

    if let Some(plugins_src) = resolve_builtin_plugins_dir() {
        let _ = copy_dir_recursive(&plugins_src, &resources_dir.join("plugins"));
    }

    let _ = ensure_cli_shims_native(state_dir, plugins);
    let _ = Command::new("/usr/bin/xattr")
        .args(["-cr", &app_path.to_string_lossy()])
        .status();

    let sign_id = env::var("CITO_CODESIGN_IDENTITY")
        .or_else(|_| env::var("APPLE_SIGNING_IDENTITY"))
        .unwrap_or_default();
    if !sign_id.trim().is_empty() {
        let _ = Command::new("/usr/bin/codesign")
            .args([
                "--force",
                "--options",
                "runtime",
                "--timestamp",
                "--deep",
                "--sign",
                sign_id.trim(),
                &app_path.to_string_lossy(),
            ])
            .status();
    } else {
        let _ = Command::new("/usr/bin/codesign")
            .args(["-s", "-", "--force", "--deep", &app_path.to_string_lossy()])
            .status();
    }

    let bytes = dir_bytes(&app_path);
    Ok((app_path, bytes))
}

pub fn uninstall_setup_native(
    state_dir: &Path,
    config_path: &Path,
    home_dir: &Path,
    plugins: &LoadedPlugins,
    purge_data: bool,
) -> Result<(String, Vec<SetupAction>), String> {
    let raw = crate::plugins::read_config_text(config_path)?;
    let (default_hub, _, rest) = try_normalize_existing_config(raw.as_deref())?;
    let daemon_port = match env::var("CITO_DAEMON_PORT") {
        Ok(value) => value
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or("Invalid CITO_DAEMON_PORT")?,
        Err(_) => rest
            .get("daemonPort")
            .and_then(Value::as_u64)
            .map(|p| {
                u16::try_from(p)
                    .ok()
                    .filter(|p| *p > 0)
                    .ok_or("Invalid daemonPort")
            })
            .transpose()?
            .unwrap_or(4290),
    };
    let config = crate::plugins::NativeConfig {
        state_dir: state_dir.to_path_buf(),
        config_path: config_path.to_path_buf(),
        home_dir: home_dir.to_path_buf(),
        daemon_port,
        user: String::new(),
        default_hub: default_hub.clone(),
    };
    if let Some(health) = crate::plugins::probe_daemon_health(&config) {
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

    let mut actions = Vec::new();
    for plugin in plugins.agent_plugins() {
        actions.extend(remove_plugin_setup(plugin)?);
    }

    let plist_path = launch_agent_plist_path(home_dir);
    if plist_path.exists() {
        fs::remove_file(&plist_path).map_err(|e| e.to_string())?;
        actions.push(SetupAction {
            target: "cito".to_string(),
            path: plist_path.to_string_lossy().to_string(),
            status: "updated".to_string(),
            backup_path: None,
            detail: "Removed LaunchAgent plist".to_string(),
        });
    }

    let bin_dir = state_dir.join("bin");
    if bin_dir.exists() {
        fs::remove_dir_all(&bin_dir).map_err(|e| e.to_string())?;
        actions.push(SetupAction {
            target: "cito".to_string(),
            path: bin_dir.to_string_lossy().to_string(),
            status: "updated".to_string(),
            backup_path: None,
            detail: "Removed CLI shims directory".to_string(),
        });
    }

    if purge_data {
        let user_app = home_dir.join("Applications").join("Cito.app");
        if user_app.exists() {
            fs::remove_dir_all(&user_app).map_err(|e| e.to_string())?;
            actions.push(SetupAction {
                target: "cito".to_string(),
                path: user_app.to_string_lossy().to_string(),
                status: "updated".to_string(),
                backup_path: None,
                detail: "Removed ~/Applications/Cito.app".to_string(),
            });
        }
        if state_dir.exists() {
            fs::remove_dir_all(state_dir).map_err(|e| e.to_string())?;
            actions.push(SetupAction {
                target: "cito".to_string(),
                path: state_dir.to_string_lossy().to_string(),
                status: "updated".to_string(),
                backup_path: None,
                detail: "Removed local state directory".to_string(),
            });
        }
    }

    Ok((default_hub, actions))
}

pub fn run_setup_cli(args: &[String]) {
    let run = || -> Result<(), String> {
        let (state_dir, config_path, home_dir) = resolve_state_and_config_paths()?;
        let plugins = crate::plugins::load_plugins(&state_dir);

        let mut status_only = false;
        let mut dry_run = false;
        let mut force = false;
        let mut json_output = false;
        let mut uninstall = false;
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
        let mut hub_token: Option<String> = None;
        let mut hub_issuer: Option<String> = None;
        let mut hub_client_id: Option<String> = None;
        let mut enable_hubs: Vec<String> = Vec::new();
        let mut disable_hubs: Vec<String> = Vec::new();
        let mut remove_hubs: Vec<String> = Vec::new();
        let mut enable_harnesses: Vec<String> = Vec::new();
        let mut disable_harnesses: Vec<String> = Vec::new();

        let read_val = |arg: &str, idx: usize, flag: &str| -> Result<(String, usize), String> {
            if let Some(rest) = arg.strip_prefix(&format!("{flag}=")) {
                let v = rest.trim();
                if v.is_empty() {
                    return Err(format!("Missing value for {flag}"));
                }
                return Ok((v.to_string(), idx));
            }
            let next = args
                .get(idx + 1)
                .ok_or_else(|| format!("Missing value for {flag}"))?;
            if next.starts_with('-') {
                return Err(format!("Missing value for {flag}"));
            }
            Ok((next.clone(), idx + 1))
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

        if hub_name.is_some() != hub_url.is_some() {
            return Err("--hub and --url must be specified together".to_string());
        }

        let raw_config = crate::plugins::read_config_text(&config_path)?;
        let (current_default, _, _) = try_normalize_existing_config(raw_config.as_deref())?;
        if dry_run && (build_app || uninstall) {
            let action = SetupAction {
                target: "cito".to_string(),
                path: if build_app {
                    build_app_dest.clone().unwrap_or_else(|| {
                        home_dir
                            .join("Applications/Cito.app")
                            .to_string_lossy()
                            .into_owned()
                    })
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
            return Ok(());
        }

        if build_app {
            let (app_path, bytes) =
                build_mac_app_bundle_native(build_app_dest.as_deref(), &state_dir, &plugins)?;
            if json_output {
                let out = json!({
                    "appPath": app_path.to_string_lossy(),
                    "bytes": bytes,
                });
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
            } else {
                println!("Built {} ({} bytes)", app_path.display(), bytes);
            }
            return Ok(());
        }

        if status_only {
            let inspection = inspect_setup_json(&state_dir, &config_path, &home_dir, &plugins);
            if json_output {
                println!("{}", serde_json::to_string_pretty(&inspection).unwrap());
            } else {
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
            return Ok(());
        }

        if uninstall {
            let (default_hub, actions) =
                uninstall_setup_native(&state_dir, &config_path, &home_dir, &plugins, purge_data)?;
            emit_setup_result(&config_path, &default_hub, false, &actions, json_output);
            return Ok(());
        }

        for h in enable_harnesses.iter().chain(disable_harnesses.iter()) {
            if plugins.rejected_ids.contains(h) {
                return Err(format!(
                    "PLUGIN_LOAD_ERROR: Invalid plugin manifest for {h}"
                ));
            }
            if !validate_harness_id(h) || plugins.find_by_id(h).is_none() {
                return Err(format!("Unknown harness: {h}"));
            }
        }

        let only_disable = !disable_harnesses.is_empty()
            && enable_harnesses.is_empty()
            && hub_name.is_none()
            && enable_hubs.is_empty()
            && disable_hubs.is_empty()
            && remove_hubs.is_empty()
            && launch_at_login.is_none()
            && share_hub.is_none()
            && share_port.is_none();

        if only_disable {
            let raw = crate::plugins::read_config_text(&config_path)?;
            let (default_hub, _, _) = try_normalize_existing_config(raw.as_deref())?;
            let mut actions = Vec::new();
            for h in &disable_harnesses {
                if let Some(p) = plugins.find_by_id(h) {
                    actions.extend(remove_plugin_setup_dry_run(p, dry_run)?);
                }
            }
            emit_setup_result(&config_path, &default_hub, dry_run, &actions, json_output);
            return Ok(());
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
        if let (Some(ref name), Some(ref url_raw)) = (&hub_name, &hub_url) {
            if !validate_hub_name(name) {
                return Err(format!("Invalid hub name: {name}"));
            }
            let url = validate_nats_url(url_raw)?;
            let auth = hub_auth.clone().unwrap_or_else(|| "none".to_string());
            if auth == "token" && hub_token.as_deref().unwrap_or("").trim().is_empty() {
                return Err("Hub token is required when auth is token".to_string());
            }
            let mut iss_norm = None;
            let mut cid_norm = None;
            if auth == "oidc" || hub_issuer.is_some() {
                let raw_iss = hub_issuer.as_deref().unwrap_or("").trim();
                if raw_iss.is_empty() {
                    return Err("Hub issuer is required when auth is oidc".to_string());
                }
                iss_norm = Some(validate_https_issuer(raw_iss)?);
                let raw_cid = hub_client_id.as_deref().unwrap_or("").trim();
                if raw_cid.is_empty() {
                    return Err("Hub client-id is required when auth is oidc".to_string());
                }
                cid_norm = Some(raw_cid.to_string());
            }
            let def_scope = if url == "local://" {
                "workspace"
            } else {
                "self"
            };
            let mut entry = Map::new();
            entry.insert("url".to_string(), json!(url));
            entry.insert("natsUrl".to_string(), json!(url));
            entry.insert("authMode".to_string(), json!(auth));
            if let Some(ref t) = hub_token {
                entry.insert("token".to_string(), json!(t.trim()));
            }
            if let Some(ref iss) = iss_norm {
                entry.insert("oidcIssuer".to_string(), json!(iss));
            }
            if let Some(ref cid) = cid_norm {
                entry.insert("oidcClientId".to_string(), json!(cid));
            }
            entry.insert("enabled".to_string(), json!(true));
            entry.insert("defaultScope".to_string(), json!(def_scope));
            validated_hub = Some((name.clone(), Value::Object(entry)));
        }

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
                m.insert("enabled".to_string(), json!(true));
            }
            for n in &disable_hubs {
                let Some(Value::Object(ref mut m)) = hubs.get_mut(n) else {
                    return Err(format!("Unknown hub: {n}"));
                };
                m.insert("enabled".to_string(), json!(false));
            }
            for n in &remove_hubs {
                if hubs.remove(n).is_none() {
                    return Err(format!("Unknown hub: {n}"));
                }
            }

            if share_hub.is_some() || share_port.is_some() {
                let prev = rest
                    .get("host_hub")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                let en = share_hub.unwrap_or_else(|| {
                    prev.get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                });
                let p = share_port
                    .unwrap_or_else(|| prev.get("port").and_then(|v| v.as_i64()).unwrap_or(4291));
                if !(0..=65535).contains(&p) {
                    return Err("host_hub.port must be an integer in 0..65535".to_string());
                }
                let tok = prev
                    .get("token")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| random_hex(32));
                rest.insert(
                    "host_hub".to_string(),
                    json!({ "enabled": en, "port": p, "token": tok }),
                );
            }

            let def_enabled = hubs
                .get(&def_hub)
                .and_then(|v| v.get("enabled"))
                .and_then(|v| v.as_bool())
                != Some(false);
            if !hubs.contains_key(&def_hub) || !def_enabled {
                def_hub = hubs
                    .iter()
                    .find(|(_, v)| v.get("enabled").and_then(|x| x.as_bool()) != Some(false))
                    .map(|(k, _)| k.clone())
                    .or_else(|| hubs.keys().next().cloned())
                    .unwrap_or_else(|| "local".to_string());
            }
            resolved_default_hub = def_hub.clone();
            let mut next_obj = Map::new();
            next_obj.insert("default_hub".to_string(), json!(def_hub));
            next_obj.insert("hubs".to_string(), Value::Object(hubs));
            for (k, v) in rest {
                next_obj.insert(k, v);
            }
            let next_content = format!(
                "{}\n",
                serde_json::to_string_pretty(&Value::Object(next_obj)).unwrap()
            );
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

        let bare_setup = enable_harnesses.is_empty()
            && disable_harnesses.is_empty()
            && hub_name.is_none()
            && enable_hubs.is_empty()
            && disable_hubs.is_empty()
            && remove_hubs.is_empty()
            && launch_at_login.is_none()
            && share_hub.is_none()
            && share_port.is_none();
        let mut actions = vec![config_action];
        if !dry_run {
            ensure_cli_shims_native(&state_dir, &plugins)?;
        }
        let enabled_set: HashSet<String> = enable_harnesses.into_iter().collect();
        let disabled_set: HashSet<String> = disable_harnesses.into_iter().collect();

        for plugin in plugins.agent_plugins() {
            if disabled_set.contains(&plugin.id) {
                actions.extend(remove_plugin_setup_dry_run(plugin, dry_run)?);
                continue;
            }
            let should_cfg = should_setup_harness(plugin, &enabled_set, bare_setup);
            if !should_cfg {
                let (_, _, paths) = inspect_plugin_setup(plugin);
                if paths.is_empty() && !plugin.is_builtin {
                    continue;
                }
                actions.push(SetupAction {
                    target: plugin.id.clone(),
                    path: paths.first().cloned().unwrap_or_else(|| plugin.id.clone()),
                    status: "skipped".to_string(),
                    backup_path: None,
                    detail: format!(
                        "Skipped {} harness '{}' (pass --enable {} to configure)",
                        plugin.tier, plugin.id, plugin.id
                    ),
                });
                continue;
            }
            actions.extend(apply_plugin_setup(plugin, force, dry_run)?);
        }

        if !dry_run {
            if let Some(en) = launch_at_login {
                set_launch_at_login(en, &state_dir, &home_dir, &plugins)?;
            }
        }

        let ok = emit_setup_result(
            &config_path,
            &resolved_default_hub,
            dry_run,
            &actions,
            json_output,
        );
        if !ok {
            std::process::exit(1);
        }
        Ok(())
    };

    if let Err(err) = run() {
        eprintln!("cito setup: {err}");
        std::process::exit(1);
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
) -> bool {
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
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, LoadedPlugins) {
        let root = PathBuf::from(env::var("HOME").expect("isolated HOME"))
            .join(format!("cito-setup-test-{}", random_hex(8)));
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
    #[ignore = "isolated CLI subprocess invoked by setup regression tests"]
    fn setup_cli_process_worker() {
        let args: Vec<String> =
            serde_json::from_str(&env::var("CITO_SETUP_TEST_ARGS").expect("worker arguments"))
                .unwrap();
        if env::var("CITO_SETUP_TEST_LOAD_CONFIG").is_ok() {
            let (config, _) = crate::plugins::load_native_config().unwrap();
            assert_eq!(
                config.config_path,
                PathBuf::from(env::var("HOME").unwrap()).join("custom/config.json")
            );
        } else {
            run_setup_cli(&args);
        }
    }

    fn cli_child(root: &Path, args: &[&str]) -> std::process::Output {
        fs::create_dir_all(root.join("bundled")).unwrap();
        Command::new(env::current_exe().unwrap())
            .args([
                "--ignored",
                "setup_cli_process_worker",
                "--test-threads=1",
                "--nocapture",
            ])
            .env("HOME", root)
            .env("CITO_STATE_DIR", root)
            .env("CITO_CONFIG_PATH", root.join("config.json"))
            .env("CITO_BUILTIN_PLUGINS_DIR", root.join("bundled"))
            .env_remove("CITO_PLUGIN_PATH")
            .env_remove("CITO_DAEMON_PORT")
            .env("CITO_SETUP_TEST_ARGS", serde_json::to_string(args).unwrap())
            .output()
            .unwrap()
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
            let result = cli_child(&root, &args);
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
    fn cli_rejects_malformed_config_before_writes() {
        let (root, _) = fixture();
        fs::write(root.join("config.json"), "{broken").unwrap();
        let result = cli_child(&root, &["--enable", "sample"]);
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
        let result = cli_child(&root, &["--enable", "sample"]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!((apply_count("sample"), apply_count("other")), (1, 0));
        let result = cli_child(
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
        let result = cli_child(&root, &[]);
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
    fn shim_install_copies_plugin_binaries_and_preserves_executable_mode() {
        let (root, plugins) = fixture();
        let installed = root.join("installed");
        ensure_cli_shims_native(&installed, &plugins).unwrap();
        let binary = installed.join("plugins/sample/bin/sample");
        assert!(binary.is_file());
        assert_eq!(
            fs::metadata(binary).unwrap().permissions().mode() & 0o111,
            0o111
        );
        let mut names = fs::read_dir(installed.join("bin"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["cito", "cito-mcp"]);
        assert!(fs::read_to_string(installed.join("bin/cito"))
            .unwrap()
            .contains(&installed.join("plugins").to_string_lossy().to_string()));
        // Reinstalling from the installed source must not copy a directory onto itself.
        let installed_plugin = crate::plugins::load_plugins(&installed)
            .find_by_id("sample")
            .unwrap()
            .clone();
        ensure_cli_shims_native(
            &installed,
            &LoadedPlugins {
                plugins: vec![installed_plugin],
                rejected_ids: HashSet::new(),
            },
        )
        .unwrap();
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
        use std::io::{Read, Write};
        let (root, plugins) = fixture();
        let state = root.join("installed");
        fs::create_dir_all(state.join("bin")).unwrap();
        fs::write(state.join("bin/sentinel"), "present until shutdown").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
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
        assert!(!crate::plugins::pid_alive(pid as i32));
        assert!(!state.join("bin").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn launch_agent_preserves_nondefault_config_path() {
        let (root, plugins) = fixture();
        let state = root.join("installed");
        let plist = set_launch_at_login(true, &state, &root, &plugins).unwrap();
        let xml = fs::read_to_string(plist).unwrap();
        let expected = env::var("CITO_CONFIG_PATH").expect("isolated config path");
        assert!(xml.contains(&format!(
            "<key>CITO_CONFIG_PATH</key><string>{}</string>",
            escape_xml(&expected)
        )));
        assert!(xml.contains(&escape_xml(&state.join("bin/cito").to_string_lossy())));
        fs::remove_dir_all(root).unwrap();
    }
}
