//! The questions file: parse, validate, and render to the wire shape.
//!
//! The file mirrors the TypeSafe request's `questions` map one-to-one — same
//! `type`, `instructions`, and `criteria` fields — so an operator reading the
//! vendor documentation is reading this file format too.
//!
//! Everything here is pure. The file is read once at startup and a malformed
//! question is a startup error, never a runtime surprise: the same fixed set is
//! asked about every message, so a typo that reaches inference would be wrong
//! for every message rather than one.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ConfigError;

/// Which front-end parses the questions file.
///
/// Both produce the same `serde_json::Value` and then the same validated
/// [`QuestionSet`], so the two formats cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionsFormat {
    /// A `.toml` file.
    Toml,
    /// A `.json` file.
    Json,
}

/// A question id: the map key the matching answer comes back under.
///
/// Restricted to `[A-Za-z_][A-Za-z0-9_]*` on purpose. Routing over the answers
/// happens downstream in `jq` selectors, and an id containing a `.` or a `-`
/// forces `.answers["my-id"]` on every router someone writes. Enforcing
/// jq-bareword safety at startup is cheaper than debugging it in a topology.
///
/// The id is never sent to the model; it is a key for code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct QuestionId(String);

impl QuestionId {
    /// Validate and wrap an id.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::QuestionsInvalid`] when the id is empty or is not
    /// a jq bareword.
    pub fn new(id: &str) -> Result<Self, ConfigError> {
        let mut chars = id.chars();
        let valid = match chars.next() {
            Some(first) if first.is_ascii_alphabetic() || first == '_' => {
                chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            }
            _ => false,
        };

        if valid {
            Ok(Self(id.to_string()))
        } else {
            Err(ConfigError::QuestionsInvalid {
                detail: format!(
                    "question id `{id}` must match [A-Za-z_][A-Za-z0-9_]* so downstream jq routers can write .answers.{id}"
                ),
            })
        }
    }

    /// The id as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for QuestionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The three System One primitives, with the criteria each one takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionKind {
    /// A yes/no judgement; the answer is a bare probability.
    Noul {
        /// Optional descriptions of what a yes and a no mean, keyed `true` and
        /// `false`.
        criteria: Option<BTreeMap<String, String>>,
    },
    /// One option out of a defined set.
    Choice {
        /// Option to rubric description; at least two options.
        criteria: BTreeMap<String, String>,
    },
    /// A position along an ordered rubric.
    Score {
        /// Ordered level descriptions; at least two, and order is the meaning.
        criteria: Vec<String>,
    },
}

impl QuestionKind {
    /// The `type` string this kind is written and answered as.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Noul { .. } => "noul",
            Self::Choice { .. } => "choice",
            Self::Score { .. } => "score",
        }
    }
}

/// One validated question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// What the model is asked; a string, or structured JSON when definitions
    /// and contrasts need room.
    instructions: Value,
    /// The primitive and its criteria.
    kind: QuestionKind,
}

impl Question {
    /// The primitive this question asks for.
    #[must_use]
    pub fn kind(&self) -> &QuestionKind {
        &self.kind
    }

    /// Render the question in the shape the API expects.
    #[must_use]
    pub fn to_wire(&self) -> Value {
        let mut wire = json!({
            "type": self.kind.type_name(),
            "instructions": self.instructions,
        });

        let criteria = match &self.kind {
            QuestionKind::Noul { criteria } => criteria.as_ref().map(|c| json!(c)),
            QuestionKind::Choice { criteria } => Some(json!(criteria)),
            QuestionKind::Score { criteria } => Some(json!(criteria)),
        };

        if let (Some(criteria), Value::Object(fields)) = (criteria, &mut wire) {
            fields.insert("criteria".to_string(), criteria);
        }

        wire
    }
}

/// The fixed set of questions asked about every message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionSet {
    questions: BTreeMap<QuestionId, Question>,
}

impl QuestionSet {
    /// The questions, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (&QuestionId, &Question)> {
        self.questions.iter()
    }

    /// Look one question up by id.
    #[must_use]
    pub fn get(&self, id: &QuestionId) -> Option<&Question> {
        self.questions.get(id)
    }

    /// How many questions are asked per message.
    #[must_use]
    pub fn len(&self) -> usize {
        self.questions.len()
    }

    /// Always false: a validated set has at least one question.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.questions.is_empty()
    }

    /// Render the whole set as the request's `questions` map.
    #[must_use]
    pub fn to_wire(&self) -> Value {
        let map: serde_json::Map<String, Value> = self
            .questions
            .iter()
            .map(|(id, question)| (id.as_str().to_string(), question.to_wire()))
            .collect();
        Value::Object(map)
    }
}

