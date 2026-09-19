//! Which address do we report as the caller?
//!
//! Two sources can answer "who sent this request", and they disagree in ways
//! that matter:
//!
//! - The **socket peer** is the other end of the TCP connection. It cannot be
//!   forged by the client, but behind a reverse proxy it is the proxy, so every
//!   request looks like it came from the same machine.
//! - The **`X-Forwarded-For` header** carries the chain the request travelled,
//!   leftmost first, each hop appending the peer it saw. It names the real
//!   client, but it is just a request header: a direct caller can write
//!   whatever it likes in it.
//!
//! So the default is the socket peer, always, and the header is only consulted
//! when the operator passes `--trust-forwarded-for` to say "I am behind a proxy
//! that overwrites this header". Enabling the flag on a directly exposed port
//! hands every caller the ability to name itself, which is why it is off by
//! default and why this is a deployment decision rather than something the
//! primitive tries to detect.
//!
//! Everything here is a pure function of (peer, header value, flag), which is
//! the whole reason the decision lives in its own module instead of inline in
//! the request handler.

use std::net::{IpAddr, SocketAddr};

/// The header consulted when `--trust-forwarded-for` is set.
pub const FORWARDED_FOR: &str = "x-forwarded-for";

/// Parses one `X-Forwarded-For` hop into an IP address.
///
/// The de-facto format is a bare IP, but proxies in the wild also emit
/// `203.0.113.7:443` and `[2001:db8::1]:443`, so both are accepted and the port
/// is dropped. Anything else (an obfuscated identifier, a hostname, garbage)
/// yields `None`.
fn parse_hop(hop: &str) -> Option<IpAddr> {
    let hop = hop.trim();
    if hop.is_empty() {
        return None;
    }
    if let Ok(ip) = hop.parse::<IpAddr>() {
        return Some(ip);
    }
    hop.parse::<SocketAddr>().ok().map(|sock| sock.ip())
}

/// Returns the client IP claimed by an `X-Forwarded-For` value.
///
/// The leftmost entry is the one each hop prepends its view of the original
/// client into, so that is the entry taken. A malformed leftmost entry returns
/// `None` rather than falling through to the next hop: the second entry is a
/// proxy, and silently reporting a proxy as the caller would be a wrong answer
/// dressed up as a right one.
///
/// Repeated header lines are not joined here. HTTP treats them as equivalent to
/// one comma-joined value, and the leftmost entry of the first line is the
/// leftmost entry overall, so the caller passes the first value only.
#[must_use]
pub fn first_forwarded_hop(header: &str) -> Option<IpAddr> {
    header.split(',').next().and_then(parse_hop)
}

/// Decides which IP address is reported as `remote_addr`.
///
/// The socket peer wins unless `trust_forwarded` is set *and* the header yields
/// a usable IP. A trusted-but-unusable header falls back to the peer, so the
/// field is never absent and never a guess.
///
/// The result is an IP with no port. The peer's port identifies a connection,
/// not a caller, and including it only when the flag happens to be off would
/// make the published shape depend on deployment configuration.
#[must_use]
pub fn resolve_remote_addr(
    peer: SocketAddr,
    forwarded: Option<&str>,
    trust_forwarded: bool,
) -> IpAddr {
    if !trust_forwarded {
        return peer.ip();
    }
    forwarded
        .and_then(first_forwarded_hop)
        .unwrap_or_else(|| peer.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER: &str = "198.51.100.9:54321";

    fn peer() -> SocketAddr {
        PEER.parse()
            .unwrap_or(SocketAddr::from(([198, 51, 100, 9], 54321)))
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap_or(IpAddr::from([0, 0, 0, 0]))
    }

    #[test]
    fn resolve_remote_addr_table() {
        // (case, forwarded header, trust flag, expected IP)
        let cases: &[(&str, Option<&str>, bool, &str)] = &[
            ("no header, flag off", None, false, "198.51.100.9"),
            ("no header, flag on", None, true, "198.51.100.9"),
            (
                "header present, flag off is ignored",
                Some("203.0.113.7"),
                false,
                "198.51.100.9",
            ),
            (
                "header present, flag on is trusted",
                Some("203.0.113.7"),
                true,
                "203.0.113.7",
            ),
            (
                "multiple hops takes the leftmost",
                Some("203.0.113.7, 70.41.3.18, 150.172.238.178"),
                true,
                "203.0.113.7",
            ),
            (
                "multiple hops without spaces",
                Some("203.0.113.7,70.41.3.18"),
                true,
                "203.0.113.7",
            ),
            (
                "multiple hops, flag off is still the peer",
                Some("203.0.113.7, 70.41.3.18"),
                false,
                "198.51.100.9",
            ),
            (
                "malformed header falls back to the peer",
                Some("not-an-ip"),
                true,
                "198.51.100.9",
            ),
            (
                "malformed leftmost does not fall through to the next hop",
                Some("garbage, 70.41.3.18"),
                true,
                "198.51.100.9",
            ),
            ("empty header", Some(""), true, "198.51.100.9"),
            ("whitespace header", Some("   "), true, "198.51.100.9"),
            ("commas only", Some(",,,"), true, "198.51.100.9"),
            (
                "obfuscated identifier is not an IP",
                Some("_hidden"),
                true,
                "198.51.100.9",
            ),
            (
                "hostname is not an IP",
                Some("proxy.example.com"),
                true,
                "198.51.100.9",
            ),
            (
                "ipv4 with port drops the port",
                Some("203.0.113.7:443"),
                true,
                "203.0.113.7",
            ),
            ("bare ipv6", Some("2001:db8::1"), true, "2001:db8::1"),
            (
                "bracketed ipv6 with port drops the port",
                Some("[2001:db8::1]:443"),
                true,
                "2001:db8::1",
            ),
            (
                "leading whitespace is trimmed",
                Some("  203.0.113.7  , 70.41.3.18"),
                true,
                "203.0.113.7",
            ),
        ];

        for (case, forwarded, trust, expected) in cases {
            let got = resolve_remote_addr(peer(), *forwarded, *trust);
            assert_eq!(got, ip(expected), "case: {case}");
        }
    }

    #[test]
    fn first_forwarded_hop_rejects_unusable_values() {
        assert_eq!(first_forwarded_hop("203.0.113.7"), Some(ip("203.0.113.7")));
        assert_eq!(first_forwarded_hop(""), None);
        assert_eq!(first_forwarded_hop("unknown"), None);
        assert_eq!(first_forwarded_hop("999.1.1.1"), None);
    }

    #[test]
    fn ipv6_peer_reports_without_brackets_or_port() {
        let peer = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 8080));
        assert_eq!(resolve_remote_addr(peer, None, false).to_string(), "::1");
    }
}
