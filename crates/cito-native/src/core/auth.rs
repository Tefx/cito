//! HTTP authentication. Only cryptographically verified OIDC principals enter the engine.
use super::transport::{http_request, HttpDeadline, HTTP_TLS};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub fn env_value(key: &str) -> Option<String> {
    let value = std::env::var(key).ok()?;
    if matches!(key, "CITO_HUB_TOKEN" | "CITO_OIDC_CLIENT_ID") {
        return Some(value);
    }
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

pub fn principal_ids_equal(a: &str, b: &str) -> bool {
    use super::identity::normalize_legacy_principal;
    !a.is_empty()
        && (a == b
            || normalize_legacy_principal(a).as_deref().unwrap_or(a)
                == normalize_legacy_principal(b).as_deref().unwrap_or(b))
}
/// `principal` must come from verified authentication or the trusted local boundary.
pub fn principal_matches(stored: &str, principal: &Value) -> bool {
    ["principal_id", "legacy_principal_id"]
        .into_iter()
        .any(|key| {
            principal[key]
                .as_str()
                .is_some_and(|id| principal_ids_equal(stored, id))
        })
}

fn validate_oidc_endpoint(value: &str, setting: &str, issuer: bool) -> Result<(), String> {
    crate::plugins::validate_oidc_url(value, issuer)
        .map(|_| ())
        .map_err(|error| {
            format!("{setting} must use HTTPS with valid OIDC URL components: {error}")
        })
}

fn signing_key(key: &jsonwebtoken::jwk::Jwk, algorithm: Algorithm) -> Result<(), &'static str> {
    use jsonwebtoken::jwk::{
        AlgorithmParameters as Key, EllipticCurve, KeyOperations, PublicKeyUse,
    };
    if key
        .common
        .public_key_use
        .as_ref()
        .is_some_and(|u| *u != PublicKeyUse::Signature)
        || key
            .common
            .key_operations
            .as_ref()
            .is_some_and(|ops| !ops.contains(&KeyOperations::Verify))
    {
        return Err("AUTHORIZATION_VIOLATION: JWK is not a verification key");
    }
    let compatible = match (&key.algorithm, algorithm) {
        (
            Key::RSA(_),
            Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 | Algorithm::PS256,
        ) => true,
        (Key::EllipticCurve(ec), Algorithm::ES256) => ec.curve == EllipticCurve::P256,
        (Key::EllipticCurve(ec), Algorithm::ES384) => ec.curve == EllipticCurve::P384,
        (Key::OctetKeyPair(key), Algorithm::EdDSA) => key.curve == EllipticCurve::Ed25519,
        _ => false,
    };
    if !compatible
        || key
            .common
            .key_algorithm
            .is_some_and(|alg| format!("{alg:?}") != format!("{algorithm:?}"))
    {
        return Err("AUTHORIZATION_VIOLATION: JWT key algorithm mismatch");
    }
    Ok(())
}

#[derive(Default)]
struct KeyCache {
    keys: Option<JwkSet>,
    refresh_at: Option<Instant>,
    loaded_at: Option<Instant>,
    refreshing: bool,
    unavailable: bool,
}

fn validate_token_times(claims: &Value, now: u64) -> Result<(), String> {
    let now = now as f64;
    // jose's baseline is strict exp > now - 30. jsonwebtoken accepts equality,
    // so retain the explicit boundary and numeric-type checks after verification.
    if claims["exp"].as_f64().is_none_or(|exp| exp <= now - 30.0)
        || claims
            .get("nbf")
            .is_some_and(|v| v.as_f64().is_none_or(|nbf| nbf > now + 30.0))
    {
        return Err("AUTHORIZATION_VIOLATION: token is expired or not yet valid".into());
    }
    Ok(())
}

pub(super) fn valid_client_id(id: &str) -> bool {
    !id.is_empty() && id.trim() == id && !id.chars().any(char::is_control)
}

pub struct OidcVerifier {
    issuer: String,
    client_id: String,
    cache: Mutex<KeyCache>,
    refreshed: Condvar,
    key_lifetime: Duration,
    refresh_cooldown: Duration,
    tls: Arc<rustls::ClientConfig>,
    jwks_uri: Option<String>,
}

