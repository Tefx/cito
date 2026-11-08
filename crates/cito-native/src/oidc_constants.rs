//! Standard identifiers from OpenID Connect Core 1.0 §§5.1, 5.4 and 11.
pub const OIDC_EMAIL_CLAIM: &str = "email";
pub const OIDC_DEFAULT_SCOPES: [&str; 4] =
    ["openid", "profile", OIDC_EMAIL_CLAIM, "offline_access"];
