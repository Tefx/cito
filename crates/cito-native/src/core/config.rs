use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub fn repair_default_hub(v: &mut Value) {
    let hubs = v["hubs"].as_object();
    let enabled = |cfg: &Value| cfg["enabled"] != false && cfg["disabled"] != true;
    let current = v["default_hub"]
        .as_str()
        .or_else(|| v["defaultHub"].as_str())
        .filter(|name| hubs.and_then(|h| h.get(*name)).is_some_and(enabled));
    let fallback = current
        .or_else(|| {
            let hubs = hubs?;
            hubs.get_key_value("local")
                .filter(|(_, cfg)| enabled(cfg))
                .map(|(name, _)| name.as_str())
                .or_else(|| {
                    hubs.iter()
                        .filter(|(_, cfg)| enabled(cfg))
                        .map(|(name, _)| name.as_str())
                        .min()
                })
                .or_else(|| hubs.get_key_value("local").map(|(name, _)| name.as_str()))
                .or_else(|| hubs.keys().map(String::as_str).min())
        })
        .unwrap_or("local")
        .to_string();
    if let Some(config) = v.as_object_mut() {
        config.insert("default_hub".into(), json!(fallback));
        config.remove("defaultHub");
    }
}

static CONFIG_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

use crate::support::publication_target;

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
    let path = publication_target(path)?;
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
    let lock_path = PathBuf::from(name);
    let result = {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|e| format!("Cannot lock {}: {e}", path.display()))?;
        // SAFETY: the descriptor remains open until after operation returns; close releases flock.
        while unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.to_string());
            }
        }
        operation()
    };
    // Keep one inode for waiting and newly arriving writers. Unlinking here lets
    // them lock different files and enter the read-modify-write section together.
    result
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
        && !user.eq_ignore_ascii_case("self")
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
                .or_else(|| hm.get("auth_mode"))
                .or_else(|| hm.get("auth"))
                .and_then(Value::as_str)
                .unwrap_or("none"),
        )?;
        for (canonical, aliases) in [
            ("issuer", &["oidcIssuer"][..]),
            ("clientId", &["client_id", "oidcClientId"][..]),
            ("redirectPort", &["redirect_port", "oidcRedirectPort"][..]),
            ("scopes", &["oidcScopes"][..]),
        ] {
            if !hm.contains_key(canonical) {
                if let Some(value) = aliases.iter().find_map(|key| hm.get(*key)).cloned() {
                    hm.insert(canonical.into(), value);
                }
            }
            for alias in aliases {
                hm.remove(*alias);
            }
        }
        hm.remove("request_id");
        hm.remove("auth");
        hm.remove("auth_mode");
        hm.insert("url".to_string(), json!(url));
        hm.insert("natsUrl".to_string(), json!(url));
        hm.insert("authMode".to_string(), json!(auth));
        let enabled = hm.get("enabled").and_then(Value::as_bool) != Some(false)
            && hm.get("disabled").and_then(Value::as_bool) != Some(true);
        hm.remove("disabled");
        hm.insert("enabled".to_string(), json!(enabled));
    }
    let camel = obj.remove("defaultHub");
    let snake = obj.remove("default_hub");
    let default_hub = camel
        .or(snake)
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|name| hubs.contains_key(name))
        .or_else(|| hubs.keys().next().cloned())
        .ok_or("No hubs configured")?;
    Ok((default_hub, hubs, obj))
}

