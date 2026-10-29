//! TLS handling inside CONNECT tunnels: the per-target decision, the handshake metadata recorded
//! on flows, and the byte pump used when a tunnel is passed through untouched.
//!
//! The decision is made from the CONNECT authority rather than from the ClientHello. That is a
//! deliberate trade: the authority is available before the tunnel is upgraded, so `drop` can refuse
//! the CONNECT itself, and `passthrough` never has to parse a ClientHello it is not allowed to
//! interpret. The price is that a client which tunnels to an IP literal and then names a different
//! host in SNI is matched on the literal — which is also what the client asked the proxy to do.

use relay_core_api::policy::TlsPolicy;
use std::net::IpAddr;

/// What this engine does with the bytes inside a CONNECT tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsDecision {
    /// Terminate TLS, capture the plaintext, and re-originate it upstream.
    Mitm,
    /// Splice the tunnel end-to-end: the client validates the upstream certificate.
    Passthrough,
    /// Refuse the CONNECT.
    Drop,
}

/// Default port for a CONNECT authority that omits one. HTTPS is the only thing a client normally
/// writes without a port here, and it is also the only default that keeps the URL truthful.
const DEFAULT_CONNECT_PORT: u16 = 443;

/// Split a CONNECT authority into host and port.
///
/// Accepts `example.com`, `example.com:8443`, `[2001:db8::1]:8443`, and a bare IPv6 literal. The
/// host is lowercased because DNS is case-insensitive and certificate matching is too; a pattern
/// that was written in mixed case should not silently fail to match.
pub fn parse_authority(raw: &str) -> Option<(String, u16)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    if let Some(rest) = raw.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        if host.is_empty() {
            return None;
        }
        let port = match tail.strip_prefix(':') {
            Some(port) => port.parse::<u16>().ok()?,
            None if tail.is_empty() => DEFAULT_CONNECT_PORT,
            None => return None,
        };
        return Some((host.to_ascii_lowercase(), port));
    }

    // A bare IPv6 literal has more than one colon and no brackets; there is nothing that could be
    // a port, so the whole thing is the host.
    if raw.matches(':').count() > 1 {
        return Some((raw.to_ascii_lowercase(), DEFAULT_CONNECT_PORT));
    }

    match raw.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => match port.parse::<u16>() {
            Ok(port) => Some((host.to_ascii_lowercase(), port)),
            // `example.com:not-a-port` is not a host name either; refuse rather than match a
            // malformed authority against a broad pattern.
            Err(_) => None,
        },
        _ => Some((raw.to_ascii_lowercase(), DEFAULT_CONNECT_PORT)),
    }
}

/// One parsed entry from a TLS policy list.
#[derive(Debug, Clone)]
enum Rule {
    Cidr(ipnetwork::IpNetwork),
    Ip(IpAddr),
    /// Port-specific host rule: `example.com:8443` or `[::1]:443`.
    HostPort(String, u16),
    /// Any-port host rule: an exact name or a glob.
    Host(String),
}

