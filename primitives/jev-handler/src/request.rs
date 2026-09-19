//! Building the one request body, from the inbound payload and the questions.
//!
//! Pure: given a payload, a pointer, a model, and a question set, the body is
//! fully determined. Nothing here touches the network.

use serde_json::{Value, json};

use crate::args::{ModelName, StatePointer};
use crate::error::RequestFailure;
use crate::questions::QuestionSet;

/// Select the part of the payload that becomes the request's `state`.
///
/// # Errors
///
/// Returns [`RequestFailure::StateNotFound`] when the pointer does not resolve.
/// This is deliberately a per-message failure rather than a startup one: a
/// pointer can be valid and one message merely shaped differently, and that
/// message deserves an error event rather than taking the handler down.
pub fn select_state<'a>(
    payload: &'a Value,
    pointer: &StatePointer,
) -> Result<&'a Value, RequestFailure> {
    pointer
        .resolve(payload)
        .ok_or_else(|| RequestFailure::StateNotFound {
            pointer: pointer.as_str().to_string(),
        })
}

/// Assemble the request body in the shape the API documents.
///
/// `questions` is a map keyed by question id, not an array; the answers come
/// back under the same keys.
#[must_use]
pub fn build_request_body(state: &Value, model: &ModelName, questions: &QuestionSet) -> Value {
    json!({
        "state": state,
        "model": model.as_str(),
        "questions": questions.to_wire(),
    })
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
criteria = { phish = "a", personal = "b" }

[questions.pressure]
type = "score"
instructions = "How much pressure?"
criteria = ["none", "some", "lots"]
"#;

    fn asked() -> Result<QuestionSet, String> {
        parse_questions(QUESTIONS, QuestionsFormat::Toml).map_err(|err| err.to_string())
    }

    fn pointer(value: &str) -> Result<StatePointer, String> {
        value
            .parse()
            .map_err(|err: crate::error::ConfigError| err.to_string())
    }

    #[test]
    fn the_body_carries_state_model_and_one_wire_question_each() -> Result<(), String> {
        let state = json!({"subject": "Action required"});
        let body = build_request_body(&state, &ModelName::default(), &asked()?);

        assert_eq!(body["state"], state);
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["questions"]["lure"]["type"], "noul");
        assert_eq!(body["questions"]["kind"]["type"], "choice");
        assert_eq!(body["questions"]["pressure"]["type"], "score");
        assert_eq!(
            body["questions"]["pressure"]["criteria"],
            json!(["none", "some", "lots"])
        );
        Ok(())
    }

    #[test]
    fn an_empty_pointer_sends_the_whole_payload_as_state() -> Result<(), String> {
        let payload = json!({"data": {"item": 7}, "issue": 42});
        let state = select_state(&payload, &pointer("")?).map_err(|err| err.to_string())?;
        assert_eq!(state, &payload);
        Ok(())
    }

    #[test]
    fn a_pointer_selects_a_sub_tree_as_state() -> Result<(), String> {
        let payload = json!({"data": {"item": "the text"}});
        let state =
            select_state(&payload, &pointer("/data/item")?).map_err(|err| err.to_string())?;
        assert_eq!(state, &json!("the text"));
        Ok(())
    }

    #[test]
    fn a_pointer_that_misses_is_a_per_message_failure() -> Result<(), String> {
        let payload = json!({"other": 1});
        let missing = pointer("/data/item")?;
        let failure = select_state(&payload, &missing);
        assert_eq!(
            failure.err(),
            Some(RequestFailure::StateNotFound {
                pointer: "/data/item".to_string()
            })
        );
        Ok(())
    }

    #[test]
    fn a_non_object_payload_can_still_be_the_whole_state() -> Result<(), String> {
        let payload = json!("just a string");
        let state = select_state(&payload, &pointer("")?).map_err(|err| err.to_string())?;
        assert_eq!(state, &json!("just a string"));
        Ok(())
    }
}
