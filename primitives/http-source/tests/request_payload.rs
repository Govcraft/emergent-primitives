//! Integration tests that drive the real router with `tower::ServiceExt::oneshot`.
//!
//! These are the tests that would have caught the bug: the pure functions can
//! be right while the handler never calls them, or calls them with the route
//! pattern instead of the request. Here the router is assembled exactly as the
//! binary assembles it, a real `Request` goes through it, and the assertion is
//! on the JSON that was handed to the publisher.
//!
//! No engine is involved. The publisher is a recorder, which is what
//! `MessagePublisher` exists for.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use emergent_client::EmergentMessage;
use http_source::app::{AppState, MessagePublisher, PublishFuture, build_router};
use http_source::route_path::RoutePath;
use serde_json::Value;
use tower::ServiceExt;

const PEER: &str = "198.51.100.9:54321";

/// Captures every published message instead of sending it to an engine.
#[derive(Default)]
struct Recorder {
    published: Mutex<Vec<EmergentMessage>>,
    /// When set, every publish fails with this message.
    failure: Option<String>,
}

impl Recorder {
    fn failing() -> Self {
        Self {
            published: Mutex::new(Vec::new()),
            failure: Some("engine is down".to_string()),
        }
    }

    /// The payload of the only published message.
    fn only_payload(&self) -> Value {
        let published = self
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match published.as_slice() {
            [message] => message.payload.clone(),
            other => panic!(
                "expected exactly one published message, got {}",
                other.len()
            ),
        }
    }

    fn count(&self) -> usize {
        self.published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl MessagePublisher for Recorder {
    fn publish(&self, message: EmergentMessage) -> PublishFuture<'_> {
        Box::pin(async move {
            if let Some(failure) = self.failure.clone() {
                return Err(failure);
            }
            self.published
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(message);
            Ok(())
        })
    }
}

/// How one test case configures the source.
struct Harness {
    route: String,
    secret: Option<String>,
    trust_forwarded_for: bool,
    recorder: Arc<Recorder>,
}

impl Harness {
    fn new(route: &str) -> Self {
        Self {
            route: route.to_string(),
            secret: None,
            trust_forwarded_for: false,
            recorder: Arc::new(Recorder::default()),
        }
    }

    fn trusting_forwarded_for(mut self) -> Self {
        self.trust_forwarded_for = true;
        self
    }

    fn with_secret(mut self, secret: &str) -> Self {
        self.secret = Some(secret.to_string());
        self
    }

    fn with_failing_publisher(mut self) -> Self {
        self.recorder = Arc::new(Recorder::failing());
        self
    }

    /// Sends one request through the router and returns its status.
    async fn send(&self, uri: &str, headers: &[(&str, &str)], body: &str) -> StatusCode {
        let publisher: Arc<dyn MessagePublisher> = self.recorder.clone();
        let state = Arc::new(AppState {
            publisher,
            secret: self.secret.clone(),
            publish_type: "http.request".to_string(),
            trust_forwarded_for: self.trust_forwarded_for,
        });
        let route = match RoutePath::parse(&self.route) {
            Ok(route) => route,
            Err(rule) => panic!("{:?} is not a route: {rule}", self.route),
        };
        let router = build_router(&route, state);

        let mut builder = Request::builder().method("POST").uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let Ok(mut request) = builder.body(Body::from(body.to_string())) else {
            panic!("could not build the request for {uri}");
        };

        let peer: SocketAddr = PEER
            .parse()
            .unwrap_or(SocketAddr::from(([198, 51, 100, 9], 54321)));
        request.extensions_mut().insert(ConnectInfo(peer));

        match router.oneshot(request).await {
            Ok(response) => response.status(),
            Err(error) => panic!("router returned an error for {uri}: {error:?}"),
        }
    }
}

/// Reads a string field out of the published payload.
fn field<'a>(payload: &'a Value, name: &str) -> Option<&'a str> {
    payload.get(name).and_then(Value::as_str)
}

