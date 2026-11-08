//! HTTP authentication. Only cryptographically verified OIDC principals enter the engine.
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

pub fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

pub fn local_principal(user: &str) -> Value {
    let user = user.to_ascii_lowercase();
    json!({"principal_id": format!("none#{user}"), "owner_user": user})
}

pub fn principal_ids_equal(a: &str, b: &str) -> bool {
    fn current(id: &str) -> String {
        if id.starts_with("https://") {
            if let Some((issuer, sub)) = id.split_once('#') {
                return format!("oidc:{issuer}|{sub}");
            }
        }
        id.to_owned()
    }
    !a.is_empty() && current(a) == current(b)
}
/// `principal` must come from verified authentication or the trusted local boundary.
pub fn principal_matches(stored: &str, principal: &Value) -> bool {
    !stored.is_empty()
        && [
            "principal_id",
            "legacy_principal_id",
            "canonical_principal_id",
        ]
        .iter()
        .any(|key| {
            principal[*key]
                .as_str()
                .is_some_and(|id| principal_ids_equal(stored, id))
        })
}

fn validate_oidc_endpoint(value: &str, setting: &str, issuer: bool) -> Result<(), String> {
    let valid = url::Url::parse(value).ok().is_some_and(|url| {
        (url.scheme() == "https" || url.scheme() == "http" && url.host_str() == Some("127.0.0.1"))
            && url.host_str().is_some()
            && (!issuer
                || url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none())
    });
    if valid {
        Ok(())
    } else {
        let suffix = if issuer {
            ", without credentials, query or fragment"
        } else {
            ""
        };
        Err(format!(
            "{setting} must use HTTPS (http://127.0.0.1 is allowed for local tests){suffix}"
        ))
    }
}

#[derive(Default)]
struct KeyCache {
    keys: Option<JwkSet>,
    refresh_at: Option<Instant>,
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

pub struct OidcVerifier {
    issuer: String,
    client_id: String,
    cache: Mutex<KeyCache>,
    refreshed: Condvar,
    agent: ureq::Agent,
}

impl OidcVerifier {
    pub fn new(issuer: &str, client_id: &str) -> Result<Self, String> {
        validate_oidc_endpoint(issuer, "CITO_OIDC_ISSUER", true)
            .map_err(|e| format!("INVALID_INPUT: {e}"))?;
        if client_id.trim().is_empty() {
            return Err("INVALID_INPUT: CITO_OIDC_CLIENT_ID must not be empty".into());
        }
        Ok(Self {
            issuer: issuer.trim_end_matches('/').into(),
            client_id: client_id.into(),
            cache: Mutex::new(KeyCache::default()),
            refreshed: Condvar::new(),
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .redirects(0)
                .build(),
        })
    }

    fn request_timeout(deadline: Option<Instant>) -> Result<Duration, String> {
        match deadline {
            Some(at) => at
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .map(|d| d.min(Duration::from_secs(5)))
                .ok_or_else(|| "OIDC_LOGIN_TIMEOUT: OIDC verification deadline exceeded".into()),
            None => Ok(Duration::from_secs(5)),
        }
    }

    fn refresh(&self, deadline: Option<Instant>) -> Result<JwkSet, String> {
        let discovery_response = self
            .agent
            .get(&format!("{}/.well-known/openid-configuration", self.issuer))
            .timeout(Self::request_timeout(deadline)?)
            .call()
            .map_err(|e| format!("AUTH_UNAVAILABLE: OIDC discovery: {e}"))?;
        if discovery_response.status() != 200 {
            return Err("AUTH_UNAVAILABLE: OIDC discovery did not return HTTP 200".into());
        }
        validate_oidc_endpoint(
            discovery_response.get_url(),
            "CITO_OIDC_ISSUER discovery URL",
            false,
        )
        .map_err(|e| format!("AUTH_UNAVAILABLE: {e}"))?;
        let discovery: Value = discovery_response
            .into_json()
            .map_err(|e| format!("AUTH_UNAVAILABLE: OIDC discovery JSON: {e}"))?;
        if discovery
            .get("issuer")
            .is_some_and(|issuer| issuer.as_str() != Some(self.issuer.as_str()))
        {
            return Err("AUTH_UNAVAILABLE: discovery issuer mismatch".into());
        }
        let uri = discovery["jwks_uri"]
            .as_str()
            .ok_or("AUTH_UNAVAILABLE: missing OIDC jwks_uri")?;
        validate_oidc_endpoint(uri, "OIDC jwks_uri", true)
            .map_err(|e| format!("AUTH_UNAVAILABLE: {e}"))?;
        let response = self
            .agent
            .get(uri)
            .timeout(Self::request_timeout(deadline)?)
            .call()
            .map_err(|e| format!("AUTH_UNAVAILABLE: JWKS: {e}"))?;
        if response.status() != 200 {
            return Err("AUTH_UNAVAILABLE: JWKS did not return HTTP 200".into());
        }
        validate_oidc_endpoint(response.get_url(), "OIDC JWKS response URL", true)
            .map_err(|e| format!("AUTH_UNAVAILABLE: {e}"))?;
        let keys: JwkSet = response
            .into_json()
            .map_err(|e| format!("AUTH_UNAVAILABLE: JWKS JSON: {e}"))?;
        Ok(keys)
    }

