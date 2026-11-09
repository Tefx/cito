use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

static CONFIG_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    if let Ok(canonical) = absolute.canonicalize() {
        return Ok(canonical);
    }
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    if let (Some(parent), Some(name)) = (normalized.parent(), normalized.file_name()) {
        if let Ok(parent) = parent.canonicalize() {
            return Ok(parent.join(name));
        }
    }
    Ok(normalized)
}

/// Serialize a complete read-modify-write operation, including the initial read.
/// Readers that need a filesystem-pure dry run can use `with_config_lock_mode`.
pub fn with_config_lock<T>(
    path: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    with_config_lock_mode(path, false, operation)
}

pub fn with_config_lock_mode<T>(
    path: &Path,
    dry_run: bool,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let path = absolute_path(path)?;
    let lock = CONFIG_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "Config lock registry poisoned".to_string())?
        .entry(path.clone())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    let _guard = lock
        .lock()
        .map_err(|_| "Config path lock poisoned".to_string())?;
    // Dry runs never create lockfiles or directories. They do not perform a mutation.
    if dry_run {
        return operation();
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(PathBuf::from(name))
        .map_err(|e| format!("Cannot lock {}: {e}", path.display()))?;
    // SAFETY: the descriptor remains open until after operation returns; close releases flock.
    while unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.to_string());
        }
    }
    operation()
}

pub fn read_config_text(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            parse_config_object(&raw)?;
            Ok(Some(raw))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Cannot read {}: {e}", path.display())),
    }
}

pub fn parse_config_object(raw: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(raw)
        .map_err(|e| format!("Invalid JSON in config.json: {e}"))?
    {
        Value::Object(map) => Ok(map),
        _ => Err("Expected JSON object in config.json".to_string()),
    }
}

