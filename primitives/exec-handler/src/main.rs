//! Exec Handler
//!
//! A Handler that subscribes to events, pipes the incoming payload through an
//! external executable's stdin, and publishes the executable's stdout as a new event.
//!
//! This enables arbitrary command-line tools (jq, claude, python scripts, etc.)
//! to participate in Emergent workflows without writing a custom primitive.
//!
//! # Data Flow
//!
//! 1. Receive an event matching configured subscriptions
//! 2. Serialize the event payload as JSON
//! 3. Spawn the command and write the payload to its stdin
//! 4. Capture stdout and publish as a new event
//! 5. On failure, publish an error event
//!
//! # Concurrency
//!
//! `--max-concurrent` bounds how many commands run at once (default: 1).
//! At 1, messages are processed strictly in arrival order. Above 1, up to N
//! commands run simultaneously and outputs are published as executions
//! complete — ordering is by completion, not arrival. Causation and
//! correlation stamps keep interleaved outputs traceable either way.
//!
//! The subscription stream is only pulled when an execution slot is free, so
//! a burst of queued events exerts backpressure on the engine instead of
//! spawning unbounded processes. On shutdown, in-flight executions finish
//! (bounded by `--timeout`) and publish their results before disconnecting.
//!
//! # Failure Semantics
//!
//! Every failed execution — non-zero exit, timeout, spawn failure — publishes
//! an error event. The only silent outcomes are:
//!
//! - exit 0 with empty stdout (the command filtered the message out)
//! - a non-zero exit code listed in `--silent-exit-codes` with empty stderr
//!   (e.g. `jq -e 'select(...)'` dropping a message by exiting 1)
//!
//! A failure can therefore never vanish without a trace unless the operator
//! explicitly declared that exit code a filter.
//!
//! # Tracing
//!
//! Published messages inherit the inbound message's `correlation_id`: a
//! transformation belongs to the same logical request as its input, so a
//! correlation seeded at the pipeline's ingress survives every hop without
//! being carried in the payload.
//!
//! The command itself receives the envelope through its environment
//! (`EMERGENT_MESSAGE_ID`, `EMERGENT_CORRELATION_ID`, ...) — only the payload
//! reaches stdin, so this is how a jq or shell step reads tracing identifiers.
//!
//! # Messages Published
//!
//! - Configurable success type (default: `exec.output`) — stdout from the command
//! - Configurable error type (default: `exec.error`) — on non-zero exit or timeout
//!
//! # Usage
//!
//! ```bash
//! # Pipe events through jq
//! exec-handler --publish-as data.transformed -- jq '.data | keys'
//!
//! # Pipe events through claude, four at a time
//! exec-handler --publish-as ai.analysis --timeout 60000 --max-concurrent 4 -- claude -p "Analyze this"
//!
//! # Filter with jq -e: exit 1 with no stderr drops the message silently
//! exec-handler --silent-exit-codes 1 -- jq -e 'select(.level == "error")'
//!
//! # Use defaults
//! exec-handler -- my-transform-script
//! ```

use std::sync::Arc;

use clap::Parser;
use emergent_client::{EmergentHandler, EmergentMessage};
use exec_common::{ExecError, ExecResult, MessageEnv, error_to_json, execute_command};
use serde_json::json;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Exec Handler — pipe event payloads through an executable.
///
/// Everything after `--` is the command and its arguments.
#[derive(Parser, Debug)]
#[command(name = "exec_handler")]
#[command(about = "Pipe event payloads through an executable and publish results")]
#[command(trailing_var_arg = true)]
struct Args {
    /// Message types to subscribe to.
    #[arg(short, long = "subscribe", required = true)]
    subscribe: Vec<String>,

    /// Message type for successful output.
    #[arg(long, default_value = "exec.output")]
    publish_as: String,

    /// Message type for error output.
    #[arg(short, long, default_value = "exec.error")]
    error_as: String,

    /// Per-execution timeout in milliseconds.
    #[arg(short, long, default_value = "30000")]
    timeout: u64,

