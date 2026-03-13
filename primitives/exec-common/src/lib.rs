//! Shared command execution logic for Emergent exec primitives.
//!
//! Provides the core `execute_command` function used by both `exec-handler`
//! and `exec-sink`. The function pipes a JSON payload to a command's stdin,
//! captures stdout/stderr, and returns structured results.

use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Result of a successful command execution.
pub struct ExecResult {
    /// Parsed stdout payload (JSON if valid, otherwise wrapped string).
    pub stdout_payload: serde_json::Value,
    /// Stderr output, if any.
    pub stderr: Option<String>,
}

/// Error from command execution.
pub enum ExecError {
    /// Command exited with non-zero status.
    Failed {
        exit_code: i32,
        stderr: String,
        command: String,
    },
    /// Command exceeded the timeout.
    Timeout { command: String },
    /// Failed to spawn the command.
    SpawnFailed { error: String, command: String },
    /// Failed to write to stdin.
    StdinFailed { error: String, command: String },
}

/// Execute a command, piping the payload JSON to its stdin and capturing stdout.
///
/// Returns the parsed stdout as a JSON value on success, or an error with
/// exit code and stderr on failure.
pub async fn execute_command(
    payload: &serde_json::Value,
    command: &[String],
    timeout_ms: u64,
) -> Result<ExecResult, ExecError> {
    let command_str = command.join(" ");

    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ExecError::SpawnFailed {
            error: e.to_string(),
            command: command_str.clone(),
        })?;

    // Write payload JSON to stdin, then close it.
    // Ignore broken pipe errors — the command may not read stdin.
    let payload_bytes = serde_json::to_vec(payload).unwrap_or_default();
    if let Some(mut stdin) = child.stdin.take()
        && let Err(e) = stdin.write_all(&payload_bytes).await
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(ExecError::StdinFailed {
            error: e.to_string(),
            command: command_str.clone(),
        });
    }
    // stdin is dropped above, closing the pipe

    // Wait for the process with timeout
    let output = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output())
        .await
        .map_err(|_| ExecError::Timeout {
            command: command_str.clone(),
        })?
        .map_err(|e| ExecError::SpawnFailed {
            error: e.to_string(),
            command: command_str.clone(),
        })?;

    let exit_code = output.status.code().unwrap_or(-1);

    if exit_code != 0 {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(ExecError::Failed {
            exit_code,
            stderr,
            command: command_str,
        });
    }

    // Parse stdout: try JSON first, fall back to wrapped string
    let stdout_payload = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        let stdout_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        serde_json::json!({"output": stdout_str})
    });

    let stderr = {
        let s = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    };

    Ok(ExecResult {
        stdout_payload,
        stderr,
    })
}

/// Execute a command with passthrough output (stdout/stderr go to the terminal).
///
/// Pipes the payload JSON to the command's stdin but lets stdout and stderr
/// inherit the parent process's terminal. This is the right choice for sink
/// primitives where the command's output IS the desired side effect (e.g.,
/// `jq .` for pretty-printing, `tee` for logging).
///
/// Returns `Ok(())` on success (exit code 0) or an `ExecError` on failure.
pub async fn execute_command_passthrough(
    payload: &serde_json::Value,
    command: &[String],
    timeout_ms: u64,
) -> Result<(), ExecError> {
    let command_str = command.join(" ");

    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| ExecError::SpawnFailed {
            error: e.to_string(),
            command: command_str.clone(),
        })?;

    // Write payload JSON to stdin, then close it.
    let payload_bytes = serde_json::to_vec(payload).unwrap_or_default();
    if let Some(mut stdin) = child.stdin.take()
        && let Err(e) = stdin.write_all(&payload_bytes).await
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(ExecError::StdinFailed {
            error: e.to_string(),
            command: command_str.clone(),
        });
    }

    // Wait for the process with timeout
    let status = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait())
        .await
        .map_err(|_| ExecError::Timeout {
            command: command_str.clone(),
        })?
        .map_err(|e| ExecError::SpawnFailed {
            error: e.to_string(),
            command: command_str.clone(),
        })?;

    let exit_code = status.code().unwrap_or(-1);

    if exit_code != 0 {
        return Err(ExecError::Failed {
            exit_code,
            stderr: String::new(),
            command: command_str,
        });
    }

    Ok(())
}