/// Merge a submitted Hub patch before performing discovery or taking a write lock.
pub fn merge_hub_config(base: &Value, patch: &Value) -> Value {
    let mut base = base.clone();
    let mut patch = patch.clone();
    for config in [&mut base, &mut patch] {
        for (canonical, aliases) in [
            ("authMode", &["auth_mode", "auth"][..]),
            ("issuer", &["oidcIssuer"][..]),
            ("clientId", &["client_id", "oidcClientId"][..]),
            ("redirectPort", &["redirect_port", "oidcRedirectPort"][..]),
            ("scopes", &["oidcScopes"][..]),
        ] {
            if config.get(canonical).is_none() {
                if let Some(value) = aliases.iter().find_map(|key| config.get(*key)).cloned() {
                    config[canonical] = value;
                }
            }
            for alias in aliases {
                if let Some(m) = config.as_object_mut() {
                    m.remove(*alias);
                }
            }
        }
    }
    let mut merged = base.as_object().cloned().unwrap_or_default();
    let changed_url = patch
        .get("url")
        .is_some_and(|url| base.get("url") != Some(url));
    if changed_url {
        merged.remove("authMode");
        merged.remove("natsUrl");
    }
    if changed_url {
        for key in ["issuer", "clientId", "redirectPort"] {
            merged.remove(key);
        }
    }
    if let Some(patch) = patch.as_object() {
        for (key, value) in patch {
            if changed_url && key == "natsUrl" {
                continue;
            }
            if matches!(
                key.as_str(),
                "token" | "clientSecret" | "client_secret" | "oidcClientSecret"
            ) && value.as_str().is_none_or(str::is_empty)
            {
                continue;
            }
            if value.is_null() {
                merged.remove(key);
            } else {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(merged)
}

/// Bounded, credential-free discovery. Call outside configuration and service locks.
pub fn discover_hub_config(mut config: Value) -> Result<Value, String> {
    let url = config["url"].as_str().unwrap_or("").trim().to_owned();
    config["url"] = json!(crate::plugins::validate_nats_url(&url)
        .map_err(|error| format!("INVALID_INPUT: {error}"))?);
    let nonempty = |key: &str| config[key].as_str().is_some_and(|s| !s.trim().is_empty());
    let explicit = config["authMode"].as_str().map(str::to_ascii_lowercase);
    let mut mode = explicit.clone().unwrap_or_else(|| {
        if nonempty("token") {
            "token".into()
        } else if nonempty("issuer") || nonempty("clientId") {
            "oidc".into()
        } else {
            String::new()
        }
    });
    if url != "local://" && (mode.is_empty() || mode == "oidc") {
        let health = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(std::time::Duration::from_secs(2))
            .timeout_connect(std::time::Duration::from_secs(1))
            .build()
            .get(&format!("{}/health", url.trim_end_matches('/')))
            .call()
            .ok()
            .and_then(|response| {
                serde_json::from_reader::<_, Value>(std::io::Read::take(
                    response.into_reader(),
                    65536,
                ))
                .ok()
            });
        if let Some(health) = health {
            if mode.is_empty() {
                mode = health["auth_mode"]
                    .as_str()
                    .unwrap_or("none")
                    .to_ascii_lowercase();
            }
            if mode == "oidc" && health["auth_mode"] == "oidc" {
                for (canonical, remote) in [
                    ("issuer", "issuer"),
                    ("clientId", "client_id"),
                    ("redirectPort", "redirect_port"),
                ] {
                    if config.get(canonical).is_none() {
                        if let Some(value) = health["oidc"].get(remote).filter(|v| !v.is_null()) {
                            config[canonical] = value.clone();
                        }
                    }
                }
            }
        }
    }
    if mode.is_empty() {
        mode = "none".into();
    }
    if !matches!(mode.as_str(), "none" | "token" | "oidc") {
        return Err("INVALID_INPUT: Invalid authMode".into());
    }
    config["authMode"] = json!(mode);
    if mode == "token" && config["token"].as_str().is_none_or(|s| s.trim().is_empty()) {
        return Err("INVALID_INPUT: Hub token is required in token mode".into());
    }
    if let Some(port) = config.get("redirectPort") {
        if port.as_u64().is_none_or(|port| port > u16::MAX as u64) {
            return Err("INVALID_INPUT: redirectPort must be an integer from 0 to 65535".into());
        }
    }
    if mode == "oidc" {
        super::login::Settings::new(&config)?;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_preserves_overrides_and_rejects_incomplete_auth() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let responses = [
            json!({"auth_mode":"oidc","oidc":{"issuer":"https://id.example.com","client_id":"discovered","redirect_port":4280}}),
            json!({"auth_mode":"oidc","oidc":{"issuer":"https://id.example.com","client_id":"discovered","redirect_port":4280}}),
            json!({"auth_mode":"token"}),
            json!({"auth_mode":"none"}),
        ];
        let server = std::thread::spawn(move || {
            for value in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                assert_eq!(line, "GET /health HTTP/1.1\r\n");
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                let body = value.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let found = discover_hub_config(json!({"url":url})).unwrap();
        assert_eq!(found["authMode"], "oidc");
        assert_eq!(found["clientId"], "discovered");
        assert_eq!(found["redirectPort"], 4280);
        let override_config = discover_hub_config(json!({"url":url,"authMode":"oidc","issuer":"https://override.example.com","clientId":"explicit","redirectPort":0})).unwrap();
        assert_eq!(override_config["issuer"], "https://override.example.com");
        assert_eq!(override_config["clientId"], "explicit");
        assert_eq!(override_config["redirectPort"], 0);
        assert!(discover_hub_config(json!({"url":url}))
            .unwrap_err()
            .starts_with("INVALID_INPUT:"));
        assert_eq!(
            discover_hub_config(json!({"url":url})).unwrap()["authMode"],
            "none"
        );
        server.join().unwrap();
        let unreachable = "http://127.0.0.1:1";
        assert_eq!(
            discover_hub_config(json!({"url":unreachable})).unwrap()["authMode"],
            "none"
        );
        assert!(discover_hub_config(json!({"url":unreachable,"authMode":"oidc"})).is_err());
        assert_eq!(
            discover_hub_config(
                json!({"url":unreachable,"issuer":"https://id.example.com","clientId":"client"})
            )
            .unwrap()["authMode"],
            "oidc"
        );
        assert_eq!(
            discover_hub_config(json!({"url":"local://","token":"secret"})).unwrap()["authMode"],
            "token"
        );
        assert!(discover_hub_config(
            json!({"url":"local://","issuer":"https://id.example.com","clientId":" padded "})
        )
        .is_err());
        for port in [json!(-1), json!(65536), json!("4280")] {
            assert!(discover_hub_config(json!({"url":"local://","redirectPort":port})).is_err());
        }
    }

    #[test]
    fn canonical_aliases_and_edit_clear_semantics() {
        let (_, hubs, _) = try_normalize_existing_config(Some(r#"{"hubs":{"work":{"url":"https://hub.example.com","auth_mode":"oidc","client_id":"client","redirect_port":0,"oidcScopes":["openid"]}}}"#)).unwrap();
        let base = &hubs["work"];
        assert_eq!(base["authMode"], "oidc");
        assert_eq!(base["redirectPort"], 0);
        assert_eq!(base["scopes"], json!(["openid"]));
        for alias in ["auth_mode", "client_id", "redirect_port", "oidcScopes"] {
            assert!(base.get(alias).is_none());
        }
        let base = json!({"url":"https://old.example.com","authMode":"oidc","issuer":"https://id.example.com","clientId":"old","redirectPort":4280,"scopes":["openid"],"autoListen":true});
        let same = merge_hub_config(&base, &json!({"url":base["url"],"enabled":false}));
        assert_eq!(same["redirectPort"], 4280);
        assert_eq!(same["scopes"], base["scopes"]);
        assert_eq!(same["autoListen"], true);
        let mut with_legacy = base.clone();
        with_legacy["natsUrl"] = base["url"].clone();
        let changed = merge_hub_config(
            &with_legacy,
            &json!({"url":"https://new.example.com","natsUrl":base["url"]}),
        );
        for key in ["issuer", "clientId", "redirectPort", "natsUrl"] {
            assert!(changed.get(key).is_none());
        }
        for key in ["issuer", "clientId", "redirectPort"] {
            let cleared = merge_hub_config(&base, &json!({key:null}));
            assert!(cleared.get(key).is_none());
            for preserved in ["issuer", "clientId", "redirectPort"]
                .into_iter()
                .filter(|k| *k != key)
            {
                assert_eq!(cleared[preserved], base[preserved]);
            }
        }
    }

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
    fn dual_default_hub_aliases_are_removed_from_rest() {
        for snake in ["work", "local"] {
            let raw = json!({
                "defaultHub":"work",
                "default_hub":snake,
                "hubs":{"local":{"url":"local://"},"work":{"url":"local://"}},
                "other":7
            })
            .to_string();
            let (default, _, rest) = try_normalize_existing_config(Some(&raw)).unwrap();
            assert_eq!(default, "work", "{raw}");
            assert_eq!(Value::Object(rest), json!({"other":7}));
        }
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
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
        }
        let results: Vec<_> = children
            .into_iter()
            .map(|child| child.wait_with_output().unwrap())
            .collect();
        for result in results {
            assert!(
                result.status.success(),
                "{}: {} {}",
                result.status,
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
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
