//! The stream runner's decision logic, as a pure state machine.
//!
//! [`step`] is the whole primitive. It takes the current [`State`] and one
//! [`Input`] and returns the next state plus the [`Effect`]s the async shell
//! should carry out. It performs no IO, owns no clock, and holds no handle to
//! the engine, so every rule below is reachable from a unit test: a duplicate
//! ack, an ack that belongs to a previous batch, a load that arrives mid
//! stream, a payload whose shape cannot be streamed.
//!
//! The async shell in `main.rs` only interprets effects. Nothing it does can
//! change what the runner decides, which is the point: the decisions are the
//! part that was hard to see, and now they are the part that is tested.
//!
//! # Failure is data
//!
//! A load that cannot be started publishes a rejection carrying the load's own
//! payload, so a topology can route it back to the load topic once the stream
//! is free, or to a quarantine when its shape is wrong. Nothing is dropped in
//! silence.

use emergent_client::types::{CausationId, CorrelationId};
use serde_json::{Value, json};

/// Everything the state machine needs from the command line.
///
/// Topics are absent on purpose: the machine decides *what* to publish, the
/// shell decides *where*, which keeps `EMERGENT_PUBLISHES` resolution out of
/// the logic under test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Object key holding the array to stream, when a load is an object.
    pub items_key: String,
    /// Field that must agree between an item and its acknowledgement.
    ///
    /// `None` keeps the original behaviour: any message on the ack topic
    /// advances the stream.
    pub ack_key: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            items_key: "items".to_string(),
            ack_key: None,
        }
    }
}

/// The identity replayed onto everything a step publishes.
///
/// A run's identity is captured from its load message and held for the length
/// of the stream, because acknowledgements are separate messages and cannot be
/// trusted to carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The message that caused this one.
    pub causation_id: CausationId,
    /// The correlation the run belongs to, when the load carried one.
    pub correlation_id: Option<CorrelationId>,
}

/// Everything that can happen to the runner from outside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A message arrived on the load topic.
    Load {
        /// The load's payload, whatever shape it turned out to be.
        payload: Value,
        /// The load's identity, replayed onto the run it starts.
        origin: Origin,
    },
    /// A message arrived on the ack topic.
    Ack {
        /// The acknowledgement's payload, matched against the item in flight
        /// when [`Config::ack_key`] is set.
        payload: Value,
    },
}

/// A message the shell should publish: payload plus the identity to replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publication {
    /// The payload to publish.
    pub payload: Value,
    /// Causation and correlation for the published message.
    pub origin: Origin,
}

/// Why an acknowledgement changed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoredAck {
    /// No stream was in flight.
    NotStreaming,
    /// [`Config::ack_key`] disagreed between the ack and the item in flight.
    KeyMismatch {
        /// The configured key.
        key: String,
        /// The value the item in flight carries, when it carries one.
        expected: Option<Value>,
        /// The value the acknowledgement carried, when it carried one.
        got: Option<Value>,
    },
}

/// What the shell should do after a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Publish one collection item on the item topic.
    PublishItem(Publication),
    /// Publish a dropped load on the rejected topic.
    PublishRejected(Publication),
    /// Publish the run's final count on the end topic.
    PublishCompleted(Publication),
    /// An acknowledgement was ignored, and the operator should hear about it.
    ///
    /// This is a log line rather than an event on purpose. A mismatched ack is
    /// not a dropped input: it is a message addressed to an item that is no
    /// longer in flight, and in a fan-in topology a retrying downstream can
    /// produce them without bound.
    LogIgnoredAck(IgnoredAck),
}

/// The run in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
    items: Vec<Value>,
    /// Index of the item currently in flight.
    index: usize,
    origin: Origin,
}

/// The runner's entire state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    run: Option<Run>,
}

impl State {
    /// A runner that has never streamed anything.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the runner is free to accept a load.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.run.is_none()
    }

    /// The item currently awaiting acknowledgement, if any.
    #[must_use]
    pub fn in_flight(&self) -> Option<&Value> {
        let run = self.run.as_ref()?;
        run.items.get(run.index)
    }
}

/// Extract the items array from a load payload.
///
/// A bare array is the collection. An object is looked up under `items_key`.
/// Anything else is a shape this primitive cannot stream.
///
/// # Errors
///
/// Returns a description of the shape mismatch.
pub fn extract_items(payload: &Value, items_key: &str) -> Result<Vec<Value>, String> {
    match payload {
        Value::Array(arr) => Ok(arr.clone()),
        Value::Object(obj) => match obj.get(items_key) {
            Some(Value::Array(arr)) => Ok(arr.clone()),
            Some(_) => Err(format!("key '{items_key}' is not an array")),
            None => Err(format!("object has no key '{items_key}'")),
        },
        _ => Err(format!("payload is not an array or object: {payload}")),
    }
}

