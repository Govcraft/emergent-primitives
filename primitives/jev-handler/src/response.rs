//! Parsing and checking one API response.
//!
//! The response is validated *and* kept verbatim. Validation is what turns a
//! surprising body into a routable `bad_response` or `answer_contract` error
//! instead of a malformed success event; the verbatim copy is what downstream
//! `jq` routers select on, so `answers` must reach them in exactly the shape the
//! vendor documents.
//!
//! Only the questions file is parsed strictly. The response types tolerate
//! unknown extra fields, because the vendor may add one and dropping data the
//! server chose to send is worse than carrying it.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::RequestFailure;
use crate::questions::{QuestionId, QuestionKind, QuestionSet};

/// A number the API documents as lying in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Probability(f64);

impl Probability {
    /// The underlying value.
    #[must_use]
    pub fn value(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Probability {
    type Error = OutOfRange;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        if (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(OutOfRange { value })
        }
    }
}

/// How concentrated a choice or score distribution is.
///
/// A noul answer carries no confidence — it *is* a probability — which is why
/// this is a separate type rather than a field on every answer.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Confidence(f64);

impl Confidence {
    /// The underlying value.
    #[must_use]
    pub fn value(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Confidence {
    type Error = OutOfRange;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        if (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(OutOfRange { value })
        }
    }
}

/// A number the API sent outside the unit interval.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutOfRange {
    /// The offending value.
    pub value: f64,
}

/// One answer, carrying only what its type really has.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// A yes/no judgement expressed as the probability of yes.
    Noul {
        /// The probability the answer is yes.
        noul: Probability,
    },
    /// One option out of the asked set, with the full distribution.
    Choice {
        /// The highest-probability option; always one of the asked options.
        choice: String,
        /// How concentrated the distribution is.
        confidence: Confidence,
        /// Every option mapped to its probability.
        probabilities: BTreeMap<String, Probability>,
    },
    /// A probability-weighted position on the asked levels.
    Score {
        /// The weighted position; may land between levels, so it is not rounded.
        score: f64,
        /// How concentrated the distribution is.
        confidence: Confidence,
        /// Level index (as a string) to the description it was asked with.
        legend: BTreeMap<String, String>,
        /// Level index (as a string) to its probability.
        probabilities: BTreeMap<String, Probability>,
    },
}

/// A checked response, plus the `answers` object exactly as it arrived.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiResponse {
    /// The model that actually answered, e.g. `jev-1.13.0`.
    pub model: String,
    /// The typed answers, checked against the questions that were asked.
    pub answers: BTreeMap<QuestionId, Answer>,
    /// The `answers` object verbatim — what downstream routers select on.
    pub answers_raw: Value,
    /// Token usage, carried through untouched.
    pub usage: Value,
    /// The `x-typesafe-request-id` response header, when the server sent one.
    pub request_id: Option<String>,
}

/// Parse and check a 2xx response body against the questions that were asked.
///
/// # Errors
///
/// Returns [`RequestFailure::BadResponse`] when the body is not the documented
/// envelope, and [`RequestFailure::AnswerContract`] when it is well-formed but
/// answers something other than what was asked.
pub fn parse_response(
    body: &str,
    asked: &QuestionSet,
    request_id: Option<String>,
) -> Result<ApiResponse, RequestFailure> {
    let bad = |message: String| RequestFailure::BadResponse {
        message,
        body: body.to_string(),
        request_id: request_id.clone(),
    };

    let envelope: Value =
        serde_json::from_str(body).map_err(|err| bad(format!("response is not JSON: {err}")))?;

    let Value::Object(fields) = envelope else {
        return Err(bad("response is not a JSON object".to_string()));
    };

    let model = match fields.get("model") {
        Some(Value::String(model)) => model.clone(),
        _ => return Err(bad("response has no string `model`".to_string())),
    };

    let answers_raw = match fields.get("answers") {
        Some(answers @ Value::Object(_)) => answers.clone(),
        _ => return Err(bad("response has no `answers` object".to_string())),
    };

    // Usage is telemetry rather than contract, so an absent one is carried as
    // null rather than failing a message that was otherwise answered.
    let usage = fields.get("usage").cloned().unwrap_or(Value::Null);

    let answers = check_answers(&answers_raw, asked, request_id.as_deref())?;

    Ok(ApiResponse {
        model,
        answers,
        answers_raw,
        usage,
        request_id,
    })
}