/// Build a JSON error payload from an `ExecError`.
pub fn error_to_json(err: &ExecError) -> serde_json::Value {
    match err {
        ExecError::Failed {
            exit_code,
            stderr,
            command,
        } => serde_json::json!({
            "exit_code": exit_code,
            "stderr": stderr,
            "command": command,
        }),
        ExecError::Timeout { command } => serde_json::json!({
            "exit_code": null,
            "stderr": "process timed out",
            "command": command,
        }),
        ExecError::SpawnFailed { error, command } => serde_json::json!({
            "exit_code": null,
            "stderr": error,
            "command": command,
        }),
        ExecError::StdinFailed { error, command } => serde_json::json!({
            "exit_code": null,
            "stderr": format!("stdin write failed: {error}"),
            "command": command,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_json_stdout_is_parsed_directly() {
        let payload = json!({"input": "hello"});
        let command = vec!["echo".to_string(), r#"{"result":"world"}"#.to_string()];

        let result = execute_command(&payload, &command, 5000).await;
        let result = result.unwrap_or_else(|_| panic!("expected success"));

        assert_eq!(result.stdout_payload, json!({"result": "world"}));
        assert!(result.stderr.is_none());
    }

    #[tokio::test]
    async fn test_plain_text_stdout_is_wrapped() {
        let payload = json!({"input": "hello"});
        let command = vec!["echo".to_string(), "plain text output".to_string()];

        let result = execute_command(&payload, &command, 5000).await;
        let result = result.unwrap_or_else(|_| panic!("expected success"));

        assert_eq!(
            result.stdout_payload,
            json!({"output": "plain text output"})
        );
    }

    #[tokio::test]
    async fn test_empty_stdout_produces_empty_output() {
        let payload = json!({"input": "hello"});
        let command = vec!["cat".to_string(), "/dev/null".to_string()];

        let result = execute_command(&payload, &command, 5000).await;
        let result = result.unwrap_or_else(|_| panic!("expected success"));

        assert_eq!(result.stdout_payload, json!({"output": ""}));
    }

    #[tokio::test]
    async fn test_nonzero_exit_returns_error() {
        let payload = json!({"input": "hello"});
        let command = vec!["false".to_string()];

        let result = execute_command(&payload, &command, 5000).await;
        assert!(result.is_err());

        if let Err(ExecError::Failed {
            exit_code, command, ..
        }) = result
        {
            assert_eq!(exit_code, 1);
            assert_eq!(command, "false");
        } else {
            panic!("expected ExecError::Failed");
        }
    }

    #[tokio::test]
    async fn test_timeout_kills_process() {
        let payload = json!({"input": "hello"});
        let command = vec!["sleep".to_string(), "10".to_string()];

        let result = execute_command(&payload, &command, 100).await;
        assert!(result.is_err());

        if let Err(ExecError::Timeout { command }) = result {
            assert_eq!(command, "sleep 10");
        } else {
            panic!("expected ExecError::Timeout");
        }
    }

    #[tokio::test]
    async fn test_stderr_on_success_is_captured() {
        let payload = json!({"input": "hello"});
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            r#"printf '{"ok":true}\n' && printf 'warning: something\n' >&2"#.to_string(),
        ];

        let result = execute_command(&payload, &command, 5000).await;
        let result = result.unwrap_or_else(|_| panic!("expected success"));

        assert_eq!(result.stdout_payload, json!({"ok": true}));
        assert_eq!(result.stderr.as_deref(), Some("warning: something"));
    }

    #[tokio::test]
    async fn test_spawn_failure_returns_error() {
        let payload = json!({"input": "hello"});
        let command = vec!["nonexistent_command_that_should_not_exist".to_string()];

        let result = execute_command(&payload, &command, 5000).await;
        assert!(result.is_err());

        if let Err(ExecError::SpawnFailed { command, .. }) = result {
            assert_eq!(command, "nonexistent_command_that_should_not_exist");
        } else {
            panic!("expected ExecError::SpawnFailed");
        }
    }

    #[tokio::test]
    async fn test_payload_is_piped_to_stdin() {
        let payload = json!({"name": "emergent"});
        let command = vec!["cat".to_string()];

        let result = execute_command(&payload, &command, 5000).await;
        let result = result.unwrap_or_else(|_| panic!("expected success"));

        assert_eq!(result.stdout_payload, json!({"name": "emergent"}));
    }

    #[tokio::test]
    async fn test_error_to_json_failed() {
        let err = ExecError::Failed {
            exit_code: 1,
            stderr: "bad input".to_string(),
            command: "my-cmd".to_string(),
        };
        let json = error_to_json(&err);
        assert_eq!(json["exit_code"], 1);
        assert_eq!(json["stderr"], "bad input");
        assert_eq!(json["command"], "my-cmd");
    }

    #[tokio::test]
    async fn test_error_to_json_timeout() {
        let err = ExecError::Timeout {
            command: "slow-cmd".to_string(),
        };
        let json = error_to_json(&err);
        assert!(json["exit_code"].is_null());
        assert_eq!(json["stderr"], "process timed out");
    }
}
