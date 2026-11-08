use crate::plugins::{
    expand_home_dir, http_post_mcp, now_ms, random_hex, resolve_builtin_plugins_dir, system_home_dir,
    validate_harness_id, validate_https_issuer, validate_hub_name, validate_nats_url, LoadedPlugins,
    PluginDescriptor, CITO_VERSION,
};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
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

pub fn ensure_cli_shims_native(state_dir: &Path, plugins: &LoadedPlugins) -> Result<Vec<PathBuf>, String> {
    let bin_dir = state_dir.join("bin");
    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    let _ = fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o700));

    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("cito"));
    let exe_str = resolve_stable_package_root(&exe.to_string_lossy());
    let plugins_dir = resolve_builtin_plugins_dir()
        .map(|p| resolve_stable_package_root(&p.to_string_lossy()))
        .unwrap_or_default();

    let mut bins = vec!["cito".to_string(), "cito-mcp".to_string()];
    if let Some(tp) = plugins.find_by_protocol("transcript-hook") {
        if let Some(hc) = tp.native_str("hookCommand") {
            bins.push(hc.to_string());
        }
    }

    let mut written = Vec::new();
    for bin_name in bins {
        let shim_path = bin_dir.join(&bin_name);
        let script = if bin_name.ends_with("-hook") {
            format!(
                "#!/bin/sh\nexport CITO_BUILTIN_PLUGINS_DIR=\"${{CITO_BUILTIN_PLUGINS_DIR:-{plugins_dir}}}\"\nexport CITO_ENDPOINT_PID=\"${{CITO_ENDPOINT_PID:-$PPID}}\"\nexec \"{exe_str}\" hook \"$@\"\n"
            )
        } else if bin_name == "cito-mcp" {
            format!(
                "#!/bin/sh\nexport CITO_BUILTIN_PLUGINS_DIR=\"${{CITO_BUILTIN_PLUGINS_DIR:-{plugins_dir}}}\"\nexport CITO_ENDPOINT_PID=\"${{CITO_ENDPOINT_PID:-$PPID}}\"\nif [ \"$#\" -eq 0 ]; then\n  exec \"{exe_str}\" mcp-stdio\nfi\nexec \"{exe_str}\" \"$@\"\n"
            )
        } else {
            format!(
                "#!/bin/sh\nexport CITO_BUILTIN_PLUGINS_DIR=\"${{CITO_BUILTIN_PLUGINS_DIR:-{plugins_dir}}}\"\nexec \"{exe_str}\" \"$@\"\n"
            )
        };
        fs::write(&shim_path, script).map_err(|e| e.to_string())?;
        let _ = fs::set_permissions(&shim_path, fs::Permissions::from_mode(0o755));
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
        let _ = fs::remove_file(&plist_path);
        return Ok(plist_path);
    }
    ensure_cli_shims_native(state_dir, plugins)?;
    let bin_dir = state_dir.join("bin");
    let cito_bin = bin_dir.join("cito");
    let path_val = format!(
        "{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        bin_dir.display()
    );
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
    let existing = fs::read_to_string(path).ok();
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
        let bak = format!("{}.bak.{}", path.display(), now_ms());
        fs::write(&bak, ex).map_err(|e| e.to_string())?;
        let _ = fs::set_permissions(&bak, fs::Permissions::from_mode(target_mode));
        backup_path = Some(bak);
    }
    if let Some(ref nx) = next_content {
        fs::write(path, nx).map_err(|e| e.to_string())?;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(target_mode));
    }
    Ok(SetupAction {
        target: target.to_string(),
        path: path.to_string_lossy().to_string(),
        status,
        backup_path,
        detail,
    })
}

fn plugin_home(plugin: &PluginDescriptor) -> PathBuf {
    let home_env = env::var("HOME").ok();
    let env_key = plugin.native_str("homeEnv").unwrap_or("HOME");
    let default_home = plugin.native_str("defaultHome").unwrap_or("~");
    let raw = env::var(env_key).unwrap_or_else(|_| default_home.to_string());
    expand_home_dir(&raw, env_key, home_env.as_deref()).unwrap_or_else(|_| PathBuf::from(&raw))
}

fn strip_toml_comment(line: &str) -> Result<&str, String> {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == b'\\' && in_double {
            i += 2;
            continue;
        }
        if ch == b'\'' && !in_double {
            in_single = !in_single;
        } else if ch == b'"' && !in_single {
            in_double = !in_double;
        } else if ch == b'#' && !in_single && !in_double {
            return Ok(&line[..i]);
        }
        i += 1;
    }
    if in_single || in_double {
        return Err("Invalid TOML syntax: unclosed string literal in config.toml".to_string());
    }
    Ok(line)
}

