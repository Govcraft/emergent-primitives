//! HTTP Source - Webhook Receiver
//!
//! A Source that receives HTTP requests and emits `http.request` events.
//! Supports optional HMAC signature validation for webhook security.
//!
//! Sources are SILENT - they only produce domain messages.
//! All lifecycle events are published by the engine.
//!
//! # Usage
//!
//! ```bash
//! # Start with default settings (port 8080, path /)
//! http-source
//!
//! # Custom port and path
//! http-source --port 3000 --path /webhook
//!
//! # With HMAC signature validation
//! http-source --secret my-secret-key
//!
//! # Behind a reverse proxy that sets X-Forwarded-For
//! http-source --trust-forwarded-for
//! ```
//!
//! This binary is the shell: it parses arguments, connects to the engine, and
//! serves. The behaviour lives in the library modules, where it is testable.

use std::{net::SocketAddr, sync::Arc};

use clap::Parser;
use emergent_client::EmergentSource;
use http_source::app::{AppState, MessagePublisher, build_router};
use http_source::args::Args;
use tokio::signal::unix::{SignalKind, signal};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Get the source name from environment (set by engine) or use default
    let name = std::env::var("EMERGENT_NAME").unwrap_or_else(|_| "http-source".to_string());

    // Connect to the Emergent engine (silently - lifecycle events come from engine)
    let source = match EmergentSource::connect(&name).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to connect to Emergent engine: {e}");
            std::process::exit(1);
        }
    };

    // Resolve publish type from EMERGENT_PUBLISHES env var or use default
    let publish_type = std::env::var("EMERGENT_PUBLISHES")
        .ok()
        .and_then(|s| s.split(',').next().map(str::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http.request".to_string());

    let source = Arc::new(source);
    let publisher: Arc<dyn MessagePublisher> = source.clone();
    let state = Arc::new(AppState {
        publisher,
        secret: args.secret.clone(),
        publish_type,
        trust_forwarded_for: args.trust_forwarded_for,
    });

    let app = build_router(&args.path, state);

    // Parse socket address
    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;

    // Set up SIGTERM handler for graceful shutdown
    let mut sigterm = signal(SignalKind::terminate())?;

    // `into_make_service_with_connect_info` is what puts the peer address in
    // reach of the handler; without it the `ConnectInfo` extractor has nothing
    // to read and every request fails.
    let server = axum::serve(
        tokio::net::TcpListener::bind(&addr).await?,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    );

    // Run server with shutdown signal
    tokio::select! {
        result = server => {
            result?;
        }
        _ = sigterm.recv() => {
            let _ = source.disconnect().await;
        }
    }

    Ok(())
}