/// The file as written, before any meaning is attached to it.
///
/// `deny_unknown_fields` is load-bearing: a typo like `critera` must be a
/// startup error rather than a silently ignored key that yields a semantically
/// wrong question on every message.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawQuestionsFile {
    questions: BTreeMap<String, RawQuestion>,
}

/// One question as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawQuestion {
    #[serde(rename = "type")]
    kind: String,
    instructions: Value,
    #[serde(default)]
    criteria: Option<Value>,
}

/// Pick the parser from the file extension.
///
/// Keying off the extension rather than sniffing the contents is deliberate: a
/// predictable error beats a clever guess when the operator has mistyped a path.
///
/// # Errors
///
/// Returns [`ConfigError::QuestionsFormat`] for any other extension.
pub fn detect_format(path: &Path) -> Result<QuestionsFormat, ConfigError> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("toml") => Ok(QuestionsFormat::Toml),
        Some("json") => Ok(QuestionsFormat::Json),
        _ => Err(ConfigError::QuestionsFormat {
            path: path.to_path_buf(),
        }),
    }
}

/// Read, parse, and validate the questions file at `path`.
///
/// # Errors
///
/// Returns a [`ConfigError`] for an unreadable file, an unsupported extension,
/// a syntax error, or any broken question.
pub fn load_questions(path: &Path) -> Result<QuestionSet, ConfigError> {
    let format = detect_format(path)?;
    let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::QuestionsRead {
        path: path.to_path_buf(),
        source,
    })?;
    parse_questions(&contents, format).map_err(|err| match err {
        // The parse front-ends do not know which file they were handed.
        ConfigError::QuestionsParse { source, .. } => ConfigError::QuestionsParse {
            path: path.to_path_buf(),
            source,
        },
        other => other,
    })
}

/// Parse and validate questions file contents.
///
/// # Errors
///
/// Returns [`ConfigError::QuestionsParse`] for a syntax error and
/// [`ConfigError::QuestionsInvalid`] for anything that parses but breaks the
/// question contract.
pub fn parse_questions(
    contents: &str,
    format: QuestionsFormat,
) -> Result<QuestionSet, ConfigError> {
    let value = to_value(contents, format)?;

    let file: RawQuestionsFile =
        serde_json::from_value(value).map_err(|err| ConfigError::QuestionsInvalid {
            detail: err.to_string(),
        })?;

    validate(file)
}

/// Run the format's own parser and hand back one common representation.
fn to_value(contents: &str, format: QuestionsFormat) -> Result<Value, ConfigError> {
    let parsed = match format {
        QuestionsFormat::Toml => toml::from_str::<Value>(contents)
            .map_err(|err| -> Box<dyn std::error::Error + Send + Sync> { Box::new(err) }),
        QuestionsFormat::Json => serde_json::from_str::<Value>(contents)
            .map_err(|err| -> Box<dyn std::error::Error + Send + Sync> { Box::new(err) }),
    };

    parsed.map_err(|source| ConfigError::QuestionsParse {
        path: std::path::PathBuf::new(),
        source,
    })
}

/// Turn a parsed file into a [`QuestionSet`], or say precisely what is wrong.
fn validate(file: RawQuestionsFile) -> Result<QuestionSet, ConfigError> {
    if file.questions.is_empty() {
        return Err(ConfigError::QuestionsInvalid {
            detail: "no questions defined; at least one is required".to_string(),
        });
    }

    let mut questions = BTreeMap::new();

    for (raw_id, raw) in file.questions {
        let id = QuestionId::new(&raw_id)?;
        let instructions = validate_instructions(&id, raw.instructions)?;
        let kind = validate_kind(&id, &raw.kind, raw.criteria)?;
        questions.insert(id, Question { instructions, kind });
    }

    Ok(QuestionSet { questions })
}

/// `instructions` carries the entire meaning of the question, so an empty one
/// asks the model nothing.
fn validate_instructions(id: &QuestionId, instructions: Value) -> Result<Value, ConfigError> {
    let ok = match &instructions {
        Value::String(text) => !text.trim().is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        Value::Array(items) => !items.is_empty(),
        _ => false,
    };

    if ok {
        Ok(instructions)
    } else {
        Err(ConfigError::QuestionsInvalid {
            detail: format!(
                "question `{id}` needs non-empty `instructions` (a string, object, or array)"
            ),
        })
    }
}

