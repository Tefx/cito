use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn local_host_name() -> String {
    if let Ok(name) = std::env::var("CITO_HOST_NAME") {
        if !name.trim().is_empty() {
            return name;
        }
    }
    let mut bytes = [0u8; 256];
    // SAFETY: gethostname writes at most the provided buffer; leave a final NUL.
    if unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len() - 1) } == 0 {
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        return String::from_utf8_lossy(&bytes[..end]).into_owned();
    }
    "localhost".into()
}
pub fn normalize_slug(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            if out.len() < 32 {
                out.push(ch.to_ascii_lowercase());
            }
        } else if !out.is_empty() && !out.ends_with('-') && out.len() < 32 {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "user".into()
    } else {
        out.into()
    }
}
pub fn listener_id(principal: &str, harness: &str, session: &str) -> String {
    let hash = Sha256::digest(format!("cito-v2|{principal}|{harness}|{session}").as_bytes());
    format!(
        "c_{}",
        hash[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
/// Candidate keys preserve existing registrations when the wire's hash representation changes.
pub fn listener_id_candidates(identity: &Value, harness: &str, session: &str) -> Vec<String> {
    let principal = string(identity, "principal_id");
    let mut principals = vec![principal.to_string()];
    if let Some(legacy) = identity["legacy_principal_id"].as_str() {
        principals.push(legacy.to_string());
    } else if let Some(oidc) = principal.strip_prefix("oidc:") {
        if let Some((issuer, subject)) = oidc.split_once('|') {
            principals.push(format!("{issuer}#{subject}"));
        }
    } else if principal.starts_with("https://") || principal.starts_with("http://") {
        if let Some((issuer, subject)) = principal.split_once('#') {
            principals.push(format!("oidc:{issuer}|{subject}"));
        }
    }
    let mut ids = Vec::new();
    for principal in principals {
        let id = listener_id(&principal, harness, session);
        if !ids.contains(&id) {
            ids.push(id);
        }
        let bytes = Sha256::digest(format!("cito-v2|{principal}|{harness}|{session}").as_bytes());
        let mut legacy = String::from("c_");
        let mut bits: u32 = 0;
        let mut available = 0;
        for byte in &bytes[..8] {
            bits = (bits << 8) | u32::from(*byte);
            available += 8;
            while available >= 5 && legacy.len() < 14 {
                available -= 5;
                legacy.push(
                    b"abcdefghijklmnopqrstuvwxyz234567"[((bits >> available) & 31) as usize]
                        as char,
                );
            }
        }
        if !ids.contains(&legacy) {
            ids.push(legacy);
        }
    }
    ids
}

/// Readable aliases are presentation-only; storage and fencing retain canonical IDs.
pub fn readable_alias(raw: &str) -> Option<(&str, Option<&str>, &str)> {
    let (owner, prefix) = raw.split_once('#')?;
    if prefix.is_empty()
        || prefix.len() > 16
        || !prefix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return None;
    }
    let (user, harness) = owner
        .split_once('/')
        .map_or((owner, None), |(u, h)| (u, Some(h)));
    if user.is_empty()
        || user.len() > 64
        || !user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        || harness.is_some_and(|h| !crate::plugins::validate_harness_id(h))
    {
        return None;
    }
    Some((user, harness, prefix))
}

pub fn display_address(hub: &str, default_hub: &str, row: &Value, peers: &[Value]) -> String {
    let user = normalize_slug(string(row, "owner_user"));
    let harness = string(row, "harness");
    let suffix = string(row, "listener_id").strip_prefix("c_").unwrap_or("");
    let address = if harness == "human" {
        format!("@{}", string(row, "owner_user"))
    } else {
        let mut chosen = None;
        for length in [4, 8, 12, 16] {
            let prefix = &suffix[..suffix.len().min(length)];
            if prefix.is_empty() {
                break;
            }
            let collisions = peers
                .iter()
                .filter(|p| {
                    normalize_slug(string(p, "owner_user")) == user
                        && string(p, "harness") == harness
                        && matches!(
                            super::lease::evaluate_node_lease(p, crate::plugins::now_ms()),
                            "ACTIVE" | "DISCONNECTED_RETAINED"
                        )
                        && string(p, "listener_id")
                            .strip_prefix("c_")
                            .is_some_and(|s| s.starts_with(prefix))
                })
                .count();
            if collisions <= 1 || (length == 12 && suffix.len() == 12) {
                chosen = Some(prefix);
                break;
            }
        }
        match chosen {
            Some(fp) => format!("{user}/{harness}#{fp}"),
            None => public_listener_id(&user, string(row, "listener_id")),
        }
    };
    if hub == default_hub {
        address
    } else {
        format!("{hub}:{address}")
    }
}

pub fn canonical_node_id(raw: &str) -> Result<String, String> {
    let suffix = if let Some(suffix) = raw.strip_prefix("c_") {
        suffix
    } else {
        let (slug, suffix) = raw
            .rsplit_once('-')
            .ok_or("INVALID_INPUT: Invalid Node address")?;
        if slug.is_empty()
            || slug.len() > 64
            || !slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("INVALID_INPUT: Invalid Node address".into());
        }
        suffix
    };
    if !(suffix.len() == 16
        && suffix
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        && !(suffix.len() == 12
            && suffix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)))
    {
        return Err("INVALID_INPUT: Invalid Node address".into());
    }
    Ok(format!("c_{suffix}"))
}

