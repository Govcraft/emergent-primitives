//! Exec Source - Shell Command Executor
//!
//! A Source that executes shell commands and emits events for stdout, stderr, and exit codes.
//! Can run commands once or repeatedly on an interval.
//!
//! Sources are SILENT - they only produce domain messages.
//! All lifecycle events are published by the engine.
//!
//! # Usage
//!
//! ```bash
//! # Run command once
//! exec-source --command "ls -la"
//!
//! # Run command every 5 seconds
//! exec-source --command "date" --interval 5000
//!
//! # Run with arguments and custom working directory
//! exec-source --command "git" --args "status" --working-dir /path/to/repo
//! ```
//!
//! # Events Published
//!
//! - `exec.output` - stdout from command
//! - `exec.error` - stderr from command
//! - `exec.exit` - exit code
//!
//! # Tracing
//!
//! `--correlate` mints one correlation ID at startup and stamps it on every
//! message this source publishes; `--correlation-id` adopts an existing one so
//! a run spanning several engines keeps a single trail. The command itself
//! reads the same value from `EMERGENT_CORRELATION_ID`, which is how a shell
//! step records the ID the event store will key on.

use clap::Parser;
use emergent_client::types::CorrelationId;
use emergent_client::{EmergentMessage, EmergentSource};
use exec_common::MessageEnv;
use serde_json::json;
use std::time::Duration;
use tokio::{
    process::Command,
    signal::unix::{SignalKind, signal},
};

/// Command executor that emits output events.
#[derive(Parser, Debug, Clone)]
#[command(name = "exec-source")]
#[command(about = "Executes shell commands and emits output events")]
struct Args {
    /// Command to execute.
    #[arg(short, long, env = "EXEC_SOURCE_COMMAND")]
    command: String,

    /// Command arguments (space-separated).
    #[arg(short, long, env = "EXEC_SOURCE_ARGS")]
    args: Option<String>,

    /// Optional interval in milliseconds for repeated execution (0 = run once).
    #[arg(short, long, env = "EXEC_SOURCE_INTERVAL", default_value = "0")]
    interval: u64,

    /// Working directory for command execution.
    #[arg(short = 'd', long, env = "EXEC_SOURCE_WORKING_DIR")]
    working_dir: Option<String>,

    /// Shell to use (e.g., "bash", "sh").
    #[arg(short, long, env = "EXEC_SOURCE_SHELL")]
    shell: Option<String>,

    /// Mint one correlation ID at startup and stamp it on everything published.
    ///
    /// Ignored when `--correlation-id` supplies one to adopt.
    #[arg(long)]
    correlate: bool,

    /// Adopt an existing correlation ID (`cor_<uuid_v7>`) instead of minting one.
    ///
    /// This is how a run that spans more than one engine keeps a single trail:
    /// the parent passes its ID down and the child stamps the same value.
    #[arg(long, env = "EMERGENT_CORRELATION_ID")]
    correlation_id: Option<String>,
}

/// Payload for exec.output events.
#[derive(Debug, serde::Serialize)]
struct ExecOutputPayload {
    command: String,
    stdout: String,
    exit_code: i32,
}

/// Payload for exec.error events.
#[derive(Debug, serde::Serialize)]
struct ExecErrorPayload {
    command: String,
    stderr: String,
    exit_code: i32,
}

/// Payload for exec.exit events.
#[derive(Debug, serde::Serialize)]
struct ExecExitPayload {
    command: String,
    exit_code: i32,
}

/// Resolve the correlation ID this source stamps on everything it publishes.
///
/// An adopted ID wins over `--correlate` so a child engine joins the parent's
/// trail rather than starting a competing one. An unparsable ID is a
/// configuration error: silently minting a fresh one instead would split the
/// trail in exactly the case the operator was trying to join it.
fn resolve_correlation_id(args: &Args) -> Result<Option<CorrelationId>, String> {
    match args.correlation_id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => CorrelationId::parse(id)
            .map(Some)
            .map_err(|e| format!("invalid --correlation-id '{id}': {e}")),
        _ if args.correlate => Ok(Some(CorrelationId::new())),
        _ => Ok(None),
    }
}

/// Builds a tokio Command from args.
fn build_command(args: &Args) -> Command {
    let mut cmd = if let Some(ref shell) = args.shell {
        let mut c = Command::new(shell);
        c.arg("-c");

        // Build full command string
        let full_cmd = if let Some(ref cmd_args) = args.args {
            format!("{} {}", args.command, cmd_args)
        } else {
            args.command.clone()
        };

        c.arg(full_cmd);
        c
    } else {
        let mut c = Command::new(&args.command);

        // Add arguments if provided
        if let Some(ref cmd_args) = args.args {
            for arg in cmd_args.split_whitespace() {
                c.arg(arg);
            }
        }

        c
    };

    // Set working directory if provided
    if let Some(ref working_dir) = args.working_dir {
        cmd.current_dir(working_dir);
    }

    cmd
}

