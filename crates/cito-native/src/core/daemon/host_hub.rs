use super::DaemonService;
use crate::core::{
    auth::HubAuth,
    hub::{HubEngine, HubServer},
    identity::{local_host_name, normalize_slug, string},
};
use serde_json::{json, Value};
use std::net::{IpAddr, ToSocketAddrs, UdpSocket};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(super) fn replace_host_hub_server(
    local: &HubEngine,
    current: &mut Option<HubServer>,
    settings: Option<(IpAddr, u16, Arc<HubAuth>)>,
) -> Result<(), String> {
    let Some((bind, port, auth)) = settings else {
        current.take();
        return Ok(());
    };
    // Validation has already resolved the IP. Port zero always requests a new socket.
    let same_socket = current.as_ref().is_some_and(|server| {
        port != 0 && server.address.port() == port && server.address.ip() == bind
    });
    let previous = if same_socket {
        let previous = current
            .as_ref()
            .map(|server| (server.address, server.auth.clone()));
        current.take();
        previous
    } else {
        None
    };
    match local.start_server(&bind.to_string(), port, auth) {
        Ok(server) => {
            *current = Some(server);
            Ok(())
        }
        Err(error) => {
            if let Some((address, auth)) = previous {
                *current = Some(
                    local
                        .start_server(&address.ip().to_string(), address.port(), auth)
                        .map_err(|rollback| {
                            format!("{error}; failed to restore host hub listener: {rollback}")
                        })?,
                );
            }
            Err(format!(
                "INVALID_INPUT: Cannot bind host hub listener: {error}"
            ))
        }
    }
}

pub(super) fn host_hub_server_config(
    host: &Value,
) -> Result<Option<(IpAddr, u16, Arc<HubAuth>)>, String> {
    if !host.is_null() && !host.is_object() {
        return Err("INVALID_INPUT: Host hub must be an object".into());
    }
    if host.get("enabled").is_some_and(|v| !v.is_boolean()) {
        return Err("INVALID_INPUT: Host hub enabled must be a boolean".into());
    }
    if host.get("token").is_some_and(|v| !v.is_string()) {
        return Err("INVALID_INPUT: Host hub token must be a string".into());
    }
    let bind = match host.get("bind") {
        Some(value) => value
            .as_str()
            .filter(|bind| !bind.trim().is_empty())
            .ok_or("INVALID_INPUT: Host hub bind must be a non-empty string")?,
        None => "0.0.0.0",
    };
    let bind = bind
        .parse::<IpAddr>()
        .map_err(|_| "INVALID_INPUT: Invalid host hub bind address")?;
    let port = match host.get("port") {
        Some(value) => value
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .ok_or("INVALID_INPUT: Host hub port must be an integer from 0 to 65535")?,
        None => 4291,
    };
    let mode = host_hub_auth_mode(host);
    if host.get("auth_mode").is_some_and(|v| !v.is_string()) || !matches!(mode, "none" | "token") {
        return Err("INVALID_INPUT: Host hub auth_mode must be none or token".into());
    }
    if host["enabled"] != true {
        return Ok(None);
    }
    let auth = HubAuth::new(mode, host["token"].as_str().map(str::to_string), None, None)?;
    Ok(Some((bind, port, Arc::new(auth))))
}

pub(super) fn host_hub_auth_mode(host: &Value) -> &str {
    host["auth_mode"].as_str().unwrap_or_else(|| {
        if string(host, "token").is_empty() {
            "none"
        } else {
            "token"
        }
    })
}

pub(super) fn tailscale_host_from_status(status: &Value) -> Option<String> {
    if status["BackendState"] != "Running" || status["Self"]["Online"] == Value::Bool(false) {
        return None;
    }
    let dns = string(&status["Self"], "DNSName").trim_end_matches('.');
    if !dns.is_empty()
        && !dns.ends_with(".local")
        && status["CurrentTailnet"]["MagicDNSEnabled"] != Value::Bool(false)
    {
        return Some(dns.to_owned());
    }
    status["Self"]["TailscaleIPs"]
        .as_array()?
        .iter()
        .filter_map(|ip| ip.as_str()?.parse::<std::net::Ipv4Addr>().ok())
        .next()
        .map(|ip| ip.to_string())
}

#[derive(Debug)]
struct ProbeError;

