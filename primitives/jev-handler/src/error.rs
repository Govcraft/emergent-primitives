//! The two error types, split by disposition rather than by layer.
//!
//! [`ConfigError`] is startup-fatal: the operator gave the primitive something
//! it cannot run with, so it prints and exits before connecting to the engine.
//! [`RequestFailure`] is per-message and recoverable: the handler publishes an
//! error event and goes on to the next message.
//!
//! [`RequestFailure`] deliberately stores *rendered* detail rather than a live
//! `reqwest::Error`. That is what lets the error payload be assembled by a pure
//! function and constructed in a unit test without fabricating a transport
//! error; `client` renders transport failures into it at the single I/O
//! boundary.

use std::fmt;
use std::path::PathBuf;

/// A startup-fatal misconfiguration.
///
/// Every variant means the primitive cannot run as configured, so it is
/// reported and the process exits before the engine ever sees a healthy
/// subscriber.
#[derive(Debug)]
pub enum ConfigError {
    /// `TYPESAFE_API_KEY` was absent or empty.
    MissingApiKey,
    /// The questions file could not be read.
    QuestionsRead {
        /// The path that was attempted.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// The questions file has an extension this primitive does not parse.
    QuestionsFormat {
        /// The path that was attempted.
        path: PathBuf,
    },
    /// The questions file is not well-formed TOML or JSON.
    QuestionsParse {
        /// The path that was attempted.
        path: PathBuf,
        /// The underlying parse failure.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The questions file parsed but breaks the question contract.
    QuestionsInvalid {
        /// What is wrong, naming the offending question id where there is one.
        detail: String,
    },
    /// `--endpoint` is not an absolute `http`/`https` URL.
    InvalidEndpoint {
        /// The value as given.
        value: String,
        /// Why it was rejected.
        detail: String,
    },
    /// `--model` is empty.
    InvalidModel {
        /// The value as given.
        value: String,
    },
    /// `--state-pointer` is neither empty nor an RFC 6901 pointer.
    InvalidStatePointer {
        /// The value as given.
        value: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingApiKey => write!(
                f,
                "TYPESAFE_API_KEY is not set; export the TypeSafe API key before starting jev-handler"
            ),
            Self::QuestionsRead { path, source } => {
                write!(f, "cannot read questions file {}: {source}", path.display())
            }
            Self::QuestionsFormat { path } => write!(
                f,
                "unsupported questions file {}: expected a .toml or .json extension",
                path.display()
            ),
            Self::QuestionsParse { path, source } => {
                write!(
                    f,
                    "cannot parse questions file {}: {source}",
                    path.display()
                )
            }
            Self::QuestionsInvalid { detail } => write!(f, "invalid questions file: {detail}"),
            Self::InvalidEndpoint { value, detail } => {
                write!(f, "invalid --endpoint `{value}`: {detail}")
            }
            Self::InvalidModel { value } => {
                write!(
                    f,
                    "invalid --model `{value}`: the model name cannot be empty"
                )
            }
            Self::InvalidStatePointer { value } => write!(
                f,
                "invalid --state-pointer `{value}`: a JSON pointer is empty or starts with `/`"
            ),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QuestionsRead { source, .. } => Some(source),
            Self::QuestionsParse { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

/// What the API returned when it refused the request.
///
/// The fields are shared by every status-carrying variant, which is why they
/// live in one struct: a 401 and a 429 differ in how a downstream router should
/// react, not in what the handler learned about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFailure {
    /// The HTTP status code.
    pub status: u16,
    /// How many attempts were made before giving up.
    pub attempts: u32,
    /// The response body, truncated by the payload assembler.
    pub body: String,
    /// The `detail` of a structured error body, when present.
    ///
    /// It arrives as an array for a FastAPI-style validation error and as an
    /// object for a single-cause error such as a 402; both are carried through
    /// as JSON rather than left buried in the raw body.
    pub detail: Option<serde_json::Value>,
    /// The `x-typesafe-request-id` response header, when present.
    pub request_id: Option<String>,
}

/// A per-message failure that publishes an error event.
///
/// The variants exist to be distinguishable *downstream*: `error.kind` is the
/// contract that keeps routing out of this primitive, so a `jq` selector can
/// page a human on `auth`, requeue `rate_limited`, and quarantine
/// `invalid_request` — which means the questions file is wrong, so retrying the
/// item would only fail again.
///
/// `billing` is the variant that looks fatal but is not: the request was
/// well-formed and the item is fine, so it is held for a human to add credit
/// and then requeued, never quarantined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestFailure {
    /// `--state-pointer` did not resolve in this message's payload.
    StateNotFound {
        /// The pointer that missed.
        pointer: String,
    },
    /// The API rejected the credentials (401).
    Auth(HttpFailure),
    /// The organization has no API credit left (402).
    ///
    /// The request itself was fine, so the item is worth keeping: only a human
    /// adding credit unblocks it.
    Billing(HttpFailure),
    /// The API rejected the request body (422, or any other 4xx that is not
    /// 401, 402, or 429).
    InvalidRequest(HttpFailure),
    /// The API rate limit was exhausted (429).
    RateLimited(HttpFailure),
    /// The API failed or was overloaded (5xx, including 529).
    ServerError(HttpFailure),
    /// The request never completed: DNS, connection, TLS, or attempt timeout.
    Transport {
        /// The rendered transport error.
        message: String,
        /// How many attempts were made before giving up.
        attempts: u32,
        /// The last `x-typesafe-request-id` seen, when any attempt returned one.
        request_id: Option<String>,
    },
    /// The whole-message budget (`--timeout`) elapsed mid-sequence.
    Timeout {
        /// The budget that elapsed, in milliseconds.
        budget_ms: u64,
    },
    /// A 2xx body that is not the documented response shape.
    BadResponse {
        /// What was wrong with the body.
        message: String,
        /// The response body, truncated by the payload assembler.
        body: String,
        /// The `x-typesafe-request-id` response header, when present.
        request_id: Option<String>,
    },
    /// A well-formed response that breaks the answer contract.
    AnswerContract {
        /// What was wrong, naming the offending question id.
        message: String,
        /// The `x-typesafe-request-id` response header, when present.
        request_id: Option<String>,
    },
}

impl RequestFailure {
    /// The stable `error.kind` string a downstream router selects on.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::StateNotFound { .. } => "state_not_found",
            Self::Auth(_) => "auth",
            Self::Billing(_) => "billing",
            Self::InvalidRequest(_) => "invalid_request",
            Self::RateLimited(_) => "rate_limited",
            Self::ServerError(_) => "server_error",
            Self::Transport { .. } => "transport",
            Self::Timeout { .. } => "timeout",
            Self::BadResponse { .. } => "bad_response",
            Self::AnswerContract { .. } => "answer_contract",
        }
    }

    /// The HTTP status, for the variants that carry one.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        self.http().map(|http| http.status)
    }