impl Rule {
    fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("empty entry".to_string());
        }

        if let Some(cidr) = raw.strip_prefix("cidr:") {
            let net: ipnetwork::IpNetwork = cidr
                .parse()
                .map_err(|e| format!("invalid CIDR '{cidr}': {e}"))?;
            return Ok(Self::Cidr(net));
        }

        // Bracketed IPv6, with or without a port.
        if let Some(rest) = raw.strip_prefix('[') {
            let (host, tail) = rest
                .split_once(']')
                .ok_or_else(|| format!("unterminated bracketed host '{raw}'"))?;
            if let Some(port) = tail.strip_prefix(':') {
                let port = port
                    .parse::<u16>()
                    .map_err(|e| format!("invalid port in '{raw}': {e}"))?;
                return Ok(Self::HostPort(host.to_ascii_lowercase(), port));
            }
            if tail.is_empty() {
                return Ok(Self::Host(host.to_ascii_lowercase()));
            }
            return Err(format!("unexpected trailing text in '{raw}'"));
        }

        // A bare IP literal, including IPv6 without brackets.
        if let Ok(ip) = raw.parse::<IpAddr>() {
            return Ok(Self::Ip(ip));
        }

        // `host:port` only when the tail really is a port; a bare IPv6 never reaches here.
        if let Some((host, port)) = raw.rsplit_once(':')
            && !host.is_empty()
            && let Ok(port) = port.parse::<u16>()
        {
            return Ok(Self::HostPort(host.to_ascii_lowercase(), port));
        }

        Ok(Self::Host(raw.to_ascii_lowercase()))
    }

    fn matches(&self, host: &str, port: u16) -> bool {
        match self {
            Self::Cidr(net) => host.parse::<IpAddr>().is_ok_and(|addr| net.contains(addr)),
            Self::Ip(ip) => host.parse::<IpAddr>().is_ok_and(|addr| addr == *ip),
            Self::HostPort(pattern_host, pattern_port) => {
                *pattern_port == port && host_pattern_matches(pattern_host, host)
            }
            Self::Host(pattern) => host_pattern_matches(pattern, host),
        }
    }
}

/// Exact match when there is no wildcard, glob match when there is.
///
/// `glob::Pattern` is used for wildcards because it has the same semantics as the upstream-proxy
/// bypass list, so a user who learned one list already knows the other. Patterns are compiled on
/// each decision: the lists are short and the decision happens once per CONNECT, not per request.
fn host_pattern_matches(pattern: &str, host: &str) -> bool {
    if !pattern.contains(['*', '?', '[']) {
        return pattern == host;
    }
    match glob::Pattern::new(pattern) {
        Ok(compiled) => compiled.matches(host),
        Err(e) => {
            tracing::warn!("invalid TLS host pattern '{pattern}': {e}");
            false
        }
    }
}

fn matches_any(patterns: &[String], host: &str, port: u16) -> bool {
    patterns.iter().any(|raw| match Rule::parse(raw) {
        Ok(rule) => rule.matches(host, port),
        Err(e) => {
            tracing::warn!("invalid TLS policy entry '{raw}': {e}");
            false
        }
    })
}

/// Decide what to do with a CONNECT to `authority`.
///
/// Precedence is `drop` > `passthrough` > `mitm`: a blocked target must not be reachable by also
/// listing it as passthrough, and an empty policy keeps the historical intercept-everything
/// behaviour. A malformed authority is intercepted rather than passed through: an unparseable
/// target is not evidence that the client wanted to bypass inspection.
pub fn decide(authority: &str, policy: &TlsPolicy) -> TlsDecision {
    let Some((host, port)) = parse_authority(authority) else {
        tracing::warn!("could not parse CONNECT authority '{authority}' for TLS policy");
        return TlsDecision::Mitm;
    };

    if matches_any(&policy.drop_hosts, &host, port) {
        return TlsDecision::Drop;
    }
    if matches_any(&policy.passthrough_hosts, &host, port) {
        return TlsDecision::Passthrough;
    }
    TlsDecision::Mitm
}

/// The TLS facts a terminated handshake established, copied onto every flow inside the tunnel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsHandshake {
    pub sni: Option<String>,
    pub alpn: Option<String>,
    pub version: Option<String>,
}

