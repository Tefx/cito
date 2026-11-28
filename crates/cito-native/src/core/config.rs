use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub(super) fn hub_enabled(cfg: &Value) -> bool {
    cfg["enabled"] != false && cfg["disabled"] != true
}

pub fn ordered_hub_names(
    hubs: &Map<String, Value>,
    hub_order: Option<&Value>,
    default_hub: &str,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names: Vec<&str> = hub_order
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|name| hubs.contains_key(*name) && seen.insert(*name))
        .collect();
    let mut remaining: Vec<_> = hubs
        .iter()
        .filter(|(name, _)| !seen.contains(name.as_str()))
        .collect();
    remaining.sort_by_key(|(name, cfg)| {
        let name = name.as_str();
        (
            !hub_enabled(cfg),
            name != default_hub,
            name != "local",
            name,
        )
    });
    names.extend(remaining.into_iter().map(|(name, _)| name.as_str()));
    names.into_iter().map(str::to_owned).collect()
}

pub(super) fn configured_hub_url(cfg: &Value) -> &str {
    cfg.get("url")
        .or_else(|| cfg.get("natsUrl"))
        .and_then(Value::as_str)
        .unwrap_or("local://")
}

fn complete_hub_profiles(hubs: &mut Map<String, Value>, requested: Option<&str>) -> String {
    let default = requested
        .filter(|name| hubs.get(*name).is_some_and(hub_enabled))
        .or_else(|| {
            hubs.iter()
                .min_by_key(|(name, cfg)| {
                    (!hub_enabled(cfg), name.as_str() != "local", name.as_str())
                })
                .map(|(name, _)| name.as_str())
        })
        .unwrap_or("local")
        .to_owned();
    // Preserve a remote-only profile's existing default. Do not add a second
    // local endpoint to standalone configurations that use a local alias.
    if !hubs.contains_key("local")
        && (hubs.is_empty()
            || hubs
                .values()
                .any(|hub| configured_hub_url(hub) != "local://"))
    {
        hubs.insert(
            "local".into(),
            json!({"url":"local://", "natsUrl":"local://", "authMode":"none", "enabled":true}),
        );
        if requested == Some("local") || !hub_enabled(&hubs[&default]) {
            return "local".into();
        }
    }
    default
}

pub fn repair_default_hub(v: &mut Value) {
    let requested = v["default_hub"]
        .as_str()
        .or_else(|| v["defaultHub"].as_str())
        .map(str::to_owned);
    let fallback = v
        .get_mut("hubs")
        .and_then(Value::as_object_mut)
        .map_or_else(
            || "local".into(),
            |hubs| complete_hub_profiles(hubs, requested.as_deref()),
        );
    if let Some(config) = v.as_object_mut() {
        config.insert("default_hub".into(), json!(fallback));
        config.remove("defaultHub");
    }
}

use crate::support::{publication_target, FileLock};

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
    // Dry runs neither mutate configuration nor create lockfiles or directories.
    let _lock = if dry_run {
        None
    } else {
        let mut name = path.into_os_string();
        name.push(".lock");
        // Keep the lockfile so queued and newly arriving writers use the same inode.
        Some(FileLock::acquire(&PathBuf::from(name))?)
    };
    operation()
}

#[derive(PartialEq, Eq)]
pub(super) enum ConfigSnapshot {
    Missing,
    Read(Vec<u8>),
    // Bind recovery to the unreadable file across ownership and writer lock waits.
    Unreadable {
        dev: u64,
        ino: u64,
        ctime: (i64, i64),
    },
}

impl ConfigSnapshot {
    pub(super) fn read(path: &Path) -> Result<Self, String> {
        match fs::read(path) {
            Ok(bytes) => Ok(Self::Read(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::Missing),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                let m = fs::metadata(path).map_err(|e| e.to_string())?;
                if !m.is_file() {
                    return Err(e.to_string());
                }
                Ok(Self::Unreadable {
                    dev: m.dev(),
                    ino: m.ino(),
                    ctime: (m.ctime(), m.ctime_nsec()),
                })
            }
            Err(e) => Err(e.to_string()),
        }
    }