    /// Maximum number of commands running at once.
    ///
    /// At 1 (the default), messages are processed serially in arrival order.
    /// Above 1, outputs are published as executions complete, so ordering is
    /// by completion rather than arrival.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..))]
    max_concurrent: u16,

    /// Exit codes treated as a silent filter when stderr is empty (comma-separated).
    ///
    /// A command exiting with a listed code and writing nothing to stderr
    /// publishes nothing — the message is dropped, e.g.
    /// `--silent-exit-codes 1 -- jq -e 'select(.level == "error")'`.
    /// Any other failure publishes an error event.
    #[arg(long, value_delimiter = ',')]
    silent_exit_codes: Vec<i32>,

    /// The command and arguments to execute (after --).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

/// What a finished execution publishes.
enum Outcome {
    /// Publish stdout as a success event.
    Publish(ExecResult),
    /// Publish nothing — the command filtered the message out.
    Drop,
    /// Publish an error event.
    Error(ExecError),
}

/// Decide what a finished execution publishes.
///
/// Exit 0 with empty stdout is always a silent filter. A non-zero exit is
/// silent only when the operator listed the code in `--silent-exit-codes`
/// and the command wrote nothing to stderr; every other failure publishes an
/// error event so it cannot vanish without a trace.
fn classify(result: Result<Option<ExecResult>, ExecError>, silent_exit_codes: &[i32]) -> Outcome {
    match result {
        Ok(Some(result)) => Outcome::Publish(result),
        Ok(None) => Outcome::Drop,
        Err(ExecError::Failed {
            exit_code,
            ref stderr,
            ..
        }) if silent_exit_codes.contains(&exit_code) && stderr.trim().is_empty() => Outcome::Drop,
        Err(err) => Outcome::Error(err),
    }
}

/// Everything an execution task needs, shared across concurrent tasks.
struct ExecContext {
    command: Vec<String>,
    timeout: u64,
    publish_as: String,
    error_as: String,
    silent_exit_codes: Vec<i32>,
    handler: Arc<EmergentHandler>,
}

/// Execute the command for one message and publish the outcome.
async fn run_and_publish(msg: EmergentMessage, ctx: &ExecContext) {
    let message_env = MessageEnv::from_message(&msg);

    let result = execute_command(msg.payload(), &ctx.command, ctx.timeout, &message_env).await;

    match classify(result, &ctx.silent_exit_codes) {
        Outcome::Publish(result) => {
            let mut output = EmergentMessage::new(&ctx.publish_as)
                .with_causation_id(msg.id())
                .with_correlation_id_option(msg.correlation_id.as_ref())
                .with_payload(result.stdout_payload);

            if let Some(stderr) = result.stderr {
                output = output.with_metadata(json!({"stderr": stderr}));
            }

            if let Err(e) = ctx.handler.publish(output).await {
                eprintln!("failed to publish output caused by {}: {e}", msg.id());
            }
        }
        Outcome::Drop => {
            // The command filtered the message out — publish nothing.
        }
        Outcome::Error(exec_err) => {
            let error_msg = EmergentMessage::new(&ctx.error_as)
                .with_causation_id(msg.id())
                .with_correlation_id_option(msg.correlation_id.as_ref())
                .with_payload(error_to_json(&exec_err, msg.payload()));

            if let Err(e) = ctx.handler.publish(error_msg).await {
                eprintln!("failed to publish error caused by {}: {e}", msg.id());
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Resolve publish types: EMERGENT_PUBLISHES env > CLI args > defaults
    let publish_types =
        exec_common::resolve_publish_types_from_env(&[&args.publish_as, &args.error_as]);

    // Validate that a command was provided after --
    if args.command.is_empty() {
        eprintln!("Error: no command specified. Provide a command after '--'.");
        eprintln!("Example: exec-handler --publish-as data.out -- jq '.'");
        std::process::exit(1);
    }

    // Get the handler name from environment (set by engine) or use default
    let name = std::env::var("EMERGENT_NAME").unwrap_or_else(|_| "exec_handler".to_string());

    // Connect to the Emergent engine
    let mut handler = match EmergentHandler::connect(&name).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Failed to connect to Emergent engine: {e}");
            std::process::exit(1);
        }
    };

    // Subscribe to configured topics
    let topics_refs: Vec<&str> = args.subscribe.iter().map(String::as_str).collect();
    let mut stream = match handler.subscribe(&topics_refs).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to subscribe: {e}");
            std::process::exit(1);
        }
    };