/// Render a rustls protocol version the way a capture should read it.
///
/// rustls' `Debug` spelling is `TLSv1_3`; the wire name people search for is `TLSv1.3`.
pub fn protocol_version_name(version: rustls::ProtocolVersion) -> String {
    use rustls::ProtocolVersion;
    match version {
        ProtocolVersion::TLSv1_2 => "TLSv1.2".to_string(),
        ProtocolVersion::TLSv1_3 => "TLSv1.3".to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(passthrough: &[&str], drop: &[&str]) -> TlsPolicy {
        TlsPolicy {
            passthrough_hosts: passthrough.iter().map(|s| s.to_string()).collect(),
            drop_hosts: drop.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn authorities_parse_with_and_without_a_port() {
        assert_eq!(
            parse_authority("example.com"),
            Some(("example.com".to_string(), 443))
        );
        assert_eq!(
            parse_authority("Example.COM:8443"),
            Some(("example.com".to_string(), 8443))
        );
        assert_eq!(
            parse_authority("[2001:db8::1]:9443"),
            Some(("2001:db8::1".to_string(), 9443))
        );
        assert_eq!(
            parse_authority("2001:db8::1"),
            Some(("2001:db8::1".to_string(), 443))
        );
        assert_eq!(parse_authority("example.com:not-a-port"), None);
        assert_eq!(parse_authority(""), None);
    }

    #[test]
    fn an_empty_policy_intercepts_everything() {
        let policy = policy(&[], &[]);
        assert_eq!(decide("example.com:443", &policy), TlsDecision::Mitm);
    }

    #[test]
    fn a_bare_host_passthrough_matches_every_port() {
        let policy = policy(&["example.com"], &[]);
        assert_eq!(decide("example.com:443", &policy), TlsDecision::Passthrough);
        assert_eq!(
            decide("example.com:8443", &policy),
            TlsDecision::Passthrough
        );
        assert_eq!(
            decide("other.example.com:443", &policy),
            TlsDecision::Mitm,
            "a bare host is exact, not a suffix match"
        );
    }

    #[test]
    fn a_port_specific_passthrough_only_matches_that_port() {
        let policy = policy(&["example.com:8443"], &[]);
        assert_eq!(
            decide("example.com:8443", &policy),
            TlsDecision::Passthrough
        );
        assert_eq!(decide("example.com:443", &policy), TlsDecision::Mitm);
    }

    #[test]
    fn a_wildcard_matches_subdomains_but_not_the_apex() {
        let policy = policy(&["*.internal.example"], &[]);
        assert_eq!(
            decide("api.internal.example:443", &policy),
            TlsDecision::Passthrough
        );
        assert_eq!(
            decide("internal.example:443", &policy),
            TlsDecision::Mitm,
            "a star label does not match zero labels"
        );
    }

    #[test]
    fn ip_and_cidr_entries_match_literals() {
        let cidr_policy = policy(&["cidr:10.0.0.0/8"], &[]);
        assert_eq!(
            decide("10.1.2.3:443", &cidr_policy),
            TlsDecision::Passthrough
        );
        assert_eq!(decide("11.1.2.3:443", &cidr_policy), TlsDecision::Mitm);

        let ipv6_policy = policy(&["[2001:db8::1]:443"], &[]);
        assert_eq!(
            decide("[2001:db8::1]:443", &ipv6_policy),
            TlsDecision::Passthrough
        );
        assert_eq!(decide("[2001:db8::2]:443", &ipv6_policy), TlsDecision::Mitm);
    }

    #[test]
    fn drop_wins_over_passthrough() {
        let policy = policy(&["example.com"], &["example.com"]);
        assert_eq!(decide("example.com:443", &policy), TlsDecision::Drop);
    }

    #[test]
    fn an_invalid_entry_never_matches_instead_of_matching_everything() {
        let policy = policy(&["cidr:not-a-network"], &[]);
        assert_eq!(decide("10.0.0.1:443", &policy), TlsDecision::Mitm);
    }

    #[test]
    fn a_malformed_authority_is_intercepted_rather_than_passed_through() {
        let policy = policy(&["*"], &[]);
        assert_eq!(decide("bad:port", &policy), TlsDecision::Mitm);
    }

    #[test]
    fn protocol_versions_use_their_wire_spelling() {
        assert_eq!(
            protocol_version_name(rustls::ProtocolVersion::TLSv1_3),
            "TLSv1.3"
        );
        assert_eq!(
            protocol_version_name(rustls::ProtocolVersion::TLSv1_2),
            "TLSv1.2"
        );
    }
}
