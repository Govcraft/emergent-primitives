//! Round-trip tests for the one I/O act, against a local `axum` mock.
//!
//! No test here reaches the real API: every one drives [`JevClient`] at a
//! server bound to `127.0.0.1:0`, which is exactly what `--endpoint` exists
//! for. Hand-rolling the mock rather than reaching for `wiremock` buys precise
//! control over response *sequencing* and `Retry-After`, which is most of what
//! these tests are checking.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use jev_handler::args::{ApiKey, Endpoint, ModelName, RetryPolicy};
use jev_handler::client::{JevClient, REQUEST_ID_HEADER};
use jev_handler::error::{ConfigError, RequestFailure};
use jev_handler::questions::{QuestionSet, QuestionsFormat, parse_questions};
use jev_handler::request::build_request_body;
use serde_json::{Value, json};
use std::num::NonZeroU32;

const QUESTIONS: &str = r#"
[questions.lure]
type = "noul"
instructions = "Is there a lure?"

[questions.kind]
type = "choice"
instructions = "What kind?"
criteria = { phish = "a", personal = "b" }
"#;

const ANSWERED: &str = r#"{"model":"jev-1.13.0",
  "answers":{"lure":{"type":"noul","noul":0.93},
             "kind":{"type":"choice","choice":"phish","confidence":0.99,
                     "probabilities":{"phish":0.99,"personal":0.01}}},
  "usage":{"input_tokens":391,"output_tokens":34}}"#;

const VALIDATION_BODY: &str = r#"{"detail":[{"type":"missing","loc":["body","questions","kind","criteria"],"msg":"Field required"}]}"#;

/// One scripted response.
#[derive(Clone)]
struct Reply {
    status: StatusCode,
    body: &'static str,
    retry_after: Option<&'static str>,
    /// Milliseconds to stall before replying, for the attempt-timeout test.
    delay_ms: u64,
}

impl Reply {
    fn ok() -> Self {
        Self {
            status: StatusCode::OK,
            body: ANSWERED,
            retry_after: None,
            delay_ms: 0,
        }
    }

    fn status(status: u16, body: &'static str) -> Self {
        Self {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            body,
            retry_after: None,
            delay_ms: 0,
        }
    }

    fn retry_after(mut self, seconds: &'static str) -> Self {
        self.retry_after = Some(seconds);
        self
    }

    fn after(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }
}

/// What the mock saw, and what it will say next.
struct Mock {
    replies: Vec<Reply>,
    calls: AtomicUsize,
    last_authorization: std::sync::Mutex<Option<String>>,
    last_body: std::sync::Mutex<Option<Value>>,
}

impl Mock {
    fn reply_for(&self, call: usize) -> Reply {
        self.replies
            .get(call)
            .or_else(|| self.replies.last())
            .cloned()
            .unwrap_or_else(Reply::ok)
    }
}

/// Handle one request: record it, then reply from the script.
async fn handle(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let call = mock.calls.fetch_add(1, Ordering::SeqCst);

    if let Ok(mut slot) = mock.last_authorization.lock() {
        *slot = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
    }
    if let Ok(mut slot) = mock.last_body.lock() {
        *slot = serde_json::from_str(&body).ok();
    }

    let reply = mock.reply_for(call);
    if reply.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(reply.delay_ms)).await;
    }

    let mut response_headers = HeaderMap::new();
    if let Ok(value) = "req_deadbeef".parse() {
        response_headers.insert(REQUEST_ID_HEADER, value);
    }
    if let Some(Ok(value)) = reply.retry_after.map(str::parse) {
        response_headers.insert("retry-after", value);
    }

    (reply.status, response_headers, reply.body)
}

/// Start the mock on an ephemeral port and return it with its base URL.
async fn serve(replies: Vec<Reply>) -> Result<(Arc<Mock>, String), String> {
    let mock = Arc::new(Mock {
        replies,
        calls: AtomicUsize::new(0),
        last_authorization: std::sync::Mutex::new(None),
        last_body: std::sync::Mutex::new(None),
    });

    let app = Router::new()
        .route("/v1/systemone", post(handle))
        .with_state(Arc::clone(&mock));

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Ok((mock, format!("http://{addr}/v1/systemone")))
}

fn questions() -> Result<QuestionSet, String> {
    parse_questions(QUESTIONS, QuestionsFormat::Toml).map_err(|err: ConfigError| err.to_string())
}

