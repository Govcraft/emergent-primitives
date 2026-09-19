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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            items_key: "items".to_string(),
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
        /// The acknowledgement's payload.
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

/// An input the runner did nothing with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ignored {
    /// An ack arrived with no stream in flight.
    AckWhileIdle,
    /// A load arrived while a stream was already running.
    LoadWhileStreaming {
        /// Index of the item in flight.
        index: usize,
        /// How many items the running collection holds.
        total: usize,
    },
    /// A load's payload held no array to stream.
    BadShape {
        /// What was wrong with the shape.
        detail: String,
    },
}

/// What the shell should do after a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Publish one collection item on the item topic.
    PublishItem(Publication),
    /// Publish the run's final count on the end topic.
    PublishCompleted(Publication),
    /// Report an input that changed nothing.
    LogIgnored(Ignored),
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
        Input::Load { payload, origin } => on_load(config, state, &payload, origin),
        Input::Ack { payload: _ } => on_ack(state),
    }
}

fn on_load(config: &Config, state: State, payload: &Value, origin: Origin) -> (State, Vec<Effect>) {
    if let Some(run) = state.run.as_ref() {
        let ignored = Ignored::LoadWhileStreaming {
            index: run.index,
            total: run.items.len(),
        };
        return (state, vec![Effect::LogIgnored(ignored)]);
    }

    let items = match extract_items(payload, &config.items_key) {
        Ok(items) => items,
        Err(detail) => {
            return (
                state,
                vec![Effect::LogIgnored(Ignored::BadShape { detail })],
            );
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

fn on_ack(state: State) -> (State, Vec<Effect>) {
    let mut state = state;
    let Some(run) = state.run.take() else {
        return (state, vec![Effect::LogIgnored(Ignored::AckWhileIdle)]);
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

/// The end event's payload.
fn end_payload(count: usize) -> Value {
    json!({ "count": count })
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
            Effect::PublishCompleted(p) => format!("completed:count={}", p.payload["count"]),
            Effect::LogIgnored(Ignored::AckWhileIdle) => "ignored:not-streaming".to_string(),
            Effect::LogIgnored(Ignored::LoadWhileStreaming { .. }) => "ignored:busy".to_string(),
            Effect::LogIgnored(Ignored::BadShape { .. }) => "ignored:bad-shape".to_string(),
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
    fn a_load_while_busy_is_ignored_and_the_run_continues() {
        run(Case {
            name: "load while busy",
            config: Config::default(),
            steps: vec![
                (load(json!([item("a"), item("b")])), vec!["item:a"]),
                (load(json!([item("c")])), vec!["ignored:busy"]),
                (ack(json!(null)), vec!["item:b"]),
                (ack(json!(null)), vec!["completed:count=2"]),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn a_payload_with_no_array_is_ignored() {
        run(Case {
            name: "bad shape",
            config: Config::default(),
            steps: vec![
                (
                    load(json!({"command": "ls", "stdout": "[]", "exit_code": 0})),
                    vec!["ignored:bad-shape"],
                ),
                (load(json!("not a collection")), vec!["ignored:bad-shape"]),
                (load(json!({"other": [1]})), vec!["ignored:bad-shape"]),
                (load(json!({"items": "no"})), vec!["ignored:bad-shape"]),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn any_message_on_the_ack_topic_advances() {
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
            config: Config::default(),
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
