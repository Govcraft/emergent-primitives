//! Assembling the two payloads this primitive publishes.
//!
//! Both are pure functions over plain data, which is the whole reason
//! [`RequestFailure`] stores rendered detail instead of a live `reqwest::Error`:
//! an error payload can be built, asserted on, and reasoned about without a
//! network anywhere near it.
//!
//! The API key is not reachable from either function — neither takes a
//! [`crate::args::ApiKey`] and no failure variant carries one — so a credential
//! cannot land in the event store by accident.

use serde_json::{Value, json};

use crate::args::{Endpoint, ModelName};
use crate::error::RequestFailure;
use crate::response::ApiResponse;

/// How much of an error response body is carried into the error event.
///
/// Enough to diagnose a validation failure, bounded so a chatty upstream cannot
/// fill the event store one failure at a time.
pub const MAX_BODY_BYTES: usize = 2048;

/// The configuration a failure is described against.
///
/// Endpoint and model are what turn "the request failed" into something an
/// operator can act on, and neither is recoverable from the failure itself.
#[derive(Debug, Clone, Copy)]
pub struct FailureContext<'a> {
    /// Where the request was posted.
    pub endpoint: &'a Endpoint,
    /// Which model was asked.
    pub model: &'a ModelName,
}

/// Build the success payload.
///
/// `answers` is the vendor's object verbatim, because downstream routers select
/// on it with `jq` — `select(.answers.kind.confidence >= 0.9)` only works if the
/// shape survives this primitive untouched. The inbound payload is nested under
/// `input` rather than spread, since `answers`, `usage`, and `model` would
/// collide with ordinary payload keys.
#[must_use]
pub fn success_payload(input: &Value, response: &ApiResponse) -> Value {
    json!({
        "input": input,
        "answers": response.answers_raw,
        "usage": response.usage,
        "model": response.model,
        "request_id": response.request_id,
    })
}

/// Build the error payload.
///
/// The merge rule is `exec-common`'s, applied through the shared function so
/// the two cannot drift: failure details go under a reserved `error` key with
/// the inbound payload spread alongside, and a payload that is not an object is
/// carried under `input` instead.
#[must_use]
pub fn error_payload(input: &Value, failure: &RequestFailure, context: &FailureContext) -> Value {
    exec_common::error_payload(error_detail(failure, context), input)
}

/// The failure details alone, without any inbound identity.
///
/// `kind` is the contract that keeps routing out of this primitive: a `jq`
/// router pages a human on `auth`, requeues `rate_limited`, and quarantines
/// `invalid_request`, which means the questions file is wrong and retrying the
/// item would only fail the same way.
#[must_use]
pub fn error_detail(failure: &RequestFailure, context: &FailureContext) -> Value {
    json!({
        "kind": failure.kind(),
        "status": failure.status(),
        "attempts": failure.attempts(),
        "message": failure.to_string(),
        "endpoint": context.endpoint.to_string(),
        "model": context.model.as_str(),
        "request_id": failure.request_id(),
        "body": failure.body().map(truncate_body),
        "detail": failure.detail(),
    })
}

/// Cut a response body down to [`MAX_BODY_BYTES`], on a character boundary.
///
/// The kept text is the operator's own state coming back in a validation error,
/// not a secret — but it does land in the event store, which the README says
/// out loud.
#[must_use]
pub fn truncate_body(body: &str) -> String {
    if body.len() <= MAX_BODY_BYTES {
        return body.to_string();
    }

    let mut end = MAX_BODY_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}… (truncated)", &body[..end])
}