/// Executes command once and publishes output events.
async fn execute_command(
    args: &Args,
    source: &EmergentSource,
    publish_types: &[String],
    correlation_id: Option<&CorrelationId>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = build_command(args);

    // The command sees the correlation its output will be stamped with, so a
    // shell step can record the same ID the event store will key on.
    MessageEnv::with_correlation(correlation_id.map(ToString::to_string)).apply_to(&mut cmd);

    let output = cmd.output().await?;

    let exit_code = output.status.code().unwrap_or(-1);
    let command_str = args.command.clone();

    // Publish stdout if non-empty
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !stdout.trim().is_empty() {
        let payload = ExecOutputPayload {
            command: command_str.clone(),
            stdout,
            exit_code,
        };
        let message = EmergentMessage::new(&publish_types[0])
            .with_correlation_id_option(correlation_id)
            .with_payload(json!(payload));
        let _ = source.publish(message).await;
    }

    // Publish stderr if non-empty
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !stderr.trim().is_empty() {
        let payload = ExecErrorPayload {
            command: command_str.clone(),
            stderr,
            exit_code,
        };
        let message = EmergentMessage::new(&publish_types[1])
            .with_correlation_id_option(correlation_id)
            .with_payload(json!(payload));
        let _ = source.publish(message).await;
    }

    // Always publish exit event
    let payload = ExecExitPayload {
        command: command_str,
        exit_code,
    };
    let message = EmergentMessage::new(&publish_types[2])
        .with_correlation_id_option(correlation_id)
        .with_payload(json!(payload));
    let _ = source.publish(message).await;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Resolve publish types from EMERGENT_PUBLISHES env var or use defaults
    let publish_types =
        exec_common::resolve_publish_types_from_env(&["exec.output", "exec.error", "exec.exit"]);

    let correlation_id = match resolve_correlation_id(&args) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // Get the source name from environment (set by engine) or use default
    let name = std::env::var("EMERGENT_NAME").unwrap_or_else(|_| "exec-source".to_string());

    // Connect to the Emergent engine (silently - lifecycle events come from engine)
    let source = match EmergentSource::connect(&name).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to connect to Emergent engine: {e}");
            std::process::exit(1);
        }
    };

    // Set up SIGTERM handler for graceful shutdown
    let mut sigterm = signal(SignalKind::terminate())?;

    if args.interval == 0 {
        // Run once and exit
        execute_command(&args, &source, &publish_types, correlation_id.as_ref()).await?;
        let _ = source.disconnect().await;
    } else {
        // Run repeatedly on interval
        let mut interval = tokio::time::interval(Duration::from_millis(args.interval));

        loop {
            tokio::select! {
                _ = sigterm.recv() => {
                    let _ = source.disconnect().await;
                    break;
                }

                _ = interval.tick() => {
                    if let Err(e) =
                        execute_command(&args, &source, &publish_types, correlation_id.as_ref())
                            .await
                    {
                        eprintln!("Command execution failed: {e}");
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_with(correlate: bool, correlation_id: Option<&str>) -> Args {
        Args {
            command: "true".to_string(),
            args: None,
            interval: 0,
            working_dir: None,
            shell: None,
            correlate,
            correlation_id: correlation_id.map(ToString::to_string),
        }
    }

    #[test]
    fn no_flags_means_no_correlation() {
        let resolved = resolve_correlation_id(&args_with(false, None));
        assert!(matches!(resolved, Ok(None)));
    }

    #[test]
    fn correlate_mints_a_fresh_id() {
        let resolved = resolve_correlation_id(&args_with(true, None))
            .unwrap_or_else(|e| panic!("expected a minted id: {e}"))
            .unwrap_or_else(|| panic!("expected Some"));

        assert!(resolved.to_string().starts_with("cor_"));
    }

    #[test]
    fn adopted_id_wins_over_correlate() {
        let inherited = CorrelationId::new().to_string();

        let resolved = resolve_correlation_id(&args_with(true, Some(&inherited)))
            .unwrap_or_else(|e| panic!("expected the inherited id: {e}"))
            .unwrap_or_else(|| panic!("expected Some"));

        assert_eq!(resolved.to_string(), inherited);
    }

    #[test]
    fn blank_adopted_id_falls_back_to_minting() {
        // An unset shell variable expands to the empty string, which is absence
        // rather than a malformed value.
        let resolved = resolve_correlation_id(&args_with(true, Some("   ")))
            .unwrap_or_else(|e| panic!("expected a minted id: {e}"))
            .unwrap_or_else(|| panic!("expected Some"));

        assert!(resolved.to_string().starts_with("cor_"));
    }

    #[test]
    fn blank_adopted_id_without_correlate_is_no_correlation() {
        let resolved = resolve_correlation_id(&args_with(false, Some("")));
        assert!(matches!(resolved, Ok(None)));
    }

    #[test]
    fn malformed_adopted_id_is_an_error() {
        // Minting a replacement here would silently split the trail the
        // operator was trying to join.
        let resolved = resolve_correlation_id(&args_with(true, Some("pipe-42-20260727")));
        assert!(resolved.is_err(), "expected a configuration error");
    }

    #[test]
    fn wrong_prefix_typeid_is_an_error() {
        let msg_id = "msg_01h455vb4pex5vsknk084sn02q";
        let resolved = resolve_correlation_id(&args_with(false, Some(msg_id)));
        assert!(resolved.is_err(), "expected a prefix error");
    }
}