/// Advance the state machine by one input.
#[must_use]
pub fn step(config: &Config, state: State, input: Input) -> (State, Vec<Effect>) {
    match input {
        Input::Load { payload, origin } => on_load(config, state, payload, origin),
        Input::Ack { payload } => on_ack(config, state, &payload),
    }
}

fn on_load(config: &Config, state: State, payload: Value, origin: Origin) -> (State, Vec<Effect>) {
    if let Some(run) = state.run.as_ref() {
        let detail = format!(
            "a load arrived while item {} of {} was in flight",
            run.index + 1,
            run.items.len()
        );
        let in_flight = json!({"index": run.index, "total": run.items.len()});
        let rejection = rejected(config, "busy", &detail, in_flight, payload, origin);
        return (state, vec![Effect::PublishRejected(rejection)]);
    }

    let items = match extract_items(&payload, &config.items_key) {
        Ok(items) => items,
        Err(detail) => {
            let rejection = rejected(config, "bad_shape", &detail, Value::Null, payload, origin);
            return (state, vec![Effect::PublishRejected(rejection)]);
        }
    };

    if items.is_empty() {
        let completed = Publication {
            payload: end_payload(0),
            origin,
        };
        return (state, vec![Effect::PublishCompleted(completed)]);
    }

    let mut state = state;
    let run = Run {
        items,
        index: 0,
        origin,
    };
    let effects = emit(&mut state, run);
    (state, effects)
}

fn on_ack(config: &Config, state: State, payload: &Value) -> (State, Vec<Effect>) {
    let Some(run) = state.run.as_ref() else {
        return (state, vec![Effect::LogIgnoredAck(IgnoredAck::NotStreaming)]);
    };

    if let Some(key) = config.ack_key.as_deref() {
        let expected = field(run.items.get(run.index), key);
        let got = field(Some(payload), key);
        if !matched(expected.as_ref(), got.as_ref()) {
            let mismatch = IgnoredAck::KeyMismatch {
                key: key.to_string(),
                expected,
                got,
            };
            return (state, vec![Effect::LogIgnoredAck(mismatch)]);
        }
    }

    let mut state = state;
    let Some(run) = state.run.take() else {
        return (state, Vec::new());
    };
    let effects = advance(&mut state, run);
    (state, effects)
}

/// Move past the item in flight: emit the next one, or end the run.
fn advance(state: &mut State, run: Run) -> Vec<Effect> {
    let mut run = run;
    run.index += 1;

    if run.index < run.items.len() {
        return emit(state, run);
    }

    let completed = Publication {
        payload: end_payload(run.items.len()),
        origin: run.origin,
    };
    state.run = None;
    vec![Effect::PublishCompleted(completed)]
}

/// Publish the item at `run.index` and store the run.
fn emit(state: &mut State, run: Run) -> Vec<Effect> {
    let item = run.items.get(run.index).cloned().unwrap_or(Value::Null);
    let effects = vec![Effect::PublishItem(Publication {
        payload: item,
        origin: run.origin.clone(),
    })];
    state.run = Some(run);
    effects
}

/// Build a rejection carrying the load that was dropped.
///
/// The load's payload is nested under `payload` rather than spread, so a
/// downstream router can replay it verbatim onto the load topic without having
/// to strip the rejection's own keys back off.
fn rejected(
    config: &Config,
    reason: &str,
    detail: &str,
    in_flight: Value,
    payload: Value,
    origin: Origin,
) -> Publication {
    Publication {
        payload: json!({
            "reason": reason,
            "detail": detail,
            "hint": exec_source_hint(&payload),
            "items_key": config.items_key,
            "in_flight": in_flight,
            "payload": payload,
        }),
        origin,
    }
}

/// The end event's payload.
fn end_payload(count: usize) -> Value {
    json!({ "count": count })
}

/// Read one top level field, absent for any payload that is not an object.
fn field(value: Option<&Value>, key: &str) -> Option<Value> {
    value?.as_object()?.get(key).cloned()
}

/// Two ack keys agree only when both are present and neither is null.
///
/// Absence cannot match absence: an item with no key would otherwise be
/// advanced by any message at all, which is the behaviour `--ack-key` exists to
/// remove.
fn matched(expected: Option<&Value>, got: Option<&Value>) -> bool {
    match (expected, got) {
        (Some(a), Some(b)) => !a.is_null() && a == b,
        _ => false,
    }
}