fn transform_toml_apply(existing: Option<&str>, force: bool) -> Result<(String, Option<String>, String), String> {
    const BLOCK: &str = "[mcp_servers.cito]\ncommand = \"cito-mcp\"\nargs = []\n";
    let Some(existing) = existing else {
        return Ok((
            "created".to_string(),
            Some(BLOCK.to_string()),
            "Created MCP configuration".to_string(),
        ));
    };
    let lines: Vec<&str> = existing.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let mut sec_start: Option<usize> = None;
    let mut sec_last_kv: usize = 0;
    let mut in_sec = false;
    let mut cmd_val: Option<String> = None;
    let mut has_dotted = false;

    for (i, &line) in lines.iter().enumerate() {
        let stripped = strip_toml_comment(line)?.trim();
        if stripped.is_empty() {
            continue;
        }
        if stripped.starts_with('[') {
            if !stripped.ends_with(']') {
                return Err(format!("Invalid TOML table header at line {}", i + 1));
            }
            let inner = stripped.trim_matches(&['[', ']'][..]).trim();
            let norm: String = inner
                .chars()
                .filter(|c| !matches!(c, '"' | '\'' | ' ' | '\t'))
                .collect();
            if norm == "mcp_servers.cito" {
                sec_start = Some(i);
                sec_last_kv = i;
                in_sec = true;
            } else {
                in_sec = false;
            }
            continue;
        }
        let Some(eq_idx) = stripped.find('=') else {
            return Err(format!("Invalid TOML key-value at line {}", i + 1));
        };
        let key: String = stripped[..eq_idx]
            .trim()
            .chars()
            .filter(|c| !matches!(c, '"' | '\''))
            .collect();
        let raw_val = stripped[eq_idx + 1..].trim();
        let unquoted = raw_val
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| raw_val.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(raw_val)
            .to_string();
        if in_sec {
            sec_last_kv = i;
            if key == "command" {
                cmd_val = Some(unquoted);
            }
        } else if key.starts_with("mcp_servers.cito.") {
            has_dotted = true;
            if key == "mcp_servers.cito.command" {
                cmd_val = Some(unquoted);
            }
        }
    }

    if sec_start.is_none() && !has_dotted {
        if existing.trim().is_empty() {
            return Ok((
                "updated".to_string(),
                Some(BLOCK.to_string()),
                "Added [mcp_servers.cito] to config.toml".to_string(),
            ));
        }
        let prefix = if existing.ends_with('\n') {
            existing.to_string()
        } else {
            format!("{existing}\n")
        };
        return Ok((
            "updated".to_string(),
            Some(format!("{prefix}{BLOCK}")),
            "Appended [mcp_servers.cito] to config.toml".to_string(),
        ));
    }

    if cmd_val.as_deref() == Some("cito-mcp") {
        return Ok((
            "unchanged".to_string(),
            None,
            "MCP configuration already up to date".to_string(),
        ));
    }
    if !force {
        return Ok((
            "conflict".to_string(),
            None,
            "Conflicting [mcp_servers.cito] command in config.toml; pass --force to replace".to_string(),
        ));
    }

    if let Some(start) = sec_start {
        let mut out: Vec<&str> = Vec::new();
        out.extend_from_slice(&lines[..start]);
        out.push("[mcp_servers.cito]");
        out.push("command = \"cito-mcp\"");
        out.push("args = []");
        out.extend_from_slice(&lines[sec_last_kv + 1..]);
        let mut joined = out.join("\n");
        if !joined.ends_with('\n') {
            joined.push('\n');
        }
        return Ok((
            "updated".to_string(),
            Some(joined),
            "Replaced [mcp_servers.cito] in config.toml".to_string(),
        ));
    }

    let kept: Vec<&str> = lines
        .into_iter()
        .filter(|line| {
            let Ok(s) = strip_toml_comment(line) else { return true };
            let trimmed = s.trim();
            if trimmed.is_empty() || trimmed.starts_with('[') {
                return true;
            }
            if let Some(eq) = trimmed.find('=') {
                let k: String = trimmed[..eq]
                    .trim()
                    .chars()
                    .filter(|c| !matches!(c, '"' | '\''))
                    .collect();
                return !k.starts_with("mcp_servers.cito.");
            }
            true
        })
        .collect();
    let base = kept.join("\n");
    let prefix = if base.is_empty() || base.ends_with('\n') {
        base
    } else {
        format!("{base}\n")
    };
    Ok((
        "updated".to_string(),
        Some(format!("{prefix}{BLOCK}")),
        "Replaced mcp_servers.cito in config.toml".to_string(),
    ))
}

