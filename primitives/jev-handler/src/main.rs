//! Jev Handler
//!
//! A Handler that subscribes to events, asks the TypeSafe System One API a
//! fixed set of typed questions about each inbound payload, and publishes the
//! answers as a new event.
//!
//! This puts programmable common sense — "is this a phishing attempt", "how
//! urgent is this", "which team owns it" — into an Emergent workflow without
//! writing a custom primitive for each judgement.
//!
//! # Data Flow
//!
//! 1. Receive an event matching configured subscriptions
//! 2. Select the request `state` from the payload (`--state-pointer`)
//! 3. POST the state and the questions file's questions, once, with retries
//! 4. Publish the answers as a new event
//! 5. On failure, publish an error event
//!
//! # No Routing
//!
//! This primitive publishes exactly two message types and makes no decision
//! about the answers. Confidence banding is topology, not code: an
//! `exec-handler` running a `jq` selector over the published `answers` turns
//! `jev.answered` into whatever message types the workflow needs. That is why
//! `answers` is passed through in the API's exact JSON shape — a downstream
//! `select(.answers.kind.confidence >= 0.9)` only works if this primitive
//! leaves the shape alone.
//!
//! # Concurrency
//!
//! `--max-concurrent` bounds how many messages are in flight at once
//! (default: 1). The subscription stream is only pulled when a slot is free, so
//! a burst of queued events backpressures the engine instead of firing
//! unbounded API calls.
//!
//! **A task waiting out a retry backoff holds its slot.** At
//! `--max-concurrent 1` a `429` storm therefore stalls the handler until the
//! rate limit clears. That is correct behaviour against a rate limiter — it is
//! honest backpressure — but an operator who does not expect it will read it as
//! a hang. For an IO-bound API call, a higher `--max-concurrent` is usually
//! right.
//!
//! On shutdown, in-flight messages finish (each bounded by `--timeout`) and
//! publish their results before disconnecting.
//!
//! # Failure Semantics
//!
//! Every failure publishes an error event; nothing is ever dropped silently.
//! The `error.kind` field is the contract a downstream router selects on:
//! `auth`, `invalid_request`, `rate_limited`, `server_error`, `transport`,
//! `timeout`, `bad_response`, `answer_contract`, `state_not_found`.
//!
//! # Tracing
//!
//! Published messages inherit the inbound message's `correlation_id` and carry
//! its `causation_id`, exactly as `exec-handler` does. They also carry the
//! API's own `request_id`, so a support conversation about one call is joinable
//! to the exact Emergent message.
//!
//! # Credentials
//!
//! The API key is read from `TYPESAFE_API_KEY` and nowhere else. There is no
//! `--api-key` flag: a key on a command line is a key in the process table and
//! in the engine's configuration file.
//!
//! # Messages Published
//!
//! - Configurable success type (default: `jev.answered`) — `{input, answers,
//!   usage, model, request_id}`
//! - Configurable error type (default: `jev.error`) — the inbound payload with
//!   the failure under a reserved `error` key
//!
//! # Usage
//!
//! ```bash
//! # Judge every inbound email
//! jev-handler -s email.received --questions /etc/emergent/triage.toml
//!
//! # Ask about one field, four messages at a time
//! jev-handler -s ticket.opened --questions ./q.toml \
//!     --state-pointer /body --max-concurrent 4
//! ```

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use emergent_client::{EmergentHandler, EmergentMessage};
use jev_handler::args::{Args, Config, ModelName, RetryPolicy, StatePointer};
use jev_handler::client::JevClient;
use jev_handler::error::RequestFailure;
use jev_handler::payload::{FailureContext, error_payload, success_payload};
use jev_handler::questions::{QuestionSet, load_questions};
use jev_handler::request::{build_request_body, select_state};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Everything a message task needs, shared across concurrent tasks.
struct JevContext {
    client: JevClient,
    questions: QuestionSet,
    model: ModelName,
    state_pointer: StatePointer,
    retry: RetryPolicy,
    budget: Duration,
    publish_as: String,
    error_as: String,
    handler: Arc<EmergentHandler>,
}

impl JevContext {
    /// The configuration a failure is described against.
    fn failure_context(&self) -> FailureContext<'_> {
        FailureContext {
            endpoint: self.client.endpoint(),
            model: &self.model,
        }
    }
}

/// Ask the questions about one message and publish the outcome.
async fn ask_and_publish(msg: EmergentMessage, ctx: &JevContext) {
    let result = ask(&msg, ctx).await;

    let outbound = match &result {
        Ok(response) => EmergentMessage::new(&ctx.publish_as)
            .with_payload(success_payload(msg.payload(), response)),
        Err(failure) => EmergentMessage::new(&ctx.error_as).with_payload(error_payload(
            msg.payload(),
            failure,
            &ctx.failure_context(),
        )),
    };

    let outbound = outbound
        .with_causation_id(msg.id())
        .with_correlation_id_option(msg.correlation_id.as_ref());

    if let Err(e) = ctx.handler.publish(outbound).await {
        tracing::error!("failed to publish result caused by {}: {e}", msg.id());
    }
}