/// Check the `type` against the criteria it was given.
fn validate_kind(
    id: &QuestionId,
    kind: &str,
    criteria: Option<Value>,
) -> Result<QuestionKind, ConfigError> {
    match kind {
        "noul" => Ok(QuestionKind::Noul {
            criteria: criteria
                .map(|c| validate_noul_criteria(id, c))
                .transpose()?,
        }),
        "choice" => Ok(QuestionKind::Choice {
            criteria: validate_choice_criteria(id, require_criteria(id, criteria, "choice")?)?,
        }),
        "score" => Ok(QuestionKind::Score {
            criteria: validate_score_criteria(id, require_criteria(id, criteria, "score")?)?,
        }),
        other => Err(ConfigError::QuestionsInvalid {
            detail: format!(
                "question `{id}` has unknown type `{other}`; expected noul | choice | score"
            ),
        }),
    }
}

/// Choice and score both make no sense without criteria.
fn require_criteria(
    id: &QuestionId,
    criteria: Option<Value>,
    kind: &str,
) -> Result<Value, ConfigError> {
    criteria.ok_or_else(|| ConfigError::QuestionsInvalid {
        detail: format!("question `{id}` is a {kind} question and needs `criteria`"),
    })
}

/// A noul's criteria describe the two poles, so `true` and `false` are the only
/// keys the API gives meaning to; anything else would be silently ignored.
fn validate_noul_criteria(
    id: &QuestionId,
    criteria: Value,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let entries = criteria_map(id, criteria, "noul")?;

    if entries.is_empty() {
        return Err(ConfigError::QuestionsInvalid {
            detail: format!(
                "question `{id}` has an empty `criteria`; omit it or describe `true` and/or `false`"
            ),
        });
    }

    for key in entries.keys() {
        if key != "true" && key != "false" {
            return Err(ConfigError::QuestionsInvalid {
                detail: format!(
                    "question `{id}` has noul criteria key `{key}`; only `true` and `false` are recognised"
                ),
            });
        }
    }

    Ok(entries)
}

/// A one-option choice is not a choice.
fn validate_choice_criteria(
    id: &QuestionId,
    criteria: Value,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let entries = criteria_map(id, criteria, "choice")?;

    if entries.len() < 2 {
        return Err(ConfigError::QuestionsInvalid {
            detail: format!(
                "question `{id}` has {} choice option(s); at least 2 are required",
                entries.len()
            ),
        });
    }

    Ok(entries)
}

/// The order of the levels *is* the scale, so they are a list rather than a map.
fn validate_score_criteria(id: &QuestionId, criteria: Value) -> Result<Vec<String>, ConfigError> {
    let Value::Array(items) = criteria else {
        return Err(ConfigError::QuestionsInvalid {
            detail: format!(
                "question `{id}` needs `criteria` as an ordered array of level descriptions"
            ),
        });
    };

    let mut levels = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::String(text) if !text.trim().is_empty() => levels.push(text),
            _ => {
                return Err(ConfigError::QuestionsInvalid {
                    detail: format!("question `{id}` has an empty or non-string score level"),
                });
            }
        }
    }

    if levels.len() < 2 {
        return Err(ConfigError::QuestionsInvalid {
            detail: format!(
                "question `{id}` has {} score level(s); at least 2 are required",
                levels.len()
            ),
        });
    }

    let mut seen = levels.clone();
    seen.sort_unstable();
    seen.dedup();
    if seen.len() != levels.len() {
        return Err(ConfigError::QuestionsInvalid {
            detail: format!("question `{id}` repeats a score level; each level must be distinct"),
        });
    }

    Ok(levels)
}