fn transform_toml_remove(existing: Option<&str>) -> Result<(String, Option<String>, String), String> {
    let Some(existing) = existing else {
        return Ok((
            "unchanged".to_string(),
            None,
            "config.toml does not exist".to_string(),
        ));
    };
    let lines: Vec<&str> = existing.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let mut remove_indices: HashSet<usize> = HashSet::new();
    let mut cur_start: Option<usize> = None;
    let mut cur_last_kv: usize = 0;
    let mut in_sec = false;

    let flush_sec = |in_sec: &mut bool, cur_start: &mut Option<usize>, cur_last_kv: usize, remove_indices: &mut HashSet<usize>| {
        if *in_sec {
            if let Some(st) = *cur_start {
                for j in st..=cur_last_kv {
                    remove_indices.insert(j);
                }
            }
        }
        *in_sec = false;
        *cur_start = None;
    };

    for (i, &line) in lines.iter().enumerate() {
        let stripped = strip_toml_comment(line)?.trim();
        if stripped.is_empty() {
            continue;
        }
        if stripped.starts_with('[') {
            flush_sec(&mut in_sec, &mut cur_start, cur_last_kv, &mut remove_indices);
            let inner = stripped.trim_matches(&['[', ']'][..]).trim();
            let norm: String = inner
                .chars()
                .filter(|c| !matches!(c, '"' | '\'' | ' ' | '\t'))
                .collect();
            if norm == "mcp_servers.cito" || norm.starts_with("mcp_servers.cito.") {
                cur_start = Some(i);
                cur_last_kv = i;
                in_sec = true;
            }
            continue;
        }
        if let Some(eq_idx) = stripped.find('=') {
            let key: String = stripped[..eq_idx]
                .trim()
                .chars()
                .filter(|c| !matches!(c, '"' | '\''))
                .collect();
            if in_sec {
                cur_last_kv = i;
            } else if key.starts_with("mcp_servers.cito.") {
                remove_indices.insert(i);
            }
        }
    }
    flush_sec(&mut in_sec, &mut cur_start, cur_last_kv, &mut remove_indices);

    if remove_indices.is_empty() {
        return Ok((
            "unchanged".to_string(),
            None,
            "config.toml has no [mcp_servers.cito] section".to_string(),
        ));
    }

    let mut next_lines: Vec<&str> = lines
        .into_iter()
        .enumerate()
        .filter_map(|(idx, line)| (!remove_indices.contains(&idx)).then_some(line))
        .collect();

    while next_lines.last() == Some(&"") {
        next_lines.pop();
    }
    let joined = next_lines.join("\n");
    let next_content = if joined.is_empty() {
        String::new()
    } else {
        format!("{joined}\n")
    };
    Ok((
        "updated".to_string(),
        Some(next_content),
        "Removed [mcp_servers.cito] from config.toml".to_string(),
    ))
}

fn transform_mcp_json_apply(existing: Option<&str>, force: bool) -> Result<(String, Option<String>, String), String> {
    let desired = json!({ "command": "cito-mcp", "args": [] });
    let Some(existing) = existing else {
        let init = json!({ "mcpServers": { "cito": desired } });
        return Ok((
            "created".to_string(),
            Some(format!("{}\n", serde_json::to_string_pretty(&init).unwrap())),
            "Created global.mcp.json with cito-mcp".to_string(),
        ));
    };
    let parsed: Value = if existing.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(existing).map_err(|_| "Invalid JSON in global.mcp.json".to_string())?
    };
    let Value::Object(mut obj) = parsed else {
        return Err("Expected JSON object in global.mcp.json".to_string());
    };
    let mut mcp_servers = obj
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if let Some(Value::Object(ex_cito)) = mcp_servers.get("cito") {
        let cmd = ex_cito.get("command").and_then(|v| v.as_str());
        let args_ok = ex_cito
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(false);
        if cmd == Some("cito-mcp") && args_ok {
            return Ok((
                "unchanged".to_string(),
                None,
                "global.mcp.json already configured".to_string(),
            ));
        }
        if cmd.is_some() && cmd != Some("cito-mcp") && !force {
            return Ok((
                "conflict".to_string(),
                None,
                "Conflicting mcpServers.cito command; pass --force to replace".to_string(),
            ));
        }
    }
    mcp_servers.insert("cito".to_string(), desired);
    obj.insert("mcpServers".to_string(), Value::Object(mcp_servers));
    Ok((
        "updated".to_string(),
        Some(format!("{}\n", serde_json::to_string_pretty(&Value::Object(obj)).unwrap())),
        "Configured mcpServers.cito".to_string(),
    ))
}

fn transform_mcp_json_remove(existing: Option<&str>) -> Result<(String, Option<String>, String), String> {
    let Some(existing) = existing else {
        return Ok(("unchanged".to_string(), None, "File does not exist".to_string()));
    };
    let parsed: Value = if existing.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(existing).map_err(|_| "Invalid JSON".to_string())?
    };
    let Value::Object(mut obj) = parsed else {
        return Err("Expected JSON object".to_string());
    };
    let Some(mut mcp_servers) = obj.get("mcpServers").and_then(|v| v.as_object()).cloned() else {
        return Ok(("unchanged".to_string(), None, "No mcpServers.cito".to_string()));
    };
    if mcp_servers.remove("cito").is_none() {
        return Ok(("unchanged".to_string(), None, "No mcpServers.cito".to_string()));
    }
    if mcp_servers.is_empty() {
        obj.remove("mcpServers");
    } else {
        obj.insert("mcpServers".to_string(), Value::Object(mcp_servers));
    }
    Ok((
        "updated".to_string(),
        Some(format!("{}\n", serde_json::to_string_pretty(&Value::Object(obj)).unwrap())),
        "Removed mcpServers.cito".to_string(),
    ))
}

