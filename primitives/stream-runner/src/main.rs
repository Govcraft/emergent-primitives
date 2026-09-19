//! Stream Runner
//!
//! A flow-control Handler that emits a JSON collection one item at a time,
//! waiting for a downstream acknowledgement before advancing to the next.
//!
//! # Data Flow
//!
//! 1. Receive a `load_topic` event containing a JSON collection
//! 2. Emit the first item on `publish_as`
//! 3. Wait for an `ack_topic` event (downstream output = ack)
//! 4. Emit the next item; repeat until exhausted
//! 5. Publish `end_topic` with `{"count": N}` when all items have been emitted
//!
//! # Structure
//!
//! Every decision lives in [`stream_runner::machine`], a pure function over
//! plain data. This file is the shell around it: it reads the command line,
//! turns inbound messages into machine inputs, and carries out the effects the
//! machine returns. It decides nothing itself.
//!
//! # Nothing Is Dropped In Silence
//!
//! A load that cannot be started is published on `rejected_topic` with a
//! `reason` of `busy` (a stream was already running) or `bad_shape` (the
//! payload held no array), carrying the load's own payload so a topology can
//! replay or quarantine it. A mismatched ack is logged rather than published:
//! it is addressed to an item that is no longer in flight, and a retrying
//! downstream can produce them without bound.
//!
//! # Matching Acks To Items
//!
//! Without `--ack-key`, any message on the ack topic advances the stream, which
//! means a duplicate or a late ack releases the next item early. With
//! `--ack-key <field>`, an ack advances the stream only when `ack[field]`
//! equals the same field on the item in flight; anything else is logged and
//! ignored.
//!
//! # Messages Published
//!
//! - Configurable item type (default: `stream.item`) — one item per ack cycle
//! - Configurable end type (default: `stream.end`) — final count payload
//! - Configurable rejected type (default: `stream.rejected`) — a load that was dropped
//!
//! The three are resolved positionally from `EMERGENT_PUBLISHES` in that order.
//!
//! # Usage
//!
//! ```bash
//! # Stream transactions one at a time, acking on classify output
//! stream-runner \
//!     --load-topic  batch.load \
//!     --publish-as  txn.raw \
//!     --ack-topic   txn.entry \
//!     --end-topic   stream.end \
//!     --items-key   transactions
//! ```

use clap::Parser;
use emergent_client::types::CausationId;
use emergent_client::{EmergentHandler, EmergentMessage};
use stream_runner::machine::{Config, Effect, IgnoredAck, Input, Origin, Publication, State, step};
use tokio::signal::unix::{SignalKind, signal};

/// Stream Runner — emit collection items one at a time, waiting for downstream ack before advancing.
#[derive(Parser, Debug)]
#[command(name = "stream-runner")]
#[command(
    about = "Emit collection items one at a time, waiting for downstream ack before advancing"
)]
struct Args {
    /// Event carrying the JSON collection to stream
    #[arg(long, default_value = "stream.load")]
    load_topic: String,

    /// Topic on which to emit each item
    #[arg(long, default_value = "stream.item")]
    publish_as: String,

    /// Topic to wait for before advancing to the next item (downstream output = ack)
    #[arg(long, default_value = "stream.ack")]
    ack_topic: String,

    /// Topic published when the collection is exhausted
    #[arg(long, default_value = "stream.end")]
    end_topic: String,

    /// Topic published when a load is dropped, with a `busy` or `bad_shape` reason
    #[arg(long, default_value = "stream.rejected")]
    rejected_topic: String,

    /// JSON object key containing the array to stream (ignored when payload is a bare array)
    #[arg(long, default_value = "items")]
    items_key: String,

    /// Field that must agree between an item and its ack for the stream to advance
    ///
    /// Unset, any message on the ack topic advances the stream.
    #[arg(long)]
    ack_key: Option<String>,
}

/// Where each kind of published message goes.
struct Topics {
    item: String,
    end: String,
    rejected: String,
}

/// The async shell: the state machine and its configuration.
struct Runner {
    config: Config,
    topics: Topics,
    state: State,
}

impl Runner {
    /// Feed one input to the machine and carry out what it returns.
    async fn drive(&mut self, handler: &EmergentHandler, input: Input) {
        let (state, effects) = step(&self.config, std::mem::take(&mut self.state), input);
        self.state = state;
        for effect in effects {
            self.apply(handler, effect).await;
        }
    }

