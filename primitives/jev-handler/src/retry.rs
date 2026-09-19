//! When to try again, and how long to wait first.
//!
//! All judgement about a failed attempt lives here, as pure functions over
//! plain data, so the client's retry loop only has to sequence what this module
//! decides.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use crate::args::RetryPolicy;

/// The share of a computed backoff that jitter may remove.
///
/// Subtracting rather than adding keeps the delay inside the operator's
/// `--retry-max-delay-ms` without a second clamp.
const JITTER_PERCENT: u64 = 25;

/// How one attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// The server answered with a non-2xx status.
    Status {
        /// The HTTP status code.
        code: u16,
        /// A parsed `Retry-After`, when the server sent a usable one.
        retry_after: Option<Duration>,
    },
    /// The request never completed: DNS, connection, TLS, or attempt timeout.
    Transport,
}

/// What to do about an attempt that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    /// Wait, then try again.
    Retry {
        /// How long to wait first.
        after: Duration,
    },
    /// Retryable, but the attempt budget is spent.
    GiveUp,
    /// Retrying cannot help; the request itself is wrong.
    Fatal,
}

/// Whether a status is worth trying again.
///
/// `429` and every `5xx` (including TypeSafe's `529 Overloaded`) are transient.
/// Any other `4xx` needs something outside this process to change — a bad key,
/// a malformed questions file, an empty credit balance — and would fail
/// identically on every attempt.
#[must_use]
pub fn is_retryable(code: u16) -> bool {
    code == 429 || code >= 500
}

/// Decide what to do after a failed attempt.
///
/// `attempt` is 1-based and counts the attempt that just failed.
#[must_use]
pub fn decide_retry(
    outcome: &AttemptOutcome,
    attempt: u32,
    policy: &RetryPolicy,
    seed: &str,
) -> RetryDecision {
    let retry_after = match outcome {
        AttemptOutcome::Status { code, .. } if !is_retryable(*code) => return RetryDecision::Fatal,
        AttemptOutcome::Status { retry_after, .. } => *retry_after,
        AttemptOutcome::Transport => None,
    };

    if attempt >= policy.max_attempts.get() {
        return RetryDecision::GiveUp;
    }

    // A server-supplied wait is honoured, but clamped: without the ceiling a
    // single `Retry-After: 3600` pins a concurrency slot for an hour and stalls
    // the pipeline with nothing in the logs to explain it.
    let after = retry_after.map_or_else(
        || backoff_delay(attempt, policy, seed),
        |wait| wait.min(policy.max_retry_after),
    );

    RetryDecision::Retry { after }
}

/// Exponential backoff with deterministic jitter.
///
/// The delay is `base * 2^(attempt-1)` capped at `max_delay`, less up to
/// [`JITTER_PERCENT`] of itself. The jitter is derived by hashing the message
/// id, which keeps the function pure and unit-testable while still spreading a
/// thundering herd across concurrent messages — and costs no `rand` dependency.
#[must_use]
pub fn backoff_delay(attempt: u32, policy: &RetryPolicy, seed: &str) -> Duration {
    let base_ms = u64::try_from(policy.base.as_millis()).unwrap_or(u64::MAX);
    let max_ms = u64::try_from(policy.max_delay.as_millis()).unwrap_or(u64::MAX);

    let doubling = attempt.saturating_sub(1).min(u32::BITS - 1);
    let capped = base_ms.saturating_mul(1_u64 << doubling).min(max_ms);

    let span = capped * JITTER_PERCENT / 100;
    let jitter = if span == 0 {
        0
    } else {
        jitter_hash(seed, attempt) % (span + 1)
    };

    Duration::from_millis(capped - jitter)
}

/// Parse a `Retry-After` header.
///
/// Only the delta-seconds form is understood. The HTTP-date form returns
/// `None` and the caller falls back to computed backoff — a known limitation,
/// taken deliberately over a date-parsing dependency for a header the API
/// documents in seconds.
#[must_use]
pub fn parse_retry_after(header: &str) -> Option<Duration> {
    let seconds: i64 = header.trim().parse().ok()?;
    u64::try_from(seconds).ok().map(Duration::from_secs)
}