fn transform_clean_legacy_pkg(
    existing: Option<&str>,
    legacy_spec: &str,
) -> Result<(String, Option<String>, String), String> {
    let Some(existing) = existing else {
        return Ok(("unchanged".to_string(), None, "settings.json absent".to_string()));
    };
    let parsed: Value = if existing.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(existing).map_err(|_| "Invalid JSON in settings.json".to_string())?
    };
    let Value::Object(mut obj) = parsed else {
        return Err("Expected JSON object in settings.json".to_string());
    };
    let Some(pkgs) = obj.get("packages").and_then(|v| v.as_array()).cloned() else {
        return Ok(("unchanged".to_string(), None, "No legacy package".to_string()));
    };
    if !pkgs.iter().any(|v| v.as_str() == Some(legacy_spec)) {
        return Ok(("unchanged".to_string(), None, "No legacy package".to_string()));
    }
    let filtered: Vec<Value> = pkgs
        .into_iter()
        .filter(|v| v.as_str() != Some(legacy_spec))
        .collect();
    if filtered.is_empty() {
        obj.remove("packages");
    } else {
        obj.insert("packages".to_string(), Value::Array(filtered));
    }
    Ok((
        "updated".to_string(),
        Some(format!("{}\n", serde_json::to_string_pretty(&Value::Object(obj)).unwrap())),
        format!("Removed legacy {legacy_spec}"),
    ))
}