    let handler = Arc::new(handler);
    let ctx = Arc::new(ExecContext {
        command: args.command,
        timeout: args.timeout,
        publish_as: publish_types[0].clone(),
        error_as: publish_types[1].clone(),
        silent_exit_codes: args.silent_exit_codes,
        handler: Arc::clone(&handler),
    });

    // Bounds concurrent executions; a permit is held for the life of each task.
    let semaphore = Arc::new(Semaphore::new(usize::from(args.max_concurrent)));
    let mut tasks: JoinSet<()> = JoinSet::new();

    // Set up SIGTERM handler for graceful shutdown
    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = sigterm.recv() => break,

            // Reap finished executions so the JoinSet doesn't grow unbounded.
            Some(joined) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(e) = joined {
                    eprintln!("execution task failed: {e}");
                }
            }

            // Acquire a slot BEFORE pulling a message: a burst of queued
            // events backpressures the engine instead of spawning unbounded
            // processes. Both awaits are cancel-safe — a permit dropped by a
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
                    run_and_publish(msg, &ctx).await;
                });
            }
        }
    }

    // Drain in-flight executions (bounded by --timeout each) so their outputs
    // and error events are published before the connection closes.
    while let Some(joined) = tasks.join_next().await {
        if let Err(e) = joined {
            eprintln!("execution task failed: {e}");
        }
    }

    let _ = handler.disconnect().await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(exit_code: i32, stderr: &str) -> Result<Option<ExecResult>, ExecError> {
        Err(ExecError::Failed {
            exit_code,
            stderr: stderr.to_string(),
            command: "cmd".to_string(),
        })
    }

    #[test]
    fn success_with_output_publishes() {
        let result = Ok(Some(ExecResult {
            stdout_payload: json!({"ok": true}),
            stderr: None,
        }));
        assert!(matches!(classify(result, &[]), Outcome::Publish(_)));
    }

    #[test]
    fn empty_stdout_drops_silently() {
        assert!(matches!(classify(Ok(None), &[]), Outcome::Drop));
    }

    #[test]
    fn nonzero_exit_with_empty_stderr_is_an_error_by_default() {
        // The dulled edge: a failure with no stderr must not vanish unless
        // the operator explicitly declared its exit code a filter.
        assert!(matches!(classify(failed(1, ""), &[]), Outcome::Error(_)));
    }

    #[test]
    fn listed_exit_code_with_empty_stderr_drops() {
        assert!(matches!(classify(failed(1, ""), &[1]), Outcome::Drop));
    }

    #[test]
    fn whitespace_only_stderr_counts_as_empty() {
        assert!(matches!(classify(failed(1, "  \n"), &[1]), Outcome::Drop));
    }

    #[test]
    fn listed_exit_code_with_stderr_is_an_error() {
        // stderr content is diagnostic output, not a filter decision.
        assert!(matches!(
            classify(failed(1, "boom"), &[1]),
            Outcome::Error(_)
        ));
    }

    #[test]
    fn unlisted_exit_code_is_an_error() {
        assert!(matches!(classify(failed(2, ""), &[1]), Outcome::Error(_)));
    }

    #[test]
    fn timeout_is_always_an_error() {
        let result = Err(ExecError::Timeout {
            command: "cmd".to_string(),
        });
        assert!(matches!(classify(result, &[1]), Outcome::Error(_)));
    }

    #[test]
    fn spawn_failure_is_always_an_error() {
        let result = Err(ExecError::SpawnFailed {
            error: "not found".to_string(),
            command: "cmd".to_string(),
        });
        assert!(matches!(classify(result, &[1]), Outcome::Error(_)));
    }
}
