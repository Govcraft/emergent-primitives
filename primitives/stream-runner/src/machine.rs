//! The stream runner's decision logic, as a pure state machine.
//!
//! [`step`] is the whole primitive. It takes the current [`State`] and one
//! [`Input`] and returns the next state plus the [`Effect`]s the async shell
//! should carry out. It performs no IO, owns no clock, and holds no handle to
//! the engine, so every rule below is reachable from a unit test: a duplicate
//! ack, an ack that belongs to a previous batch, a load that arrives mid
//! stream, a timer that fires for an item that was already acknowledged.
//!
//! # Why a generation counter
//!
//! An item's timer outlives the item. If item 3 is acknowledged a millisecond
//! before its timer fires, the firing must not skip item 4. Every emitted item
//! therefore carries a `generation`, monotonic for the life of the process, and
//! [`Effect::ArmTimer`] carries the generation of the item it was armed for. A
//! [`Input::TimerFired`] whose generation is not the one in flight is a no-op.
//! That is the only ordering guarantee this primitive needs, and it holds
//! across runs as well as within one.
//!
//! # Failure is data
//!
//! Nothing is dropped in silence. A load that cannot be started publishes a
//! rejection carrying the load's own payload, so a topology can route it back
//! to the load topic once the stream is free, or to a quarantine when its shape
//! is wrong. An item whose acknowledgement never arrives publishes a timeout
//! carrying the item, so the run either moves on or ends, but never stalls.

use clap::ValueEnum;
use emergent_client::types::{CausationId, CorrelationId};
use serde_json::{Value, json};

/// What the runner does when an item's acknowledgement does not arrive in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
#[value(rename_all = "kebab-case")]
pub enum OnTimeout {
    /// Abandon the item and emit the next one, continuing the run.
    #[default]
    Skip,
    /// Abandon the whole run and publish the end event as incomplete.
    End,
}

impl OnTimeout {
    /// The value written into a published `action` field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::End => "end",
        }
    }
}

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
    /// How long an emitted item may wait for its acknowledgement.
    ///
    /// `None` keeps the original behaviour: an item waits forever.
    pub ack_timeout_ms: Option<u64>,
    /// What a timeout does to the run.
    pub on_timeout: OnTimeout,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            items_key: "items".to_string(),
            ack_key: None,
            ack_timeout_ms: None,
            on_timeout: OnTimeout::Skip,
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
    /// A timer armed by [`Effect::ArmTimer`] elapsed.
    TimerFired {
        /// The generation the timer was armed for.
        generation: u64,
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
    /// Publish an item whose acknowledgement never arrived, on the timeout topic.
    PublishTimedOut(Publication),
    /// Publish the run's final count on the end topic.
    PublishCompleted(Publication),
    /// Arm a timer for the item just emitted.
    ///
    /// An earlier timer is superseded rather than cancelled: a stale firing is
    /// already a no-op, so the shell never has to reason about cancellation.
    ArmTimer {
        /// The generation the timer belongs to.
        generation: u64,
        /// How long to wait before firing.
        timeout_ms: u64,
    },
    /// An acknowledgement was ignored, and the operator should hear about it.
    ///
    /// This is a log line rather than an event on purpose. A mismatched ack is
    /// not a dropped input: it is a message addressed to an item that is no
    /// longer in flight, and in a fan-in topology a retrying downstream can
    /// produce them without bound. The item that lost its acknowledgement is
    /// covered by [`Config::ack_timeout_ms`], which does publish an event.
    LogIgnoredAck(IgnoredAck),
}

/// The run in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
    items: Vec<Value>,
    /// Index of the item currently in flight.
    index: usize,
    /// Generation of the item currently in flight.
    generation: u64,
    /// How many items have been abandoned to a timeout so far.
    timed_out: usize,
    origin: Origin,
}

/// The runner's entire state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    /// The generation the next emitted item will carry. Never reused.
    next_generation: u64,
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

    /// The generation of the item currently awaiting acknowledgement, if any.
    #[must_use]
    pub fn generation(&self) -> Option<u64> {
        self.run.as_ref().map(|run| run.generation)
    }
}