    /// How many attempts were made, where the count is known.
    ///
    /// A whole-message timeout cuts the sequence at an arbitrary point, so it
    /// reports no count rather than an invented one.
    #[must_use]
    pub fn attempts(&self) -> Option<u32> {
        match self {
            Self::Transport { attempts, .. } => Some(*attempts),
            Self::StateNotFound { .. }
            | Self::Timeout { .. }
            | Self::BadResponse { .. }
            | Self::AnswerContract { .. } => None,
            other => other.http().map(|http| http.attempts),
        }
    }

    /// The `x-typesafe-request-id` seen for this message, when any was.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::StateNotFound { .. } | Self::Timeout { .. } => None,
            Self::Transport { request_id, .. }
            | Self::BadResponse { request_id, .. }
            | Self::AnswerContract { request_id, .. } => request_id.as_deref(),
            other => other.http().and_then(|http| http.request_id.as_deref()),
        }
    }

    /// The raw response body, for the variants that captured one.
    #[must_use]
    pub fn body(&self) -> Option<&str> {
        match self {
            Self::BadResponse { body, .. } => Some(body),
            other => other.http().map(|http| http.body.as_str()),
        }
    }

    /// The structured `detail` of a validation error body, when the API sent one.
    #[must_use]
    pub fn detail(&self) -> Option<&serde_json::Value> {
        self.http().and_then(|http| http.detail.as_ref())
    }

    /// The shared status-failure fields, for the variants that carry them.
    fn http(&self) -> Option<&HttpFailure> {
        match self {
            Self::Auth(http)
            | Self::Billing(http)
            | Self::InvalidRequest(http)
            | Self::RateLimited(http)
            | Self::ServerError(http) => Some(http),
            _ => None,
        }
    }
}

