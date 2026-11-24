//! OIDC browser authorization and refresh. Only verified identities enter the token cache.
use super::{auth::OidcVerifier, identity::hash};
use crate::oidc_constants::OIDC_DEFAULT_SCOPES;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(45);
pub struct Cancellation {
    flag: std::sync::atomic::AtomicBool,
    wake: Result<crate::support::WakePipe, String>,
}
impl Default for Cancellation {
    fn default() -> Self {
        Self {
            flag: std::sync::atomic::AtomicBool::new(false),
            wake: crate::support::WakePipe::new(),
        }
    }
}
impl Cancellation {
    pub fn store(&self, value: bool, order: std::sync::atomic::Ordering) {
        self.flag.store(value, order);
        if value {
            if let Ok(wake) = &self.wake {
                wake.notify();
            }
        }
    }
    pub fn load(&self, order: std::sync::atomic::Ordering) -> bool {
        self.flag.load(order)
    }
    pub fn fd(&self) -> Result<i32, String> {
        self.wake
            .as_ref()
            .map(|w| w.read_fd())
            .map_err(Clone::clone)
    }
}
const MAX_JSON: u64 = 1024 * 1024;
type LoginResult = (String, Value);

struct Deadline<'a> {
    at: Instant,
    cancelled: &'a dyn Fn() -> bool,
    cancellation_fd: Option<i32>,
}
impl Deadline<'_> {
    fn remaining(&self) -> Result<Duration, String> {
        if (self.cancelled)() {
            return Err("OIDC_LOGIN_TIMEOUT: OIDC login cancelled".into());
        }
        self.at
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| "OIDC_LOGIN_TIMEOUT: OIDC login deadline exceeded".into())
    }
    fn wait(&self, fds: &mut Vec<libc::pollfd>, until: Instant) -> Result<(), String> {
        self.remaining()?;
        if let Some(fd) = self.cancellation_fd {
            fds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
        }
        crate::support::poll_until(fds, Some(until.min(self.at)))?;
        self.remaining().map(|_| ())
    }
    fn request(&self, request: ureq::Request) -> Result<ureq::Request, String> {
        Ok(request.timeout(self.remaining()?.min(Duration::from_secs(10))))
    }
    fn network_error(&self, error: impl std::error::Error + 'static) -> String {
        if let Err(error) = self.remaining() {
            return error;
        }
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&error);
        while let Some(current) = cause {
            if current
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| {
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    )
                })
            {
                return "OIDC_LOGIN_TIMEOUT: OIDC endpoint timed out".into();
            }
            cause = current.source();
        }
        "AUTH_UNAVAILABLE: OIDC endpoint unavailable".into()
    }
}

/// The open descriptor owns the flock. Never unlink a lockfile: queued users may own it.
struct FileLock(File);
impl FileLock {
    fn acquire(path: &Path, deadline: &Deadline<'_>) -> Result<Self, String> {
        deadline.remaining()?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("AUTH_UNAVAILABLE: Cannot lock {}: {e}", path.display()))?;
        let mut delay = Duration::from_millis(25);
        loop {
            deadline.remaining()?;
            // SAFETY: file owns a valid descriptor; flock neither transfers nor closes it.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                deadline.remaining()?;
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) {
                return Err(format!("AUTH_UNAVAILABLE: OIDC lock: {error}"));
            }
            deadline.wait(&mut Vec::new(), Instant::now() + delay)?;
            delay = (delay * 2).min(Duration::from_secs(1));
        }
    }
}
impl Drop for FileLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor is live until this guard's File is dropped next.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

struct BrowserGuard(Child);
impl BrowserGuard {
    fn check(&mut self) -> Result<(), String> {
        match self.0.try_wait() {
            Ok(Some(status)) if !status.success() => Err(format!(
                "AUTH_UNAVAILABLE: Browser opener exited with {status}"
            )),
            Ok(_) => Ok(()),
            Err(error) => Err(format!("AUTH_UNAVAILABLE: Browser opener: {error}")),
        }
    }
}
impl Drop for BrowserGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