/// The per-message sequence: select state, build the body, ask, bounded by the
/// whole-message budget.
async fn ask(
    msg: &EmergentMessage,
    ctx: &JevContext,
) -> Result<jev_handler::response::ApiResponse, RequestFailure> {
    let state = select_state(msg.payload(), &ctx.state_pointer)?;
    let body = build_request_body(state, &ctx.model, &ctx.questions);
    let seed = msg.id().to_string();

    // `--timeout` bounds the whole retry sequence, not one attempt; the
    // per-attempt bound is `--request-timeout`, inside the client.
    tokio::time::timeout(
        ctx.budget,
        ctx.client.ask(&body, &seed, &ctx.retry, &ctx.questions),
    )
    .await
    .unwrap_or_else(|_| {
        Err(RequestFailure::Timeout {
            budget_ms: budget_millis(ctx.budget),
        })
    })
}

/// The budget in milliseconds, for the error payload.
fn budget_millis(budget: Duration) -> u64 {
    u64::try_from(budget.as_millis()).unwrap_or(u64::MAX)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    // Everything that can be wrong is checked before the engine is connected,
    // so a misconfigured primitive never appears healthy in a topology.
    let config = match Config::from_args(&args) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    let questions = match load_questions(&args.questions) {
        Ok(questions) => questions,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    let client = match JevClient::new(config.endpoint, config.api_key, config.request_timeout) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Failed to build the HTTP client: {e}");
            std::process::exit(1);
        }
    };

    // Resolve publish types: EMERGENT_PUBLISHES env > CLI args > defaults
    let publish_types =
        exec_common::resolve_publish_types_from_env(&[&args.publish_as, &args.error_as]);

    // Get the handler name from environment (set by engine) or use default
    let name = std::env::var("EMERGENT_NAME").unwrap_or_else(|_| "jev_handler".to_string());

    let mut handler = match EmergentHandler::connect(&name).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Failed to connect to Emergent engine: {e}");
            std::process::exit(1);
        }
    };

    let topics_refs: Vec<&str> = args.subscribe.iter().map(String::as_str).collect();
    let mut stream = match handler.subscribe(&topics_refs).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to subscribe: {e}");
            std::process::exit(1);
        }
    };

    let handler = Arc::new(handler);
    let ctx = Arc::new(JevContext {
        client,
        questions,
        model: config.model,
        state_pointer: config.state_pointer,
        retry: config.retry,
        budget: config.budget,
        publish_as: publish_types[0].clone(),
        error_as: publish_types[1].clone(),
        handler: Arc::clone(&handler),
    });

    // Bounds messages in flight; a permit is held for the life of each task,
    // retry backoff included.
    let semaphore = Arc::new(Semaphore::new(usize::from(args.max_concurrent)));
    let mut tasks: JoinSet<()> = JoinSet::new();

    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = sigterm.recv() => break,

            // Reap finished tasks so the JoinSet doesn't grow unbounded.
            Some(joined) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(e) = joined {
                    tracing::error!("message task failed: {e}");
                }
            }

            // Acquire a slot BEFORE pulling a message: a burst of queued
            // events backpressures the engine instead of firing unbounded API
            // calls. Both awaits are cancel-safe — a permit dropped by a
            // competing branch is simply reacquired, and no message is lost.
            (permit, msg) = async {
                let permit = Arc::clone(&semaphore).acquire_owned().await;
                let msg = stream.next().await;
                (permit, msg)
            } => {
                // The semaphore is never closed, and `None` means the stream
                // ended (graceful shutdown).
                let (Ok(permit), Some(msg)) = (permit, msg) else {
                    break;
                };

                let ctx = Arc::clone(&ctx);
                tasks.spawn(async move {
                    let _slot = permit;
                    ask_and_publish(msg, &ctx).await;
                });
            }
        }
    }

    // Drain in-flight messages (bounded by --timeout each) so their answers and
    // error events are published before the connection closes.
    while let Some(joined) = tasks.join_next().await {
        if let Err(e) = joined {
            tracing::error!("message task failed: {e}");
        }
    }

    let _ = handler.disconnect().await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_budget_renders_to_whole_milliseconds() {
        assert_eq!(budget_millis(Duration::from_millis(120_000)), 120_000);
        assert_eq!(budget_millis(Duration::MAX), u64::MAX);
    }
}