impl OidcVerifier {
    pub fn new(issuer: &str, client_id: &str) -> Result<Self, String> {
        let cooldown = env_value("CITO_JWKS_COOLDOWN_MS")
            .and_then(|s| s.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(60));
        Self::with_cache_policy(issuer, client_id, Duration::from_secs(300), cooldown)
    }

    fn with_cache_policy(
        issuer: &str,
        client_id: &str,
        key_lifetime: Duration,
        refresh_cooldown: Duration,
    ) -> Result<Self, String> {
        validate_oidc_endpoint(issuer, "CITO_OIDC_ISSUER", true)
            .map_err(|e| format!("INVALID_INPUT: {e}"))?;
        if !valid_client_id(client_id) {
            return Err("INVALID_INPUT: Invalid CITO_OIDC_CLIENT_ID".into());
        }
        Ok(Self {
            issuer: issuer.into(),
            client_id: client_id.into(),
            cache: Mutex::new(KeyCache::default()),
            refreshed: Condvar::new(),
            key_lifetime,
            refresh_cooldown,
            tls: HTTP_TLS.clone(),
            jwks_uri: None,
        })
    }

    pub(super) fn with_jwks_uri(mut self, uri: String) -> Self {
        self.jwks_uri = Some(uri);
        self
    }

    fn check_deadline(deadline: Option<HttpDeadline<'_>>) -> Result<(), String> {
        if let Some(deadline) = deadline {
            if deadline.cancelled.is_some_and(|check| check()) {
                return Err("OIDC_LOGIN_TIMEOUT: OIDC login cancelled".into());
            }
            if Instant::now() >= deadline.at {
                return Err("OIDC_LOGIN_TIMEOUT: OIDC verification deadline exceeded".into());
            }
        }
        Ok(())
    }

    fn get_json(
        &self,
        raw: &str,
        setting: &str,
        endpoint: &str,
        deadline: Option<HttpDeadline<'_>>,
    ) -> Result<Value, String> {
        validate_oidc_endpoint(raw, setting, false)
            .map_err(|e| format!("AUTH_UNAVAILABLE: {e}"))?;
        let at = Instant::now() + Duration::from_secs(5);
        let deadline = match deadline {
            Some(deadline) => HttpDeadline {
                at: at.min(deadline.at),
                ..deadline
            },
            None => HttpDeadline::uncancelled(at),
        };
        let request_failed = || format!("AUTH_UNAVAILABLE: {endpoint} request failed");
        let url = url::Url::parse(raw).map_err(|_| request_failed())?;
        let response = http_request(&url, "GET", &[], None, deadline, &self.tls)
            .map_err(|_| request_failed())?;
        if response.status != 200 {
            return Err(if response.status >= 400 {
                request_failed()
            } else {
                format!("AUTH_UNAVAILABLE: {endpoint} did not return HTTP 200")
            });
        }
        let invalid = || format!("AUTH_UNAVAILABLE: Invalid {endpoint} JSON");
        let bytes = response.read_body(1_048_576).map_err(|_| invalid())?;
        serde_json::from_slice(&bytes).map_err(|_| invalid())
    }

    fn refresh(&self, deadline: Option<HttpDeadline<'_>>) -> Result<JwkSet, String> {
        let discovery;
        let uri = if let Some(uri) = &self.jwks_uri {
            uri
        } else {
            discovery = self.get_json(
                &format!(
                    "{}/.well-known/openid-configuration",
                    self.issuer.trim_end_matches('/')
                ),
                "CITO_OIDC_ISSUER discovery URL",
                "OIDC discovery",
                deadline,
            )?;
            if discovery["issuer"] != self.issuer {
                return Err("AUTH_UNAVAILABLE: discovery issuer mismatch".into());
            }
            discovery["jwks_uri"]
                .as_str()
                .ok_or("AUTH_UNAVAILABLE: missing OIDC jwks_uri")?
        };
        let mut document = self.get_json(uri, "OIDC jwks_uri", "JWKS", deadline)?;
        let keys = document
            .get_mut("keys")
            .and_then(Value::as_array_mut)
            .ok_or("AUTH_UNAVAILABLE: Invalid JWKS JSON")?;
        Ok(JwkSet {
            keys: std::mem::take(keys)
                .into_iter()
                .filter_map(|key| serde_json::from_value(key).ok())
                .collect(),
        })
    }