impl fmt::Display for RequestFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StateNotFound { pointer } => {
                write!(
                    f,
                    "state pointer `{pointer}` did not resolve in the payload"
                )
            }
            Self::Auth(http) => {
                write!(f, "the API rejected the credentials (HTTP {})", http.status)
            }
            Self::Billing(http) => write!(
                f,
                "the organization is out of TypeSafe API credit (HTTP {}); add credit to resume",
                http.status
            ),
            Self::InvalidRequest(http) => {
                write!(f, "the API rejected the request (HTTP {})", http.status)
            }
            Self::RateLimited(http) => write!(
                f,
                "rate limited by the API (HTTP {}) after {} attempt(s)",
                http.status, http.attempts
            ),
            Self::ServerError(http) => write!(
                f,
                "the API failed (HTTP {}) after {} attempt(s)",
                http.status, http.attempts
            ),
            Self::Transport {
                message, attempts, ..
            } => write!(f, "request failed after {attempts} attempt(s): {message}"),
            Self::Timeout { budget_ms } => {
                write!(f, "the {budget_ms}ms message budget elapsed")
            }
            Self::BadResponse { message, .. } => write!(f, "unusable response: {message}"),
            Self::AnswerContract { message, .. } => write!(f, "answer contract broken: {message}"),
        }
    }
}

impl std::error::Error for RequestFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(status: u16) -> HttpFailure {
        HttpFailure {
            status,
            attempts: 3,
            body: "body".to_string(),
            detail: None,
            request_id: Some("req_abc".to_string()),
        }
    }

    #[test]
    fn every_variant_has_a_distinct_kind() {
        let kinds = [
            RequestFailure::StateNotFound {
                pointer: "/a".to_string(),
            }
            .kind(),
            RequestFailure::Auth(http(401)).kind(),
            RequestFailure::Billing(http(402)).kind(),
            RequestFailure::InvalidRequest(http(422)).kind(),
            RequestFailure::RateLimited(http(429)).kind(),
            RequestFailure::ServerError(http(529)).kind(),
            RequestFailure::Transport {
                message: "reset".to_string(),
                attempts: 4,
                request_id: None,
            }
            .kind(),
            RequestFailure::Timeout { budget_ms: 1 }.kind(),
            RequestFailure::BadResponse {
                message: "m".to_string(),
                body: "b".to_string(),
                request_id: None,
            }
            .kind(),
            RequestFailure::AnswerContract {
                message: "m".to_string(),
                request_id: None,
            }
            .kind(),
        ];

        let mut unique: Vec<&str> = kinds.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), kinds.len());
    }

    #[test]
    fn status_variants_expose_status_attempts_and_request_id() {
        let failure = RequestFailure::RateLimited(http(429));
        assert_eq!(failure.status(), Some(429));
        assert_eq!(failure.attempts(), Some(3));
        assert_eq!(failure.request_id(), Some("req_abc"));
        assert_eq!(failure.body(), Some("body"));
    }

    #[test]
    fn billing_is_a_status_failure_like_any_other() {
        let failure = RequestFailure::Billing(http(402));
        assert_eq!(failure.kind(), "billing");
        assert_eq!(failure.status(), Some(402));
        assert_eq!(failure.attempts(), Some(3));
        assert_eq!(failure.request_id(), Some("req_abc"));
        assert_eq!(failure.body(), Some("body"));
        // The message must send the operator somewhere, not just restate 402.
        assert!(failure.to_string().contains("credit"));
    }

    #[test]
    fn a_budget_timeout_invents_no_attempt_count() {
        let failure = RequestFailure::Timeout { budget_ms: 500 };
        assert_eq!(failure.attempts(), None);
        assert_eq!(failure.status(), None);
        assert_eq!(failure.request_id(), None);
    }

    #[test]
    fn transport_reports_its_own_attempt_count() {
        let failure = RequestFailure::Transport {
            message: "connection reset".to_string(),
            attempts: 4,
            request_id: Some("req_zz".to_string()),
        };
        assert_eq!(failure.attempts(), Some(4));
        assert_eq!(failure.request_id(), Some("req_zz"));
        assert_eq!(failure.body(), None);
    }

    #[test]
    fn config_errors_chain_to_their_source() {
        use std::error::Error as _;

        let err = ConfigError::QuestionsRead {
            path: PathBuf::from("/nope.toml"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        };
        assert!(err.source().is_some());
        assert!(err.to_string().contains("/nope.toml"));

        assert!(ConfigError::MissingApiKey.source().is_none());
    }
}
