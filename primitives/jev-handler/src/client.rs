//! The one irreducible I/O act, and the loop that drives it.
//!
//! Everything else in this crate is pure. `post_once` makes exactly one POST;
//! `ask` sequences attempts according to [`crate::retry`] and hands a 2xx body
//! to [`crate::response`]. No judgement lives here — only ordering — which is
//! what keeps the decisions unit-testable without a socket.

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, RETRY_AFTER};
use serde_json::Value;

use crate::args::{ApiKey, Endpoint, RetryPolicy};
use crate::error::{HttpFailure, RequestFailure};
use crate::payload::extract_detail;
use crate::questions::QuestionSet;
use crate::response::{ApiResponse, parse_response};
use crate::retry::{AttemptOutcome, RetryDecision, decide_retry, parse_retry_after};

/// The response header carrying the vendor's own request identifier.
///
/// It is captured on every attempt, success or failure, and carried into both
/// published payloads: it is what makes a support conversation about a failed
/// call joinable to the exact Emergent message.
pub const REQUEST_ID_HEADER: &str = "x-typesafe-request-id";

/// What one completed HTTP attempt returned.
#[derive(Debug, Clone)]
struct Attempt {
    status: u16,
    request_id: Option<String>,
    retry_after: Option<Duration>,
    body: String,
}

/// The System One client: one pooled HTTP client, shared across all messages.
///
/// Building a client per request would discard connection pooling and TLS
/// session reuse, which for an API called once per event is most of the
/// per-request cost.
#[derive(Debug)]
pub struct JevClient {
    http: reqwest::Client,
    endpoint: Endpoint,
    api_key: ApiKey,
}

impl JevClient {
    /// Build a client that posts to `endpoint`, bounding each attempt by
    /// `request_timeout`.
    ///
    /// # Errors
    ///
    /// Returns the `reqwest` error when the HTTP client cannot be built, which
    /// in practice means the TLS backend is unavailable.
    pub fn new(
        endpoint: Endpoint,
        api_key: ApiKey,
        request_timeout: Duration,
    ) -> Result<Self, reqwest::Error> {
        // `rustls` is built with no default crypto provider so the release
        // binary stays pure Rust and cross-compiles to aarch64 without a C
        // toolchain; `reqwest` then *panics* when a client is built before one
        // is installed. Installing it here, rather than leaving it to each
        // caller, is what makes that panic unreachable. A competing install
        // from elsewhere in the process is not an error worth failing over.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let http = reqwest::Client::builder()
            .timeout(request_timeout)
            .user_agent(concat!("jev-handler/", env!("CARGO_PKG_VERSION")))
            .build()?;

        Ok(Self {
            http,
            endpoint,
            api_key,
        })
    }

