//! Console Sink - Console Output for Emergent Messages
//!
//! A Sink that subscribes to message types and outputs their payloads to stdout.
//! Useful for debugging, monitoring, and development workflows.
//!
//! # Usage
//!
//! ```bash
//! # Subscribe to specific message types
//! console-sink --subscribe "timer.tick" --subscribe "user.created"
//!
//! # Pretty-print JSON output
//! console-sink -s "timer.tick" --pretty
//!
//! # Include timestamps in output
//! console-sink -s "timer.tick" --timestamp
//! ```

use clap::Parser;
use emergent_client::helpers::run_sink;

/// Console output sink for Emergent messages.
#[derive(Parser, Debug)]
#[command(name = "console-sink")]
#[command(about = "Outputs message payloads to the console")]
struct Args {
    /// Message types to subscribe to.
    #[arg(short, long = "subscribe", required = true)]
    subscribe: Vec<String>,

    /// Pretty-print JSON output.
    #[arg(short, long, env = "CONSOLE_SINK_PRETTY")]
    pretty: bool,

    /// Include timestamps in output.
    #[arg(short, long, env = "CONSOLE_SINK_TIMESTAMP")]
    timestamp: bool,
}

/// Formats the payload for console output.
fn format_payload(payload: &serde_json::Value, pretty: bool) -> String {
    if pretty {
        serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string())
    } else {
        payload.to_string()
    }
}

/// Formats the current timestamp.
fn format_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let secs = now / 1000;
    let millis = now % 1000;
    format!("{secs}.{millis:03}")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let subscriptions: Vec<&str> = args.subscribe.iter().map(String::as_str).collect();
    let pretty = args.pretty;
    let timestamp = args.timestamp;

    run_sink(None, &subscriptions, |msg| async move {
        let payload = msg.payload();
        let formatted = format_payload(payload, pretty);

        if timestamp {
            let ts = format_timestamp();
            println!("[{ts}] {formatted}");
        } else {
            println!("{formatted}");
        }

        Ok(())
    })
    .await?;

    Ok(())
}