pub fn public_listener_id(slug: &str, id: &str) -> String {
    format!(
        "{}-{}",
        normalize_slug(slug),
        id.strip_prefix("c_").unwrap_or(id)
    )
}
pub fn principal(user: &str) -> Value {
    json!({"principal_id":format!("none#{}",user.to_ascii_lowercase()),"owner_user":user.to_ascii_lowercase()})
}
pub fn hash(raw: &str) -> String {
    Sha256::digest(raw.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn sanitize_message(raw: &str) -> Result<String, String> {
    let clean: String = raw
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t') || c.is_whitespace())
        .collect();
    if clean.trim().is_empty() || clean.chars().count() > 65_536 {
        Err("INVALID_INPUT: message must contain 1..65,536 characters".into())
    } else {
        Ok(clean.trim().to_string())
    }
}
pub fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn iso(ms: u64) -> String {
    let seconds = (ms / 1000) as libc::time_t;
    // SAFETY: tm is initialized by reentrant conversion and output buffer is fixed and sized.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::gmtime_r(&seconds, &mut tm).is_null() {
            return format!("{ms}");
        }
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            ms % 1000
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slugs_and_node_ids() {
        assert_eq!(normalize_slug("Alice._Smith"), "alice-smith");
        assert_eq!(normalize_slug("___"), "user");
        assert_eq!(listener_id("p", "human", "default").len(), 18);
        assert_eq!(
            listener_id("p", "human", "default"),
            listener_id("p", "human", "default")
        );
    }
    #[test]
    fn readable_addresses_preserve_hub_routing_and_disambiguate_live_collisions() {
        let row = |id: &str| json!({"listener_id":id,"owner_user":"Alice","harness":"agent","state":"ACTIVE","updated_at_ms":crate::plugins::now_ms()});
        let first = row("c_abcd123456789012");
        let second = row("c_abcd987654321098");
        assert_eq!(
            display_address("work", "work", &first, &[first.clone()]),
            "alice/agent#abcd"
        );
        assert_eq!(
            display_address("local", "work", &first, &[first.clone(), second]),
            "local:alice/agent#abcd1234"
        );
        let legacy = row("c_abcdefabcdef");
        let extended = row("c_abcdefabcdef1234");
        let peers = [legacy.clone(), extended.clone()];
        assert_eq!(
            display_address("work", "work", &legacy, &peers),
            "alice/agent#abcdefabcdef"
        );
        assert_eq!(
            display_address("work", "work", &extended, &peers),
            "alice/agent#abcdefabcdef1234"
        );
        assert_eq!(
            display_address(
                "local",
                "work",
                &json!({"harness":"human","owner_user":"alice"}),
                &[]
            ),
            "local:@alice"
        );
        assert!(readable_alias("alice/agent#abcd").is_some());
        assert!(readable_alias("agent#abcd").is_some());
        assert!(readable_alias("alice/agent#").is_none());
        assert!(!super::super::config::validate_user("SELF"));
    }
    #[test]
    fn message_validation() {
        for raw in ["\u{0}", "\u{0081}", " \u{0081}\t\u{0085}"] {
            assert_eq!(
                sanitize_message(raw),
                Err("INVALID_INPUT: message must contain 1..65,536 characters".into())
            );
        }
        for (raw, expected) in [
            ("a\u{0}\nb", "a\nb"),
            ("a\u{0081}b", "ab"),
            (" \ta\u{0081}b\u{2003}", "ab"),
            (
                "a\n\r\t\u{000b}\u{000c}\u{0085}b",
                "a\n\r\t\u{000b}\u{000c}\u{0085}b",
            ),
        ] {
            assert_eq!(sanitize_message(raw).as_deref(), Ok(expected));
        }
        assert!(sanitize_message(&"x".repeat(65537)).is_err());
    }
}