/// Check every asked question was answered, and answered in kind.
fn check_answers(
    answers_raw: &Value,
    asked: &QuestionSet,
    request_id: Option<&str>,
) -> Result<BTreeMap<QuestionId, Answer>, RequestFailure> {
    let contract = |message: String| RequestFailure::AnswerContract {
        message,
        request_id: request_id.map(ToString::to_string),
    };

    let mut answers = BTreeMap::new();

    for (id, question) in asked.iter() {
        let Some(raw) = answers_raw.get(id.as_str()) else {
            return Err(contract(format!("no answer for question `{id}`")));
        };
        let answer =
            parse_answer(id, question.kind(), raw).map_err(|err| attribute(err, request_id))?;
        answers.insert(id.clone(), answer);
    }

    // Ids the server answered that were not asked are left in `answers_raw` and
    // reported, not dropped: discarding data the server chose to send hides a
    // contract change instead of surfacing it.
    if let Value::Object(fields) = answers_raw {
        for key in fields.keys() {
            if QuestionId::new(key)
                .ok()
                .is_none_or(|id| asked.get(&id).is_none())
            {
                tracing::warn!("response answered `{key}`, which was not asked");
            }
        }
    }

    Ok(answers)
}

/// Stamp the vendor's request id onto a per-answer failure.
///
/// `parse_answer` works on one answer and does not know the response it came
/// from, so the id is attached here rather than threaded through every check.
fn attribute(failure: RequestFailure, request_id: Option<&str>) -> RequestFailure {
    match failure {
        RequestFailure::AnswerContract { message, .. } => RequestFailure::AnswerContract {
            message,
            request_id: request_id.map(ToString::to_string),
        },
        other => other,
    }
}

/// Turn one raw answer into its typed form, or say exactly what broke.
///
/// The declared `type` is checked against both the question that was asked and
/// the value field that is actually populated. A `#[serde(tag = "type")]` enum
/// would reject the same bodies with a far less useful message.
fn parse_answer(
    id: &QuestionId,
    asked: &QuestionKind,
    raw: &Value,
) -> Result<Answer, RequestFailure> {
    let contract = |message: String| RequestFailure::AnswerContract {
        message,
        request_id: None,
    };

    let Value::Object(fields) = raw else {
        return Err(contract(format!("answer `{id}` is not an object")));
    };

    let declared = match fields.get("type") {
        Some(Value::String(declared)) => declared.as_str(),
        _ => return Err(contract(format!("answer `{id}` has no string `type`"))),
    };

    if declared != asked.type_name() {
        return Err(contract(format!(
            "answer `{id}` declared type=`{declared}` but the question asked for `{}`",
            asked.type_name()
        )));
    }

    match asked {
        QuestionKind::Noul { .. } => Ok(Answer::Noul {
            noul: probability(id, "noul", number(id, fields, "noul")?)?,
        }),
        QuestionKind::Choice { criteria } => {
            let choice = match fields.get("choice") {
                Some(Value::String(choice)) => choice.clone(),
                _ => {
                    return Err(contract(format!(
                        "answer `{id}` declared type=choice but carried no string `choice`"
                    )));
                }
            };

            if !criteria.contains_key(&choice) {
                return Err(contract(format!(
                    "answer `{id}` chose `{choice}`, which is not one of the options it was asked about"
                )));
            }

            Ok(Answer::Choice {
                choice,
                confidence: confidence(id, number(id, fields, "confidence")?)?,
                probabilities: probabilities(id, fields)?,
            })
        }
        QuestionKind::Score { criteria } => {
            let score = number(id, fields, "score")?;
            let highest = match u32::try_from(criteria.len()) {
                Ok(levels) => f64::from(levels.saturating_sub(1)),
                Err(_) => f64::INFINITY,
            };

            if !score.is_finite() || !(0.0..=highest).contains(&score) {
                return Err(contract(format!(
                    "answer `{id}` scored {score}, outside the 0..={highest} range its levels define"
                )));
            }

            Ok(Answer::Score {
                score,
                confidence: confidence(id, number(id, fields, "confidence")?)?,
                legend: legend(id, fields)?,
                probabilities: probabilities(id, fields)?,
            })
        }
    }
}

