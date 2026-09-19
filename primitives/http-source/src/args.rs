//! Command line arguments.

use clap::Parser;

/// HTTP webhook receiver that emits `http.request` events.
#[derive(Parser, Debug, Clone)]
#[command(name = "http-source")]
#[command(about = "Receives HTTP webhooks and emits events")]
pub struct Args {
    /// Port to listen on.
    #[arg(short, long, env = "HTTP_SOURCE_PORT", default_value = "8080")]
    pub port: u16,

    /// Host to bind to.
    #[arg(long, env = "HTTP_SOURCE_HOST", default_value = "0.0.0.0")]
    pub host: String,

    /// Path to accept requests on.
    ///
    /// This is an exact axum route, not a prefix: `--path /inject` serves
    /// `/inject` and nothing below it. Capture syntax works, so
    /// `--path '/hook/{id}'` serves `/hook/42` and `--path '/hook/{*rest}'`
    /// serves everything under `/hook/`. The published `path` is always what
    /// the client requested.
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