fn has_hook_cmd(entries: &[Value], hook_cmd: &str) -> bool {
    entries.iter().any(|entry| {
        entry
            .as_object()
            .and_then(|o| o.get("hooks"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|h| {
                    h.as_object()
                        .map(|ho| {
                            ho.get("type").and_then(|v| v.as_str()) == Some("command")
                                && ho
                                    .get("command")
                                    .and_then(|v| v.as_str())
                                    .map(|c| c == hook_cmd || c.ends_with(&format!("/{hook_cmd}")))
                                    .unwrap_or(false)
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
}

fn transform_transcript_settings_apply(
    existing: Option<&str>,
    force: bool,
    channel_env: &str,
    hook_cmd: &str,
) -> Result<(String, Option<String>, String), String> {
    let desired_mcp = json!({
        "command": "cito-mcp",
        "args": [],
        "env": { channel_env: "1" }
    });
    let hook_item = json!({
        "hooks": [{ "type": "command", "command": hook_cmd }]
    });
    let events = ["Stop", "SessionStart", "UserPromptSubmit"];
    let Some(existing) = existing else {
        let init = json!({
            "mcpServers": { "cito": desired_mcp },
            "hooks": {
                "Stop": [hook_item],
                "SessionStart": [hook_item],
                "UserPromptSubmit": [hook_item]
            }
        });
        return Ok((
            "created".to_string(),
            Some(format!("{}\n", serde_json::to_string_pretty(&init).unwrap())),
            "Created settings.json with cito-mcp and hooks".to_string(),
        ));
    };
    let parsed: Value = if existing.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(existing).map_err(|_| "Invalid JSON in settings.json".to_string())?
    };
    let Value::Object(mut obj) = parsed else {
        return Err("Expected JSON object in settings.json".to_string());
    };
    let mut mcp_servers = obj
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let mut mcp_ok = false;
    if let Some(Value::Object(ex_cito)) = mcp_servers.get("cito") {
        let cmd = ex_cito.get("command").and_then(|v| v.as_str());
        if cmd.is_some() && cmd != Some("cito-mcp") && !force {
            return Ok((
                "conflict".to_string(),
                None,
                "Conflicting mcpServers.cito command in settings.json; pass --force to replace".to_string(),
            ));
        }
        let args_ok = ex_cito
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(false);
        let env_ok = ex_cito
            .get("env")
            .and_then(|v| v.as_object())
            .and_then(|e| e.get(channel_env))
            .and_then(|v| v.as_str())
            == Some("1");
        mcp_ok = cmd == Some("cito-mcp") && args_ok && env_ok;
    }

    let mut hooks_obj = obj
        .get("hooks")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let mut hooks_ok = true;
    for ev in events {
        let mut list = hooks_obj
            .get(ev)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if !has_hook_cmd(&list, hook_cmd) {
            hooks_ok = false;
            list.push(hook_item.clone());
        }
        hooks_obj.insert(ev.to_string(), Value::Array(list));
    }

    if mcp_ok && hooks_ok {
        return Ok((
            "unchanged".to_string(),
            None,
            "settings.json already configured".to_string(),
        ));
    }
    mcp_servers.insert("cito".to_string(), desired_mcp);
    obj.insert("mcpServers".to_string(), Value::Object(mcp_servers));
    obj.insert("hooks".to_string(), Value::Object(hooks_obj));
    Ok((
        "updated".to_string(),
        Some(format!("{}\n", serde_json::to_string_pretty(&Value::Object(obj)).unwrap())),
        "Updated settings.json with cito-mcp and hooks".to_string(),
    ))
}

fn transform_transcript_settings_remove(
    existing: Option<&str>,
    hook_cmd: &str,
) -> Result<(String, Option<String>, String), String> {
    let Some(existing) = existing else {
        return Ok(("unchanged".to_string(), None, "settings.json does not exist".to_string()));
    };
    let parsed: Value = if existing.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(existing).map_err(|_| "Invalid JSON in settings.json".to_string())?
    };
    let Value::Object(mut obj) = parsed else {
        return Err("Expected JSON object in settings.json".to_string());
    };
    let mut changed = false;

    if let Some(mut mcp_servers) = obj.get("mcpServers").and_then(|v| v.as_object()).cloned() {
        if mcp_servers.remove("cito").is_some() {
            changed = true;
            if mcp_servers.is_empty() {
                obj.remove("mcpServers");
            } else {
                obj.insert("mcpServers".to_string(), Value::Object(mcp_servers));
            }
        }
    }

    if let Some(hooks_obj) = obj.get("hooks").and_then(|v| v.as_object()).cloned() {
        let mut next_hooks_obj = Map::new();
        for (ev_name, val) in hooks_obj {
            let Some(arr) = val.as_array() else {
                next_hooks_obj.insert(ev_name, val);
                continue;
            };
            let mut next_groups = Vec::new();
            for group_val in arr {
                let Some(mut group_obj) = group_val.as_object().cloned() else {
                    next_groups.push(group_val.clone());
                    continue;
                };
                if let Some(h_arr) = group_obj.get("hooks").and_then(|v| v.as_array()) {
                    let filtered: Vec<Value> = h_arr
                        .iter()
                        .filter(|h| {
                            let is_target = h
                                .as_object()
                                .map(|ho| {
                                    ho.get("type").and_then(|v| v.as_str()) == Some("command")
                                        && ho
                                            .get("command")
                                            .and_then(|v| v.as_str())
                                            .map(|c| c == hook_cmd || c.ends_with(&format!("/{hook_cmd}")))
                                            .unwrap_or(false)
                                })
                                .unwrap_or(false);
                            !is_target
                        })
                        .cloned()
                        .collect();
                    if filtered.len() != h_arr.len() {
                        changed = true;
                    }
                    if !filtered.is_empty() {
                        group_obj.insert("hooks".to_string(), Value::Array(filtered));
                        next_groups.push(Value::Object(group_obj));
                    }
                } else {
                    next_groups.push(Value::Object(group_obj));
                }
            }
            if !next_groups.is_empty() {
                next_hooks_obj.insert(ev_name, Value::Array(next_groups));
            }
        }
        if next_hooks_obj.is_empty() {
            obj.remove("hooks");
        } else {
            obj.insert("hooks".to_string(), Value::Object(next_hooks_obj));
        }
    }

    if !changed {
        return Ok(("unchanged".to_string(), None, "No Cito entries in settings.json".to_string()));
    }
    Ok((
        "updated".to_string(),
        Some(format!("{}\n", serde_json::to_string_pretty(&Value::Object(obj)).unwrap())),
        "Removed Cito mcpServers and hooks from settings.json".to_string(),
    ))
}

pub fn inspect_plugin_setup(plugin: &PluginDescriptor) -> (bool, bool, Vec<String>) {
    let home = plugin_home(plugin);
    match plugin.protocol.as_str() {
        "socket-bridge" => {
            let Some(ext_rel) = plugin.native_str("extensionRelPath") else {
                return (false, false, Vec::new());
            };
            let mcp_rel = plugin.native_str("mcpJsonRelPath").unwrap_or("mcp/global.mcp.json");
            let settings_rel = plugin.native_str("legacySettingsRelPath").unwrap_or("settings.json");
            let legacy_spec = plugin.native_str("legacyPackageSpec").unwrap_or("npm:@tefx/cito");
            let ext_path = home.join(ext_rel);
            let mcp_path = home.join(mcp_rel);
            let settings_path = home.join(settings_rel);
            let installed = home.exists() || ext_path.exists() || mcp_path.exists() || settings_path.exists();
            let ext_ok = fs::read_to_string(&ext_path)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            let mcp_ok = fs::read_to_string(&mcp_path)
                .ok()
                .and_then(|s| transform_mcp_json_apply(Some(&s), false).ok())
                .map(|(st, _, _)| st == "unchanged")
                .unwrap_or(false);
            let has_legacy = fs::read_to_string(&settings_path)
                .ok()
                .and_then(|s| transform_clean_legacy_pkg(Some(&s), legacy_spec).ok())
                .map(|(st, _, _)| st != "unchanged")
                .unwrap_or(false);
            (
                installed,
                ext_ok && mcp_ok && !has_legacy,
                vec![
                    ext_path.to_string_lossy().to_string(),
                    mcp_path.to_string_lossy().to_string(),
                ],
            )
        }
        "sqlite-cli" => {
            let toml_rel = plugin.native_str("tomlRelPath").unwrap_or("config.toml");
            let toml_path = home.join(toml_rel);
            let installed = home.exists() || toml_path.exists();
            let configured = fs::read_to_string(&toml_path)
                .ok()
                .and_then(|s| transform_toml_apply(Some(&s), false).ok())
                .map(|(st, _, _)| st == "unchanged")
                .unwrap_or(false);
            (installed, configured, vec![toml_path.to_string_lossy().to_string()])
        }
        "transcript-hook" => {
            let settings_rel = plugin.native_str("settingsRelPath").unwrap_or("settings.json");
            let channel_env = plugin.native_str("channelEnv").unwrap_or("CITO_TRANSCRIPT_CHANNEL");
            let hook_cmd = plugin.native_str("hookCommand").unwrap_or("cito-hook");
            let settings_path = home.join(settings_rel);
            let installed = home.exists() || settings_path.exists();
            let configured = fs::read_to_string(&settings_path)
                .ok()
                .and_then(|s| transform_transcript_settings_apply(Some(&s), false, channel_env, hook_cmd).ok())
                .map(|(st, _, _)| st == "unchanged")
                .unwrap_or(false);
            (installed, configured, vec![settings_path.to_string_lossy().to_string()])
        }
        _ => (false, false, Vec::new()),
    }
}

pub fn apply_plugin_setup(plugin: &PluginDescriptor, force: bool, dry_run: bool) -> Result<Vec<SetupAction>, String> {
    let home = plugin_home(plugin);
    match plugin.protocol.as_str() {
        "socket-bridge" => {
            let Some(ext_rel) = plugin.native_str("extensionRelPath") else {
                return Ok(Vec::new());
            };
            let ext_src_rel = plugin.native_str("extensionSource").unwrap_or("cito-bridge.ts");
            let mcp_rel = plugin.native_str("mcpJsonRelPath").unwrap_or("mcp/global.mcp.json");
            let settings_rel = plugin.native_str("legacySettingsRelPath").unwrap_or("settings.json");
            let legacy_spec = plugin.native_str("legacyPackageSpec").unwrap_or("npm:@tefx/cito");
            let ext_path = home.join(ext_rel);
            let mcp_path = home.join(mcp_rel);
            let settings_path = home.join(settings_rel);
            let desired_ext = fs::read_to_string(plugin.plugin_dir.join(ext_src_rel)).map_err(|e| e.to_string())?;
            let mut actions = Vec::new();
            actions.push(mutate_file(&plugin.id, &ext_path, dry_run, None, |ex| {
                match ex {
                    None => Ok(("created".to_string(), Some(desired_ext.clone()), "Installed bridge extension".to_string())),
                    Some(s) if s == desired_ext => Ok(("unchanged".to_string(), None, "Bridge extension already up to date".to_string())),
                    Some(_) => Ok(("updated".to_string(), Some(desired_ext.clone()), "Updated bridge extension".to_string())),
                }
            })?);
            actions.push(mutate_file(&plugin.id, &mcp_path, dry_run, None, |ex| {
                transform_mcp_json_apply(ex, force)
            })?);
            if settings_path.exists() {
                let check = mutate_file(&plugin.id, &settings_path, true, None, |ex| {
                    transform_clean_legacy_pkg(ex, legacy_spec)
                })?;
                if check.status != "unchanged" {
                    actions.push(mutate_file(&plugin.id, &settings_path, dry_run, None, |ex| {
                        transform_clean_legacy_pkg(ex, legacy_spec)
                    })?);
                }
            }
            Ok(actions)
        }
        "sqlite-cli" => {
            let toml_rel = plugin.native_str("tomlRelPath").unwrap_or("config.toml");
            let toml_path = home.join(toml_rel);
            Ok(vec![mutate_file(&plugin.id, &toml_path, dry_run, None, |ex| {
                transform_toml_apply(ex, force)
            })?])
        }
        "transcript-hook" => {
            let settings_rel = plugin.native_str("settingsRelPath").unwrap_or("settings.json");
            let channel_env = plugin.native_str("channelEnv").unwrap_or("CITO_TRANSCRIPT_CHANNEL");
            let hook_cmd = plugin.native_str("hookCommand").unwrap_or("cito-hook");
            let settings_path = home.join(settings_rel);
            Ok(vec![mutate_file(&plugin.id, &settings_path, dry_run, None, |ex| {
                transform_transcript_settings_apply(ex, force, channel_env, hook_cmd)
            })?])
        }
        _ => Ok(Vec::new()),
    }
}

pub fn remove_plugin_setup(plugin: &PluginDescriptor) -> Result<Vec<SetupAction>, String> {
    let home = plugin_home(plugin);
    match plugin.protocol.as_str() {
        "socket-bridge" => {
            let Some(ext_rel) = plugin.native_str("extensionRelPath") else {
                return Ok(Vec::new());
            };
            let mcp_rel = plugin.native_str("mcpJsonRelPath").unwrap_or("mcp/global.mcp.json");
            let settings_rel = plugin.native_str("legacySettingsRelPath").unwrap_or("settings.json");
            let legacy_spec = plugin.native_str("legacyPackageSpec").unwrap_or("npm:@tefx/cito");
            let ext_path = home.join(ext_rel);
            let mcp_path = home.join(mcp_rel);
            let settings_path = home.join(settings_rel);
            let mut actions = Vec::new();
            if ext_path.exists() {
                let _ = fs::remove_file(&ext_path);
                actions.push(SetupAction {
                    target: plugin.id.clone(),
                    path: ext_path.to_string_lossy().to_string(),
                    status: "updated".to_string(),
                    backup_path: None,
                    detail: "Removed bridge extension".to_string(),
                });
            } else {
                actions.push(SetupAction {
                    target: plugin.id.clone(),
                    path: ext_path.to_string_lossy().to_string(),
                    status: "unchanged".to_string(),
                    backup_path: None,
                    detail: "Bridge extension already absent".to_string(),
                });
            }
            actions.push(mutate_file(&plugin.id, &mcp_path, false, None, transform_mcp_json_remove)?);
            if settings_path.exists() {
                actions.push(mutate_file(&plugin.id, &settings_path, false, None, |ex| {
                    transform_clean_legacy_pkg(ex, legacy_spec)
                })?);
            }
            Ok(actions)
        }
        "sqlite-cli" => {
            let toml_rel = plugin.native_str("tomlRelPath").unwrap_or("config.toml");
            let toml_path = home.join(toml_rel);
            Ok(vec![mutate_file(&plugin.id, &toml_path, false, None, transform_toml_remove)?])
        }
        "transcript-hook" => {
            let settings_rel = plugin.native_str("settingsRelPath").unwrap_or("settings.json");
            let root_rel = plugin.native_str("rootConfigRelPath").unwrap_or("../.config.json");
            let hook_cmd = plugin.native_str("hookCommand").unwrap_or("cito-hook");
            let settings_path = home.join(settings_rel);
            let root_cfg_path = home.join(root_rel);
            let mut actions = vec![mutate_file(&plugin.id, &settings_path, false, None, |ex| {
                transform_transcript_settings_remove(ex, hook_cmd)
            })?];
            if root_cfg_path.exists() {
                actions.push(mutate_file(&plugin.id, &root_cfg_path, false, None, |ex| {
                    transform_transcript_settings_remove(ex, hook_cmd)
                })?);
            }
            Ok(actions)
        }
        _ => Ok(Vec::new()),
    }
}

pub fn inspect_setup_json(state_dir: &Path, config_path: &Path, home_dir: &Path, plugins: &LoadedPlugins) -> Value {
    let config_exists = config_path.exists();
    let raw = fs::read_to_string(config_path).ok();
    let (default_hub, hubs_obj, rest_obj) = normalize_existing_config(raw.as_deref());
    let mut hubs_out = Map::new();
    for (k, v) in &hubs_obj {
        if let Some(m) = v.as_object() {
            let url = m.get("url").or_else(|| m.get("natsUrl")).and_then(|x| x.as_str()).unwrap_or("local://");
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

pub fn normalize_existing_config(raw: Option<&str>) -> (String, Map<String, Value>, Map<String, Value>) {
    let default_local = || {
        let mut hubs = Map::new();
        hubs.insert(
            "local".to_string(),
            json!({
                "url": "local://",
                "natsUrl": "local://",
                "authMode": "none",
                "enabled": true,
                "defaultScope": "workspace",
            }),
        );
        ("local".to_string(), hubs, Map::new())
    };
    let Some(text) = raw.filter(|s| !s.trim().is_empty()) else {
        return default_local();
    };
    let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(text) else {
        return default_local();
    };
    let mut hubs = Map::new();
    if let Some(Value::Object(raw_hubs)) = obj.remove("hubs") {
        for (k, v) in raw_hubs {
            if let Value::Object(mut hm) = v {
                let url = hm
                    .get("url")
                    .or_else(|| hm.get("natsUrl"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("local://")
                    .to_string();
                let auth = hm
                    .get("authMode")
                    .or_else(|| hm.get("auth"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("none")
                    .to_string();
                let enabled = hm.get("enabled").and_then(|x| x.as_bool()) != Some(false);
                hm.insert("url".to_string(), json!(url));
                hm.insert("natsUrl".to_string(), json!(url));
                hm.insert("authMode".to_string(), json!(auth));
                hm.insert("enabled".to_string(), json!(enabled));
                hubs.insert(k, Value::Object(hm));
            }
        }
    }
    if hubs.is_empty() {
        return default_local();
    }
    let default_hub = obj
        .remove("default_hub")
        .and_then(|v| v.as_str().map(String::from))
        .filter(|d| hubs.contains_key(d))
        .unwrap_or_else(|| hubs.keys().next().unwrap().clone());
    (default_hub, hubs, obj)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
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
    let Ok(meta) = fs::metadata(path) else { return 0 };
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
    let raw = fs::read_to_string(config_path).ok();
    let (default_hub, _, _) = normalize_existing_config(raw.as_deref());

    let daemon_port = env::var("CITO_DAEMON_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(4290);
    let rpc_body = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "cito_unlisten",
            "arguments": {},
            "_meta": { "expected_harness": "human", "expected_session_id": "default" }
        }
    }))
    .unwrap_or_default();
    let _ = http_post_mcp(daemon_port, &rpc_body, Duration::from_millis(300));

    let mut actions = Vec::new();
    for plugin in plugins.agent_plugins() {
        actions.extend(remove_plugin_setup(plugin)?);
    }

    let plist_path = launch_agent_plist_path(home_dir);
    if plist_path.exists() {
        let _ = fs::remove_file(&plist_path);
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
        let _ = fs::remove_dir_all(&bin_dir);
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
            let _ = fs::remove_dir_all(&user_app);
            actions.push(SetupAction {
                target: "cito".to_string(),
                path: user_app.to_string_lossy().to_string(),
                status: "updated".to_string(),
                backup_path: None,
                detail: "Removed ~/Applications/Cito.app".to_string(),
            });
        }
        if state_dir.exists() {
            let _ = fs::remove_dir_all(state_dir);
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
                if !matches!(v.as_str(), "none" | "token" | "oidc") {
                    return Err(format!("Invalid --auth mode: {v}"));
                }
                hub_auth = Some(v);
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
                let exists_str = if config_path.exists() { "exists" } else { "default" };
                println!("Cito Configuration: {} ({exists_str})", config_path.display());
                println!(
                    "Default Hub: {}",
                    inspection.get("defaultHub").and_then(|v| v.as_str()).unwrap_or("local")
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
                        let inst = h.get("installed").and_then(|v| v.as_bool()).unwrap_or(false);
                        let cfg = h.get("configured").and_then(|v| v.as_bool()).unwrap_or(false);
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
            if !validate_harness_id(h)
                || plugins.rejected_ids.contains(h)
                || plugins.find_by_id(h).is_none()
            {
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
            let raw = fs::read_to_string(&config_path).ok();
            let (default_hub, _, _) = normalize_existing_config(raw.as_deref());
            let mut actions = Vec::new();
            for h in &disable_harnesses {
                if let Some(p) = plugins.find_by_id(h) {
                    actions.extend(remove_plugin_setup(p)?);
                }
            }
            emit_setup_result(&config_path, &default_hub, false, &actions, json_output);
            return Ok(());
        }

        for n in enable_hubs.iter().chain(disable_hubs.iter()).chain(remove_hubs.iter()) {
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
            let def_scope = if url == "local://" { "workspace" } else { "self" };
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
            let (mut def_hub, mut hubs, mut rest) = normalize_existing_config(existing);
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
                let en = share_hub.unwrap_or_else(|| prev.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false));
                let p = share_port.unwrap_or_else(|| prev.get("port").and_then(|v| v.as_i64()).unwrap_or(4291));
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
            let next_content = format!("{}\n", serde_json::to_string_pretty(&Value::Object(next_obj)).unwrap());
            match existing {
                None => Ok((
                    "created".to_string(),
                    Some(next_content),
                    format!("Created {} (default_hub: {resolved_default_hub})", config_path.display()),
                )),
                Some(ex) if ex == next_content => Ok((
                    "unchanged".to_string(),
                    None,
                    format!("{} already up to date", config_path.display()),
                )),
                Some(_) => Ok((
                    "updated".to_string(),
                    Some(next_content),
                    format!("Updated {} (default_hub: {resolved_default_hub})", config_path.display()),
                )),
            }
        })?;

        let mut actions = vec![config_action];
        let enabled_set: HashSet<String> = enable_harnesses.into_iter().collect();
        let disabled_set: HashSet<String> = disable_harnesses.into_iter().collect();

        for plugin in plugins.agent_plugins() {
            if disabled_set.contains(&plugin.id) {
                if !dry_run {
                    actions.extend(remove_plugin_setup(plugin)?);
                }
                continue;
            }
            let should_cfg = plugin.tier == "stable" || enabled_set.contains(&plugin.id);
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
            ensure_cli_shims_native(&state_dir, &plugins)?;
            if let Some(en) = launch_at_login {
                set_launch_at_login(en, &state_dir, &home_dir, &plugins)?;
            }
        }

        let ok = emit_setup_result(&config_path, &resolved_default_hub, dry_run, &actions, json_output);
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
            eprintln!("cito setup: conflict in {}: {}", conflict.path, conflict.detail);
        }
    }
    ok
}