/// Hash the message id and attempt into a jitter offset.
fn jitter_hash(seed: &str, attempt: u32) -> u64 {
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    attempt.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;

    const SEED: &str = "msg_01h4abcdefghijklmnopqrstu";

    fn policy() -> RetryPolicy {
        RetryPolicy::default()
    }

    fn status(code: u16) -> AttemptOutcome {
        AttemptOutcome::Status {
            code,
            retry_after: None,
        }
    }

    fn status_after(code: u16, seconds: u64) -> AttemptOutcome {
        AttemptOutcome::Status {
            code,
            retry_after: Some(Duration::from_secs(seconds)),
        }
    }

    #[test]
    fn a_client_error_is_fatal_on_the_first_attempt() {
        for code in [400, 401, 402, 403, 404, 422] {
            assert_eq!(
                decide_retry(&status(code), 1, &policy(), SEED),
                RetryDecision::Fatal,
                "HTTP {code} should not be retried"
            );
        }
    }

    #[test]
    fn rate_limits_server_errors_and_transport_failures_are_retried() {
        for outcome in [status(429), status(500), status(503), status(529)] {
            assert!(matches!(
                decide_retry(&outcome, 1, &policy(), SEED),
                RetryDecision::Retry { .. }
            ));
        }
        assert!(matches!(
            decide_retry(&AttemptOutcome::Transport, 1, &policy(), SEED),
            RetryDecision::Retry { .. }
        ));
    }

    #[test]
    fn the_last_attempt_gives_up_rather_than_waiting() {
        let policy = policy();
        let last = policy.max_attempts.get();
        assert_eq!(
            decide_retry(&status(429), last, &policy, SEED),
            RetryDecision::GiveUp
        );
        assert_eq!(
            decide_retry(&AttemptOutcome::Transport, last + 7, &policy, SEED),
            RetryDecision::GiveUp
        );
    }

    #[test]
    fn a_single_attempt_policy_never_retries() {
        let policy = RetryPolicy {
            max_attempts: NonZeroU32::MIN,
            ..policy()
        };
        assert_eq!(
            decide_retry(&status(429), 1, &policy, SEED),
            RetryDecision::GiveUp
        );
    }

    #[test]
    fn a_retry_after_header_is_honoured() {
        assert_eq!(
            decide_retry(&status_after(429, 30), 1, &policy(), SEED),
            RetryDecision::Retry {
                after: Duration::from_secs(30)
            }
        );
    }

    #[test]
    fn an_outlandish_retry_after_is_clamped_to_the_ceiling() {
        let policy = policy();
        assert_eq!(
            decide_retry(&status_after(429, 99_999), 1, &policy, SEED),
            RetryDecision::Retry {
                after: policy.max_retry_after
            }
        );
    }

    #[test]
    fn a_transport_failure_ignores_any_earlier_retry_after() {
        // Transport carries no header, so it always computes its own backoff.
        assert!(matches!(
            decide_retry(&AttemptOutcome::Transport, 2, &policy(), SEED),
            RetryDecision::Retry { after } if after <= policy().max_delay
        ));
    }

    #[test]
    fn backoff_doubles_per_attempt_within_its_jitter_band() {
        let policy = policy();
        for (attempt, nominal_ms) in [(1_u32, 500_u64), (2, 1_000), (3, 2_000), (4, 4_000)] {
            let delay = backoff_delay(attempt, &policy, SEED).as_millis();
            let nominal = u128::from(nominal_ms);
            assert!(
                delay <= nominal && delay >= nominal * 3 / 4,
                "attempt {attempt} gave {delay}ms, outside the {nominal}ms jitter band"
            );
        }
    }

    #[test]
    fn backoff_never_exceeds_the_configured_ceiling() {
        let policy = policy();
        for attempt in [10_u32, 32, 64, u32::MAX] {
            assert!(backoff_delay(attempt, &policy, SEED) <= policy.max_delay);
        }
    }

    #[test]
    fn jitter_is_deterministic_per_message_and_differs_across_messages() {
        let policy = policy();
        assert_eq!(
            backoff_delay(2, &policy, SEED),
            backoff_delay(2, &policy, SEED)
        );

        // Not every pair of ids must differ, but a spread of them must not all
        // collapse onto the same delay.
        let delays: Vec<Duration> = (0..16)
            .map(|n| backoff_delay(2, &policy, &format!("msg_{n:02}")))
            .collect();
        let mut unique = delays.clone();
        unique.sort_unstable();
        unique.dedup();
        assert!(
            unique.len() > 1,
            "jitter collapsed to one value: {delays:?}"
        );
    }

    #[test]
    fn parse_retry_after_reads_delta_seconds_only() {
        assert_eq!(parse_retry_after("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after("  30 "), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("-5"), None);
        assert_eq!(parse_retry_after("abc"), None);
        assert_eq!(parse_retry_after(""), None);
        // The HTTP-date form is the documented limitation.
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
    }
}