/// Pull the structured `detail` out of a FastAPI-style validation body.
///
/// A 422 says which question the API rejected, and a router can only act on
/// that if it arrives as JSON rather than as a string of JSON. Any other body
/// shape yields `None` and the truncated raw body carries the information
/// instead.
#[must_use]
pub fn extract_detail(body: &str) -> Option<Value> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    match parsed.get("detail") {
        Some(detail @ (Value::Array(_) | Value::Object(_))) => Some(detail.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::API_KEY_VAR;
    use crate::error::{ConfigError, HttpFailure};
    use std::collections::BTreeMap;

    /// The `answers` object exactly as the live API returned it.
    const SAMPLE_ANSWERS: &str = r#"{
       "lure":     {"type":"noul","noul":0.93},
       "kind":     {"type":"choice","choice":"phish","confidence":0.99,
                    "probabilities":{"vendor_notice":0.01,"cold_pitch":0.0,"phish":0.99,"personal":0.0}},
       "pressure": {"type":"score","score":2.0,"confidence":1.0,
                    "legend":{"0":"No deadline mentioned.","1":"...","2":"..."},
                    "probabilities":{"0":0.0,"1":0.0,"2":1.0}}}"#;

    const API_KEY: &str = "sk-live-do-not-leak-me";

    fn endpoint() -> Result<Endpoint, ConfigError> {
        "https://api.typesafe.ai/v1/systemone".parse()
    }

    fn sample_response() -> Result<ApiResponse, String> {
        Ok(ApiResponse {
            model: "jev-1.13.0".to_string(),
            answers: BTreeMap::new(),
            answers_raw: serde_json::from_str(SAMPLE_ANSWERS).map_err(|e| e.to_string())?,
            usage: json!({"input_tokens": 391, "output_tokens": 34}),
            request_id: Some("req_abc123".to_string()),
        })
    }

    fn http(status: u16) -> HttpFailure {
        HttpFailure {
            status,
            attempts: 4,
            body: r#"{"detail":[{"type":"missing","loc":["body","questions","q","criteria"],"msg":"Field required"}]}"#.to_string(),
            detail: extract_detail(r#"{"detail":[{"type":"missing","loc":["body","questions","q","criteria"],"msg":"Field required"}]}"#),
            request_id: Some("req_zz".to_string()),
        }
    }

    #[test]
    fn success_carries_input_answers_usage_model_and_request_id() -> Result<(), String> {
        let input = json!({"issue": 42, "subject": "Action required"});
        let payload = success_payload(&input, &sample_response()?);

        assert_eq!(payload["input"], input);
        assert_eq!(payload["usage"]["input_tokens"], 391);
        assert_eq!(payload["model"], "jev-1.13.0");
        assert_eq!(payload["request_id"], "req_abc123");
        Ok(())
    }

    #[test]
    fn the_answers_object_reaches_a_router_in_the_vendors_exact_shape() -> Result<(), String> {
        let verbatim: Value = serde_json::from_str(SAMPLE_ANSWERS).map_err(|e| e.to_string())?;
        let payload = success_payload(&json!({}), &sample_response()?);

        assert_eq!(payload["answers"], verbatim);
        // The selectors the README's router blocks are written with.
        assert_eq!(payload["answers"]["kind"]["confidence"], 0.99);
        assert_eq!(payload["answers"]["lure"]["noul"], 0.93);
        assert_eq!(payload["answers"]["pressure"]["score"], 2.0);
        Ok(())
    }

    #[test]
    fn the_input_is_carried_verbatim_whatever_its_shape() -> Result<(), String> {
        for input in [
            json!({"a": 1}),
            json!("a string"),
            json!([1, 2, 3]),
            json!(7),
        ] {
            let payload = success_payload(&input, &sample_response()?);
            assert_eq!(payload["input"], input);
        }
        Ok(())
    }

    #[test]
    fn an_absent_request_id_is_null_rather_than_missing() -> Result<(), String> {
        let mut response = sample_response()?;
        response.request_id = None;
        assert_eq!(
            success_payload(&json!({}), &response)["request_id"],
            Value::Null
        );
        Ok(())
    }

    #[test]
    fn an_error_spreads_into_an_object_payload_under_a_reserved_key() -> Result<(), ConfigError> {
        let endpoint = endpoint()?;
        let model = ModelName::default();
        let context = FailureContext {
            endpoint: &endpoint,
            model: &model,
        };
        let input = json!({"issue": 42, "workspace": "wt-7"});
        let payload = error_payload(&input, &RequestFailure::RateLimited(http(429)), &context);

        assert_eq!(payload["issue"], 42);
        assert_eq!(payload["workspace"], "wt-7");
        assert_eq!(payload["error"]["kind"], "rate_limited");
        assert_eq!(payload["error"]["status"], 429);
        assert_eq!(payload["error"]["attempts"], 4);
        assert_eq!(payload["error"]["request_id"], "req_zz");
        assert_eq!(payload["error"]["model"], "jev-latest");
        assert_eq!(
            payload["error"]["endpoint"],
            "https://api.typesafe.ai/v1/systemone"
        );
        Ok(())
    }

    #[test]
    fn a_non_object_payload_is_carried_under_input() -> Result<(), ConfigError> {
        let endpoint = endpoint()?;
        let model = ModelName::default();
        let context = FailureContext {
            endpoint: &endpoint,
            model: &model,
        };
        let payload = error_payload(
            &json!("just a string"),
            &RequestFailure::Timeout { budget_ms: 120_000 },
            &context,
        );

        assert_eq!(payload["input"], "just a string");
        assert_eq!(payload["error"]["kind"], "timeout");
        assert_eq!(payload["error"]["status"], Value::Null);
        assert_eq!(payload["error"]["attempts"], Value::Null);
        Ok(())
    }

    #[test]
    fn an_inbound_error_key_loses_to_the_reserved_one() -> Result<(), ConfigError> {
        let endpoint = endpoint()?;
        let model = ModelName::default();
        let context = FailureContext {
            endpoint: &endpoint,
            model: &model,
        };
        let payload = error_payload(
            &json!({"error": "mine", "issue": 1}),
            &RequestFailure::Auth(http(401)),
            &context,
        );

        assert_eq!(payload["error"]["kind"], "auth");
        assert_eq!(payload["issue"], 1);
        Ok(())
    }

    /// One of every failure variant, for the tests that must cover them all.
    fn every_failure() -> Vec<RequestFailure> {
        vec![
            RequestFailure::StateNotFound {
                pointer: "/data".to_string(),
            },
            RequestFailure::Auth(http(401)),
            RequestFailure::InvalidRequest(http(422)),
            RequestFailure::RateLimited(http(429)),
            RequestFailure::ServerError(http(529)),
            RequestFailure::Transport {
                message: "connection reset".to_string(),
                attempts: 4,
                request_id: None,
            },
            RequestFailure::Timeout { budget_ms: 1 },
            RequestFailure::BadResponse {
                message: "not JSON".to_string(),
                body: "<html>".to_string(),
                request_id: None,
            },
            RequestFailure::AnswerContract {
                message: "no answer for question `kind`".to_string(),
                request_id: None,
            },
        ]
    }

    #[test]
    fn every_failure_variant_lands_under_its_own_kind() -> Result<(), ConfigError> {
        let endpoint = endpoint()?;
        let model = ModelName::default();
        let context = FailureContext {
            endpoint: &endpoint,
            model: &model,
        };

        let expected = [
            "state_not_found",
            "auth",
            "invalid_request",
            "rate_limited",
            "server_error",
            "transport",
            "timeout",
            "bad_response",
            "answer_contract",
        ];

        for (failure, kind) in every_failure().into_iter().zip(expected) {
            let payload = error_payload(&json!({}), &failure, &context);
            assert_eq!(payload["error"]["kind"], kind);
            assert!(payload["error"]["message"].is_string());
        }
        Ok(())
    }

    #[test]
    fn a_validation_body_surfaces_its_detail_as_structured_json() -> Result<(), ConfigError> {
        let endpoint = endpoint()?;
        let model = ModelName::default();
        let context = FailureContext {
            endpoint: &endpoint,
            model: &model,
        };
        let payload = error_payload(
            &json!({}),
            &RequestFailure::InvalidRequest(http(422)),
            &context,
        );

        assert_eq!(payload["error"]["detail"][0]["type"], "missing");
        assert_eq!(payload["error"]["detail"][0]["loc"][1], "questions");
        // The raw body is kept alongside the structured form.
        assert!(payload["error"]["body"].is_string());
        Ok(())
    }

    #[test]
    fn a_body_of_another_shape_yields_no_structured_detail() {
        assert_eq!(extract_detail("plain text"), None);
        assert_eq!(extract_detail(r#"{"message":"nope"}"#), None);
        assert_eq!(extract_detail(r#"{"detail":"a string"}"#), None);
        assert!(extract_detail(r#"{"detail":{"field":"questions"}}"#).is_some());
    }

    #[test]
    fn an_oversized_body_is_truncated_on_a_character_boundary() {
        let body = "é".repeat(MAX_BODY_BYTES);
        let truncated = truncate_body(&body);
        assert!(truncated.ends_with("… (truncated)"));
        assert!(truncated.len() < body.len());

        let short = "small";
        assert_eq!(truncate_body(short), short);
    }

    #[test]
    fn nothing_this_primitive_publishes_can_carry_the_api_key() -> Result<(), String> {
        // The credential is unreachable from here by construction: neither
        // payload function takes an `ApiKey` and no failure variant carries
        // one. This test is what starts failing if someone later widens
        // `FailureContext` or a failure variant to carry it.
        let endpoint = endpoint().map_err(|err| err.to_string())?;
        let model = ModelName::default();
        let context = FailureContext {
            endpoint: &endpoint,
            model: &model,
        };

        let mut published = success_payload(&json!({"issue": 1}), &sample_response()?).to_string();
        for failure in every_failure() {
            published
                .push_str(&error_payload(&json!({"issue": 1}), &failure, &context).to_string());
        }

        for forbidden in [
            "Bearer",
            "Authorization",
            "authorization",
            "api_key",
            "apiKey",
            API_KEY_VAR,
            API_KEY,
        ] {
            assert!(
                !published.contains(forbidden),
                "a published payload mentioned `{forbidden}`"
            );
        }
        Ok(())
    }
}