/// Extract the items array from a load payload.
///
/// A bare array is the collection. An object is looked up under `items_key`.
/// Anything else is a shape this primitive cannot stream, and the `Err` string
/// is what an operator reads in the rejection event.
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
///
/// The returned effects are ordered: a timeout publishes the abandoned item
/// before whatever the run does next, so the event store reads in the order
/// things happened.
#[must_use]
pub fn step(config: &Config, state: State, input: Input) -> (State, Vec<Effect>) {
    match input {
        Input::Load { payload, origin } => on_load(config, state, payload, origin),
        Input::Ack { payload } => on_ack(config, state, &payload),
        Input::TimerFired { generation } => on_timer(config, state, generation),
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
            payload: end_payload(0, 0, 0, false),
            origin,
        };
        return (state, vec![Effect::PublishCompleted(completed)]);
    }

    let mut state = state;
    let run = Run {
        items,
        index: 0,
        generation: 0,
        timed_out: 0,
        origin,
    };
    let effects = emit(config, &mut state, run);
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
    let effects = advance(config, &mut state, run, 0);
    (state, effects)
}

fn on_timer(config: &Config, state: State, generation: u64) -> (State, Vec<Effect>) {
    // A timer for an item that was already acknowledged, or for a run that has
    // since ended, changes nothing. This is the guard that makes "timeout then
    // late ack" safe from either direction.
    if state.generation() != Some(generation) {
        return (state, Vec::new());
    }

    let mut state = state;
    let Some(run) = state.run.take() else {
        return (state, Vec::new());
    };

    let timeout_ms = config.ack_timeout_ms.unwrap_or_default();
    let item = run.items.get(run.index).cloned().unwrap_or(Value::Null);
    let timed_out = Publication {
        payload: json!({
            "reason": "ack_timeout",
            "action": config.on_timeout.as_str(),
            "timeout_ms": timeout_ms,
            "index": run.index,
            "total": run.items.len(),
            "item": item,
        }),
        origin: run.origin.clone(),
    };
    let mut effects = vec![Effect::PublishTimedOut(timed_out)];

    match config.on_timeout {
        OnTimeout::Skip => effects.extend(advance(config, &mut state, run, 1)),
        OnTimeout::End => {
            let completed = Publication {
                payload: end_payload(run.index + 1, run.items.len(), run.timed_out + 1, true),
                origin: run.origin,
            };
            effects.push(Effect::PublishCompleted(completed));
        }
    }

    (state, effects)
}

/// Move past the item in flight: emit the next one, or end the run.
fn advance(config: &Config, state: &mut State, run: Run, timed_out: usize) -> Vec<Effect> {
    let mut run = run;
    run.timed_out += timed_out;
    run.index += 1;

    if run.index < run.items.len() {
        return emit(config, state, run);
    }

    let completed = Publication {
        payload: end_payload(run.items.len(), run.items.len(), run.timed_out, false),
        origin: run.origin,
    };
    state.run = None;
    vec![Effect::PublishCompleted(completed)]
}