    fn verification_key(
        &self,
        header: &jsonwebtoken::Header,
        deadline: Option<HttpDeadline<'_>>,
        mut refresh_after: Option<Instant>,
    ) -> Result<(jsonwebtoken::jwk::Jwk, Instant), String> {
        let mut attempted = false;
        loop {
            Self::check_deadline(deadline)?;
            let mut cache = self
                .cache
                .lock()
                .map_err(|_| "AUTH_UNAVAILABLE: key cache poisoned")?;
            let mut rejected = "AUTHORIZATION_VIOLATION: unknown JWT kid";
            if let Some(at) = cache
                .loaded_at
                .filter(|at| Some(*at) > refresh_after && at.elapsed() < self.key_lifetime)
            {
                if let Some(keys) = &cache.keys {
                    let mut selected = None;
                    for key in keys
                        .keys
                        .iter()
                        .filter(|key| header.kid.is_none() || key.common.key_id == header.kid)
                    {
                        match signing_key(key, header.alg) {
                            Ok(()) if selected.is_some() => {
                                if header.kid.is_some() {
                                    return Err(
                                        "AUTHORIZATION_VIOLATION: ambiguous JWT signing key".into(),
                                    );
                                }
                                selected = None;
                                break;
                            }
                            Ok(()) => selected = Some(key),
                            Err(reason) if header.kid.is_some() => rejected = reason,
                            Err(_) => {}
                        }
                    }
                    if let Some(key) = selected {
                        return Ok((key.clone(), at));
                    }
                }
            }
            if cache.refreshing {
                let _cache = if let Some(deadline) = deadline {
                    let remaining = deadline.at.saturating_duration_since(Instant::now());
                    self.refreshed
                        .wait_timeout(cache, remaining.min(Duration::from_millis(50)))
                        .map_err(|_| "AUTH_UNAVAILABLE: key cache poisoned")?
                        .0
                } else {
                    self.refreshed
                        .wait(cache)
                        .map_err(|_| "AUTH_UNAVAILABLE: key cache poisoned")?
                };
                continue;
            }
            if attempted
                || cache
                    .refresh_at
                    .is_some_and(|at| at.elapsed() < self.refresh_cooldown)
            {
                return Err(if cache.unavailable
                    || cache
                        .loaded_at
                        .is_none_or(|at| at.elapsed() >= self.key_lifetime)
                {
                    "AUTH_UNAVAILABLE: OIDC keys unavailable; refresh cooldown active"
                } else {
                    rejected
                }
                .into());
            }
            attempted = true;
            cache.refreshing = true;
            cache.refresh_at = Some(Instant::now());
            drop(cache);
            // Network I/O owns no cache lock; cached-key verification stays available.
            let refreshed = self.refresh(deadline);
            let interrupted = Self::check_deadline(deadline);
            cache = self
                .cache
                .lock()
                .map_err(|_| "AUTH_UNAVAILABLE: key cache poisoned")?;
            cache.refreshing = false;
            if let Err(error) = interrupted {
                cache.refresh_at = None;
                self.refreshed.notify_all();
                return Err(error);
            }
            match refreshed {
                Ok(keys) => {
                    cache.keys = Some(keys);
                    cache.loaded_at = Some(Instant::now());
                    refresh_after = None;
                    cache.unavailable = false;
                }
                Err(error) => {
                    cache.unavailable = true;
                    self.refreshed.notify_all();
                    return Err(error);
                }
            }
            self.refreshed.notify_all();
        }
    }

    pub fn verify(&self, token: &str) -> Result<Value, String> {
        self.verify_inner(token, None).map(|(identity, _)| identity)
    }

    pub(super) fn verify_with_deadline(
        &self,
        token: &str,
        deadline: HttpDeadline<'_>,
    ) -> Result<(Value, Value), String> {
        Self::check_deadline(Some(deadline))?;
        let result = self.verify_inner(token, Some(deadline));
        Self::check_deadline(Some(deadline))?;
        result
    }