    /// Where this client posts.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Make exactly one POST.
    ///
    /// A transport failure — DNS, connection, TLS, or the per-attempt timeout —
    /// is rendered to a string here, at the single I/O boundary, so no live
    /// `reqwest::Error` escapes into the rest of the crate.
    async fn post_once(&self, body: &Value) -> Result<Attempt, String> {
        let response = self
            .http
            .post(self.endpoint.url().clone())
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key.expose()))
            .header(CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .await
            .map_err(render_transport_error)?;

        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response.text().await.map_err(render_transport_error)?;

        Ok(Attempt {
            status,
            request_id: request_id(&headers),
            retry_after: retry_after(&headers),
            body,
        })
    }

    /// Ask the questions, retrying what is worth retrying.
    ///
    /// `seed` is the inbound message id; it seeds the backoff jitter so the
    /// delay stays a pure function of the message while still spreading
    /// concurrent retries.
    ///
    /// This loop is bounded by the attempt budget alone. The whole-message
    /// budget is applied by the caller wrapping this call in a timeout, which
    /// is what makes `--timeout` genuinely cover the retry sequence.
    ///
    /// # Errors
    ///
    /// Returns the [`RequestFailure`] the last attempt produced.
    pub async fn ask(
        &self,
        body: &Value,
        seed: &str,
        policy: &RetryPolicy,
        asked: &QuestionSet,
    ) -> Result<ApiResponse, RequestFailure> {
        let mut attempt: u32 = 1;
        let mut last_request_id: Option<String> = None;

        loop {
            let outcome = match self.post_once(body).await {
                Ok(completed) => {
                    if completed.request_id.is_some() {
                        last_request_id.clone_from(&completed.request_id);
                    }

                    if (200..300).contains(&completed.status) {
                        return parse_response(&completed.body, asked, completed.request_id);
                    }

                    let decision = decide_retry(
                        &AttemptOutcome::Status {
                            code: completed.status,
                            retry_after: completed.retry_after,
                        },
                        attempt,
                        policy,
                        seed,
                    );

                    match decision {
                        RetryDecision::Retry { after } => Some(after),
                        RetryDecision::GiveUp | RetryDecision::Fatal => {
                            return Err(status_failure(&completed, attempt));
                        }
                    }
                }
                Err(message) => {
                    match decide_retry(&AttemptOutcome::Transport, attempt, policy, seed) {
                        RetryDecision::Retry { after } => Some(after),
                        RetryDecision::GiveUp | RetryDecision::Fatal => {
                            return Err(RequestFailure::Transport {
                                message,
                                attempts: attempt,
                                request_id: last_request_id,
                            });
                        }
                    }
                }
            };

            if let Some(after) = outcome {
                tokio::time::sleep(after).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// Classify a non-2xx status into the failure a router can act on.
///
/// A 4xx that is neither 401 nor 429 means the request itself is wrong — in
/// practice a questions file the API rejected — so it is `invalid_request`
/// regardless of which 4xx it is.
fn status_failure(attempt: &Attempt, attempts: u32) -> RequestFailure {
    let http = HttpFailure {
        status: attempt.status,
        attempts,
        body: attempt.body.clone(),
        detail: extract_detail(&attempt.body),
        request_id: attempt.request_id.clone(),
    };

    match attempt.status {
        401 => RequestFailure::Auth(http),
        429 => RequestFailure::RateLimited(http),
        status if status >= 500 => RequestFailure::ServerError(http),
        _ => RequestFailure::InvalidRequest(http),
    }
}

/// Render a transport error, saying plainly when it was the attempt timeout.
fn render_transport_error(err: reqwest::Error) -> String {
    if err.is_timeout() {
        format!("request timed out: {err}")
    } else {
        err.to_string()
    }
}

/// Read the vendor's request id off a response.
fn request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

/// Read and parse `Retry-After`.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attempt(status: u16, body: &str) -> Attempt {
        Attempt {
            status,
            request_id: Some("req_1".to_string()),
            retry_after: None,
            body: body.to_string(),
        }
    }

    #[test]
    fn statuses_map_to_the_kinds_a_router_selects_on() {
        assert_eq!(status_failure(&attempt(401, ""), 1).kind(), "auth");
        assert_eq!(status_failure(&attempt(429, ""), 4).kind(), "rate_limited");
        assert_eq!(status_failure(&attempt(500, ""), 4).kind(), "server_error");
        assert_eq!(status_failure(&attempt(529, ""), 4).kind(), "server_error");
        assert_eq!(
            status_failure(&attempt(422, ""), 1).kind(),
            "invalid_request"
        );
        // Any other client error is still the request's own fault.
        assert_eq!(
            status_failure(&attempt(404, ""), 1).kind(),
            "invalid_request"
        );
    }

    #[test]
    fn a_status_failure_carries_status_attempts_and_request_id() {
        let failure = status_failure(&attempt(429, "slow down"), 4);
        assert_eq!(failure.status(), Some(429));
        assert_eq!(failure.attempts(), Some(4));
        assert_eq!(failure.request_id(), Some("req_1"));
        assert_eq!(failure.body(), Some("slow down"));
    }

    #[test]
    fn a_validation_body_is_surfaced_as_structured_detail() {
        let body = r#"{"detail":[{"type":"missing","loc":["body","questions","q","criteria"],"msg":"Field required"}]}"#;
        let failure = status_failure(&attempt(422, body), 1);
        assert_eq!(
            failure.detail().and_then(|detail| detail.get(0)),
            Some(
                &json!({"type":"missing","loc":["body","questions","q","criteria"],"msg":"Field required"})
            )
        );
    }

    #[test]
    fn headers_are_read_for_the_request_id_and_retry_after() -> Result<(), String> {
        let mut headers = HeaderMap::new();
        headers.insert(
            REQUEST_ID_HEADER,
            "req_deadbeef"
                .parse()
                .map_err(|_| "bad header".to_string())?,
        );
        headers.insert(
            RETRY_AFTER,
            "30".parse().map_err(|_| "bad header".to_string())?,
        );

        assert_eq!(request_id(&headers).as_deref(), Some("req_deadbeef"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));

        let empty = HeaderMap::new();
        assert_eq!(request_id(&empty), None);
        assert_eq!(retry_after(&empty), None);
        Ok(())
    }

    #[test]
    fn an_http_date_retry_after_falls_back_to_computed_backoff() {
        let mut headers = HeaderMap::new();
        let Ok(value) = "Wed, 21 Oct 2026 07:28:00 GMT".parse() else {
            return;
        };
        headers.insert(RETRY_AFTER, value);
        assert_eq!(retry_after(&headers), None);
    }
}