fn probe_tailscale_status() -> Result<Value, ProbeError> {
    let executable = [
        "/opt/homebrew/bin/tailscale",
        "/usr/local/bin/tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).is_file())
    .unwrap_or("tailscale");
    let mut command = Command::new(executable);
    command
        .args(["status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = crate::support::run_command_with_timeout(command, Duration::from_millis(500))
        .map_err(|_| ProbeError)?
        .ok_or(ProbeError)?;
    if !output.status.success() || output.stdout.len() > 4 * 1024 * 1024 {
        return Err(ProbeError);
    }
    serde_json::from_slice(&output.stdout).map_err(|_| ProbeError)
}

fn retain_confirmed_host(
    previous: Option<String>,
    probe: Result<Value, ProbeError>,
) -> Option<String> {
    match probe {
        Ok(status) if status["Self"]["Online"] == false => None,
        Ok(status) if status["BackendState"] == "Running" => {
            tailscale_host_from_status(&status).or(previous)
        }
        Ok(status)
            if status["BackendState"].as_str().is_some_and(|state| {
                matches!(
                    state,
                    "Stopped" | "NeedsLogin" | "NeedsMachineAuth" | "NoState"
                )
            }) =>
        {
            None
        }
        // An incomplete response carries no confirmed online/offline observation.
        _ => previous,
    }
}

pub(super) fn tailscale_advertised_host() -> Option<String> {
    if let Ok(host) = std::env::var("CITO_TAILSCALE_HOST") {
        let host = host.trim().trim_end_matches('.');
        if !host.is_empty() {
            return Some(host.to_owned());
        }
    }
    // Unit tests use the pure JSON projection; integration fixtures are headless.
    if cfg!(test)
        || std::env::var("CITO_UI_HEADLESS").as_deref() == Ok("1")
        || std::env::var("CITO_DISABLE_TAILSCALE_PROBE").as_deref() == Ok("1")
    {
        return None;
    }
    static CACHE: Mutex<Option<(Instant, Option<String>)>> = Mutex::new(None);
    let mut cached = CACHE.lock().ok()?;
    if let Some((_, host)) = cached
        .as_ref()
        .filter(|(time, _)| time.elapsed() < Duration::from_secs(30))
    {
        return host.clone();
    }
    let host = retain_confirmed_host(
        cached.as_ref().and_then(|(_, host)| host.clone()),
        probe_tailscale_status(),
    );
    *cached = Some((Instant::now(), host.clone()));
    host
}

pub(super) fn advertised_hub_host(bind: &str) -> String {
    if !matches!(bind, "" | "0.0.0.0" | "::") {
        return bind.into();
    }
    if let Some(host) = tailscale_advertised_host() {
        return host;
    }
    lan_advertised_host(bind)
}

pub(super) fn lan_advertised_host(bind: &str) -> String {
    if !matches!(bind, "" | "0.0.0.0" | "::") {
        return bind.into();
    }
    // A UDP connect selects the default route's source IPv4 without sending traffic.
    let routed_ip = UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|socket| {
            socket.connect("192.0.2.1:9").ok()?;
            socket.local_addr().ok().map(|address| address.ip())
        })
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified());
    if let Some(ip) = routed_ip {
        return ip.to_string();
    }
    let hostname = local_host_name();
    if !hostname.is_empty()
        && (hostname.as_str(), 0)
            .to_socket_addrs()
            .is_ok_and(|mut addresses| {
                addresses.any(|addr| !addr.ip().is_loopback() && !addr.ip().is_unspecified())
            })
    {
        return hostname;
    }
    "127.0.0.1".into()
}

pub(super) fn hub_name_from_url(raw: &str) -> String {
    let candidate = url::Url::parse(raw.trim())
        .ok()
        .and_then(|url| match url.host()? {
            url::Host::Domain(host) => host
                .split('.')
                .find(|label| label.chars().any(|c| c.is_ascii_alphanumeric()))
                .map(str::to_owned),
            url::Host::Ipv4(ip) => {
                let octets = ip.octets();
                Some(
                    if ip.is_private() {
                        "lan"
                    } else if octets[0] == 100 && (64..=127).contains(&octets[1]) {
                        "tailscale"
                    } else {
                        "remote"
                    }
                    .into(),
                )
            }
            _ => None,
        })
        .unwrap_or_else(|| "remote".into());
    normalize_slug(&candidate)
}

pub(super) fn parse_hub_invite(raw: &str) -> Result<Value, String> {
    let raw = raw.trim();
    let invalid = || "INVALID_INPUT: Invalid hub invite".to_owned();
    let mut value = if raw.starts_with('{') {
        let value: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
        if !value.is_object() {
            return Err(invalid());
        }
        value
    } else if raw.starts_with("http://") || raw.starts_with("https://") {
        let url = url::Url::parse(raw).map_err(|_| invalid())?;
        url.host_str().ok_or_else(invalid)?;
        json!({"url":raw})
    } else {
        // Join codes generated by Cito contain no shell quoting or expansion.
        // Reject malformed/unknown options rather than executing clipboard text.
        let mut words = raw.split_whitespace();
        if words.next() != Some("cito") || words.next() != Some("setup") {
            return Err(invalid());
        }
        let mut value = json!({});
        while let Some(word) = words.next() {
            if word == "--default" {
                value["set_default"] = json!(true);
                continue;
            }
            let (option, argument) = match word.split_once('=') {
                Some(pair) => pair,
                None => (word, words.next().ok_or_else(invalid)?),
            };
            let key = match option {
                "--hub" => "name",
                "--url" => "url",
                "--auth" => "auth_mode",
                "--token" => "token",
                "--issuer" => "issuer",
                "--client-id" => "client_id",
                "--redirect-port" => "redirect_port",
                "--user" => "user",
                _ => return Err(invalid()),
            };
            if argument.is_empty() || argument.starts_with("--") {
                return Err(invalid());
            }
            value[key] = if key == "redirect_port" {
                json!(argument.parse::<u16>().map_err(|_| invalid())?)
            } else {
                json!(argument)
            };
        }
        value
    };
    if value["name"].is_null() {
        value["name"] = value["hub"].clone();
    }
    // Leave absent names for setup_save_hub to derive and disambiguate under
    // the configuration lock. Explicit names use the same normalization there.
    if !string(&value, "name").is_empty()
        && !string(&value, "name")
            .chars()
            .any(|c| c.is_ascii_alphanumeric())
    {
        return Err(invalid());
    }
    if string(&value, "url") != "local://" {
        let url = url::Url::parse(string(&value, "url")).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(invalid());
        }
    }
    value["op"] = json!("setup_save_hub");
    if value["set_default"].is_null() {
        value["set_default"] = json!(true);
    }
    Ok(value)
}