/// A client pointed at `url`, with a short per-attempt timeout.
fn client(url: &str, request_timeout_ms: u64) -> Result<JevClient, String> {
    let endpoint: Endpoint = url.parse().map_err(|err: ConfigError| err.to_string())?;
    JevClient::new(
        endpoint,
        test_api_key()?,
        Duration::from_millis(request_timeout_ms),
    )
    .map_err(|err| err.to_string())
}

/// A key that exists only in this process; the mock never checks it.
fn test_api_key() -> Result<ApiKey, String> {
    ApiKey::new("test-key-not-a-real-credential").map_err(|err: ConfigError| err.to_string())
}

/// A fast policy so the tests do not wait out real backoff.
fn policy(max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts: NonZeroU32::new(max_attempts).unwrap_or(NonZeroU32::MIN),
        base: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
        max_retry_after: Duration::from_millis(20),
    }
}

fn body() -> Result<Value, String> {
    Ok(build_request_body(
        &json!({"subject": "Action required"}),
        &ModelName::default(),
        &questions()?,
    ))
}

#[tokio::test]
async fn a_successful_call_makes_exactly_one_request() -> Result<(), String> {
    let (mock, url) = serve(vec![Reply::ok()]).await?;
    let response = client(&url, 2_000)?
        .ask(&body()?, "msg_1", &policy(4), &questions()?)
        .await
        .map_err(|err| err.to_string())?;

    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert_eq!(response.model, "jev-1.13.0");
    assert_eq!(response.request_id.as_deref(), Some("req_deadbeef"));
    assert_eq!(response.answers_raw["kind"]["choice"], "phish");
    Ok(())
}

#[tokio::test]
async fn the_request_carries_a_bearer_header_and_the_documented_body() -> Result<(), String> {
    let (mock, url) = serve(vec![Reply::ok()]).await?;
    client(&url, 2_000)?
        .ask(&body()?, "msg_1", &policy(4), &questions()?)
        .await
        .map_err(|err| err.to_string())?;

    let authorization = mock
        .last_authorization
        .lock()
        .map_err(|_| "poisoned".to_string())?
        .clone();
    assert!(
        authorization.is_some_and(|value| value.starts_with("Bearer ")),
        "the Authorization header should be a bearer token"
    );

    let sent = mock
        .last_body
        .lock()
        .map_err(|_| "poisoned".to_string())?
        .clone()
        .ok_or_else(|| "the mock saw no JSON body".to_string())?;
    assert_eq!(sent["model"], "jev-latest");
    assert_eq!(sent["state"]["subject"], "Action required");
    // `questions` is a map keyed by question id, not an array.
    assert!(sent["questions"].is_object());
    assert_eq!(sent["questions"]["kind"]["type"], "choice");
    assert_eq!(
        sent["questions"]["lure"]["instructions"],
        "Is there a lure?"
    );
    Ok(())
}

