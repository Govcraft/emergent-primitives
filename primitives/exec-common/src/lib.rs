//! Shared logic for Emergent exec primitives.
//!
//! Provides:
//! - `execute_command` / `execute_command_passthrough` — pipe JSON to stdin
//! - `MessageEnv` — expose envelope fields to the executed command
//! - `resolve_publish_types_from_env` — read `EMERGENT_PUBLISHES` env var

use emergent_client::EmergentMessage;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Envelope fields exported to an executed command's environment.
///
/// Exec primitives pipe only the message *payload* to a command's stdin, so the
/// envelope — message id, correlation, causation — is otherwise invisible to a
/// jq filter or shell step. Exporting it as environment variables lets a
/// zero-code pipeline read the same tracing identifiers the event store records,
/// without smuggling them through the payload.
///
/// | Variable | Source |
/// |----------|--------|
/// | `EMERGENT_MESSAGE_ID` | `message.id` |
/// | `EMERGENT_MESSAGE_TYPE` | `message.message_type` |
/// | `EMERGENT_MESSAGE_SOURCE` | `message.source` |
/// | `EMERGENT_CORRELATION_ID` | `message.correlation_id` |
/// | `EMERGENT_CAUSATION_ID` | `message.causation_id` |
#[derive(Debug, Default, Clone)]
pub struct MessageEnv {
    /// Value for `EMERGENT_MESSAGE_ID`.
    pub message_id: Option<String>,
    /// Value for `EMERGENT_MESSAGE_TYPE`.
    pub message_type: Option<String>,
    /// Value for `EMERGENT_MESSAGE_SOURCE`.
    pub message_source: Option<String>,
    /// Value for `EMERGENT_CORRELATION_ID`.
    pub correlation_id: Option<String>,
    /// Value for `EMERGENT_CAUSATION_ID`.
    pub causation_id: Option<String>,
}

impl MessageEnv {
    /// Build the environment for a command triggered by `message`.
    #[must_use]
    pub fn from_message(message: &EmergentMessage) -> Self {
        Self {
            message_id: Some(message.id.to_string()),
            message_type: Some(message.message_type.to_string()),
            message_source: Some(message.source.to_string()),
            correlation_id: message.correlation_id.as_ref().map(ToString::to_string),
            causation_id: message.causation_id.as_ref().map(ToString::to_string),
        }
    }

    /// Build an environment carrying only a correlation id.
    ///
    /// Sources have no triggering message; the correlation they stamp on what
    /// they publish is the only envelope field they can offer the command.
    #[must_use]
    pub fn with_correlation(correlation_id: Option<String>) -> Self {
        Self {
            correlation_id,
            ..Self::default()
        }
    }