/// Read a required numeric field.
fn number(
    id: &QuestionId,
    fields: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<f64, RequestFailure> {
    fields
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| RequestFailure::AnswerContract {
            message: format!("answer `{id}` has no numeric `{key}`"),
            request_id: None,
        })
}

/// Check one unit-interval probability, naming the field it came from.
fn probability(id: &QuestionId, key: &str, value: f64) -> Result<Probability, RequestFailure> {
    Probability::try_from(value).map_err(|err| RequestFailure::AnswerContract {
        message: format!("answer `{id}` has `{key}` = {}, outside 0..=1", err.value),
        request_id: None,
    })
}

/// Check the confidence field.
fn confidence(id: &QuestionId, value: f64) -> Result<Confidence, RequestFailure> {
    Confidence::try_from(value).map_err(|err| RequestFailure::AnswerContract {
        message: format!(
            "answer `{id}` has `confidence` = {}, outside 0..=1",
            err.value
        ),
        request_id: None,
    })
}

/// Read the probability distribution, checking every entry.
fn probabilities(
    id: &QuestionId,
    fields: &serde_json::Map<String, Value>,
) -> Result<BTreeMap<String, Probability>, RequestFailure> {
    let Some(Value::Object(entries)) = fields.get("probabilities") else {
        return Err(RequestFailure::AnswerContract {
            message: format!("answer `{id}` has no `probabilities` object"),
            request_id: None,
        });
    };

    let mut distribution = BTreeMap::new();
    for (key, value) in entries {
        let Some(number) = value.as_f64() else {
            return Err(RequestFailure::AnswerContract {
                message: format!("answer `{id}` has a non-numeric probability for `{key}`"),
                request_id: None,
            });
        };
        distribution.insert(key.clone(), probability(id, key, number)?);
    }

    Ok(distribution)
}