    fn verify_inner(
        &self,
        token: &str,
        deadline: Option<HttpDeadline<'_>>,
    ) -> Result<(Value, Value), String> {
        if !super::config::valid_hub_token(token) {
            return Err("AUTHORIZATION_VIOLATION: invalid bearer token".into());
        }
        let invalid = "AUTHORIZATION_VIOLATION: invalid JWT header";
        let header = decode_header(token).map_err(|_| invalid)?;
        // The library header does not expose JOSE critical extensions.
        let (raw_header, _) = token.split_once('.').ok_or(invalid)?;
        let extensions: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(raw_header).map_err(|_| invalid)?)
                .map_err(|_| invalid)?;
        if extensions.get("crit").is_some()
            || header.typ.as_deref().is_some_and(|typ| {
                typ.eq_ignore_ascii_case("at+jwt") || typ.eq_ignore_ascii_case("application/at+jwt")
            })
        {
            return Err("AUTHORIZATION_VIOLATION: unsupported JWT header".into());
        }
        if !matches!(
            header.alg,
            Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::PS256
                | Algorithm::ES256
                | Algorithm::ES384
                | Algorithm::EdDSA
        ) {
            return Err("AUTHORIZATION_VIOLATION: unsupported JWT algorithm".into());
        }
        if header.kid.as_deref() == Some("") {
            return Err("AUTHORIZATION_VIOLATION: empty JWT kid".into());
        }
        let (jwk, loaded_at) = self.verification_key(&header, deadline, None)?;
        let decoding_key = |jwk: &jsonwebtoken::jwk::Jwk| -> Result<DecodingKey, String> {
            DecodingKey::from_jwk(jwk)
                .map_err(|_| "AUTHORIZATION_VIOLATION: invalid verification key".into())
        };
        let key = decoding_key(&jwk)?;
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.client_id]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_nbf = true;
        validation.leeway = 30;
        let decoded = decode::<Value>(token, &key, &validation);
        let decoded = if decoded
            .as_ref()
            .is_err_and(|e| matches!(e.kind(), jsonwebtoken::errors::ErrorKind::InvalidSignature))
        {
            let (refreshed, _) = self.verification_key(&header, deadline, Some(loaded_at))?;
            let key = decoding_key(&refreshed)?;
            decode::<Value>(token, &key, &validation)
        } else {
            decoded
        };
        let claims = decoded
            .map_err(|_| "AUTHORIZATION_VIOLATION: invalid JWT signature or claims")?
            .claims;
        if (claims["aud"].as_array().is_some_and(|aud| aud.len() > 1)
            || claims.get("azp").is_some())
            && claims["azp"].as_str() != Some(self.client_id.as_str())
        {
            return Err("AUTHORIZATION_VIOLATION: invalid authorized party".into());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "AUTH_UNAVAILABLE: invalid system clock")?
            .as_secs();
        validate_token_times(&claims, now)?;
        if claims["iat"].as_f64().is_none() {
            return Err("AUTHORIZATION_VIOLATION: missing or invalid JWT iat".into());
        }
        if claims["iss"].as_str() != Some(self.issuer.as_str()) {
            return Err("AUTHORIZATION_VIOLATION: invalid issuer".into());
        }
        let sub = claims["sub"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or("AUTHORIZATION_VIOLATION: missing subject")?;
        let owner = owner_user(&claims, sub);
        let identity = json!({"principal_id": format!("oidc:{}|{}", self.issuer, sub), "owner_user": owner, "sub": sub, "oidc": true, "token_expires_at":claims["exp"], "legacy_principal_id":format!("{}#{}",self.issuer,sub)});
        Ok((identity, claims))
    }
}