#[tokio::test]
async fn a_rate_limit_is_retried_and_then_succeeds() -> Result<(), String> {
    let (mock, url) = serve(vec![
        Reply::status(429, "slow down").retry_after("0"),
        Reply::ok(),
    ])
    .await?;

    client(&url, 2_000)?
        .ask(&body()?, "msg_1", &policy(4), &questions()?)
        .await
        .map_err(|err| err.to_string())?;

    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn an_overloaded_server_is_retried_and_then_succeeds() -> Result<(), String> {
    let (mock, url) = serve(vec![Reply::status(529, "overloaded"), Reply::ok()]).await?;

    client(&url, 2_000)?
        .ask(&body()?, "msg_1", &policy(4), &questions()?)
        .await
        .map_err(|err| err.to_string())?;

    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn a_persistent_rate_limit_exhausts_the_attempt_budget() -> Result<(), String> {
    let (mock, url) = serve(vec![Reply::status(429, "slow down")]).await?;

    let failure = client(&url, 2_000)?
        .ask(&body()?, "msg_1", &policy(3), &questions()?)
        .await
        .err()
        .ok_or_else(|| "a persistent 429 should fail".to_string())?;

    assert_eq!(failure.kind(), "rate_limited");
    assert_eq!(failure.attempts(), Some(3));
    assert_eq!(failure.status(), Some(429));
    assert_eq!(failure.request_id(), Some("req_deadbeef"));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 3);
    Ok(())
}

#[tokio::test]
async fn an_unauthorized_response_is_not_retried() -> Result<(), String> {
    let (mock, url) = serve(vec![Reply::status(401, r#"{"error":"bad key"}"#)]).await?;

    let failure = client(&url, 2_000)?
        .ask(&body()?, "msg_1", &policy(4), &questions()?)
        .await
        .err()
        .ok_or_else(|| "a 401 should fail".to_string())?;

    assert_eq!(failure.kind(), "auth");
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn a_validation_failure_is_not_retried_and_surfaces_its_detail() -> Result<(), String> {
    let (mock, url) = serve(vec![Reply::status(422, VALIDATION_BODY)]).await?;

    let failure = client(&url, 2_000)?
        .ask(&body()?, "msg_1", &policy(4), &questions()?)
        .await
        .err()
        .ok_or_else(|| "a 422 should fail".to_string())?;

    assert_eq!(failure.kind(), "invalid_request");
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        failure
            .detail()
            .and_then(|detail| detail.get(0))
            .and_then(|entry| entry.get("loc"))
            .and_then(|loc| loc.get(2)),
        Some(&json!("kind"))
    );
    Ok(())
}

#[tokio::test]
async fn a_stalled_server_times_out_the_attempt_and_is_retried() -> Result<(), String> {
    let (mock, url) = serve(vec![Reply::ok().after(400), Reply::ok()]).await?;

    client(&url, 80)?
        .ask(&body()?, "msg_1", &policy(4), &questions()?)
        .await
        .map_err(|err| err.to_string())?;

    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn a_server_that_never_answers_ends_as_a_transport_failure() -> Result<(), String> {
    let (_mock, url) = serve(vec![Reply::ok().after(400)]).await?;

    let failure = client(&url, 60)?
        .ask(&body()?, "msg_1", &policy(2), &questions()?)
        .await
        .err()
        .ok_or_else(|| "a stalled server should fail".to_string())?;

    assert_eq!(failure.kind(), "transport");
    assert_eq!(failure.attempts(), Some(2));
    Ok(())
}

#[tokio::test]
async fn a_two_hundred_that_answers_the_wrong_question_breaks_the_contract() -> Result<(), String> {
    let (mock, url) = serve(vec![Reply::status(
        200,
        r#"{"model":"jev-1","answers":{"lure":{"type":"noul","noul":0.5}},"usage":{}}"#,
    )])
    .await?;

    let failure = client(&url, 2_000)?
        .ask(&body()?, "msg_1", &policy(4), &questions()?)
        .await
        .err()
        .ok_or_else(|| "a missing answer should fail".to_string())?;

    assert_eq!(failure.kind(), "answer_contract");
    assert_eq!(failure.request_id(), Some("req_deadbeef"));
    // A broken contract is the server's final word, not something to retry.
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn a_two_hundred_that_is_not_the_documented_envelope_is_a_bad_response() -> Result<(), String>
{
    let (_mock, url) = serve(vec![Reply::status(200, "<html>maintenance</html>")]).await?;

    let failure = client(&url, 2_000)?
        .ask(&body()?, "msg_1", &policy(4), &questions()?)
        .await
        .err()
        .ok_or_else(|| "an unparseable 200 should fail".to_string())?;

    assert_eq!(failure.kind(), "bad_response");
    assert!(
        failure
            .body()
            .is_some_and(|body| body.contains("maintenance")),
        "the raw body should be carried for diagnosis"
    );
    Ok(())
}

#[tokio::test]
async fn the_whole_message_budget_cuts_a_long_retry_sequence_short() -> Result<(), String> {
    let (mock, url) = serve(vec![Reply::status(429, "slow down").retry_after("1")]).await?;

    let policy = RetryPolicy {
        max_attempts: NonZeroU32::new(50).unwrap_or(NonZeroU32::MIN),
        base: Duration::from_millis(200),
        max_delay: Duration::from_millis(200),
        max_retry_after: Duration::from_millis(200),
    };

    let client = client(&url, 2_000)?;
    let outcome = tokio::time::timeout(
        Duration::from_millis(500),
        client.ask(&body()?, "msg_1", &policy, &questions()?),
    )
    .await;

    // The budget elapsed before the attempt budget did: this is exactly how
    // `main` applies `--timeout` over the whole retry sequence.
    assert!(outcome.is_err(), "the budget should have cut the sequence");
    let calls = mock.calls.load(Ordering::SeqCst);
    assert!(
        (1..50).contains(&calls),
        "the sequence should have been cut mid-flight, saw {calls} call(s)"
    );

    // And the failure the handler publishes for it.
    let failure = RequestFailure::Timeout { budget_ms: 500 };
    assert_eq!(failure.kind(), "timeout");
    Ok(())
}