pub fn validate_user(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 64
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub fn canonical_auth_mode(raw: &str) -> Result<String, String> {
    let mode = raw.trim().to_ascii_lowercase();
    if matches!(mode.as_str(), "none" | "token" | "oidc") {
        Ok(mode)
    } else {
        Err(format!("Invalid auth mode: {raw}"))
    }
}

pub type NormalizedConfig = (String, Map<String, Value>, Map<String, Value>);

pub fn try_normalize_existing_config(raw: Option<&str>) -> Result<NormalizedConfig, String> {
    let mut obj = match raw {
        Some(raw) => parse_config_object(raw)?,
        None => Map::new(),
    };
    let mut hubs = match obj.remove("hubs") {
        Some(Value::Object(hubs)) => hubs,
        Some(_) => return Err("Expected hubs object in config.json".to_string()),
        None => Map::new(),
    };
    if hubs.is_empty() {
        hubs.insert("local".to_string(), json!({"url":"local://", "natsUrl":"local://", "authMode":"none", "enabled":true, "defaultScope":"workspace"}));
    }
    for (name, value) in &mut hubs {
        let hm = value
            .as_object_mut()
            .ok_or_else(|| format!("Expected object for hub {name}"))?;
        let url = hm
            .get("url")
            .or_else(|| hm.get("natsUrl"))
            .and_then(Value::as_str)
            .unwrap_or("local://")
            .to_string();
        let auth = canonical_auth_mode(
            hm.get("authMode")
                .or_else(|| hm.get("auth"))
                .and_then(Value::as_str)
                .unwrap_or("none"),
        )?;
        hm.remove("auth");
        hm.insert("url".to_string(), json!(url));
        hm.insert("natsUrl".to_string(), json!(url));
        hm.insert("authMode".to_string(), json!(auth));
        let enabled = hm.get("enabled").and_then(Value::as_bool) != Some(false)
            && hm.get("disabled").and_then(Value::as_bool) != Some(true);
        hm.remove("disabled");
        hm.insert("enabled".to_string(), json!(enabled));
    }
    let default_hub = obj
        .remove("defaultHub")
        .or_else(|| obj.remove("default_hub"))
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|name| hubs.contains_key(name))
        .or_else(|| hubs.keys().next().cloned())
        .ok_or("No hubs configured")?;
    Ok((default_hub, hubs, obj))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_and_non_object_configs_are_rejected() {
        for raw in ["", "{", "[]", "null", "{\"hubs\":[]}"] {
            assert!(try_normalize_existing_config(Some(raw)).is_err(), "{raw}");
        }
    }
    #[test]
    fn alias_is_removed_and_unrelated_fields_survive() {
        let (default, hubs, rest) = try_normalize_existing_config(Some(r#"{"default_hub":"cloud","other":7,"hubs":{"cloud":{"url":"https://example.com","auth":"ToKeN","user":"bob"}}}"#)).unwrap();
        assert_eq!(default, "cloud");
        assert_eq!(hubs["cloud"]["authMode"], "token");
        assert!(hubs["cloud"].get("auth").is_none());
        assert_eq!(hubs["cloud"]["user"], "bob");
        assert_eq!(rest["other"], 7);
    }
    #[test]
    fn disabled_hub_alias_is_folded_into_enabled() {
        for (cfg, enabled) in [
            (json!({}), true),
            (json!({"enabled":true}), true),
            (json!({"enabled":false}), false),
            (json!({"disabled":false}), true),
            (json!({"disabled":true}), false),
            (json!({"enabled":true,"disabled":false}), true),
            (json!({"enabled":true,"disabled":true}), false),
            (json!({"enabled":false,"disabled":false}), false),
            (json!({"enabled":false,"disabled":true}), false),
        ] {
            let raw = json!({"hubs":{"work":cfg}}).to_string();
            let (_, hubs, _) = try_normalize_existing_config(Some(&raw)).unwrap();
            assert_eq!(hubs["work"]["enabled"], enabled, "{raw}");
            assert!(hubs["work"].get("disabled").is_none(), "{raw}");
        }
    }
    #[test]
    fn concurrent_mutations_preserve_all_updates() {
        let dir = PathBuf::from(std::env::var("HOME").expect("isolated HOME")).join(format!(
            "cito-config-lock-{}-{}",
            std::process::id(),
            crate::plugins::random_hex(6)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        fs::write(&path, "0").unwrap();
        let mut workers = Vec::new();
        for _ in 0..8 {
            let path = path.clone();
            workers.push(std::thread::spawn(move || {
                for _ in 0..20 {
                    with_config_lock(&path, || {
                        let n: usize = fs::read_to_string(&path).unwrap().parse().unwrap();
                        std::thread::yield_now();
                        fs::write(&path, (n + 1).to_string()).map_err(|e| e.to_string())
                    })
                    .unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "160");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    #[ignore = "subprocess worker invoked by process_locks_serialize_updates"]
    fn config_lock_process_worker() {
        let path = PathBuf::from(std::env::var("CITO_CONFIG_LOCK_TEST_PATH").expect("worker path"));
        for _ in 0..25 {
            with_config_lock(&path, || {
                let n: usize = fs::read_to_string(&path).unwrap().parse().unwrap();
                std::thread::sleep(std::time::Duration::from_millis(1));
                fs::write(&path, (n + 1).to_string()).map_err(|e| e.to_string())
            })
            .unwrap();
        }
    }

    #[test]
    fn process_locks_serialize_updates() {
        let dir = PathBuf::from(std::env::var("HOME").expect("isolated HOME")).join(format!(
            "cito-process-lock-{}",
            crate::plugins::random_hex(6)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("counter");
        fs::write(&path, "0").unwrap();
        let mut children = Vec::new();
        for _ in 0..4 {
            children.push(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--ignored",
                        "config_lock_process_worker",
                        "--test-threads=1",
                    ])
                    .env("CITO_CONFIG_LOCK_TEST_PATH", &path)
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        assert_eq!(fs::read_to_string(path).unwrap(), "100");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dry_run_lock_does_not_create_paths() {
        let path = PathBuf::from(std::env::var("HOME").expect("isolated HOME"))
            .join(format!("cito-dry-{}", crate::plugins::random_hex(6)))
            .join("config.json");
        with_config_lock_mode(&path, true, || Ok(())).unwrap();
        assert!(!path.parent().unwrap().exists());
    }
}
