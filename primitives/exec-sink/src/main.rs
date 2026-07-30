//! Exec Sink - Pipe Event Payloads Through Executables
//!
//! A Sink that subscribes to events and pipes each payload through an external
//! command's stdin. The command's output is discarded (sink pattern — terminal
//! consumer). This makes any command-line tool a valid event consumer.
//!
//! # Concurrency
//!
//! `--max-concurrent` bounds how many commands run at once (default: 1).
//! At 1, messages are consumed strictly in arrival order. Above 1, up to N
//! commands run simultaneously and side effects land in completion order.
//!
//! The subscription stream is only pulled when an execution slot is free, so
//! a burst of queued events exerts backpressure on the engine instead of
//! spawning unbounded processes. On shutdown, in-flight executions finish
//! (bounded by `--timeout`) before the sink disconnects.
//!
//! # Failure Reporting
//!
//! Sinks are terminal consumers — they cannot publish, so failures are
//! reported on stderr (captured in the engine's primitive logs), tagged with
//! the ID of the message that caused them. Exit codes listed in
//! `--silent-exit-codes` are not reported, e.g. a grep-style command that
//! signals "no match" by exiting 1. Unlike exec-handler, there is no
//! empty-stderr condition: passthrough mode hands stderr to the terminal
//! rather than capturing it.
//!
//! # Examples
//!
//! ```bash
//! # Print payloads with jq (replaces console-sink)
//! exec-sink -s timer.tick -- jq .
//!
//! # POST payloads to a webhook, four at a time
//! exec-sink -s alert.fired --max-concurrent 4 -- curl -s -X POST -H "Content-Type: application/json" -d @- https://hooks.example.com/webhook
//!
//! # Append payloads to a file
//! exec-sink -s data.processed -- tee -a /var/log/events.jsonl
//!
//! # Grep-style filter: exit 1 means "no match", not a failure
//! exec-sink -s log.line --silent-exit-codes 1 -- grep -q ERROR
//! ```

use std::sync::Arc;

use clap::Parser;
use emergent_client::EmergentSink;
use exec_common::{ExecError, MessageEnv, execute_command_passthrough};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Exec Sink — pipe event payloads through an executable.
///
/// Everything after `--` is the command and its arguments.
#[derive(Parser, Debug)]
#[command(name = "exec_sink")]
#[command(about = "Pipe event payloads through an executable (fire-and-forget)")]
#[command(trailing_var_arg = true)]
struct Args {
    /// Message types to subscribe to.
    #[arg(short, long = "subscribe", required = true)]
    subscribe: Vec<String>,

    /// Per-execution timeout in milliseconds.
    #[arg(short, long, default_value = "30000")]
    timeout: u64,

    /// Maximum number of commands running at once.
    ///
    /// At 1 (the default), messages are consumed serially in arrival order.
    /// Above 1, side effects land in completion order rather than arrival
    /// order.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..))]
    max_concurrent: u16,

    /// Exit codes that are not reported as failures (comma-separated).
    ///
    /// For commands that use a non-zero exit to signal something other than
    /// failure, e.g. `--silent-exit-codes 1 -- grep -q ERROR`. Timeouts and
    /// spawn failures are always reported.
    #[arg(long, value_delimiter = ',')]
    silent_exit_codes: Vec<i32>,

    /// The command and arguments to execute (after --).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

/// Render a failure for the error log, or `None` if the operator declared
/// this exit code non-reportable.
///
/// Timeouts, spawn failures, and stdin failures are always reported — only a
/// clean exit with a listed code can be silenced.
fn failure_detail(err: &ExecError, silent_exit_codes: &[i32]) -> Option<String> {
    match err {
        ExecError::Failed {
            command, exit_code, ..
        } => {
            if silent_exit_codes.contains(exit_code) {
                None
            } else {
                Some(format!("{command}: exit code {exit_code}"))
            }
        }
        ExecError::Timeout { command } => Some(format!("{command}: timed out")),
        ExecError::SpawnFailed { error, command } => Some(format!("{command}: {error}")),
        ExecError::StdinFailed { error, command } => Some(format!("{command}: stdin: {error}")),
    }
}

/// Everything an execution task needs, shared across concurrent tasks.
struct ExecContext {
    command: Vec<String>,
    timeout: u64,
    silent_exit_codes: Vec<i32>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.command.is_empty() {
        eprintln!("Error: no command specified. Provide a command after '--'.");
        eprintln!("Example: exec-sink --subscribe timer.tick -- jq .");
        std::process::exit(1);
    }

