//! Command line arguments.

use clap::Parser;

/// The address `--host` falls back to. Loopback, so that a source nobody
/// configured is reachable from this machine only.
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// HTTP webhook receiver that emits `http.request` events.
#[derive(Parser, Debug, Clone)]
#[command(name = "http-source")]
#[command(about = "Receives HTTP webhooks and emits events")]
pub struct Args {
    /// Port to listen on.
    #[arg(short, long, env = "HTTP_SOURCE_PORT", default_value = "8080")]
    pub port: u16,

    /// Host to bind to.
    ///
    /// Loopback by default: anything that can reach this port can publish
    /// events, so listening on another interface is a decision, not a default.
    /// Pass `--host 0.0.0.0` to take webhooks from other machines.
    #[arg(long, env = "HTTP_SOURCE_HOST", default_value = DEFAULT_HOST)]
    pub host: String,

    /// Path to accept requests on.
    ///
    /// This is an exact axum route, not a prefix: `--path /inject` serves
    /// `/inject` and nothing below it. Capture syntax works, so
    /// `--path '/hook/{id}'` serves `/hook/42` and `--path '/hook/{*rest}'`
    /// serves everything under `/hook/`. The published `path` is always what
    /// the client requested.
    ///
    /// A value the router would refuse (no leading `/`, unbalanced braces, the
    /// `:id` syntax from before axum 0.8) or could never match (a `?` or `#`,
    /// since the query string is not part of the route; a space or a
    /// non-ASCII character, which a request carries percent-encoded, so write
    /// `/h%C3%BCk` for `/hük`) is reported on one line and the process exits 1.
    /// The README lists the rules.
    #[arg(long, env = "HTTP_SOURCE_PATH", default_value = "/")]
    pub path: String,

    /// Optional HMAC secret for signature validation.
    ///
    /// If provided, requests must include an `X-Signature` header with an
    /// HMAC-SHA256 signature of the body.
    #[arg(long, env = "HTTP_SOURCE_SECRET")]
    pub secret: Option<String>,

    /// Trust the `X-Forwarded-For` header when reporting `remote_addr`.
    ///
    /// Off by default, because the header is client-supplied: on a directly
    /// exposed port it lets any caller name itself. Set it only when a reverse
    /// proxy in front of this port overwrites the header.
    #[arg(long)]
    pub trust_forwarded_for: bool,
}

#[cfg(test)]
mod tests {
    use super::{Args, DEFAULT_HOST};
    use clap::Parser;
    use std::net::IpAddr;

    #[test]
    fn the_default_host_is_loopback() {
        let host: Result<IpAddr, _> = DEFAULT_HOST.parse();
        assert!(host.is_ok_and(|ip| ip.is_loopback()));
    }

    #[test]
    fn an_explicit_host_overrides_the_default() {
        let args = Args::try_parse_from(["http-source", "--host", "0.0.0.0"]);
        assert!(args.is_ok_and(|a| a.host == "0.0.0.0"));
    }
}
