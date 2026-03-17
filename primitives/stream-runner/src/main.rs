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
//! # Messages Published
//!
//! - Configurable item type (default: `stream.item`) — one item per ack cycle
//! - Configurable end type (default: `stream.end`) — final count payload
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
use serde_json::{Value, json};
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

    /// JSON object key containing the array to stream (ignored when payload is a bare array)
    #[arg(long, default_value = "items")]
    items_key: String,
}

enum State {
    Idle,
    Streaming {
        items: Vec<Value>,
        next_index: usize,
        causation_id: CausationId,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let publish_types = resolve_publish_types_from_env(&[&args.publish_as, &args.end_topic]);
    let publish_as = publish_types[0].clone();
    let end_topic = publish_types[1].clone();

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
    let mut state = State::Idle;

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                let _ = handler.disconnect().await;
                break;
            }

            msg = stream.next() => match msg {
                None => break,
                Some(msg) if msg.message_type.as_str() == args.load_topic => {
                    handle_load(msg, &args, &mut state, &handler, &publish_as, &end_topic).await;
                }
                Some(msg) if msg.message_type.as_str() == args.ack_topic => {
                    handle_ack(&mut state, &handler, &publish_as, &end_topic).await;
                }
                Some(_) => {}
            }
        }
    }

    Ok(())
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

/// Extract the items array from a payload.
///
/// If `payload` is a bare array, returns it directly.
/// If `payload` is an object, looks up `items_key` and returns its array value.
/// Returns `Err` for any other shape.
fn extract_items(payload: &Value, items_key: &str) -> Result<Vec<Value>, String> {
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

async fn handle_load(
    msg: EmergentMessage,
    args: &Args,
    state: &mut State,
    handler: &EmergentHandler,
    publish_as: &str,
    end_topic: &str,
) {
    if matches!(state, State::Streaming { .. }) {
        tracing::warn!("Received load while already streaming, ignoring");
        return;
    }

    let payload = msg.payload().clone();
    let items = match extract_items(&payload, &args.items_key) {
        Ok(items) => items,
        Err(e) => {
            tracing::warn!("Failed to extract items from payload: {e}");
            return;
        }
    };

    let causation_id = CausationId::from(msg.id());

    if items.is_empty() {
        let end_msg = EmergentMessage::new(end_topic)
            .with_causation_id(causation_id)
            .with_payload(json!({"count": 0}));
        if let Err(e) = handler.publish(end_msg).await {
            tracing::warn!("Failed to publish end event for empty collection: {e}");
        }
        return;
    }

    let first_item = items[0].clone();
    *state = State::Streaming {
        items,
        next_index: 0,
        causation_id: causation_id.clone(),
    };
    emit_current(&first_item, &causation_id, handler, publish_as).await;
}

async fn handle_ack(
    state: &mut State,
    handler: &EmergentHandler,
    publish_as: &str,
    end_topic: &str,
) {
    let (emit_item, end_info) = match state {
        State::Idle => {
            tracing::debug!("Received ack while idle, ignoring");
            return;
        }
        State::Streaming {
            items,
            next_index,
            causation_id,
        } => {
            *next_index += 1;
            if *next_index < items.len() {
                (
                    Some((items[*next_index].clone(), causation_id.clone())),
                    None,
                )
            } else {
                (None, Some((items.len(), causation_id.clone())))
            }
        }
    };

    if let Some((item, cid)) = emit_item {
        emit_current(&item, &cid, handler, publish_as).await;
    } else if let Some((count, cid)) = end_info {
        *state = State::Idle;
        let end_msg = EmergentMessage::new(end_topic)
            .with_causation_id(cid)
            .with_payload(json!({"count": count}));
        if let Err(e) = handler.publish(end_msg).await {
            tracing::warn!("Failed to publish end event: {e}");
        }
    }
}

/// Emit the current item from a `Streaming` state at a single publish site.
async fn emit_current(
    item: &Value,
    causation_id: &CausationId,
    handler: &EmergentHandler,
    publish_as: &str,
) {
    let msg = EmergentMessage::new(publish_as)
        .with_causation_id(causation_id.clone())
        .with_payload(item.clone());
    if let Err(e) = handler.publish(msg).await {
        tracing::warn!("Failed to publish stream item: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
