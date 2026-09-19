//! The command line, and the validated configuration it turns into.
//!
//! Every flag is parsed into a type that carries its invariant, so nothing
//! downstream has to re-check an endpoint, a pointer, or a retry bound. The
//! validation happens at startup, before the engine is connected, so a
//! misconfigured primitive never appears healthy in a topology.

use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;
use std::time::Duration;

use clap::Parser;
use serde_json::Value;

use crate::error::ConfigError;

/// The environment variable the API key is read from.
pub const API_KEY_VAR: &str = "TYPESAFE_API_KEY";

/// The TypeSafe System One evaluation endpoint.
pub const DEFAULT_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// The default model alias.
pub const DEFAULT_MODEL: &str = "jev-latest";

/// The TypeSafe API key.
///
/// `Debug` is hand-written and there is deliberately no `Display` and no
/// `Serialize`: a derived `Debug` is exactly how a credential reaches a log
/// line, an error payload, or a panic message. The only way out is
/// [`ApiKey::expose`], which is easy to grep for.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wrap a non-blank key.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::MissingApiKey`] when the key is blank.
    pub fn new(key: &str) -> Result<Self, ConfigError> {
        if key.trim().is_empty() {
            Err(ConfigError::MissingApiKey)
        } else {
            Ok(Self(key.to_string()))
        }
    }

    /// Read the key from [`API_KEY_VAR`].
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::MissingApiKey`] when the variable is unset or
    /// blank. There is no `--api-key` flag: a key on a command line is a key in
    /// the process table and in the engine's own configuration file.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::new(&std::env::var(API_KEY_VAR).unwrap_or_default())
    }

    /// The key itself, for the one place that sends it: the `Authorization` header.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// The model alias or pinned model id sent with every request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelName(String);

impl ModelName {
    /// The name as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ModelName {
    fn default() -> Self {
        Self(DEFAULT_MODEL.to_string())
    }
}

impl FromStr for ModelName {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim().is_empty() {
            Err(ConfigError::InvalidModel {
                value: value.to_string(),
            })
        } else {
            Ok(Self(value.to_string()))
        }
    }
}

impl fmt::Display for ModelName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The URL every request is posted to.
///
/// This is load-bearing beyond deployment flexibility: it is how the test suite
/// drives the client against a local mock instead of the real API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint(reqwest::Url);

impl Endpoint {
    /// The URL, for the one place that needs it.
    #[must_use]
    pub fn url(&self) -> &reqwest::Url {
        &self.0
    }
}

impl FromStr for Endpoint {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let url = reqwest::Url::parse(value).map_err(|err| ConfigError::InvalidEndpoint {
            value: value.to_string(),
            detail: err.to_string(),
        })?;

        if !matches!(url.scheme(), "http" | "https") {
            return Err(ConfigError::InvalidEndpoint {
                value: value.to_string(),
                detail: format!("scheme `{}` is not http or https", url.scheme()),
            });
        }

        Ok(Self(url))
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Which part of the inbound payload is sent as the request's `state`.
///
/// Empty means the whole payload. Anything else is an RFC 6901 JSON pointer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatePointer(String);

impl StatePointer {
    /// The pointer as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Resolve the pointer against a payload.
    ///
    /// A pointer that misses is a *per-message* condition, not a startup one:
    /// the pointer can be perfectly valid and this one message merely shaped
    /// differently, so resolution returns an option rather than an error.
    #[must_use]
    pub fn resolve<'a>(&self, payload: &'a Value) -> Option<&'a Value> {
        if self.0.is_empty() {
            Some(payload)
        } else {
            payload.pointer(&self.0)
        }
    }
}

impl FromStr for StatePointer {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || value.starts_with('/') {
            Ok(Self(value.to_string()))
        } else {
            Err(ConfigError::InvalidStatePointer {
                value: value.to_string(),
            })
        }
    }
}

impl fmt::Display for StatePointer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How hard, and how patiently, a failed attempt is retried.
///
/// The four values are grouped for the reason `exec-common::ExecTimeouts`
/// groups its two: as bare arguments in a row they are silently swappable at a
/// call site with no type error to catch it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts, including the first.
    pub max_attempts: NonZeroU32,
    /// The first backoff step; doubled per attempt.
    pub base: Duration,
    /// The ceiling on a computed backoff.
    pub max_delay: Duration,
    /// The ceiling on a server-supplied `Retry-After`.
    ///
    /// Without it a server can pin a concurrency permit for an hour and stall
    /// the pipeline invisibly.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: NonZeroU32::MIN.saturating_add(3),
            base: Duration::from_millis(500),
            max_delay: Duration::from_millis(30_000),
            max_retry_after: Duration::from_millis(60_000),
        }
    }
}

/// Ask TypeSafe System One a fixed set of typed questions about every event.
///
/// The questions file is read once at startup and asked about every message;
/// the answers are published unchanged so routing can live in the topology.
#[derive(Parser, Debug)]
#[command(name = "jev-handler")]
#[command(about = "Ask TypeSafe System One typed questions about event payloads")]
pub struct Args {
    /// Message types to subscribe to.
    #[arg(short, long = "subscribe", required = true)]
    pub subscribe: Vec<String>,

    /// Path to the questions file (`.toml` or `.json`).
    #[arg(long, required = true)]
    pub questions: std::path::PathBuf,

    /// Message type for an answered message.
    #[arg(long, default_value = "jev.answered")]
    pub publish_as: String,

    /// Message type for a failed message.
    #[arg(short, long, default_value = "jev.error")]
    pub error_as: String,

    /// Model alias or pinned model id.
    #[arg(long, default_value = DEFAULT_MODEL)]
    pub model: String,

    /// JSON pointer to the part of the payload sent as `state` (default: the whole payload).
    #[arg(long, default_value = "")]
    pub state_pointer: String,

    /// Total budget for one message in milliseconds, retries included.
    #[arg(short, long, default_value_t = 120_000, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,

    /// Timeout for a single HTTP attempt in milliseconds.
    ///
    /// Kept separate from `--timeout` deliberately: with one knob, retries
    /// silently blow past the budget the operator thought they set.
    #[arg(long, default_value_t = 30_000, value_parser = clap::value_parser!(u64).range(1..))]
    pub request_timeout: u64,

    /// Maximum number of messages in flight at once.
    ///
    /// This is the real rate control against a rate-limited API. A task in
    /// retry backoff holds its slot, so at 1 a 429 storm stalls the handler —
    /// honest backpressure, but easy to mistake for a hang.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..))]
    pub max_concurrent: u16,

    /// Total attempts per message, including the first.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..))]
    pub max_attempts: u32,

    /// First backoff step in milliseconds; doubled per attempt.
    #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u64).range(1..))]
    pub retry_base_ms: u64,

    /// Ceiling on a computed backoff, in milliseconds.
    #[arg(long, default_value_t = 30_000, value_parser = clap::value_parser!(u64).range(1..))]
    pub retry_max_delay_ms: u64,

    /// Ceiling on a server-supplied `Retry-After`, in milliseconds.
    #[arg(long, default_value_t = 60_000, value_parser = clap::value_parser!(u64).range(1..))]
    pub max_retry_after_ms: u64,

    /// The evaluation endpoint to post to.
    #[arg(long, default_value = DEFAULT_ENDPOINT)]
    pub endpoint: String,
}

/// The validated form of [`Args`], with the API key pulled from the environment.
#[derive(Debug)]
pub struct Config {
    /// The credential, never logged and never published.
    pub api_key: ApiKey,
    /// Where to post.
    pub endpoint: Endpoint,
    /// The model to ask.
    pub model: ModelName,
    /// Which part of the payload becomes `state`.
    pub state_pointer: StatePointer,
    /// The whole-message budget, retries included.
    pub budget: Duration,
    /// The per-attempt HTTP timeout.
    pub request_timeout: Duration,
    /// How failures are retried.
    pub retry: RetryPolicy,
}

impl Config {
    /// Validate parsed arguments and read the API key from the environment.
    ///
    /// # Errors
    ///
    /// Returns the first [`ConfigError`] the arguments produce.
    pub fn from_args(args: &Args) -> Result<Self, ConfigError> {
        Ok(Self {
            api_key: ApiKey::from_env()?,
            endpoint: args.endpoint.parse()?,
            model: args.model.parse()?,
            state_pointer: args.state_pointer.parse()?,
            budget: Duration::from_millis(args.timeout),
            request_timeout: Duration::from_millis(args.request_timeout),
            retry: RetryPolicy {
                max_attempts: NonZeroU32::new(args.max_attempts).unwrap_or(NonZeroU32::MIN),
                base: Duration::from_millis(args.retry_base_ms),
                max_delay: Duration::from_millis(args.retry_max_delay_ms),
                max_retry_after: Duration::from_millis(args.max_retry_after_ms),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_api_key_never_renders_itself() -> Result<(), ConfigError> {
        let key = ApiKey::new("sk-super-secret")?;
        assert_eq!(format!("{key:?}"), "ApiKey(<redacted>)");
        assert!(!format!("{key:?}").contains("secret"));
        assert_eq!(key.expose(), "sk-super-secret");
        Ok(())
    }

    #[test]
    fn a_blank_key_is_no_key() {
        assert!(ApiKey::new("").is_err());
        assert!(ApiKey::new("   ").is_err());
    }

    #[test]
    fn a_config_debug_line_carries_no_credential() -> Result<(), ConfigError> {
        let config = Config {
            api_key: ApiKey::new("sk-super-secret")?,
            endpoint: DEFAULT_ENDPOINT.parse()?,
            model: ModelName::default(),
            state_pointer: StatePointer::default(),
            budget: Duration::from_millis(1),
            request_timeout: Duration::from_millis(1),
            retry: RetryPolicy::default(),
        };
        assert!(!format!("{config:?}").contains("sk-super-secret"));
        Ok(())
    }

    #[test]
    fn the_documented_default_endpoint_parses() -> Result<(), ConfigError> {
        assert_eq!(
            DEFAULT_ENDPOINT.parse::<Endpoint>()?.to_string(),
            DEFAULT_ENDPOINT
        );
        Ok(())
    }

    #[test]
    fn an_endpoint_must_be_an_absolute_http_url() {
        assert!("https://example.test/v1".parse::<Endpoint>().is_ok());
        assert!("http://127.0.0.1:8080/v1".parse::<Endpoint>().is_ok());
        assert!("ftp://example.test/v1".parse::<Endpoint>().is_err());
        assert!("/v1/systemone".parse::<Endpoint>().is_err());
        assert!("not a url".parse::<Endpoint>().is_err());
    }

    #[test]
    fn the_default_model_is_the_flagship_alias() {
        assert_eq!(ModelName::default().as_str(), "jev-latest");
        assert!("".parse::<ModelName>().is_err());
        assert!("  ".parse::<ModelName>().is_err());
    }

    #[test]
    fn an_empty_pointer_selects_the_whole_payload() -> Result<(), ConfigError> {
        let payload = json!({"data": {"item": 7}});
        let pointer: StatePointer = "".parse()?;
        assert_eq!(pointer.resolve(&payload), Some(&payload));
        Ok(())
    }

    #[test]
    fn a_pointer_selects_a_sub_tree() -> Result<(), ConfigError> {
        let payload = json!({"data": {"item": 7}});
        let pointer: StatePointer = "/data/item".parse()?;
        assert_eq!(pointer.resolve(&payload), Some(&json!(7)));
        Ok(())
    }

    #[test]
    fn a_pointer_that_misses_resolves_to_nothing() -> Result<(), ConfigError> {
        let pointer: StatePointer = "/data/item".parse()?;
        assert_eq!(pointer.resolve(&json!({"other": 1})), None);
        Ok(())
    }

    #[test]
    fn a_pointer_that_is_not_rfc_6901_is_a_startup_error() {
        assert!("data/item".parse::<StatePointer>().is_err());
        assert!("data".parse::<StatePointer>().is_err());
    }

    #[test]
    fn the_default_retry_policy_allows_four_attempts() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_attempts.get(), 4);
        assert_eq!(policy.base, Duration::from_millis(500));
    }

    #[test]
    fn the_cli_definition_is_internally_consistent() {
        use clap::CommandFactory as _;
        Args::command().debug_assert();
    }
}