pub(super) struct Settings {
    issuer: String,
    client: String,
    port: u16,
    scopes: Option<String>,
}
impl Settings {
    pub(super) fn new(config: &Value) -> Result<Self, String> {
        let mut config = config.clone();
        super::config::canonicalize_hub_aliases(&mut config);
        let issuer = config
            .get("issuer")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or("INVALID_INPUT: issuer required")?;
        crate::plugins::validate_oidc_url(issuer, true)
            .map_err(|error| format!("INVALID_INPUT: {error}"))?;
        let client = config
            .get("clientId")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or("INVALID_INPUT: clientId required")?;
        if !super::auth::valid_client_id(client) {
            return Err("INVALID_INPUT: Invalid clientId".into());
        }
        let port = match super::config::redirect_port(&config)? {
            Some(port) => port,
            None => match std::env::var("CITO_OIDC_REDIRECT_PORT") {
                Ok(raw) => raw.trim().parse::<u16>().ok().filter(|p| *p > 0).ok_or(
                    "INVALID_INPUT: CITO_OIDC_REDIRECT_PORT must be an integer in 1..65535",
                )?,
                Err(_) => 0,
            },
        };
        let scopes = config
            .get("scopes")
            .map(|value| {
                let scopes: Vec<_> = value
                    .as_array()
                    .ok_or("INVALID_INPUT: scopes must be an array")?
                    .iter()
                    .map(|scope| {
                        scope
                            .as_str()
                            .filter(|scope| {
                                !scope.is_empty()
                                    && scope
                                        .bytes()
                                        .all(|b| matches!(b, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
                            })
                            .ok_or("INVALID_INPUT: Invalid OIDC scope")
                    })
                    .collect::<Result<_, _>>()?;
                if !scopes.contains(&"openid") {
                    return Err("INVALID_INPUT: OIDC scopes must include openid");
                }
                Ok(scopes.join(" "))
            })
            .transpose()?;
        Ok(Self {
            issuer: issuer.to_owned(),
            client: client.to_owned(),
            port,
            scopes,
        })
    }
    fn scopes(&self, discovery: &Value) -> Result<String, String> {
        let supported = discovery
            .get("scopes_supported")
            .map(|value| {
                value
                    .as_array()
                    .and_then(|values| values.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
                    .ok_or("AUTH_UNAVAILABLE: Invalid scopes_supported")
            })
            .transpose()?;
        if supported
            .as_ref()
            .is_some_and(|values| !values.contains(&"openid"))
        {
            return Err("AUTH_UNAVAILABLE: OIDC provider does not support openid".into());
        }
        Ok(self.scopes.clone().unwrap_or_else(|| {
            OIDC_DEFAULT_SCOPES
                .into_iter()
                .filter(|scope| {
                    supported
                        .as_ref()
                        .is_none_or(|values| values.contains(scope))
                })
                .collect::<Vec<_>>()
                .join(" ")
        }))
    }
}

fn cache_path(dir: &Path, hub: &str) -> Result<PathBuf, String> {
    if !crate::plugins::validate_hub_name(hub) {
        return Err("INVALID_INPUT: Invalid hub name".into());
    }
    Ok(dir.join(format!("oidc-token-{hub}.json")))
}
fn read_cache(path: &Path, settings: &Settings) -> std::io::Result<Option<Value>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
        return Ok(None);
    }
    let mut raw = Vec::new();
    file.take(MAX_JSON + 1).read_to_end(&mut raw)?;
    if raw.len() as u64 > MAX_JSON {
        return Ok(None);
    }
    let cache: Value = match serde_json::from_slice(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(token) = cache["id_token"]
        .as_str()
        .filter(|s| super::config::valid_hub_token(s))
    else {
        return Ok(None);
    };
    if cache["issuer"] != settings.issuer
        || cache["client_id"] != settings.client
        || cache["token_sha256"] != hash(token)
        || cache["expires_at"].as_f64().is_none()
        || ["verified_principal_id", "verified_user"]
            .iter()
            .any(|key| cache[*key].as_str().is_none_or(str::is_empty))
        || cache
            .get("refresh_token")
            .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return Ok(None);
    }
    Ok(Some(cache))
}
fn fresh(cache: &Value) -> bool {
    cache["expires_at"]
        .as_f64()
        .is_some_and(|expires| expires > (crate::plugins::now_ms() / 1000 + 60) as f64)
}
fn verify_cache(
    cache: &Value,
    settings: &Settings,
    deadline: &Deadline<'_>,
) -> Result<LoginResult, String> {
    deadline.remaining()?;
    let token = cache["id_token"]
        .as_str()
        .ok_or("UNAUTHORIZED: Invalid cache token")?;
    let identity = OidcVerifier::new(&settings.issuer, &settings.client)?
        .verify_with_deadline(token, deadline.at)?;
    deadline.remaining()?;
    Ok((token.to_owned(), identity))
}

pub fn offline_identity(dir: &Path, hub: &str, config: &Value) -> Option<Value> {
    let settings = Settings::new(config).ok()?;
    let cache = read_cache(&cache_path(dir, hub).ok()?, &settings).ok()??;
    Some(
        json!({"principal_id":cache["verified_principal_id"],"owner_user":cache["verified_user"],"oidc":true}),
    )
}
fn random() -> Result<String, String> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn endpoint(value: &Value, key: &str) -> Result<String, String> {
    let raw = value[key]
        .as_str()
        .ok_or_else(|| format!("AUTH_UNAVAILABLE: Missing {key}"))?;
    crate::plugins::validate_oidc_url(raw, false)
        .map_err(|error| format!("AUTH_UNAVAILABLE: {key}: {error}"))?;
    Ok(raw.into())
}
fn payload(token: &str) -> Result<Value, String> {
    let part = token
        .split('.')
        .nth(1)
        .ok_or("UNAUTHORIZED: Invalid token")?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn response_json(response: ureq::Response, deadline: &Deadline<'_>) -> Result<Value, String> {
    deadline.remaining()?;
    if response.status() != 200 {
        return Err(format!(
            "AUTH_UNAVAILABLE: OIDC endpoint returned HTTP {}",
            response.status()
        ));
    }
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_JSON + 1)
        .read_to_end(&mut body)
        .map_err(|e| deadline.network_error(e))?;
    deadline.remaining()?;
    if body.len() as u64 > MAX_JSON {
        return Err("AUTH_UNAVAILABLE: OIDC response too large".into());
    }
    serde_json::from_slice(&body).map_err(|_| "AUTH_UNAVAILABLE: Invalid OIDC JSON".into())
}
fn publish(path: &Path, cache: &Value, deadline: &Deadline<'_>) -> Result<(), String> {
    deadline.remaining()?;
    if !cache["id_token"]
        .as_str()
        .is_some_and(super::config::valid_hub_token)
    {
        return Err("UNAUTHORIZED: Invalid OIDC bearer token".into());
    }
    let data = cache.to_string();
    if data.len() as u64 > MAX_JSON {
        return Err("AUTH_UNAVAILABLE: OIDC token cache exceeds 1 MiB".into());
    }
    crate::support::atomic_write_checked(path, data.as_bytes(), Some(0o600), || {
        deadline.remaining().map(|_| ())
    })
}

/// Recover a rejected bearer generation even if its JWT has not expired. Callers must pass
/// the hash of the bearer that actually received 401, never the subsequently reloaded cache.
pub fn recover_cancellable(
    dir: &Path,
    hub: &str,
    config: &Value,
    rejected_hash: Option<&str>,
    interactive: bool,
    force_account_select: bool,
    (cancelled, cancellation_fd): (&dyn Fn() -> bool, Option<i32>),
) -> Result<LoginResult, String> {
    let deadline = Deadline {
        at: Instant::now() + LOGIN_TIMEOUT,
        cancelled,
        cancellation_fd,
    };
    recover_inner(
        dir,
        hub,
        &Settings::new(config)?,
        rejected_hash,
        interactive,
        force_account_select,
        &deadline,
    )
}
fn recover_inner(
    dir: &Path,
    hub: &str,
    settings: &Settings,
    rejected: Option<&str>,
    interactive: bool,
    force_account_select: bool,
    deadline: &Deadline<'_>,
) -> Result<LoginResult, String> {
    deadline.remaining()?;
    let path = cache_path(dir, hub)?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let _cache_lock = FileLock::acquire(&path.with_extension("json.lock"), deadline)?;
    let old = match read_cache(&path, settings) {
        Ok(cache) => cache,
        Err(e) if interactive && e.kind() == std::io::ErrorKind::PermissionDenied => None,
        Err(e) => return Err(format!("AUTH_UNAVAILABLE: Cannot read OIDC cache: {e}")),
    };
    let old_refresh = old
        .as_ref()
        .filter(|_| !force_account_select)
        .and_then(|cache| cache["refresh_token"].as_str())
        .filter(|s| !s.is_empty());
    if let Some(cache) = old.as_ref().filter(|cache| {
        !force_account_select && fresh(cache) && cache["token_sha256"].as_str() != rejected
    }) {
        match verify_cache(cache, settings, deadline) {
            Err(error)
                if old_refresh.is_some() && error.starts_with("AUTHORIZATION_VIOLATION:") => {}
            result => return result,
        }
    }
    let verifier = OidcVerifier::new(&settings.issuer, &settings.client)?;
    let agent = ureq::AgentBuilder::new().redirects(0).build();
    let discovery = response_json(
        deadline
            .request(agent.get(&format!(
                "{}/.well-known/openid-configuration",
                settings.issuer.trim_end_matches('/')
            )))?
            .call()
            .map_err(|e| deadline.network_error(e))?,
        deadline,
    )?;
    if discovery["issuer"] != settings.issuer {
        return Err("AUTH_UNAVAILABLE: Discovery issuer mismatch".into());
    }
    let scopes = settings.scopes(&discovery)?;
    let token_url = endpoint(&discovery, "token_endpoint")?;
    let authorization = endpoint(&discovery, "authorization_endpoint")?;
    let refreshed = if let Some(refresh) = old_refresh {
        match deadline.request(agent.post(&token_url))?.send_form(&[
            ("client_id", settings.client.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
        ]) {
            Ok(response) => Some(response_json(response, deadline)?),
            Err(ureq::Error::Status(400 | 401, _)) => {
                deadline.remaining()?;
                None
            }
            Err(error) => return Err(deadline.network_error(error)),
        }
    } else {
        None
    };
    let is_refresh = refreshed.is_some();
    let mut nonce = None;
    // Keep the fixed-port lock until verified publication, including token exchange.
    let mut _port_lock = None;
    let body = if let Some(body) = refreshed {
        body
    } else {
        if !interactive {
            return Err("UNAUTHORIZED: OIDC login required".into());
        }
        if settings.port != 0 {
            // /tmp is host-local and independent of each process's HOME, stateDir and TMPDIR.
            // SAFETY: geteuid has no preconditions and does not access Rust memory.
            let uid = unsafe { libc::geteuid() };
            let lock_path = PathBuf::from(format!(
                "/tmp/cito-oidc-callback-{uid}-{}.lock",
                settings.port
            ));
            _port_lock = Some(FileLock::acquire(&lock_path, deadline)?);
        }
        deadline.remaining()?;
        let listener = TcpListener::bind(("127.0.0.1", settings.port))
            .map_err(|e| format!("AUTH_UNAVAILABLE: OIDC callback bind: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let redirect = format!(
            "http://127.0.0.1:{}/callback",
            listener.local_addr().map_err(|e| e.to_string())?.port()
        );
        let state = random()?;
        let pkce = random()?;
        let request_nonce = random()?;
        nonce = Some(request_nonce.clone());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.as_bytes()));
        let mut url = url::Url::parse(&authorization).map_err(|e| e.to_string())?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", settings.client.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("scope", scopes.as_str()),
            ("state", state.as_str()),
            ("nonce", request_nonce.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]);
        if force_account_select {
            url.query_pairs_mut()
                .append_pair("prompt", "select_account");
        }
        let opener = std::env::var("CITO_BROWSER_BIN").unwrap_or_else(|_| {
            if cfg!(target_os = "macos") {
                "/usr/bin/open".into()
            } else {
                "xdg-open".into()
            }
        });
        deadline.remaining()?;
        let mut browser = BrowserGuard(
            std::process::Command::new(opener)
                .arg(url.as_str())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("AUTH_UNAVAILABLE: Cannot open browser: {e}"))?,
        );
        let mut browser_exit = crate::support::child_exit_fd(browser.0.id()).ok();
        let code = loop {
            deadline.remaining()?;
            browser.check()?;
            match listener.accept() {
                Ok((mut stream, _)) => {
                    if let Some(code) = callback(&mut stream, &state, deadline)? {
                        break code;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut fds = vec![libc::pollfd {
                        fd: listener.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    }];
                    if let Some(exit) = &browser_exit {
                        fds.push(libc::pollfd {
                            fd: exit.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        });
                    }
                    deadline.wait(&mut fds, deadline.at)?;
                    if browser_exit.is_some() && fds[1].revents != 0 {
                        browser.check()?;
                        browser_exit = None;
                    }
                }
                Err(error) => return Err(format!("AUTH_UNAVAILABLE: OIDC callback: {error}")),
            }
        };
        drop(listener);
        response_json(
            deadline
                .request(agent.post(&token_url))?
                .send_form(&[
                    ("client_id", settings.client.as_str()),
                    ("grant_type", "authorization_code"),
                    ("code", &code),
                    ("redirect_uri", &redirect),
                    ("code_verifier", &pkce),
                ])
                .map_err(|error| match error {
                    ureq::Error::Status(status, _) if status < 500 => {
                        format!("UNAUTHORIZED: OIDC token exchange rejected (HTTP {status})")
                    }
                    error => deadline.network_error(error),
                })?,
            deadline,
        )?
    };
    deadline.remaining()?;
    let token = body["id_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("UNAUTHORIZED: Token exchange omitted id_token")?;
    if body
        .get("refresh_token")
        .is_some_and(|value| !value.is_string())
    {
        return Err("UNAUTHORIZED: Invalid refresh_token".into());
    }
    let identity = verifier.verify_with_deadline(token, deadline.at)?;
    deadline.remaining()?;
    let claims = payload(token)?;
    if nonce.is_some_and(|nonce| claims["nonce"] != nonce) {
        return Err("UNAUTHORIZED: OIDC nonce mismatch".into());
    }
    let same_principal = old
        .as_ref()
        .and_then(|cache| cache["verified_principal_id"].as_str())
        .is_some_and(|prev| super::auth::principal_matches(prev, &identity));
    let refresh = body["refresh_token"].as_str().or({
        if is_refresh && same_principal {
            old_refresh
        } else {
            None
        }
    });
    let cache = json!({"issuer":settings.issuer,"client_id":settings.client,"id_token":token,"refresh_token":refresh,"token_sha256":hash(token),"expires_at":claims["exp"],"verified_principal_id":identity["principal_id"],"verified_user":identity["owner_user"]});
    publish(&path, &cache, deadline)?;
    Ok((token.to_owned(), identity))
}

fn callback(
    stream: &mut TcpStream,
    state: &str,
    deadline: &Deadline<'_>,
) -> Result<Option<String>, String> {
    // Darwin can inherit O_NONBLOCK from the accepting listener; timeouts need blocking I/O.
    stream
        .set_nonblocking(false)
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(deadline.remaining()?.min(Duration::from_millis(100))))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(deadline.remaining()?.min(Duration::from_millis(100))))
        .map_err(|e| e.to_string())?;
    let mut line = Vec::new();
    loop {
        deadline.remaining()?;
        let mut byte = [0u8; 1];
        stream
            .set_read_timeout(Some(deadline.remaining()?.min(Duration::from_millis(100))))
            .map_err(|e| e.to_string())?;
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' || line.len() > 8192 {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None)
            }
            Err(_) => return Ok(None),
        }
    }
    let parsed = std::str::from_utf8(&line)
        .ok()
        .filter(|_| line.len() <= 8192)
        .and_then(|line| {
            let mut words = line.split_whitespace();
            if words.next()? != "GET" {
                return None;
            }
            let target = words.next()?.strip_prefix('/')?;
            url::Url::parse(&format!("http://127.0.0.1/{target}")).ok()
        });
    let mut code = None;
    let valid = parsed.as_ref().is_some_and(|url| {
        let states: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .collect();
        let codes: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .collect();
        if codes.len() == 1 {
            code = codes.into_iter().next().filter(|value| !value.is_empty());
        }
        url.path() == "/callback" && states.len() == 1 && states[0] == state
    });
    let message = if valid && code.is_some() {
        "Cito login complete. You may close this window."
    } else {
        "Invalid callback"
    };
    let status = if valid && code.is_some() {
        "200 OK"
    } else {
        "400 Bad Request"
    };
    stream
        .set_write_timeout(Some(deadline.remaining()?.min(Duration::from_millis(100))))
        .map_err(|e| e.to_string())?;
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{message}",
        message.len()
    );
    if valid && code.is_none() {
        return Err("UNAUTHORIZED: OIDC login cancelled".into());
    }
    deadline.remaining()?;
    Ok(if valid { code } else { None })
}

#[cfg(test)]
#[path = "../../tests/hub/login_cases.rs"]
mod tests;
