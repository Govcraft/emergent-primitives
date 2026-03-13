//! Exec Sink - Pipe Event Payloads Through Executables
//!
//! A Sink that subscribes to events and pipes each payload through an external
//! command's stdin. The command's output is discarded (sink pattern — terminal
//! consumer). This makes any command-line tool a valid event consumer.
//!
//! # Examples
//!
//! ```bash
//! # Print payloads with jq (replaces console-sink)
//! exec-sink -s timer.tick -- jq .
//!
//! # POST payloads to a webhook (replaces http-sink for simple cases)
//! exec-sink -s alert.fired -- curl -s -X POST -H "Content-Type: application/json" -d @- https://hooks.example.com/webhook
//!
//! # Append payloads to a file
//! exec-sink -s data.processed -- tee -a /var/log/events.jsonl
//!
//! # Pipe through any script
//! exec-sink -s user.created -- ./scripts/send-welcome-email.sh
//! ```

use clap::Parser;
use emergent_client::EmergentSink;
use exec_common::{ExecError, execute_command_passthrough};
use tokio::signal::unix::{SignalKind, signal};

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

    /// The command and arguments to execute (after --).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
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

    let sink = match EmergentSink::connect(&name).await {
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

    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                let _ = sink.disconnect().await;
                break;
            }

            msg = stream.next() => {
                match msg {
                    Some(msg) => {
                        if let Err(err) = execute_command_passthrough(msg.payload(), &args.command, args.timeout).await {
                            let detail = match &err {
                                ExecError::Failed { command, exit_code, .. } => {
                                    format!("{command}: exit code {exit_code}")
                                }
                                ExecError::Timeout { command } => {
                                    format!("{command}: timed out")
                                }
                                ExecError::SpawnFailed { error, command } => {
                                    format!("{command}: {error}")
                                }
                                ExecError::StdinFailed { error, command } => {
                                    format!("{command}: stdin: {error}")
                                }
                            };
                            eprintln!("exec-sink: {detail}");
                        }
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}