    pub(super) fn bytes(&self) -> Result<&[u8], String> {
        match self {
            Self::Missing => Ok(b"{}"),
            Self::Read(bytes) => Ok(bytes),
            Self::Unreadable { .. } => Err("Cannot read config.json: Permission denied".into()),
        }
    }

    pub(super) fn normalized(&self) -> Result<NormalizedConfig, String> {
        let text =
            std::str::from_utf8(self.bytes()?).map_err(|e| format!("Invalid config.json: {e}"))?;
        try_normalize_existing_config(Some(text))
    }

    pub(super) fn check_current(&self, path: &Path) -> Result<(), String> {
        if *self != Self::read(path)? {
            return Err("STALE_GENERATION: Configuration changed during reset".into());
        }
        Ok(())
    }

    pub(super) fn recovery_object(&self) -> Map<String, Value> {
        self.bytes()
            .ok()
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or_default()
    }

    pub(super) fn reset_hubs_locked(&self, path: &Path, target: &Path) -> Result<(), String> {
        self.check_current(path)?;
        if crate::support::publication_target(path)? != target {
            return Err("STALE_GENERATION: Configuration target changed during reset".into());
        }
        let mut config = self.recovery_object();
        config.remove("defaultHub");
        config.remove("hostHub");
        config.remove("hub_order");
        config.insert("default_hub".into(), json!("local"));
        config.insert("hubs".into(), json!({}));
        config.insert("host_hub".into(), json!({"enabled":false}));
        let bytes = serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?;
        crate::support::atomic_write_with_mode(target, &bytes, Some(0o600))
    }
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

pub(super) fn valid_hub_token(token: &str) -> bool {
    (1..=32 * 1024).contains(&token.len())
        && !token.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn validate_hub_token(token: Option<&Value>) -> Result<(), String> {
    if let Some(value) = token {
        let token = value
            .as_str()
            .ok_or("INVALID_INPUT: Hub token must be a string")?;
        if !token.is_empty() && !valid_hub_token(token) {
            return Err("INVALID_INPUT: Invalid Hub token".into());
        }
    }
    Ok(())
}

pub fn canonical_auth_mode(raw: &str) -> Result<String, String> {
    let mode = raw.trim().to_ascii_lowercase();
    if matches!(mode.as_str(), "none" | "token" | "oidc") {
        Ok(mode)
    } else {
        Err(format!("Invalid auth mode: {raw}"))
    }
}

/// Collapse supported Hub/OIDC aliases with canonical-key precedence and drop retired scope overrides.
pub fn canonicalize_hub_aliases(config: &mut Value) {
    let Some(map) = config.as_object_mut() else {
        return;
    };
    map.remove("defaultScope");
    for (canonical, aliases) in [
        ("authMode", &["auth_mode", "auth"][..]),
        ("issuer", &["oidcIssuer"][..]),
        ("clientId", &["client_id", "oidcClientId"][..]),
        ("redirectPort", &["redirect_port", "oidcRedirectPort"][..]),
        ("scopes", &["oidcScopes"][..]),
    ] {
        if !map.contains_key(canonical) {
            if let Some(value) = aliases.iter().find_map(|key| map.get(*key)).cloned() {
                map.insert(canonical.into(), value);
            }
        }
        for alias in aliases {
            map.remove(*alias);
        }
    }
}

pub(super) fn redirect_port(config: &Value) -> Result<Option<u16>, String> {
    config
        .get("redirectPort")
        .map(|value| {
            value
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| {
                    "INVALID_INPUT: redirectPort must be an integer from 0 to 65535".into()
                })
        })
        .transpose()
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
    for (name, value) in &mut hubs {
        canonicalize_hub_aliases(value);
        let enabled = hub_enabled(value);
        let url = configured_hub_url(value).to_owned();
        let hm = value
            .as_object_mut()
            .ok_or_else(|| format!("Expected object for hub {name}"))?;
        let auth =
            canonical_auth_mode(hm.get("authMode").and_then(Value::as_str).unwrap_or("none"))?;
        validate_hub_token(hm.get("token"))?;
        hm.remove("request_id");
        hm.insert("url".to_string(), json!(url));
        hm.insert("natsUrl".to_string(), json!(url));
        hm.insert("authMode".to_string(), json!(auth));
        hm.remove("disabled");
        hm.insert("enabled".to_string(), json!(enabled));
    }
    if let Some(legacy) = obj.remove("hostHub") {
        obj.entry("host_hub").or_insert(legacy);
    }
    let camel = obj.remove("defaultHub");
    let snake = obj.remove("default_hub");
    let requested = snake
        .as_ref()
        .and_then(Value::as_str)
        .or_else(|| camel.as_ref().and_then(Value::as_str));
    let default_hub = complete_hub_profiles(&mut hubs, requested);
    if let Some(order) = obj.get_mut("hub_order") {
        if !order
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
        {
            return Err("Expected hub_order array of strings in config.json".into());
        }
        *order = json!(ordered_hub_names(&hubs, Some(order), &default_hub));
    }
    Ok((default_hub, hubs, obj))
}

/// Merge only writable host settings; retain extension fields and existing credentials.
/// The caller publishes the result only after all fields and the listener are validated.
pub(crate) fn apply_host_hub_patch(base: &Value, patch: &Value) -> Result<Value, String> {
    let mut host = match base {
        Value::Null => Map::new(),
        Value::Object(settings) => settings.clone(),
        _ => return Err("INVALID_INPUT: Host hub must be an object".into()),
    };
    for key in ["enabled", "port", "bind", "auth_mode", "token"] {
        if let Some(value) = patch.get(key) {
            host.insert(key.into(), value.clone());
        }
    }
    let enabled = host
        .entry("enabled")
        .or_insert(json!(false))
        .as_bool()
        .ok_or("INVALID_INPUT: Host hub enabled must be a boolean")?;
    host.entry("port")
        .or_insert(json!(4291))
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .ok_or("INVALID_INPUT: Host hub port must be an integer from 0 to 65535")?;
    host.entry("bind")
        .or_insert(json!("0.0.0.0"))
        .as_str()
        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        .ok_or("INVALID_INPUT: Invalid host hub bind address")?;
    let token = host
        .get("token")
        .map(|v| {
            v.as_str()
                .ok_or("INVALID_INPUT: Host hub token must be a string")
        })
        .transpose()?
        .unwrap_or("");
    let mode = match host.get("auth_mode") {
        None if enabled || !token.is_empty() => "token",
        None => "none",
        Some(value) => match value.as_str() {
            Some("token") => "token",
            Some("none") => "none",
            _ => return Err("INVALID_INPUT: Host hub auth_mode must be none or token".into()),
        },
    };
    if !token.is_empty() && !valid_hub_token(token) {
        return Err("INVALID_INPUT: Invalid host hub token (maximum 32 KiB; no whitespace or control characters)".into());
    }
    if (enabled || mode == "token") && token.is_empty() {
        host.insert("token".into(), json!(crate::support::try_random_hex(32)?));
    }
    // Keep implicit authentication implicit when disabled and tokenless. Persisting
    // "none" here would opt a future --share-hub into anonymous access.
    if mode == "token" {
        host.insert("auth_mode".into(), json!(mode));
    }
    Ok(Value::Object(host))
}

/// Merge a submitted Hub patch before performing discovery or taking a write lock.
pub fn merge_hub_config(base: &Value, patch: &Value) -> Value {
    let mut base = base.clone();
    let mut patch = patch.clone();
    canonicalize_hub_aliases(&mut base);
    canonicalize_hub_aliases(&mut patch);
    let mut merged = base.as_object().cloned().unwrap_or_default();
    if let Some(fields) = patch.as_object_mut() {
        if let Some(url) = fields.remove("natsUrl") {
            fields.entry("url").or_insert(url);
        }
    }
    let changed_url = patch
        .get("url")
        .is_some_and(|url| url.as_str() != Some(configured_hub_url(&base)));
    if changed_url {
        for key in [
            "authMode",
            "natsUrl",
            "issuer",
            "clientId",
            "redirectPort",
            "token",
        ] {
            merged.remove(key);
        }
    }
    if let Some(patch) = patch.as_object() {
        for (key, value) in patch {
            if matches!(
                key.as_str(),
                "clientSecret" | "client_secret" | "oidcClientSecret"
            ) && value.as_str().is_none_or(str::is_empty)
                || key == "token" && value.as_str() == Some("")
            {
                continue;
            }
            // Keep a submitted non-string token for the validator to reject.
            if value.is_null() && key != "token" {
                merged.remove(key);
            } else {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(merged)
}

pub(super) fn valid_advertised_name(name: &str) -> bool {
    name != "local" && crate::plugins::validate_hub_name(name)
}

pub(super) fn available_hub_name(
    base: Option<String>,
    hubs: Option<&Map<String, Value>>,
    url: &Value,
    editing: Option<&str>,
) -> String {
    let base = base.unwrap_or_else(|| {
        super::daemon::host_hub::hub_name_from_url(url.as_str().unwrap_or_default())
    });
    let mut name = base.clone();
    let mut suffix = 2;
    loop {
        if !name.eq_ignore_ascii_case("local") {
            let other = hubs.and_then(|hubs| {
                hubs.iter().find(|(other, _)| {
                    other.eq_ignore_ascii_case(&name) && editing != Some(other.as_str())
                })
            });
            match other {
                None => return name,
                // Reuse the saved spelling when rejoining the same profile.
                Some((saved, existing)) if editing.is_none() && existing["url"] == *url => {
                    return saved.clone();
                }
                _ => {}
            }
        }
        let ending = format!("-{suffix}");
        name = format!(
            "{}{}",
            base[..base.len().min(32 - ending.len())].trim_end_matches('-'),
            ending
        );
        suffix += 1;
    }
}

/// Bounded, credential-free discovery. Call outside configuration and service locks.
pub fn discover_hub_config(mut config: Value) -> Result<Value, String> {
    let url = crate::plugins::validate_nats_url(config["url"].as_str().unwrap_or_default())?;
    let health = probe_hub_health(&url, hub_enabled(&config));
    config["url"] = json!(url);
    let mut config = apply_hub_auth(config, &health)?;
    if let Some(name) = advertised_name(&health) {
        config["discovered_name"] = json!(name);
    }
    Ok(config)
}

pub(super) fn advertised_name(health: &Value) -> Option<&str> {
    health["name"].as_str().filter(|s| valid_advertised_name(s))
}

pub(super) fn probe_hub_health(url: &str, enabled: bool) -> Value {
    if !enabled || url == "local://" {
        return Value::Null;
    }
    use super::transport::{http_request, HttpDeadline, HTTP_TLS};
    use std::time::{Duration, Instant};

    let Ok(url) = url::Url::parse(&format!("{url}/health")) else {
        return Value::Null;
    };
    let now = Instant::now();
    let deadline = HttpDeadline {
        connect_at: Some(now + Duration::from_secs(1)),
        ..HttpDeadline::uncancelled(now + Duration::from_secs(2))
    };
    http_request(&url, "GET", &[], None, deadline, &HTTP_TLS)
        .ok()
        .filter(|response| (200..300).contains(&response.status))
        .and_then(|response| response.read_body(65_536).ok())
        .and_then(|body| serde_json::from_slice(&body).ok())
        .unwrap_or(Value::Null)
}

pub(super) fn apply_hub_auth(mut config: Value, health: &Value) -> Result<Value, String> {
    if let Some(fields) = config.as_object_mut() {
        fields.remove("discovered_name");
    }
    let nonempty = |key: &str| config[key].as_str().is_some_and(|s| !s.trim().is_empty());
    validate_hub_token(config.get("token"))?;
    let configured = config["authMode"].as_str().unwrap_or_else(|| {
        if nonempty("token") {
            "token"
        } else if nonempty("issuer") || nonempty("clientId") {
            "oidc"
        } else {
            ""
        }
    });
    let mode = if configured.is_empty() {
        health["auth_mode"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("none")
    } else {
        configured
    }
    .to_ascii_lowercase();
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
    if !matches!(mode.as_str(), "none" | "token" | "oidc") {
        return Err("INVALID_INPUT: Invalid authMode".into());
    }
    config["authMode"] = json!(mode);
    if mode == "token" && config["token"].as_str().is_none_or(|s| s.trim().is_empty()) {
        return Err("INVALID_INPUT: Hub token is required in token mode".into());
    }
    if mode == "oidc" {
        super::login::Settings::new(&config)?;
    } else {
        redirect_port(&config)?;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_order_normalization_preserves_explicit_ranks_and_validates_type() {
        let mut raw = json!({"default_hub":"work","hubs":{
            "work":{"url":"https://hub.example.com"},"alpha":{},
            "archive":{"disabled":true},"backup":{"enabled":false}
        }});
        let (default, hubs, rest) = try_normalize_existing_config(Some(&raw.to_string())).unwrap();
        assert!(!rest.contains_key("hub_order"));
        assert_eq!(
            ordered_hub_names(&hubs, None, &default),
            ["work", "local", "alpha", "archive", "backup"]
        );
        raw["hub_order"] = json!(["archive", "unknown", "archive", "alpha"]);
        let (_, _, rest) = try_normalize_existing_config(Some(&raw.to_string())).unwrap();
        assert_eq!(
            rest["hub_order"],
            json!(["archive", "alpha", "work", "local", "backup"])
        );
        for invalid in [Value::Null, json!("work"), json!(["work", 1])] {
            raw["hub_order"] = invalid;
            assert!(try_normalize_existing_config(Some(&raw.to_string()))
                .unwrap_err()
                .contains("hub_order"));
        }
    }

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
            json!({"auth_mode":"token"}),
            json!({"auth_mode":"none", "name":"local"}),
            json!({"auth_mode":"none", "name":"Bad Name"}),
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
                    assert!(!line.to_ascii_lowercase().contains("authorization:"));
                    assert!(!line.to_ascii_lowercase().contains("cookie:"));
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
        let authenticated = discover_hub_config(json!({"url":url,"token":"configured"})).unwrap();
        assert_eq!(authenticated["token"], "configured");
        for _ in 0..2 {
            let found = discover_hub_config(json!({"url":url})).unwrap();
            assert_eq!(found["authMode"], "none");
            assert!(found.get("discovered_name").is_none());
        }
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
    fn discovery_rejects_oversized_and_stalled_health_responses() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        assert!(probe_hub_health("local://", true).is_null());
        assert!(probe_hub_health(&url, false).is_null());
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        listener.set_nonblocking(false).unwrap();
        let server = std::thread::spawn(move || {
            for response in [
                // A truncated read could accept the valid JSON at the front.
                "HTTP/1.1 200 OK\r\nContent-Length: 65537\r\n\r\n{}",
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                stream.write_all(response.as_bytes()).unwrap();
                // The peer must close without needing us to finish the response.
                assert_eq!(stream.read(&mut [0]).unwrap(), 0);
            }
        });
        let start = Instant::now();
        assert!(probe_hub_health(&url, true).is_null());
        assert!(start.elapsed() < Duration::from_secs(1));
        let start = Instant::now();
        assert!(probe_hub_health(&url, true).is_null());
        assert!(start.elapsed() < Duration::from_secs(3));
        server.join().unwrap();
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
        let base = json!({"url":"https://old.example.com","authMode":"oidc","issuer":"https://id.example.com","clientId":"old","redirectPort":4280,"scopes":["openid"],"autoListen":true,"defaultScope":"workspace"});
        let same = merge_hub_config(&base, &json!({"url":base["url"],"enabled":false}));
        assert!(same.get("defaultScope").is_none());
        assert!(merge_hub_config(&same, &json!({"defaultScope":"self"}))
            .get("defaultScope")
            .is_none());
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
    fn token_is_bound_to_the_configured_endpoint() {
        let base = json!({"natsUrl":"https://old.example.com","authMode":"token","token":"old-token","autoListen":true});
        let rejoin = merge_hub_config(&base, &json!({"url":"https://old.example.com"}));
        assert_eq!(rejoin["token"], "old-token");
        assert_eq!(rejoin["autoListen"], true);
        for url_key in ["url", "natsUrl"] {
            let patch = json!({url_key:"https://new.example.com"});
            let changed = merge_hub_config(&base, &patch);
            assert_eq!(changed["url"], "https://new.example.com");
            assert!(changed.get("token").is_none());
            let mut patch = patch;
            patch["token"] = json!("new-token");
            let changed = merge_hub_config(&base, &patch);
            assert_eq!(changed["token"], "new-token");
        }
        for invalid in [Value::Null, json!(true), json!(42)] {
            let patch = json!({"url":"local://","token":invalid});
            let merged = merge_hub_config(&base, &patch);
            assert!(discover_hub_config(merged)
                .unwrap_err()
                .contains("must be a string"));
            let raw = json!({"hubs":{"work":patch}}).to_string();
            assert!(try_normalize_existing_config(Some(&raw))
                .unwrap_err()
                .contains("must be a string"));
        }
    }

    #[test]
    fn hub_tokens_are_limited_by_encoded_bytes() {
        let mut token = "é".repeat(16 * 1024);
        for admitted in [true, false] {
            let config = json!({"authMode":"token","token":token,"url":"local://"});
            let raw = json!({"hubs":{"work":config}}).to_string();
            assert_eq!(try_normalize_existing_config(Some(&raw)).is_ok(), admitted);
            assert_eq!(discover_hub_config(config).is_ok(), admitted);
            assert_eq!(
                super::super::auth::HubAuth::new("token", Some(token.clone()), None, None).is_ok(),
                admitted
            );
            token.push('x');
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
        assert_eq!(hubs["local"]["url"], "local://");
        assert_eq!(hubs["local"]["enabled"], true);
        assert!(hubs["cloud"].get("auth").is_none());
        assert_eq!(hubs["cloud"]["user"], "bob");
        assert_eq!(rest["other"], 7);
    }
    #[test]
    fn host_hub_patch_normalizes_aliases_and_admits_only_valid_listener_settings() {
        for (raw, port) in [
            (json!({"hostHub":{"port":0}}), 0),
            (
                json!({"hostHub":{"port":0},"host_hub":{"port":65535}}),
                65535,
            ),
        ] {
            let (_, _, rest) = try_normalize_existing_config(Some(&raw.to_string())).unwrap();
            assert!(!rest.contains_key("hostHub"));
            let host = apply_host_hub_patch(&rest["host_hub"], &json!({"enabled":true})).unwrap();
            assert_eq!(host["port"], port);
            assert_eq!(host["bind"], "0.0.0.0");
            assert_eq!(host["auth_mode"], "token");
            let token = host["token"].as_str().unwrap();
            assert_eq!(token.len(), 64);
            assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
            assert_eq!(apply_host_hub_patch(&host, &json!({})).unwrap(), host);
        }
        let off = apply_host_hub_patch(&Value::Null, &json!({"enabled":false})).unwrap();
        assert_eq!(off["port"], 4291);
        assert!(off["token"].is_null());
        assert_eq!(
            apply_host_hub_patch(&off, &json!({"enabled":true})).unwrap()["auth_mode"],
            "token"
        );
        assert!(
            apply_host_hub_patch(&off, &json!({"auth_mode":"token"})).unwrap()["token"].is_string()
        );
        for invalid in [
            json!({"port":65536}),
            json!({"port":-1}),
            json!({"port":"80"}),
            json!({"enabled":1}),
            json!({"bind":"?"}),
            json!({"auth_mode":"oidc"}),
            json!({"token":42}),
        ] {
            assert!(apply_host_hub_patch(&off, &invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn repair_and_normalize_choose_the_same_enabled_default() {
        for (hubs, expected) in [
            (json!({"a":{},"local":{}}), "local"),
            (json!({"b":{},"a":{},"local":{"disabled":true}}), "a"),
            (
                json!({"a":{"enabled":false},"local":{"disabled":true}}),
                "local",
            ),
            (json!({"a":{"enabled":false},"z":{"disabled":true}}), "a"),
            (
                json!({"work":{"natsUrl":"https://work.example.com"}}),
                "work",
            ),
            (
                json!({"work":{"url":"https://work.example.com","disabled":true}}),
                "local",
            ),
        ] {
            let mut config = json!({"default_hub":"missing","hubs":hubs});
            assert_eq!(
                try_normalize_existing_config(Some(&config.to_string()))
                    .unwrap()
                    .0,
                expected
            );
            repair_default_hub(&mut config);
            assert_eq!(config["default_hub"], expected);
        }
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
            assert_eq!(default, snake, "{raw}");
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