impl DaemonService {
    pub(super) fn sync_host_hub(&mut self) -> Result<(), String> {
        if self.closing {
            return Ok(());
        }
        let raw = self.raw_config()?;
        let host = raw
            .get("host_hub")
            .or_else(|| raw.get("hostHub"))
            .unwrap_or(&Value::Null);
        if self.applied_host_config.as_ref() == Some(host) {
            // Restoring the applied config resolves reload errors, not rejected UI actions.
            if self.host_hub_reload_failed {
                self.last_host_hub_error = None;
                self.host_hub_reload_failed = false;
            }
            return Ok(());
        }
        match host_hub_server_config(host).and_then(|settings| {
            replace_host_hub_server(&self.local, &mut self.host_server, settings)
        }) {
            Ok(()) => {
                self.applied_host_config = Some(host.clone());
                self.last_host_hub_error = None;
                self.host_hub_reload_failed = false;
                Ok(())
            }
            Err(error) => {
                self.last_host_hub_error = Some(error.clone());
                self.host_hub_reload_failed = true;
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    #[test]
    fn probe_errors_retain_last_confirmed_host_until_confirmed_offline() {
        let host = Some("device.tailnet.ts.net".to_owned());
        assert_eq!(retain_confirmed_host(host.clone(), Err(ProbeError)), host);
        for status in [
            json!({}),
            json!({"BackendState":"Running"}),
            json!({"BackendState":"Running","Self":null}),
            json!({"BackendState":"Running","Self":{}}),
            json!({"BackendState":"Running","Self":{"Online":true,"DNSName":"","TailscaleIPs":[]}}),
            json!({"BackendState":"Starting"}),
        ] {
            assert_eq!(
                retain_confirmed_host(host.clone(), Ok(status.clone())),
                host,
                "{status}"
            );
            assert_eq!(retain_confirmed_host(None, Ok(status)), None);
        }
        assert_eq!(
            retain_confirmed_host(host.clone(), Ok(json!({"BackendState":"Stopped"}))),
            None
        );
        assert_eq!(
            retain_confirmed_host(
                host.clone(),
                Ok(json!({"BackendState":"Running","Self":{"Online":false}}))
            ),
            None
        );
        assert_eq!(
            retain_confirmed_host(
                None,
                Ok(json!({"BackendState":"Running","Self":{"DNSName":"device.tailnet.ts.net."}}))
            ),
            host
        );
    }
}