/// Read the level legend a score answer carries.
fn legend(
    id: &QuestionId,
    fields: &serde_json::Map<String, Value>,
) -> Result<BTreeMap<String, String>, RequestFailure> {
    let Some(Value::Object(entries)) = fields.get("legend") else {
        return Err(RequestFailure::AnswerContract {
            message: format!("answer `{id}` has no `legend` object"),
            request_id: None,
        });
    };

    let mut levels = BTreeMap::new();
    for (key, value) in entries {
        let Some(text) = value.as_str() else {
            return Err(RequestFailure::AnswerContract {
                message: format!("answer `{id}` has a non-string legend entry for `{key}`"),
                request_id: None,
            });
        };
        levels.insert(key.clone(), text.to_string());
    }

    Ok(levels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::questions::{QuestionsFormat, parse_questions};

    const QUESTIONS: &str = r#"
[questions.lure]
type = "noul"
instructions = "Is there a lure?"

[questions.kind]
type = "choice"
instructions = "What kind?"
criteria = { vendor_notice = "a", cold_pitch = "b", phish = "c", personal = "d" }

[questions.pressure]
type = "score"
instructions = "How much pressure?"
criteria = ["No deadline mentioned.", "A soft deadline.", "Act now."]
"#;

    /// Captured verbatim from the live API.
    const SAMPLE_BODY: &str = r#"{"model":"jev-1.13.0",
     "answers":{
       "lure":     {"type":"noul","noul":0.93},
       "kind":     {"type":"choice","choice":"phish","confidence":0.99,
                    "probabilities":{"vendor_notice":0.01,"cold_pitch":0.0,"phish":0.99,"personal":0.0}},
       "pressure": {"type":"score","score":2.0,"confidence":1.0,
                    "legend":{"0":"No deadline mentioned.","1":"...","2":"..."},
                    "probabilities":{"0":0.0,"1":0.0,"2":1.0}}},
     "usage":{"input_tokens":391,"output_tokens":34}}"#;

    /// The test question set, rendered as a `String` error so tests can `?`.
    fn asked() -> Result<QuestionSet, String> {
        parse_questions(QUESTIONS, QuestionsFormat::Toml).map_err(|err| err.to_string())
    }

    /// A response that is expected to parse.
    fn parsed(body: &str) -> Result<ApiResponse, String> {
        parse_response(body, &asked()?, None).map_err(|err| err.to_string())
    }

    /// The rejection message for a response that is expected to fail.
    fn failure(body: &str) -> Result<String, String> {
        match parse_response(body, &asked()?, None) {
            Err(err) => Ok(err.to_string()),
            Ok(_) => Ok("unexpectedly accepted the response".to_string()),
        }
    }

    /// A question id, rendered as a `String` error so tests can `?`.
    fn id(name: &str) -> Result<QuestionId, String> {
        QuestionId::new(name).map_err(|err| err.to_string())
    }

    #[test]
    fn the_live_sample_parses_into_all_three_answer_shapes() -> Result<(), String> {
        let response = parse_response(SAMPLE_BODY, &asked()?, Some("req_1".to_string()))
            .map_err(|err| err.to_string())?;

        assert_eq!(response.model, "jev-1.13.0");
        assert_eq!(response.request_id.as_deref(), Some("req_1"));
        assert_eq!(response.usage["input_tokens"], 391);
        assert_eq!(response.answers.len(), 3);

        assert!(matches!(
            response.answers.get(&id("lure")?),
            Some(Answer::Noul { noul }) if (noul.value() - 0.93).abs() < f64::EPSILON
        ));
        assert!(matches!(
            response.answers.get(&id("kind")?),
            Some(Answer::Choice { choice, confidence, probabilities })
                if choice == "phish"
                    && (confidence.value() - 0.99).abs() < f64::EPSILON
                    && probabilities.len() == 4
        ));
        assert!(matches!(
            response.answers.get(&id("pressure")?),
            Some(Answer::Score { score, legend, .. })
                if (*score - 2.0).abs() < f64::EPSILON && legend.len() == 3
        ));
        Ok(())
    }

    #[test]
    fn the_answers_object_survives_parsing_byte_for_byte() -> Result<(), String> {
        let verbatim: Value = serde_json::from_str(SAMPLE_BODY).map_err(|e| e.to_string())?;
        assert_eq!(parsed(SAMPLE_BODY)?.answers_raw, verbatim["answers"]);
        Ok(())
    }

    #[test]
    fn a_score_between_levels_is_not_rounded() -> Result<(), String> {
        let body = SAMPLE_BODY.replace(r#""score":2.0"#, r#""score":1.6"#);
        assert!(matches!(
            parsed(&body)?.answers.get(&id("pressure")?),
            Some(Answer::Score { score, .. }) if (*score - 1.6).abs() < f64::EPSILON
        ));
        Ok(())
    }

    #[test]
    fn unknown_extra_fields_are_tolerated_and_carried_through() -> Result<(), String> {
        let body = SAMPLE_BODY.replace(r#""noul":0.93"#, r#""noul":0.93,"rationale":"new field""#);
        assert_eq!(parsed(&body)?.answers_raw["lure"]["rationale"], "new field");
        Ok(())
    }

    #[test]
    fn an_unanswered_question_names_itself() -> Result<(), String> {
        let body = r#"{"model":"jev-1","answers":{},"usage":{}}"#;
        assert!(failure(body)?.contains("no answer for question `kind`"));
        Ok(())
    }

    #[test]
    fn an_extra_answer_is_retained_rather_than_rejected() -> Result<(), String> {
        let body = SAMPLE_BODY.replace(
            r#""lure":     {"type":"noul","noul":0.93},"#,
            r#""lure": {"type":"noul","noul":0.93}, "surprise": {"type":"noul","noul":0.1},"#,
        );
        let response = parsed(&body)?;
        assert_eq!(response.answers.len(), 3);
        assert_eq!(response.answers_raw["surprise"]["noul"], 0.1);
        Ok(())
    }

    #[test]
    fn a_declared_type_that_carries_no_value_field_names_the_question() -> Result<(), String> {
        let body = SAMPLE_BODY.replace(r#""score":2.0,"#, "");
        let message = failure(&body)?;
        assert!(
            message.contains("answer `pressure` has no numeric `score`"),
            "{message}"
        );
        Ok(())
    }

    #[test]
    fn a_type_that_disagrees_with_the_question_is_rejected() -> Result<(), String> {
        let body = SAMPLE_BODY.replace(
            r#""lure":     {"type":"noul""#,
            r#""lure": {"type":"choice""#,
        );
        let message = failure(&body)?;
        assert!(message.contains("declared type=`choice`"), "{message}");
        assert!(message.contains("asked for `noul`"), "{message}");
        Ok(())
    }

    #[test]
    fn a_probability_outside_the_unit_interval_names_the_question() -> Result<(), String> {
        for value in ["1.5", "-0.1"] {
            let body = SAMPLE_BODY.replace(r#""noul":0.93"#, &format!(r#""noul":{value}"#));
            let message = failure(&body)?;
            assert!(message.contains("`lure`"), "{message}");
            assert!(message.contains("outside 0..=1"), "{message}");
        }
        Ok(())
    }

    #[test]
    fn a_confidence_outside_the_unit_interval_names_the_question() -> Result<(), String> {
        let body = SAMPLE_BODY.replace(r#""confidence":0.99"#, r#""confidence":1.5"#);
        let message = failure(&body)?;
        assert!(message.contains("`kind`"), "{message}");
        assert!(message.contains("`confidence`"), "{message}");
        Ok(())
    }

    #[test]
    fn a_score_beyond_the_highest_level_is_rejected() -> Result<(), String> {
        let body = SAMPLE_BODY.replace(r#""score":2.0"#, r#""score":3.5"#);
        let message = failure(&body)?;
        assert!(message.contains("outside the 0..=2 range"), "{message}");
        Ok(())
    }

    #[test]
    fn a_choice_outside_the_asked_options_is_rejected() -> Result<(), String> {
        let body = SAMPLE_BODY.replace(r#""choice":"phish""#, r#""choice":"smishing""#);
        let message = failure(&body)?;
        assert!(message.contains("not one of the options"), "{message}");
        Ok(())
    }

    #[test]
    fn a_missing_distribution_is_rejected() -> Result<(), String> {
        let body = SAMPLE_BODY.replace(
            r#""probabilities":{"vendor_notice":0.01,"cold_pitch":0.0,"phish":0.99,"personal":0.0}"#,
            r#""unused":0"#,
        );
        assert!(failure(&body)?.contains("no `probabilities` object"));
        Ok(())
    }

    #[test]
    fn a_missing_legend_is_rejected() -> Result<(), String> {
        let body = SAMPLE_BODY.replace(
            r#""legend":{"0":"No deadline mentioned.","1":"...","2":"..."},"#,
            "",
        );
        assert!(failure(&body)?.contains("no `legend` object"));
        Ok(())
    }

    #[test]
    fn a_body_that_is_not_the_documented_envelope_is_a_bad_response() -> Result<(), String> {
        assert!(failure("not json at all")?.contains("response is not JSON"));
        assert!(failure("[1, 2, 3]")?.contains("not a JSON object"));
        assert!(failure(r#"{"answers":{}}"#)?.contains("no string `model`"));
        assert!(failure(r#"{"model":"jev-1"}"#)?.contains("no `answers` object"));
        Ok(())
    }

    #[test]
    fn an_absent_usage_block_is_carried_as_null() -> Result<(), String> {
        let body = r#"{"model":"jev-1","answers":{"lure":{"type":"noul","noul":0.5},
            "kind":{"type":"choice","choice":"phish","confidence":0.5,"probabilities":{"phish":1.0}},
            "pressure":{"type":"score","score":0.0,"confidence":0.5,"legend":{"0":"a"},"probabilities":{"0":1.0}}}}"#;
        assert_eq!(parsed(body)?.usage, Value::Null);
        Ok(())
    }

    #[test]
    fn the_unit_interval_newtypes_reject_what_is_outside_it() {
        assert!(Probability::try_from(0.0).is_ok());
        assert!(Probability::try_from(1.0).is_ok());
        assert!(Probability::try_from(1.000_001).is_err());
        assert!(Probability::try_from(f64::NAN).is_err());
        assert!(Confidence::try_from(-0.000_001).is_err());
    }
}