/// Publish the item at `run.index` under a fresh generation and store the run.
fn emit(config: &Config, state: &mut State, run: Run) -> Vec<Effect> {
    let mut run = run;
    run.generation = state.next_generation;
    state.next_generation = state.next_generation.saturating_add(1);

    let item = run.items.get(run.index).cloned().unwrap_or(Value::Null);
    let mut effects = vec![Effect::PublishItem(Publication {
        payload: item,
        origin: run.origin.clone(),
    })];
    if let Some(timeout_ms) = config.ack_timeout_ms {
        effects.push(Effect::ArmTimer {
            generation: run.generation,
            timeout_ms,
        });
    }

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
///
/// `count` is how many items were emitted and `total` how many the collection
/// held; they differ only when a run ended early. `timed_out` counts the items
/// that were emitted but never acknowledged.
fn end_payload(count: usize, total: usize, timed_out: usize, incomplete: bool) -> Value {
    json!({
        "count": count,
        "total": total,
        "timed_out": timed_out,
        "incomplete": incomplete,
    })
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
            Effect::PublishTimedOut(p) => format!("timed-out:{}", label(&p.payload["item"])),
            Effect::PublishCompleted(p) => format!(
                "completed:count={},total={},timed_out={},incomplete={}",
                p.payload["count"],
                p.payload["total"],
                p.payload["timed_out"],
                p.payload["incomplete"]
            ),
            Effect::ArmTimer { generation, .. } => format!("arm:{generation}"),
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

    fn timer(generation: u64) -> Input {
        Input::TimerFired { generation }
    }

    fn keyed() -> Config {
        Config {
            ack_key: Some("id".to_string()),
            ..Config::default()
        }
    }

    fn keyed_with_timeout(on_timeout: OnTimeout) -> Config {
        Config {
            ack_key: Some("id".to_string()),
            ack_timeout_ms: Some(50),
            on_timeout,
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

    const DONE_2: &str = "completed:count=2,total=2,timed_out=0,incomplete=false";

    #[test]
    fn load_while_busy_is_rejected_and_the_run_continues() {
        run(Case {
            name: "load while busy",
            config: Config::default(),
            steps: vec![
                (load(json!([item("a"), item("b")])), vec!["item:a"]),
                (load(json!([item("c")])), vec!["rejected:busy"]),
                (ack(json!(null)), vec!["item:b"]),
                (ack(json!(null)), vec![DONE_2]),
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
                (ack(json!({})), vec![DONE_2]),
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
                (
                    ack(item("a")),
                    vec!["completed:count=1,total=1,timed_out=0,incomplete=false"],
                ),
                (ack(item("a")), vec!["ignored:not-streaming"]),
                (load(json!([item("b"), item("c")])), vec!["item:b"]),
                (ack(item("a")), vec!["ignored:mismatch"]),
                (ack(item("b")), vec!["item:c"]),
            ],
            ends_in_flight: Some("c"),
        });
    }

    #[test]
    fn a_lost_ack_times_out_and_the_run_skips_on() {
        run(Case {
            name: "timeout skip",
            config: keyed_with_timeout(OnTimeout::Skip),
            steps: vec![
                (load(json!([item("a"), item("b")])), vec!["item:a", "arm:0"]),
                (timer(0), vec!["timed-out:a", "item:b", "arm:1"]),
                (
                    ack(item("b")),
                    vec!["completed:count=2,total=2,timed_out=1,incomplete=false"],
                ),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn a_timeout_followed_by_a_late_ack_does_not_double_advance() {
        run(Case {
            name: "timeout then late ack",
            config: keyed_with_timeout(OnTimeout::Skip),
            steps: vec![
                (
                    load(json!([item("a"), item("b"), item("c")])),
                    vec!["item:a", "arm:0"],
                ),
                (timer(0), vec!["timed-out:a", "item:b", "arm:1"]),
                // The ack for the abandoned item finally arrives.
                (ack(item("a")), vec!["ignored:mismatch"]),
                // And its timer fires a second time for good measure.
                (timer(0), vec![]),
                (ack(item("b")), vec!["item:c", "arm:2"]),
            ],
            ends_in_flight: Some("c"),
        });
    }

    #[test]
    fn a_timer_for_an_acked_item_is_a_no_op() {
        run(Case {
            name: "stale timer",
            config: keyed_with_timeout(OnTimeout::Skip),
            steps: vec![
                (load(json!([item("a"), item("b")])), vec!["item:a", "arm:0"]),
                (ack(item("a")), vec!["item:b", "arm:1"]),
                (timer(0), vec![]),
                (
                    timer(1),
                    vec![
                        "timed-out:b",
                        "completed:count=2,total=2,timed_out=1,incomplete=false",
                    ],
                ),
                (timer(1), vec![]),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn on_timeout_end_abandons_the_rest_of_the_collection() {
        run(Case {
            name: "timeout end",
            config: keyed_with_timeout(OnTimeout::End),
            steps: vec![
                (
                    load(json!([item("a"), item("b"), item("c")])),
                    vec!["item:a", "arm:0"],
                ),
                (
                    timer(0),
                    vec![
                        "timed-out:a",
                        "completed:count=1,total=3,timed_out=1,incomplete=true",
                    ],
                ),
                (ack(item("a")), vec!["ignored:not-streaming"]),
                (timer(0), vec![]),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn the_last_item_timing_out_still_completes_the_run() {
        run(Case {
            name: "last item times out",
            config: keyed_with_timeout(OnTimeout::Skip),
            steps: vec![
                (load(json!([item("a"), item("b")])), vec!["item:a", "arm:0"]),
                (ack(item("a")), vec!["item:b", "arm:1"]),
                (
                    timer(1),
                    vec![
                        "timed-out:b",
                        "completed:count=2,total=2,timed_out=1,incomplete=false",
                    ],
                ),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn a_single_item_collection_that_times_out_under_end_completes() {
        run(Case {
            name: "only item times out under end",
            config: keyed_with_timeout(OnTimeout::End),
            steps: vec![
                (load(json!([item("a")])), vec!["item:a", "arm:0"]),
                (
                    timer(0),
                    vec![
                        "timed-out:a",
                        "completed:count=1,total=1,timed_out=1,incomplete=true",
                    ],
                ),
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
                (
                    load(json!([])),
                    vec!["completed:count=0,total=0,timed_out=0,incomplete=false"],
                ),
                (
                    load(json!({"items": []})),
                    vec!["completed:count=0,total=0,timed_out=0,incomplete=false"],
                ),
            ],
            ends_in_flight: None,
        });
    }

    #[test]
    fn an_ack_while_idle_is_ignored() {
        run(Case {
            name: "idle ack",
            config: keyed(),
            steps: vec![
                (ack(item("a")), vec!["ignored:not-streaming"]),
                (timer(0), vec![]),
            ],
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
                (
                    ack(json!(null)),
                    vec!["completed:count=1,total=1,timed_out=0,incomplete=false"],
                ),
            ],
            ends_in_flight: None,
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
    fn no_timer_is_armed_when_no_timeout_is_configured() {
        let (_, effects) = step(
            &Config::default(),
            State::new(),
            load(json!([item("a"), item("b")])),
        );
        assert_eq!(trace(&effects), vec!["item:a"]);
    }

    #[test]
    fn generations_are_never_reused_across_runs() {
        let config = keyed_with_timeout(OnTimeout::Skip);
        let mut state = State::new();
        let mut seen = Vec::new();

        for _ in 0..3 {
            let (next, _) = step(&config, state, load(json!([item("a")])));
            state = next;
            seen.push(state.generation());
            let (next, _) = step(&config, state, ack(item("a")));
            state = next;
        }

        assert_eq!(seen, vec![Some(0), Some(1), Some(2)]);
        assert!(state.is_idle());
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
    fn the_timeout_event_carries_the_item_and_the_action() {
        let config = keyed_with_timeout(OnTimeout::End);
        let (state, _) = step(&config, State::new(), load(json!([item("a"), item("b")])));
        let (_, effects) = step(&config, state, timer(0));

        let [
            Effect::PublishTimedOut(timed_out),
            Effect::PublishCompleted(_),
        ] = effects.as_slice()
        else {
            panic!("expected a timeout then a completion, got {effects:?}");
        };
        assert_eq!(timed_out.payload["item"], item("a"));
        assert_eq!(timed_out.payload["reason"], json!("ack_timeout"));
        assert_eq!(timed_out.payload["action"], json!("end"));
        assert_eq!(timed_out.payload["timeout_ms"], json!(50));
        assert_eq!(timed_out.payload["index"], json!(0));
        assert_eq!(timed_out.payload["total"], json!(2));
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
