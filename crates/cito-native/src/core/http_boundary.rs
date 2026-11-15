//! Shared browser boundary for the native HTTP listeners.
use std::net::SocketAddr;
use url::{Host, Url};

fn authority(value: &str) -> Option<Url> {
    let url = Url::parse(&format!("http://{value}")).ok()?;
    (url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.host().is_some())
    .then_some(url)
}

pub(super) fn validate(
    bound: SocketAddr,
    method: &str,
    host: Option<&str>,
    origin: Option<&str>,
    content_type: Option<&str>,
) -> Result<(), (u16, &'static str)> {
    validate_hub(bound, method, host, origin, content_type, false)
}

pub(super) fn validate_hub(
    bound: SocketAddr,
    method: &str,
    host: Option<&str>,
    origin: Option<&str>,
    content_type: Option<&str>,
    authenticated: bool,
) -> Result<(), (u16, &'static str)> {
    let host = host.and_then(authority).ok_or((403, "Forbidden Host"))?;
    let destination_allowed = if bound.ip().is_unspecified() {
        true
    } else {
        match host.host() {
            Some(Host::Ipv4(ip)) => bound.ip() == ip,
            Some(Host::Ipv6(ip)) => bound.ip() == ip,
            Some(Host::Domain(name)) => bound.ip().is_loopback() && name == "localhost",
            None => false,
        }
    };
    if !authenticated
        && (!destination_allowed || host.port_or_known_default() != Some(bound.port()))
    {
        return Err((403, "Forbidden Host"));
    }
    if let Some(origin) = origin {
        let allowed = Url::parse(origin).ok().is_some_and(|url| {
            url.scheme() == host.scheme()
                && url.host() == host.host()
                && url.port_or_known_default() == host.port_or_known_default()
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
        });
        if !allowed {
            return Err((403, "Forbidden Origin"));
        }
    }
    if method == "POST"
        && !content_type.is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
    {
        return Err((415, "Content-Type must be application/json"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticated_proxy_authorities_preserve_the_browser_boundary() {
        for bound in ["127.0.0.1:4291", "0.0.0.0:4291", "[::]:4291"] {
            let bound = bound.parse().unwrap();
            for host in ["cito.example.com", "cito.example.com:8443", "[::1]"] {
                assert!(validate_hub(bound, "GET", Some(host), None, None, true).is_ok());
                assert_eq!(
                    validate_hub(
                        bound,
                        "GET",
                        Some(host),
                        Some("http://other.example.com"),
                        None,
                        true
                    )
                    .unwrap_err()
                    .0,
                    403
                );
                assert!(validate_hub(bound, "GET", Some(host), None, None, false).is_err());
            }
            for bad in [
                "alice@cito.example.com",
                "cito.example.com/path",
                "cito.example.com#x",
                "",
            ] {
                assert!(validate_hub(bound, "GET", Some(bad), None, None, true).is_err());
            }
        }
    }

    #[test]
    fn browser_boundary_checks_authority_origin_and_media_type() {
        let bound = "127.0.0.1:4291".parse().unwrap();
        let check = |host, origin, media| validate(bound, "POST", Some(host), origin, media);
        assert!(check(
            "127.0.0.1:4291",
            None,
            Some("application/json; charset=utf-8")
        )
        .is_ok());
        assert!(check(
            "127.0.0.1:4291",
            Some("http://127.0.0.1:4291"),
            Some("application/json")
        )
        .is_ok());
        for origin in [
            "http://127.0.0.1:4292",
            "https://127.0.0.1:4291",
            "http://localhost:4291",
            "null",
        ] {
            assert_eq!(
                check("127.0.0.1:4291", Some(origin), Some("application/json"))
                    .unwrap_err()
                    .0,
                403
            );
        }
        for host in [
            "attacker.example:4291",
            "127.0.0.1:4292",
            "127.0.0.1:4291/path",
            "alice@127.0.0.1:4291",
        ] {
            assert_eq!(
                check(host, None, Some("application/json")).unwrap_err().0,
                403
            );
        }
        assert_eq!(
            check("127.0.0.1:4291", None, Some("text/plain"))
                .unwrap_err()
                .0,
            415
        );
        for bind in ["0.0.0.0:4291", "[::]:4291"] {
            for host in [
                "192.168.1.10:4291",
                "100.64.0.1:4291",
                "127.0.0.1:4291",
                "[::1]:4291",
                "hub.example:4291",
            ] {
                assert!(validate(bind.parse().unwrap(), "GET", Some(host), None, None).is_ok());
            }
        }
    }
}