/// Name the most common wrong shape instead of making the operator guess.
///
/// An `exec-source` payload is `{command, stdout, exit_code}`, so a topology
/// that wires a command's output straight into the load topic hands this
/// primitive an object with no array in it.
fn exec_source_hint(payload: &Value) -> Value {
    let is_exec_envelope = payload
        .as_object()
        .and_then(|obj| obj.get("stdout"))
        .is_some_and(Value::is_string);
    if is_exec_envelope {
        json!(
            "payload looks like an exec-source envelope; unwrap it before the load topic, for example with an exec-handler running `jq -c '.stdout | fromjson'`"
        )
    } else {
        Value::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A readable one line summary of an effect.
    ///
    /// Table cases compare these strings rather than whole effects, so a case
    /// reads as the sequence of things that happened and is not rewritten every
    /// time a payload gains a field.
    fn trace(effects: &[Effect]) -> Vec<String> {
        effects.iter().map(summarise).collect()
    }

    fn summarise(effect: &Effect) -> String {
        match effect {
            Effect::PublishItem(p) => format!("item:{}", label(&p.payload)),
            Effect::PublishRejected(p) => {
                format!("rejected:{}", p.payload["reason"].as_str().unwrap_or("?"))
            }
            Effect::PublishCompleted(p) => format!("completed:count={}", p.payload["count"]),
            Effect::LogIgnoredAck(IgnoredAck::NotStreaming) => "ignored:not-streaming".to_string(),
            Effect::LogIgnoredAck(IgnoredAck::KeyMismatch { .. }) => "ignored:mismatch".to_string(),
        }
    }

    fn label(value: &Value) -> String {
        match value.get("id") {
            Some(Value::String(id)) => id.clone(),
            Some(other) => other.to_string(),
            None => value.to_string(),
        }
    }

    fn item(id: &str) -> Value {
        json!({"id": id, "body": format!("payload for {id}")})
    }

    fn origin() -> Origin {
        Origin {
            causation_id: CausationId::new(),
            correlation_id: None,
        }
    }

    fn load(payload: Value) -> Input {
        Input::Load {
            payload,
            origin: origin(),
        }
    }

    fn ack(payload: Value) -> Input {
        Input::Ack { payload }
    }

    fn keyed() -> Config {
        Config {
            ack_key: Some("id".to_string()),
            ..Config::default()
        }
    }

    /// One scripted run: every input paired with the effects it must produce.
    struct Case {
        name: &'static str,
        config: Config,
        /// Each step is an input and the exact effect trace it must return.
        steps: Vec<(Input, Vec<&'static str>)>,
        /// The item left in flight when the script ends, by id.
        ends_in_flight: Option<&'static str>,
    }

    fn run(case: Case) {
        let mut state = State::new();
        for (index, (input, expected)) in case.steps.into_iter().enumerate() {
            let (next, effects) = step(&case.config, state, input);
            state = next;
            assert_eq!(
                trace(&effects),
                expected,
                "case '{}', step {index}",
                case.name
            );
        }
        let in_flight = state.in_flight().map(label);
        assert_eq!(
            in_flight.as_deref(),
            case.ends_in_flight,
            "case '{}': wrong item left in flight",
            case.name
        );
    }

    #[test]
    fn a_load_while_busy_is_rejected_and_the_run_continues() {
        run(Case {
            name: "load while busy",
            config: Config::default(),
            steps: vec![
                (load(json!([item("a"), item("b")])), vec!["item:a"]),
                (load(json!([item("c")])), vec!["rejected:busy"]),
                (ack(json!(null)), vec!["item:b"]),
                (ack(json!(null)), vec!["completed:count=2"]),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn a_payload_with_no_array_is_rejected() {
        run(Case {
            name: "bad shape",
            config: Config::default(),
            steps: vec![
                (
                    load(json!({"command": "ls", "stdout": "[]", "exit_code": 0})),
                    vec!["rejected:bad_shape"],
                ),
                (load(json!("not a collection")), vec!["rejected:bad_shape"]),
                (load(json!({"other": [1]})), vec!["rejected:bad_shape"]),
                (load(json!({"items": "no"})), vec!["rejected:bad_shape"]),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn a_rejection_carries_the_dropped_load_and_the_run_in_flight() {
        let config = Config::default();
        let (state, _) = step(&config, State::new(), load(json!([item("a"), item("b")])));
        let dropped = json!({"items": [item("c")], "batch": 7});
        let (_, effects) = step(&config, state, load(dropped.clone()));

        let [Effect::PublishRejected(rejection)] = effects.as_slice() else {
            panic!("expected exactly one rejection, got {effects:?}");
        };
        assert_eq!(rejection.payload["reason"], json!("busy"));
        assert_eq!(rejection.payload["payload"], dropped);
        assert_eq!(
            rejection.payload["in_flight"],
            json!({"index": 0, "total": 2})
        );
        assert_eq!(rejection.payload["items_key"], json!("items"));
        assert_eq!(rejection.payload["hint"], Value::Null);
        assert!(
            rejection.payload["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("item 1 of 2")
        );
    }

    #[test]
    fn a_rejected_exec_source_envelope_says_what_to_do_about_it() {
        let envelope = json!({"command": "list-batch", "stdout": "{\"items\":[]}", "exit_code": 0});
        let (_, effects) = step(&Config::default(), State::new(), load(envelope.clone()));

        let [Effect::PublishRejected(rejection)] = effects.as_slice() else {
            panic!("expected exactly one rejection, got {effects:?}");
        };
        assert_eq!(rejection.payload["reason"], json!("bad_shape"));
        assert_eq!(rejection.payload["payload"], envelope);
        assert_eq!(rejection.payload["in_flight"], Value::Null);
        assert!(
            rejection.payload["hint"]
                .as_str()
                .unwrap_or_default()
                .contains("exec-source envelope")
        );
    }

    #[test]
    fn a_rejection_is_caused_by_the_load_it_dropped() {
        let config = Config::default();
        let (state, _) = step(&config, State::new(), load(json!([item("a")])));
        let second = Origin {
            causation_id: CausationId::new(),
            correlation_id: Some(CorrelationId::new()),
        };
        let (_, effects) = step(
            &config,
            state,
            Input::Load {
                payload: json!([item("b")]),
                origin: second.clone(),
            },
        );
        let [Effect::PublishRejected(rejection)] = effects.as_slice() else {
            panic!("expected one rejection, got {effects:?}");
        };
        assert_eq!(rejection.origin, second);
    }

    #[test]
    fn without_an_ack_key_any_message_advances() {
        // The original behaviour, kept deliberately: this is what `--ack-key`
        // exists to replace, and a topology that has not adopted it must not
        // change under it.
        run(Case {
            name: "unkeyed ack",
            config: Config::default(),
            steps: vec![
                (load(json!([item("a"), item("b")])), vec!["item:a"]),
                (ack(json!({"id": "something-else"})), vec!["item:b"]),
                (ack(json!({})), vec!["completed:count=2"]),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn a_duplicate_ack_does_not_release_a_second_item() {
        run(Case {
            name: "duplicate ack",
            config: keyed(),
            steps: vec![
                (
                    load(json!([item("a"), item("b"), item("c")])),
                    vec!["item:a"],
                ),
                (ack(item("a")), vec!["item:b"]),
                (ack(item("a")), vec!["ignored:mismatch"]),
                (ack(item("a")), vec!["ignored:mismatch"]),
                (ack(item("b")), vec!["item:c"]),
            ],
            ends_in_flight: Some("c"),
        });
    }

    #[test]
    fn an_ack_for_an_injected_item_is_ignored() {
        run(Case {
            name: "injected item",
            config: keyed(),
            steps: vec![
                (load(json!([item("a"), item("b")])), vec!["item:a"]),
                (ack(item("injected")), vec!["ignored:mismatch"]),
                (ack(json!({"id": null})), vec!["ignored:mismatch"]),
                (ack(json!(["a"])), vec!["ignored:mismatch"]),
                (ack(item("a")), vec!["item:b"]),
            ],
            ends_in_flight: Some("b"),
        });
    }

    #[test]
    fn a_late_ack_from_a_previous_batch_is_ignored() {
        run(Case {
            name: "late ack across batches",
            config: keyed(),
            steps: vec![
                (load(json!([item("a")])), vec!["item:a"]),
                (ack(item("a")), vec!["completed:count=1"]),
                (ack(item("a")), vec!["ignored:not-streaming"]),
                (load(json!([item("b"), item("c")])), vec!["item:b"]),
                (ack(item("a")), vec!["ignored:mismatch"]),
                (ack(item("b")), vec!["item:c"]),
            ],
            ends_in_flight: Some("c"),
        });
    }

    #[test]
    fn an_item_without_the_ack_key_never_advances() {
        // Documented in the README: `--ack-key` requires the field on both
        // sides, so a collection of bare values needs `--ack-timeout-ms` to
        // make progress. Absence must not match absence, or any message at all
        // would advance the stream.
        run(Case {
            name: "item lacks the ack key",
            config: keyed(),
            steps: vec![
                (load(json!(["a", "b"])), vec!["item:\"a\""]),
                (ack(json!({"id": "a"})), vec!["ignored:mismatch"]),
                (ack(json!("a")), vec!["ignored:mismatch"]),
            ],
            ends_in_flight: Some("\"a\""),
        });
    }

    #[test]
    fn a_mismatch_reports_both_sides_of_the_comparison() {
        let config = keyed();
        let (state, _) = step(&config, State::new(), load(json!([item("a")])));
        let (_, effects) = step(&config, state, ack(json!({"id": "b"})));

        let [Effect::LogIgnoredAck(IgnoredAck::KeyMismatch { key, expected, got })] =
            effects.as_slice()
        else {
            panic!("expected exactly one mismatch, got {effects:?}");
        };
        assert_eq!(key, "id");
        assert_eq!(expected.as_ref(), Some(&json!("a")));
        assert_eq!(got.as_ref(), Some(&json!("b")));
    }

    #[test]
    fn an_empty_collection_completes_without_emitting() {
        run(Case {
            name: "empty collection",
            config: Config::default(),
            steps: vec![
                (load(json!([])), vec!["completed:count=0"]),
                (load(json!({"items": []})), vec!["completed:count=0"]),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn an_ack_while_idle_is_ignored() {
        run(Case {
            name: "idle ack",
            config: keyed(),
            steps: vec![(ack(item("a")), vec!["ignored:not-streaming"])],
            ends_in_flight: None,
        });
    }

    #[test]
    fn a_configured_items_key_selects_the_array() {
        run(Case {
            name: "items key",
            config: Config {
                items_key: "transactions".to_string(),
                ..Config::default()
            },
            steps: vec![
                (
                    load(json!({"transactions": [item("a")], "items": [item("z")]})),
                    vec!["item:a"],
                ),
                (ack(json!(null)), vec!["completed:count=1"]),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn every_published_message_carries_the_loads_identity() {
        let config = Config::default();
        let correlation = CorrelationId::new();
        let load_origin = Origin {
            causation_id: CausationId::new(),
            correlation_id: Some(correlation.clone()),
        };
        let (state, effects) = step(
            &config,
            State::new(),
            Input::Load {
                payload: json!([item("a")]),
                origin: load_origin.clone(),
            },
        );
        let [Effect::PublishItem(emitted)] = effects.as_slice() else {
            panic!("expected one item, got {effects:?}");
        };
        assert_eq!(emitted.origin, load_origin);

        let (_, effects) = step(&config, state, ack(json!(null)));
        let [Effect::PublishCompleted(completed)] = effects.as_slice() else {
            panic!("expected one completion, got {effects:?}");
        };
        assert_eq!(completed.origin, load_origin);
        assert_eq!(completed.origin.correlation_id, Some(correlation));
    }

    #[test]
    fn bare_array_returns_all_items() {
        let payload = json!([1, 2, 3]);
        let result = extract_items(&payload, "items")
            .unwrap_or_else(|e| panic!("expected Ok, got Err: {e}"));
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn object_with_default_key_returns_items() {
        let payload = json!({"items": [1, 2, 3]});
        let result = extract_items(&payload, "items")
            .unwrap_or_else(|e| panic!("expected Ok, got Err: {e}"));
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn object_with_custom_key_returns_items() {
        let payload = json!({"records": [1]});
        let result = extract_items(&payload, "records")
            .unwrap_or_else(|e| panic!("expected Ok, got Err: {e}"));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn object_missing_key_returns_err() {
        let payload = json!({"other": [1, 2]});
        assert!(extract_items(&payload, "items").is_err());
    }

    #[test]
    fn null_payload_returns_err() {
        let payload = json!(null);
        assert!(extract_items(&payload, "items").is_err());
    }

    #[test]
    fn key_maps_to_non_array_returns_err() {
        let payload = json!({"items": "not-an-array"});
        assert!(extract_items(&payload, "items").is_err());
    }

    #[test]
    fn empty_array_returns_ok_empty() {
        let payload = json!([]);
        let result = extract_items(&payload, "items")
            .unwrap_or_else(|e| panic!("expected Ok, got Err: {e}"));
        assert!(result.is_empty());
    }
}