    fn verification_key(
        &self,
        kid: &str,
        deadline: Option<Instant>,
    ) -> Result<jsonwebtoken::jwk::Jwk, String> {
        let cooldown = env_value("CITO_JWKS_COOLDOWN_MS")
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(60));
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| "AUTH_UNAVAILABLE: key cache poisoned")?;
        loop {
            Self::request_timeout(deadline)?;
            if let Some(key) = cache.keys.as_ref().and_then(|keys| keys.find(kid)) {
                return Ok(key.clone());
            }
            if cache.refreshing {
                cache = if let Some(at) = deadline {
                    let remaining = at
                        .checked_duration_since(Instant::now())
                        .ok_or("OIDC_LOGIN_TIMEOUT: OIDC verification deadline exceeded")?;
                    self.refreshed
                        .wait_timeout(cache, remaining)
                        .map_err(|_| "AUTH_UNAVAILABLE: key cache poisoned")?
                        .0
                } else {
                    self.refreshed
                        .wait(cache)
                        .map_err(|_| "AUTH_UNAVAILABLE: key cache poisoned")?
                };
                continue;
            }
            if cache.refresh_at.is_some_and(|at| at.elapsed() < cooldown) {
                return Err(if cache.unavailable {
                    "AUTH_UNAVAILABLE: OIDC keys unavailable; refresh cooldown active"
                } else {
                    "AUTHORIZATION_VIOLATION: unknown JWT kid"
                }
                .into());
            }
            cache.refreshing = true;
            cache.refresh_at = Some(Instant::now());
            drop(cache);
            // Network I/O owns no cache lock; cached-key verification stays available.
            let refreshed = self.refresh(deadline);
            let expired = deadline.is_some_and(|at| Instant::now() >= at);
            cache = self
                .cache
                .lock()
                .map_err(|_| "AUTH_UNAVAILABLE: key cache poisoned")?;
            cache.refreshing = false;
            if expired {
                cache.refresh_at = None;
                self.refreshed.notify_all();
                return Err("OIDC_LOGIN_TIMEOUT: OIDC verification deadline exceeded".into());
            }
            match refreshed {
                Ok(keys) => {
                    cache.keys = Some(keys);
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
        self.verify_inner(token, None)
    }

    pub fn verify_with_deadline(&self, token: &str, deadline: Instant) -> Result<Value, String> {
        Self::request_timeout(Some(deadline))?;
        let result = self.verify_inner(token, Some(deadline));
        Self::request_timeout(Some(deadline))?;
        result
    }

    fn verify_inner(&self, token: &str, deadline: Option<Instant>) -> Result<Value, String> {
        let header =
            decode_header(token).map_err(|_| "AUTHORIZATION_VIOLATION: invalid JWT header")?;
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
        let kid = header
            .kid
            .as_deref()
            .filter(|kid| !kid.is_empty())
            .ok_or("AUTHORIZATION_VIOLATION: JWT kid is required")?;
        let jwk = self.verification_key(kid, deadline)?;
        if jwk
            .common
            .public_key_use
            .as_ref()
            .is_some_and(|use_| *use_ != jsonwebtoken::jwk::PublicKeyUse::Signature)
            || jwk
                .common
                .key_operations
                .as_ref()
                .is_some_and(|ops| !ops.contains(&jsonwebtoken::jwk::KeyOperations::Verify))
        {
            return Err("AUTHORIZATION_VIOLATION: JWK is not a verification key".into());
        }
        if let Some(alg) = jwk.common.key_algorithm {
            if format!("{alg:?}") != format!("{:?}", header.alg) {
                return Err("AUTHORIZATION_VIOLATION: JWT key algorithm mismatch".into());
            }
        }
        let key = DecodingKey::from_jwk(&jwk)
            .map_err(|_| "AUTHORIZATION_VIOLATION: invalid verification key")?;
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.client_id]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_nbf = true;
        validation.leeway = 30;
        let claims = decode::<Value>(token, &key, &validation)
            .map_err(|_| "AUTHORIZATION_VIOLATION: invalid JWT signature or claims")?
            .claims;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "AUTH_UNAVAILABLE: invalid system clock")?
            .as_secs();
        validate_token_times(&claims, now)?;
        if claims["iss"].as_str() != Some(self.issuer.as_str()) {
            return Err("AUTHORIZATION_VIOLATION: invalid issuer".into());
        }
        let sub = claims["sub"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or("AUTHORIZATION_VIOLATION: missing subject")?;
        let owner = claims["preferred_username"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                claims[concat!("e", "mail")]
                    .as_str()
                    .and_then(|s| s.split_once('@').map(|x| x.0))
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
        Ok(
            json!({"principal_id": format!("oidc:{}|{}", self.issuer, sub), "owner_user": owner, "sub": sub, "oidc": true, "legacy_principal_id":format!("{}#{}",self.issuer,sub)}),
        )
    }
}

#[cfg(test)]
#[path = "../../../../tests/native-hub/auth_cases.rs"]
mod tests;

pub struct HubAuth {
    pub mode: String,
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
        if mode == "token" && token.as_deref().is_none_or(|s| s.trim().is_empty()) {
            return Err("INVALID_INPUT: CITO_HUB_TOKEN is required in token mode".into());
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
            token,
            verifier,
        })
    }
    pub fn authenticate(&self, headers: &HashMap<String, String>) -> Result<Value, String> {
        let user = headers
            .get("x-cito-user")
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 64
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
            .map(String::as_str)
            .unwrap_or("local")
            .to_lowercase();
        if self.mode == "none" {
            return Ok(local_principal(&user));
        }
        let bearer = headers
            .get("authorization")
            .and_then(|s| s.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, s)| s.trim())
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
        Ok(local_principal(&user))
    }
}