    async fn apply(&mut self, handler: &EmergentHandler, effect: Effect) {
        match effect {
            Effect::PublishItem(publication) => {
                publish(handler, &self.topics.item, publication).await;
            }
            Effect::PublishRejected(publication) => {
                tracing::warn!(
                    reason = %publication.payload["reason"],
                    detail = %publication.payload["detail"],
                    "Load dropped, publishing on {}",
                    self.topics.rejected
                );
                publish(handler, &self.topics.rejected, publication).await;
            }
            Effect::PublishCompleted(publication) => {
                publish(handler, &self.topics.end, publication).await;
            }
            Effect::LogIgnoredAck(IgnoredAck::NotStreaming) => {
                tracing::debug!("Received ack while idle, ignoring");
            }
            Effect::LogIgnoredAck(IgnoredAck::KeyMismatch { key, expected, got }) => {
                tracing::warn!(
                    key = %key,
                    expected = %json_or_absent(expected.as_ref()),
                    got = %json_or_absent(got.as_ref()),
                    "Ack does not match the item in flight, ignoring"
                );
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // WARN by default rather than ERROR: a dropped load is a warning, and
    // under an engine nobody sets RUST_LOG.
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::WARN.into())
        .from_env_lossy();
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let args = Args::parse();

    let publish_types =
        resolve_publish_types_from_env(&[&args.publish_as, &args.end_topic, &args.rejected_topic]);
    let topics = Topics {
        item: publish_types[0].clone(),
        end: publish_types[1].clone(),
        rejected: publish_types[2].clone(),
    };

    let config = Config {
        items_key: args.items_key.clone(),
        ack_key: args.ack_key.clone(),
    };

    let name = std::env::var("EMERGENT_NAME").unwrap_or_else(|_| "stream-runner".to_string());

    let mut handler = match EmergentHandler::connect(&name).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Failed to connect to Emergent engine: {e}");
            std::process::exit(1);
        }
    };

    let subscribe_topics = [args.load_topic.as_str(), args.ack_topic.as_str()];
    let mut stream = match handler.subscribe(&subscribe_topics).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to subscribe: {e}");
            std::process::exit(1);
        }
    };

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut runner = Runner {
        config,
        topics,
        state: State::new(),
    };

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                let _ = handler.disconnect().await;
                break;
            }

            msg = stream.next() => match msg {
                None => break,
                Some(msg) => {
                    if let Some(input) = classify(&msg, &args.load_topic, &args.ack_topic) {
                        runner.drive(&handler, input).await;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Turn an inbound message into a machine input, or ignore it.
fn classify(msg: &EmergentMessage, load_topic: &str, ack_topic: &str) -> Option<Input> {
    let message_type = msg.message_type.as_str();
    if message_type == load_topic {
        Some(Input::Load {
            payload: msg.payload().clone(),
            origin: Origin {
                causation_id: CausationId::from(msg.id()),
                correlation_id: msg.correlation_id.clone(),
            },
        })
    } else if message_type == ack_topic {
        Some(Input::Ack {
            payload: msg.payload().clone(),
        })
    } else {
        None
    }
}

/// Publish one machine [`Publication`] on a topic, replaying the run's identity.
async fn publish(handler: &EmergentHandler, message_type: &str, publication: Publication) {
    let msg = EmergentMessage::new(message_type)
        .with_causation_id(publication.origin.causation_id)
        .with_correlation_id_option(publication.origin.correlation_id.as_ref())
        .with_payload(publication.payload);
    if let Err(e) = handler.publish(msg).await {
        tracing::warn!("Failed to publish {message_type}: {e}");
    }
}

/// Render an optional JSON value for a log line.
fn json_or_absent(value: Option<&serde_json::Value>) -> String {
    value.map_or_else(|| "<absent>".to_string(), serde_json::Value::to_string)
}

/// Resolve publish message types from the `EMERGENT_PUBLISHES` environment variable.
///
/// Maps `EMERGENT_PUBLISHES` (comma-separated, set by the engine) positionally to defaults.
fn resolve_publish_types_from_env(defaults: &[&str]) -> Vec<String> {
    if let Ok(publishes) = std::env::var("EMERGENT_PUBLISHES") {
        let env_types: Vec<&str> = publishes.split(',').filter(|s| !s.is_empty()).collect();
        defaults
            .iter()
            .enumerate()
            .map(|(i, default)| env_types.get(i).unwrap_or(default).to_string())
            .collect()
    } else {
        defaults.iter().map(|s| s.to_string()).collect()
    }
}