#[tokio::test]
async fn publishes_the_requested_path_and_query() {
    let harness = Harness::new("/inject");
    let status = harness
        .send("/inject?a=1&b=2", &[], r#"{"hello":"world"}"#)
        .await;

    assert_eq!(status, StatusCode::ACCEPTED);
    let payload = harness.recorder.only_payload();
    assert_eq!(field(&payload, "path"), Some("/inject"));
    assert_eq!(field(&payload, "query"), Some("a=1&b=2"));
    assert_eq!(field(&payload, "method"), Some("POST"));
    assert_eq!(
        payload.get("body"),
        Some(&serde_json::json!({"hello": "world"}))
    );
}

#[tokio::test]
async fn a_request_with_no_query_publishes_a_null_query() {
    let harness = Harness::new("/inject");
    assert_eq!(
        harness.send("/inject", &[], "{}").await,
        StatusCode::ACCEPTED
    );

    let payload = harness.recorder.only_payload();
    assert_eq!(field(&payload, "path"), Some("/inject"));
    assert_eq!(payload.get("query"), Some(&Value::Null));
}

#[tokio::test]
async fn the_default_route_publishes_the_root_path() {
    let harness = Harness::new("/");
    assert_eq!(harness.send("/", &[], "{}").await, StatusCode::ACCEPTED);
    assert_eq!(field(&harness.recorder.only_payload(), "path"), Some("/"));
}

#[tokio::test]
async fn a_capture_route_publishes_the_concrete_path() {
    // This is the case where the configured `--path` and the requested path
    // genuinely differ, so publishing the route pattern would be wrong.
    let harness = Harness::new("/hook/{id}");
    assert_eq!(
        harness.send("/hook/42?retry=1", &[], "{}").await,
        StatusCode::ACCEPTED
    );

    let payload = harness.recorder.only_payload();
    assert_eq!(field(&payload, "path"), Some("/hook/42"));
    assert_eq!(field(&payload, "query"), Some("retry=1"));
}

#[tokio::test]
async fn a_wildcard_route_publishes_the_full_path() {
    let harness = Harness::new("/hook/{*rest}");
    assert_eq!(
        harness.send("/hook/a/b/c", &[], "{}").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        field(&harness.recorder.only_payload(), "path"),
        Some("/hook/a/b/c")
    );
}

#[tokio::test]
async fn an_exact_route_does_not_match_a_deeper_path() {
    // `--path` is an exact route, not a prefix. Recorded here so a change in
    // that behaviour is a deliberate one.
    let harness = Harness::new("/inject");
    assert_eq!(
        harness.send("/inject/extra", &[], "{}").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(harness.recorder.count(), 0);
}

#[tokio::test]
async fn remote_addr_is_the_socket_peer_by_default() {
    let harness = Harness::new("/inject");
    assert_eq!(
        harness.send("/inject", &[], "{}").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        field(&harness.recorder.only_payload(), "remote_addr"),
        Some("198.51.100.9")
    );
}

#[tokio::test]
async fn forwarded_for_is_ignored_when_the_flag_is_off() {
    let harness = Harness::new("/inject");
    assert_eq!(
        harness
            .send("/inject", &[("x-forwarded-for", "203.0.113.7")], "{}")
            .await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        field(&harness.recorder.only_payload(), "remote_addr"),
        Some("198.51.100.9")
    );
}

#[tokio::test]
async fn forwarded_for_is_used_when_the_flag_is_on() {
    let harness = Harness::new("/inject").trusting_forwarded_for();
    assert_eq!(
        harness
            .send("/inject", &[("x-forwarded-for", "203.0.113.7")], "{}")
            .await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        field(&harness.recorder.only_payload(), "remote_addr"),
        Some("203.0.113.7")
    );
}

#[tokio::test]
async fn forwarded_for_with_multiple_hops_takes_the_leftmost() {
    let harness = Harness::new("/inject").trusting_forwarded_for();
    assert_eq!(
        harness
            .send(
                "/inject",
                &[(
                    "x-forwarded-for",
                    "203.0.113.7, 70.41.3.18, 150.172.238.178"
                )],
                "{}"
            )
            .await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        field(&harness.recorder.only_payload(), "remote_addr"),
        Some("203.0.113.7")
    );
}

#[tokio::test]
async fn a_malformed_forwarded_for_falls_back_to_the_socket_peer() {
    let harness = Harness::new("/inject").trusting_forwarded_for();
    assert_eq!(
        harness
            .send("/inject", &[("x-forwarded-for", "not-an-ip")], "{}")
            .await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        field(&harness.recorder.only_payload(), "remote_addr"),
        Some("198.51.100.9")
    );
}

#[tokio::test]
async fn a_missing_signature_is_rejected_and_publishes_nothing() {
    let harness = Harness::new("/inject").with_secret("shh");
    assert_eq!(
        harness.send("/inject", &[], "{}").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(harness.recorder.count(), 0);
}

#[tokio::test]
async fn a_bad_signature_is_rejected_and_publishes_nothing() {
    let harness = Harness::new("/inject").with_secret("shh");
    assert_eq!(
        harness
            .send("/inject", &[("x-signature", "sha256=deadbeef")], "{}")
            .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(harness.recorder.count(), 0);
}

#[tokio::test]
async fn a_publish_failure_answers_500() {
    let harness = Harness::new("/inject").with_failing_publisher();
    assert_eq!(
        harness.send("/inject", &[], "{}").await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(harness.recorder.count(), 0);
}

#[tokio::test]
async fn headers_reach_the_payload() {
    let harness = Harness::new("/inject");
    assert_eq!(
        harness
            .send(
                "/inject",
                &[("content-type", "application/json"), ("x-custom", "value")],
                "{}"
            )
            .await,
        StatusCode::ACCEPTED
    );

    let payload = harness.recorder.only_payload();
    let headers = payload.get("headers").and_then(Value::as_object);
    let Some(headers) = headers else {
        panic!("headers missing from the payload");
    };
    assert_eq!(
        headers.get("x-custom").and_then(Value::as_str),
        Some("value")
    );
    assert_eq!(
        headers.get("content-type").and_then(Value::as_str),
        Some("application/json")
    );
}