/// Shared shape check for the two map-valued criteria forms.
fn criteria_map(
    id: &QuestionId,
    criteria: Value,
    kind: &str,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let Value::Object(fields) = criteria else {
        return Err(ConfigError::QuestionsInvalid {
            detail: format!(
                "question `{id}` needs `criteria` as a {kind} option-to-description map"
            ),
        });
    };

    let mut entries = BTreeMap::new();
    for (key, value) in fields {
        match value {
            Value::String(text) if !text.trim().is_empty() => {
                entries.insert(key, text);
            }
            _ => {
                return Err(ConfigError::QuestionsInvalid {
                    detail: format!(
                        "question `{id}` has an empty or non-string description for criteria key `{key}`"
                    ),
                });
            }
        }
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_TOML: &str = r#"
[questions.lure]
type = "noul"
instructions = "Does the message try to get the reader to click something?"
criteria = { "true" = "There is a link or attachment to act on", "false" = "Nothing to act on" }

[questions.kind]
type = "choice"
instructions = "What kind of message is this?"
criteria = { phish = "Credential theft", cold_pitch = "Unsolicited sales" }

[questions.pressure]
type = "score"
instructions = "How much time pressure does the message apply?"
criteria = ["No deadline mentioned", "A soft deadline", "Act now or lose access"]
"#;

    const VALID_JSON: &str = r#"
{
  "questions": {
    "lure": {
      "type": "noul",
      "instructions": "Does the message try to get the reader to click something?",
      "criteria": { "true": "There is a link or attachment to act on", "false": "Nothing to act on" }
    },
    "kind": {
      "type": "choice",
      "instructions": "What kind of message is this?",
      "criteria": { "phish": "Credential theft", "cold_pitch": "Unsolicited sales" }
    },
    "pressure": {
      "type": "score",
      "instructions": "How much time pressure does the message apply?",
      "criteria": ["No deadline mentioned", "A soft deadline", "Act now or lose access"]
    }
  }
}
"#;

    fn toml_err(contents: &str) -> String {
        match parse_questions(contents, QuestionsFormat::Toml) {
            Err(err) => err.to_string(),
            Ok(set) => format!("unexpectedly accepted {} question(s)", set.len()),
        }
    }

    #[test]
    fn a_valid_toml_file_yields_all_three_kinds() -> Result<(), ConfigError> {
        let set = parse_questions(VALID_TOML, QuestionsFormat::Toml)?;
        assert_eq!(set.len(), 3);

        let lure = QuestionId::new("lure")?;
        assert!(matches!(
            set.get(&lure).map(Question::kind),
            Some(QuestionKind::Noul { criteria: Some(_) })
        ));
        assert!(matches!(
            set.get(&QuestionId::new("kind")?).map(Question::kind),
            Some(QuestionKind::Choice { .. })
        ));
        assert!(matches!(
            set.get(&QuestionId::new("pressure")?).map(Question::kind),
            Some(QuestionKind::Score { .. })
        ));
        Ok(())
    }

    #[test]
    fn toml_and_json_front_ends_converge_on_the_same_set() -> Result<(), ConfigError> {
        let from_toml = parse_questions(VALID_TOML, QuestionsFormat::Toml)?;
        let from_json = parse_questions(VALID_JSON, QuestionsFormat::Json)?;
        assert_eq!(from_toml, from_json);
        Ok(())
    }

    #[test]
    fn the_wire_shape_is_a_map_keyed_by_question_id() -> Result<(), ConfigError> {
        let set = parse_questions(VALID_TOML, QuestionsFormat::Toml)?;
        assert_eq!(
            set.to_wire(),
            json!({
                "lure": {
                    "type": "noul",
                    "instructions": "Does the message try to get the reader to click something?",
                    "criteria": { "true": "There is a link or attachment to act on", "false": "Nothing to act on" }
                },
                "kind": {
                    "type": "choice",
                    "instructions": "What kind of message is this?",
                    "criteria": { "phish": "Credential theft", "cold_pitch": "Unsolicited sales" }
                },
                "pressure": {
                    "type": "score",
                    "instructions": "How much time pressure does the message apply?",
                    "criteria": ["No deadline mentioned", "A soft deadline", "Act now or lose access"]
                }
            })
        );
        Ok(())
    }

    #[test]
    fn a_noul_without_criteria_omits_the_key_entirely() -> Result<(), ConfigError> {
        let set = parse_questions(
            r#"
[questions.urgent]
type = "noul"
instructions = "Is this urgent?"
"#,
            QuestionsFormat::Toml,
        )?;
        assert_eq!(
            set.to_wire(),
            json!({"urgent": {"type": "noul", "instructions": "Is this urgent?"}})
        );
        Ok(())
    }

    #[test]
    fn structured_instructions_are_carried_through() -> Result<(), ConfigError> {
        let set = parse_questions(
            r#"
{"questions": {"q": {"type": "noul", "instructions": {"ask": "Is it urgent?", "note": "ignore tone"}}}}
"#,
            QuestionsFormat::Json,
        )?;
        assert_eq!(
            set.to_wire()["q"]["instructions"],
            json!({"ask": "Is it urgent?", "note": "ignore tone"})
        );
        Ok(())
    }

    #[test]
    fn an_empty_question_map_is_rejected() {
        assert!(toml_err("questions = {}").contains("at least one"));
    }

    #[test]
    fn an_id_that_is_not_a_jq_bareword_is_rejected() {
        for id in ["my-id", "my.id", "my id", "1st", ""] {
            let contents =
                format!("[questions.\"{id}\"]\ntype = \"noul\"\ninstructions = \"Is it?\"\n");
            assert!(
                toml_err(&contents).contains("jq"),
                "id `{id}` should have been rejected"
            );
        }
    }

    #[test]
    fn a_duplicate_id_is_a_toml_syntax_error() {
        // Ids are map keys, so a duplicate cannot be represented; TOML rejects
        // the redefinition outright rather than letting the last one win.
        let contents = r#"
[questions.q]
type = "noul"
instructions = "Is it?"

[questions.q]
type = "noul"
instructions = "Is it again?"
"#;
        assert!(toml_err(contents).contains("cannot parse"));
    }

    #[test]
    fn empty_instructions_are_rejected() {
        let contents = "[questions.q]\ntype = \"noul\"\ninstructions = \"   \"\n";
        assert!(toml_err(contents).contains("non-empty `instructions`"));
    }

    #[test]
    fn a_choice_needs_at_least_two_options() {
        let one = r#"
[questions.q]
type = "choice"
instructions = "Which?"
criteria = { only = "the only one" }
"#;
        assert!(toml_err(one).contains("at least 2"));

        let none = r#"
[questions.q]
type = "choice"
instructions = "Which?"
criteria = {}
"#;
        assert!(toml_err(none).contains("at least 2"));
    }

    #[test]
    fn a_choice_without_criteria_is_rejected() {
        let contents = "[questions.q]\ntype = \"choice\"\ninstructions = \"Which?\"\n";
        assert!(toml_err(contents).contains("needs `criteria`"));
    }

    #[test]
    fn a_score_needs_at_least_two_distinct_levels() {
        let one = r#"
[questions.q]
type = "score"
instructions = "How much?"
criteria = ["only"]
"#;
        assert!(toml_err(one).contains("at least 2"));

        let dupe = r#"
[questions.q]
type = "score"
instructions = "How much?"
criteria = ["same", "same"]
"#;
        assert!(toml_err(dupe).contains("repeats a score level"));

        let empty_level = r#"
[questions.q]
type = "score"
instructions = "How much?"
criteria = ["ok", ""]
"#;
        assert!(toml_err(empty_level).contains("empty or non-string score level"));
    }

    #[test]
    fn a_score_with_a_map_of_criteria_is_rejected() {
        let contents = r#"
[questions.q]
type = "score"
instructions = "How much?"
criteria = { low = "a", high = "b" }
"#;
        assert!(toml_err(contents).contains("ordered array"));
    }

    #[test]
    fn noul_criteria_may_only_describe_true_and_false() {
        let contents = r#"
[questions.q]
type = "noul"
instructions = "Is it?"
criteria = { "true" = "yes", maybe = "sort of" }
"#;
        assert!(toml_err(contents).contains("only `true` and `false`"));
    }

    #[test]
    fn an_empty_criteria_description_is_rejected() {
        let contents = r#"
[questions.q]
type = "choice"
instructions = "Which?"
criteria = { a = "fine", b = "  " }
"#;
        assert!(toml_err(contents).contains("empty or non-string description"));
    }

    #[test]
    fn an_unknown_type_names_the_three_that_exist() {
        let contents = "[questions.q]\ntype = \"vibes\"\ninstructions = \"Is it?\"\n";
        let message = toml_err(contents);
        assert!(message.contains("noul | choice | score"), "{message}");
    }

    #[test]
    fn a_misspelled_key_is_rejected_rather_than_ignored() {
        // `critera` would otherwise leave a choice question with no options.
        let contents = r#"
[questions.q]
type = "choice"
instructions = "Which?"
critera = { a = "one", b = "two" }
"#;
        assert!(toml_err(contents).contains("critera"));
    }

    #[test]
    fn malformed_toml_is_a_parse_error() {
        assert!(toml_err("[questions.q\n").contains("cannot parse"));
    }

    #[test]
    fn detect_format_keys_off_the_extension() -> Result<(), ConfigError> {
        assert_eq!(
            detect_format(Path::new("/etc/q.toml"))?,
            QuestionsFormat::Toml
        );
        assert_eq!(
            detect_format(Path::new("/etc/Q.JSON"))?,
            QuestionsFormat::Json
        );
        assert!(detect_format(Path::new("/etc/q.yaml")).is_err());
        assert!(detect_format(Path::new("/etc/questions")).is_err());
        Ok(())
    }

    #[test]
    fn the_checked_in_example_file_is_valid() -> Result<(), ConfigError> {
        let set = parse_questions(
            include_str!("../examples/questions.toml"),
            QuestionsFormat::Toml,
        )?;
        assert_eq!(set.len(), 4);
        Ok(())
    }
}
