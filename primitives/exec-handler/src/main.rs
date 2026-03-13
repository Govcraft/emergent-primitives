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
//! # Pipe events through claude
//! exec-handler --publish-as ai.analysis --timeout 60000 -- claude -p "Analyze this"
//!
//! # Use defaults
//! exec-handler -- my-transform-script
//! ```

use clap::Parser;
use emergent_client::{EmergentHandler, EmergentMessage};
use exec_common::{error_to_json, execute_command};
use serde_json::json;
use tokio::signal::unix::{SignalKind, signal};

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

    /// The command and arguments to execute (after --).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Resolve publish types: EMERGENT_PUBLISHES env > CLI args > defaults
    let publish_types =
        exec_common::resolve_publish_types_from_env(&[&args.publish_as, &args.error_as]);
    let publish_as = &publish_types[0];
    let error_as = &publish_types[1];

    // Validate that a command was provided after --
    if args.command.is_empty() {
        eprintln!("Error: no command specified. Provide a command after '--'.");
        eprintln!("Example: exec-handler --publish-as data.out -- jq '.'");
        std::process::exit(1);
    }

    // Get the handler name from environment (set by engine) or use default
    let name = std::env::var("EMERGENT_NAME").unwrap_or_else(|_| "exec_handler".to_string());

    // Connect to the Emergent engine
    let handler = match EmergentHandler::connect(&name).await {
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

    // Set up SIGTERM handler for graceful shutdown
    let mut sigterm = signal(SignalKind::terminate())?;

    // Process incoming messages
    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                let _ = handler.disconnect().await;
                break;
            }

            msg = stream.next() => {
                match msg {
                    Some(msg) => {
                        match execute_command(msg.payload(), &args.command, args.timeout).await {
                            Ok(result) => {
                                let mut output = EmergentMessage::new(publish_as)
                                    .with_causation_id(msg.id())
                                    .with_payload(result.stdout_payload);

                                if let Some(stderr) = result.stderr {
                                    output = output.with_metadata(json!({"stderr": stderr}));
                                }

                                let _ = handler.publish(output).await;
                            }
                            Err(exec_err) => {
                                let error_msg = EmergentMessage::new(error_as)
                                    .with_causation_id(msg.id())
                                    .with_payload(error_to_json(&exec_err));

                                let _ = handler.publish(error_msg).await;
                            }
                        }
                    }
                    None => {
                        // Stream ended (graceful shutdown)
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