    // Get the sink name from environment (set by engine) or use default
    let name = std::env::var("EMERGENT_NAME").unwrap_or_else(|_| "exec_sink".to_string());

    let mut sink = match EmergentSink::connect(&name).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to connect to Emergent engine: {e}");
            std::process::exit(1);
        }
    };

    let topics_refs: Vec<&str> = args.subscribe.iter().map(String::as_str).collect();
    let mut stream = match sink.subscribe(&topics_refs).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to subscribe: {e}");
            std::process::exit(1);
        }
    };

    let ctx = Arc::new(ExecContext {
        command: args.command,
        timeout: args.timeout,
        silent_exit_codes: args.silent_exit_codes,
    });

    // Bounds concurrent executions; a permit is held for the life of each task.
    let semaphore = Arc::new(Semaphore::new(usize::from(args.max_concurrent)));
    let mut tasks: JoinSet<()> = JoinSet::new();

    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = sigterm.recv() => break,

            // Reap finished executions so the JoinSet doesn't grow unbounded.
            Some(joined) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(e) = joined {
                    eprintln!("exec-sink: execution task failed: {e}");
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
                    let message_env = MessageEnv::from_message(&msg);

                    if let Err(err) = execute_command_passthrough(
                        msg.payload(),
                        &ctx.command,
                        ctx.timeout,
                        &message_env,
                    )
                    .await
                        && let Some(detail) = failure_detail(&err, &ctx.silent_exit_codes)
                    {
                        eprintln!("exec-sink: {detail} (caused by {})", msg.id());
                    }
                });
            }
        }
    }

    // Drain in-flight executions (bounded by --timeout each) before the
    // connection closes.
    while let Some(joined) = tasks.join_next().await {
        if let Err(e) = joined {
            eprintln!("exec-sink: execution task failed: {e}");
        }
    }

    let _ = sink.disconnect().await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(exit_code: i32) -> ExecError {
        ExecError::Failed {
            exit_code,
            stderr: String::new(),
            command: "cmd".to_string(),
        }
    }

    #[test]
    fn nonzero_exit_is_reported_by_default() {
        let detail = failure_detail(&failed(1), &[]);
        assert_eq!(detail.as_deref(), Some("cmd: exit code 1"));
    }

    #[test]
    fn listed_exit_code_is_not_reported() {
        assert!(failure_detail(&failed(1), &[1]).is_none());
    }

    #[test]
    fn unlisted_exit_code_is_reported() {
        let detail = failure_detail(&failed(2), &[1]);
        assert_eq!(detail.as_deref(), Some("cmd: exit code 2"));
    }

    #[test]
    fn timeout_is_always_reported() {
        let err = ExecError::Timeout {
            command: "cmd".to_string(),
        };
        assert_eq!(
            failure_detail(&err, &[1]).as_deref(),
            Some("cmd: timed out")
        );
    }

    #[test]
    fn spawn_failure_is_always_reported() {
        let err = ExecError::SpawnFailed {
            error: "not found".to_string(),
            command: "cmd".to_string(),
        };
        assert_eq!(
            failure_detail(&err, &[1]).as_deref(),
            Some("cmd: not found")
        );
    }

    #[test]
    fn stdin_failure_is_always_reported() {
        let err = ExecError::StdinFailed {
            error: "closed".to_string(),
            command: "cmd".to_string(),
        };
        assert_eq!(
            failure_detail(&err, &[1]).as_deref(),
            Some("cmd: stdin: closed")
        );
    }
}