fn owner_user(claims: &Value, sub: &str) -> String {
    let owner = claims["preferred_username"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            claims[crate::oidc_constants::OIDC_EMAIL_CLAIM]
                .as_str()
                .and_then(|s| s.split_once('@').map(|x| x.0))
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or(sub);
    let owner: String = owner
        .trim()
        .to_lowercase()
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    owner
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    #[test]
    fn blank_username_and_email_fall_back_to_nonempty_subject() {
        assert_eq!(
            owner_user(
                &json!({"preferred_username":"  ","email":" @example.com"}),
                " Subject "
            ),
            "subject"
        );
        assert_eq!(
            owner_user(
                &json!({"preferred_username":"  ","email":" Alice @example.com"}),
                "subject"
            ),
            "alice"
        );
        assert_eq!(
            owner_user(&json!({"preferred_username":" Bob "}), "subject"),
            "bob"
        );
    }
}

#[cfg(test)]
#[path = "../../tests/hub/auth_cases.rs"]
mod tests;

pub struct HubAuth {
    pub mode: String,
    name: Option<String>,
    pub redirect_port: Option<u16>,
    token: Option<String>,
    verifier: Option<OidcVerifier>,
}
impl HubAuth {
    pub fn new(
        mode: &str,
        token: Option<String>,
        issuer: Option<String>,
        client_id: Option<String>,
    ) -> Result<Self, String> {
        if !matches!(mode, "none" | "token" | "oidc") {
            return Err("INVALID_INPUT: CITO_AUTH_MODE must be none, token, or oidc".into());
        }
        let credential = token.as_deref().filter(|s| !s.is_empty());
        if mode == "token" && credential.is_none() {
            return Err("INVALID_INPUT: CITO_HUB_TOKEN is required in token mode".into());
        }
        if credential.is_some_and(|s| !super::config::valid_hub_token(s)) {
            return Err("INVALID_INPUT: Invalid CITO_HUB_TOKEN".into());
        }
        let verifier = if mode == "oidc" {
            Some(OidcVerifier::new(
                issuer
                    .as_deref()
                    .ok_or("CITO_OIDC_ISSUER is required in oidc mode")?,
                client_id
                    .as_deref()
                    .ok_or("CITO_OIDC_CLIENT_ID is required in oidc mode")?,
            )?)
        } else {
            None
        };
        Ok(Self {
            mode: mode.into(),
            name: None,
            redirect_port: None,
            token,
            verifier,
        })
    }
    pub(super) fn advertise_name(&mut self, explicit: Option<&str>) -> Result<(), String> {
        let normalize = |raw: &str| {
            raw.bytes()
                .any(|b| b.is_ascii_alphanumeric())
                .then(|| super::identity::normalize_slug(raw))
                .filter(|s| super::config::valid_advertised_name(s))
        };
        self.name = match explicit {
            Some(raw) => {
                Some(normalize(raw).ok_or("INVALID_INPUT: Invalid Hub name (local is reserved)")?)
            }
            None => self
                .verifier
                .as_ref()
                .and_then(|v| url::Url::parse(&v.issuer).ok())
                .and_then(|url| super::daemon::host_hub::domain_slug(url.domain()?))
                .filter(|s| super::config::valid_advertised_name(s))
                .or_else(|| normalize(&super::identity::local_host_name())),
        };
        Ok(())
    }
    pub fn health(&self) -> Value {
        let mut health = json!({"ok":true,"pid":std::process::id(),"version":env!("CARGO_PKG_VERSION"),"auth_mode":self.mode});
        if let Some(name) = &self.name {
            health["name"] = json!(name);
        }
        if let Some(verifier) = &self.verifier {
            health["oidc"] = json!({"issuer":verifier.issuer,"client_id":verifier.client_id});
            if let Some(port) = self.redirect_port.filter(|port| *port != 0) {
                health["oidc"]["redirect_port"] = json!(port);
            }
        }
        health
    }
    pub fn authenticate(&self, headers: &HashMap<String, String>) -> Result<Value, String> {
        let user = headers
            .get("x-cito-user")
            .filter(|s| super::config::validate_user(s))
            .map(String::as_str)
            .unwrap_or("local")
            .to_lowercase();
        if self.mode == "none" {
            return Ok(super::identity::principal(&user));
        }
        let bearer = headers
            .get("authorization")
            .and_then(|s| s.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, s)| s)
            .or_else(|| headers.get("x-cito-token").map(String::as_str))
            .ok_or("AUTHORIZATION_VIOLATION: bearer token required")?;
        if let Some(verifier) = &self.verifier {
            return verifier.verify(bearer);
        }
        let a = Sha256::digest(self.token.as_deref().unwrap_or("").as_bytes());
        let b = Sha256::digest(bearer.as_bytes());
        let diff = a.iter().zip(b.iter()).fold(0u8, |d, (a, b)| d | (a ^ b));
        if diff != 0 {
            return Err("AUTHORIZATION_VIOLATION: invalid token".into());
        }
        let mut principal = super::identity::principal(&user);
        principal["auth_mode"] = json!("token");
        Ok(principal)
    }
}
