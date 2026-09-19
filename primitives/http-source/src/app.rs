//! The HTTP surface: router, shared state, and the one request handler.
//!
//! The handler does as little as it can. Everything it decides (which address
//! to report, what the payload looks like, whether a signature is good) lives
//! in [`crate::addr`], [`crate::payload`] and [`crate::signature`] as pure
//! functions. What is left here is extraction, one branch on the signature, and
//! the publish.
//!
//! Publishing goes through [`MessagePublisher`] rather than straight at
//! `EmergentSource`, so the router can be driven in a test without an engine on
//! the other end of a socket.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::{ConnectInfo, OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use emergent_client::{EmergentMessage, EmergentSource};
use serde_json::json;

use crate::addr::{FORWARDED_FOR, resolve_remote_addr};
use crate::payload::build_payload;
use crate::signature::validate_signature;

/// A boxed publish in flight.
pub type PublishFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// Somewhere to send a published message.
///
/// In the binary this is an [`EmergentSource`] talking to the engine. In tests
/// it is a recorder, which is the point: the router is exercised end to end
/// without a running engine.
pub trait MessagePublisher: Send + Sync + 'static {
    /// Publishes one message, or returns why it could not be published.
    fn publish(&self, message: EmergentMessage) -> PublishFuture<'_>;
}

impl MessagePublisher for EmergentSource {
    fn publish(&self, message: EmergentMessage) -> PublishFuture<'_> {
        Box::pin(async move {
            EmergentSource::publish(self, message)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

/// Shared application state.
pub struct AppState {
    /// Where published messages go.
    pub publisher: Arc<dyn MessagePublisher>,
    /// HMAC secret, when signature validation is enabled.
    pub secret: Option<String>,
    /// The message type published for each request.
    pub publish_type: String,
    /// Whether `X-Forwarded-For` is trusted for `remote_addr`.
    pub trust_forwarded_for: bool,
}

/// Builds the router serving `path`.
///
/// `path` is an exact axum route. It may use capture syntax (`/hook/{id}`,
/// `/hook/{*rest}`), in which case the published `path` is the concrete path
/// the client requested, not the pattern.
pub fn build_router(path: &str, state: Arc<AppState>) -> Router {
    Router::new()
        .route(path, any(handle_request))
        .with_state(state)
}

/// Handles one incoming HTTP request.
async fn handle_request(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(secret) = state.secret.as_deref() {
        let Some(signature) = headers.get("x-signature").and_then(|h| h.to_str().ok()) else {
            return (StatusCode::UNAUTHORIZED, "Missing signature").into_response();
        };
        if !validate_signature(secret, &body, signature) {
            return (StatusCode::UNAUTHORIZED, "Invalid signature").into_response();
        }
    }

    let forwarded = headers.get(FORWARDED_FOR).and_then(|h| h.to_str().ok());
    let remote_addr = resolve_remote_addr(peer, forwarded, state.trust_forwarded_for);

    let payload = build_payload(&method, &uri, &headers, &body, remote_addr);
    let message = EmergentMessage::new(&state.publish_type).with_payload(json!(payload));

    match state.publisher.publish(message).await {
        Ok(()) => (StatusCode::ACCEPTED, "").into_response(),
        Err(error) => {
            eprintln!("Failed to publish event: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to publish event").into_response()
        }
    }
}