    /// The variable names and values this environment resolves to.
    ///
    /// A `None` value means the variable is cleared, not left untouched.
    #[must_use]
    pub fn vars(&self) -> [(&'static str, Option<&String>); 5] {
        [
            ("EMERGENT_MESSAGE_ID", self.message_id.as_ref()),
            ("EMERGENT_MESSAGE_TYPE", self.message_type.as_ref()),
            ("EMERGENT_MESSAGE_SOURCE", self.message_source.as_ref()),
            ("EMERGENT_CORRELATION_ID", self.correlation_id.as_ref()),
            ("EMERGENT_CAUSATION_ID", self.causation_id.as_ref()),
        ]
    }

    /// Apply the variables to a command.
    ///
    /// Absent fields are *removed* rather than skipped. The engine forwards its
    /// own environment to every primitive, which forwards it again to the
    /// command — so an ambient `EMERGENT_CORRELATION_ID` would otherwise leak
    /// into a command handling an uncorrelated message and mislabel its output.
    pub fn apply_to(&self, cmd: &mut Command) {
        for (key, value) in self.vars() {
            match value {
                Some(value) => cmd.env(key, value),
                None => cmd.env_remove(key),
            };
        }
    }
}

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
/// Returns the parsed stdout as a JSON value on success, `None` if the command
/// produced no output (silent filter), or an error on failure.
pub async fn execute_command(
    payload: &serde_json::Value,
    command: &[String],
    timeout_ms: u64,
    message_env: &MessageEnv,
) -> Result<Option<ExecResult>, ExecError> {
    let command_str = command.join(" ");

    let mut builder = Command::new(&command[0]);
    builder
        .args(&command[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    message_env.apply_to(&mut builder);

    let mut child = builder.spawn().map_err(|e| ExecError::SpawnFailed {
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

    // Parse stdout: return None if empty, try JSON first, fall back to wrapped string
    let stdout_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout_str.is_empty() {
        return Ok(None);
    }

    let stdout_payload = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| serde_json::json!({"output": stdout_str}));

    let stderr = {
        let s = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    };

    Ok(Some(ExecResult {
        stdout_payload,
        stderr,
    }))
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
    message_env: &MessageEnv,
) -> Result<(), ExecError> {
    let command_str = command.join(" ");

    let mut builder = Command::new(&command[0]);
    builder
        .args(&command[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    message_env.apply_to(&mut builder);

    let mut child = builder.spawn().map_err(|e| ExecError::SpawnFailed {
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

/// Resolve publish message types from the `EMERGENT_PUBLISHES` environment variable.
///
/// The engine sets `EMERGENT_PUBLISHES` to a comma-separated list of message types
/// from the TOML config's `publishes` array. This function maps them positionally
/// to the provided defaults:
///
/// - If the env var is set, each position maps to the corresponding default
/// - If the env var is absent or a position is missing, the default is used
///
/// This allows the TOML config to be the single source of truth for message types.
pub fn resolve_publish_types_from_env(defaults: &[&str]) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_json_stdout_is_parsed_directly() {
        let payload = json!({"input": "hello"});
        let command = vec!["echo".to_string(), r#"{"result":"world"}"#.to_string()];

        let result = execute_command(&payload, &command, 5000, &MessageEnv::default())
            .await
            .unwrap_or_else(|_| panic!("expected success"))
            .unwrap_or_else(|| panic!("expected Some"));

        assert_eq!(result.stdout_payload, json!({"result": "world"}));
        assert!(result.stderr.is_none());
    }

    #[tokio::test]
    async fn test_plain_text_stdout_is_wrapped() {
        let payload = json!({"input": "hello"});
        let command = vec!["echo".to_string(), "plain text output".to_string()];

        let result = execute_command(&payload, &command, 5000, &MessageEnv::default())
            .await
            .unwrap_or_else(|_| panic!("expected success"))
            .unwrap_or_else(|| panic!("expected Some"));

        assert_eq!(
            result.stdout_payload,
            json!({"output": "plain text output"})
        );
    }

    #[tokio::test]
    async fn test_empty_stdout_returns_none() {
        let payload = json!({"input": "hello"});
        let command = vec!["cat".to_string(), "/dev/null".to_string()];

        let result = execute_command(&payload, &command, 5000, &MessageEnv::default())
            .await
            .unwrap_or_else(|_| panic!("expected success"));

        assert!(result.is_none(), "empty stdout should return None");
    }

    #[tokio::test]
    async fn test_nonzero_exit_returns_error() {
        let payload = json!({"input": "hello"});
        let command = vec!["false".to_string()];

        let result = execute_command(&payload, &command, 5000, &MessageEnv::default()).await;
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

        let result = execute_command(&payload, &command, 100, &MessageEnv::default()).await;
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

        let result = execute_command(&payload, &command, 5000, &MessageEnv::default())
            .await
            .unwrap_or_else(|_| panic!("expected success"))
            .unwrap_or_else(|| panic!("expected Some"));

        assert_eq!(result.stdout_payload, json!({"ok": true}));
        assert_eq!(result.stderr.as_deref(), Some("warning: something"));
    }

    #[tokio::test]
    async fn test_spawn_failure_returns_error() {
        let payload = json!({"input": "hello"});
        let command = vec!["nonexistent_command_that_should_not_exist".to_string()];

        let result = execute_command(&payload, &command, 5000, &MessageEnv::default()).await;
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

        let result = execute_command(&payload, &command, 5000, &MessageEnv::default())
            .await
            .unwrap_or_else(|_| panic!("expected success"))
            .unwrap_or_else(|| panic!("expected Some"));

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
    async fn test_message_env_is_exported_to_command() {
        let payload = json!({});
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            r#"jq -nc --arg c "$EMERGENT_CORRELATION_ID" --arg m "$EMERGENT_MESSAGE_ID" '{c: $c, m: $m}'"#
                .to_string(),
        ];
        let message_env = MessageEnv {
            message_id: Some("msg_01h455vb4pex5vsknk084sn02q".to_string()),
            correlation_id: Some("cor_01h455vb4pex5vsknk084sn02q".to_string()),
            ..MessageEnv::default()
        };

        let result = execute_command(&payload, &command, 5000, &message_env)
            .await
            .unwrap_or_else(|_| panic!("expected success"))
            .unwrap_or_else(|| panic!("expected Some"));

        assert_eq!(
            result.stdout_payload,
            json!({
                "c": "cor_01h455vb4pex5vsknk084sn02q",
                "m": "msg_01h455vb4pex5vsknk084sn02q",
            })
        );
    }

    #[test]
    fn test_absent_message_env_fields_are_marked_for_removal() {
        // The engine forwards its own environment to every primitive, which
        // forwards it again to the command. An absent field must resolve to
        // None so `apply` removes it, rather than letting an ambient value leak
        // into a command handling an uncorrelated message.
        let message_env = MessageEnv::default();
        let vars = message_env.vars();

        assert_eq!(vars.len(), 5);
        assert!(
            vars.iter().all(|(_, value)| value.is_none()),
            "default MessageEnv must clear every variable"
        );
    }

    #[test]
    fn test_message_env_vars_cover_the_documented_names() {
        let message_env = MessageEnv::with_correlation(Some("cor_x".to_string()));
        let named: Vec<&str> = message_env
            .vars()
            .iter()
            .filter(|(_, value)| value.is_some())
            .map(|(key, _)| *key)
            .collect();

        assert_eq!(named, vec!["EMERGENT_CORRELATION_ID"]);
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
