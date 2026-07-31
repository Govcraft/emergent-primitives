//! Shared logic for Emergent exec primitives.
//!
//! Provides:
//! - `execute_command` / `execute_command_passthrough` — pipe JSON to stdin
//! - `MessageEnv` — expose envelope fields to the executed command
//! - `resolve_publish_types_from_env` — read `EMERGENT_PUBLISHES` env var

use emergent_client::EmergentMessage;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

/// How long a timed-out process group may run after `SIGTERM` before `SIGKILL`.
///
/// A command that holds real state — a checked-out worktree, a half-written
/// file — deserves the chance to finish that write. A command that ignores
/// `SIGTERM` does not deserve to be waited for indefinitely.
const TERM_GRACE: Duration = Duration::from_secs(5);

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

/// Put the command in a new process group of its own.
///
/// A timeout must be able to reclaim everything the command started, not just
/// the command. The usual shape here is a shell that spawns the real work:
/// signalling only the direct child orphans its children, which keep running
/// unsupervised. Giving the command its own group makes the whole tree
/// addressable by a single signal to the negated group id.
///
/// On platforms without process groups this is a no-op, and termination falls
/// back to killing the direct child.
fn isolate_process_group(builder: &mut Command) {
    #[cfg(unix)]
    builder.process_group(0);
    #[cfg(not(unix))]
    builder.kill_on_drop(true);
}

/// Terminate a running child and everything it started.
///
/// Sends `SIGTERM` to the child's process group, waits up to [`TERM_GRACE`] for
/// it to exit, then sends `SIGKILL`. Reaps the child either way, so the caller
/// leaves no zombie behind.
#[cfg(unix)]
async fn terminate_tree(child: &mut Child) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    // `id()` is None once the child has been reaped — nothing left to signal.
    let Some(pid) = child.id() else { return };
    let Ok(raw) = i32::try_from(pid) else { return };
    let pgid = Pid::from_raw(raw);

    // The child leads its own group (see `isolate_process_group`), so the group
    // id equals its pid and this reaches every descendant that stayed in it.
    let _ = killpg(pgid, Signal::SIGTERM);

    if tokio::time::timeout(TERM_GRACE, child.wait()).await.is_ok() {
        return;
    }

    let _ = killpg(pgid, Signal::SIGKILL);
    let _ = child.wait().await;
}

/// Terminate a running child.
///
/// Without process groups only the direct child can be reclaimed; a command
/// that spawned its own children may still leave them behind.
#[cfg(not(unix))]
async fn terminate_tree(child: &mut Child) {
    let _ = child.kill().await;
}

/// Read a pipe to end-of-file, ignoring read errors.
///
/// Both pipes must be drained while the child runs. A command that fills its
/// stdout buffer blocks on the write until someone reads, so waiting on exit
/// without reading turns a chatty command into a false timeout.
async fn drain<R: AsyncReadExt + Unpin>(pipe: &mut Option<R>, buf: &mut Vec<u8>) {
    if let Some(pipe) = pipe.as_mut() {
        let _ = pipe.read_to_end(buf).await;
    }
}

/// Execute a command, piping the payload JSON to its stdin and capturing stdout.
///
/// Returns the parsed stdout as a JSON value on success, `None` if the command
/// produced no output (silent filter), or an error on failure.
///
/// The command runs in its own process group. On timeout that group is
/// terminated — `SIGTERM`, then `SIGKILL` after [`TERM_GRACE`] — so a timed-out
/// execution stops running rather than merely stopping being waited on.
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
    isolate_process_group(&mut builder);

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

    // Wait for the process with timeout, draining both pipes as it runs.
    //
    // The child is kept owned here rather than moved into `wait_with_output()`,
    // so that a timeout still has something to signal. Handing ownership to the
    // wait future means an elapsed timeout drops the child instead of killing
    // it, which releases the process from supervision rather than stopping it.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();

    let waited = tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        let (status, (), ()) = tokio::join!(
            child.wait(),
            drain(&mut stdout_pipe, &mut stdout_buf),
            drain(&mut stderr_pipe, &mut stderr_buf),
        );
        status
    })
    .await;

    let Ok(status) = waited else {
        terminate_tree(&mut child).await;
        return Err(ExecError::Timeout {
            command: command_str,
        });
    };

    let status = status.map_err(|e| ExecError::SpawnFailed {
        error: e.to_string(),
        command: command_str.clone(),
    })?;

    let exit_code = status.code().unwrap_or(-1);

    if exit_code != 0 {
        let stderr = String::from_utf8_lossy(&stderr_buf).to_string();
        return Err(ExecError::Failed {
            exit_code,
            stderr,
            command: command_str,
        });
    }

    // Parse stdout: return None if empty, try JSON first, fall back to wrapped string
    let stdout_str = String::from_utf8_lossy(&stdout_buf).trim().to_string();
    if stdout_str.is_empty() {
        return Ok(None);
    }

    let stdout_payload = serde_json::from_slice(&stdout_buf)
        .unwrap_or_else(|_| serde_json::json!({"output": stdout_str}));

    let stderr = {
        let s = String::from_utf8_lossy(&stderr_buf).trim().to_string();
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
///
/// As with [`execute_command`], the command runs in its own process group and a
/// timeout terminates that whole group rather than abandoning it.
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
    isolate_process_group(&mut builder);

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

    // Wait for the process with timeout, terminating its group if it elapses.
    let waited = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await;

    let Ok(status) = waited else {
        terminate_tree(&mut child).await;
        return Err(ExecError::Timeout {
            command: command_str,
        });
    };

    let status = status.map_err(|e| ExecError::SpawnFailed {
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

/// Build a JSON error payload from an `ExecError` and the payload that caused it.
///
/// The failure details go under a reserved `error` key and the inbound payload
/// is spread alongside them, so an event published on the error topic keeps
/// whatever identified the work:
///
/// ```json
/// {"issue": 42, "error": {"exit_code": 1, "stderr": "...", "command": "..."}}
/// ```
///
/// Identity has to survive structurally rather than by opt-in. A failure event
/// carrying only `{command, exit_code, stderr}` cannot be joined, attributed, or
/// routed to a terminal: nothing in it says *which* work failed, so a consumer
/// accumulating by a natural key silently drops it and the run waits for a
/// reaper instead of reacting to the failure it was told about.
///
/// Two shapes it cannot spread into the top level:
/// - A payload that is not a JSON object (a string, array, or number) is placed
///   under `input` instead, since there are no keys to merge.
/// - A payload with its own `error` key loses it to the reserved one.
#[must_use]
pub fn error_to_json(err: &ExecError, payload: &serde_json::Value) -> serde_json::Value {
    let error = error_detail_to_json(err);

    match payload {
        serde_json::Value::Object(fields) => {
            let mut merged = fields.clone();
            merged.insert("error".to_string(), error);
            serde_json::Value::Object(merged)
        }
        other => serde_json::json!({ "error": error, "input": other }),
    }
}

/// The failure details alone, without any inbound identity.
fn error_detail_to_json(err: &ExecError) -> serde_json::Value {
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

    /// Poll for a pid to leave the process table, up to `attempts` 50ms ticks.
    ///
    /// Returns whether the process is gone. Termination is not instantaneous:
    /// the signal is delivered, then the kernel reaps.
    #[cfg(unix)]
    async fn is_gone(pid: u32, attempts: u32) -> bool {
        for _ in 0..attempts {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    #[tokio::test]
    async fn test_timeout_kills_process() {
        let payload = json!({"input": "hello"});
        // Report the pid, then outlive the timeout by a wide margin.
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo $$ >&2; sleep 30".to_string(),
        ];

        let started = std::time::Instant::now();
        let result = execute_command(&payload, &command, 100, &MessageEnv::default()).await;

        let Err(ExecError::Timeout { .. }) = result else {
            panic!("expected ExecError::Timeout");
        };

        // The point of the test: the process is gone, not merely unwaited-for.
        // `sleep 30` dies on SIGTERM, so this must not take the full grace.
        assert!(
            started.elapsed() < TERM_GRACE,
            "timeout should terminate promptly, took {:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_timeout_kills_grandchildren() {
        // The common shape: a shell that spawns the real work. Signalling only
        // the direct child would orphan the grandchild, which is exactly the
        // process that keeps running with a worktree checked out.
        let payload = json!({});
        let pid_file =
            std::env::temp_dir().join(format!("exec-common-grandchild-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pid_file);

        // The grandchild sleeps far longer than this test can run, so it cannot
        // pass by outliving its own sleep — only by being killed.
        let script = format!("sleep 600 & echo $! > {}; wait", pid_file.to_string_lossy());
        let command = vec!["sh".to_string(), "-c".to_string(), script];

        let started = std::time::Instant::now();
        let result = execute_command(&payload, &command, 300, &MessageEnv::default()).await;
        assert!(matches!(result, Err(ExecError::Timeout { .. })));
        assert!(
            started.elapsed() < TERM_GRACE,
            "reclaiming the tree should not need the full grace period, took {:?}",
            started.elapsed()
        );

        let recorded = std::fs::read_to_string(&pid_file).unwrap_or_default();
        let grandchild: u32 = recorded
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("grandchild pid was not recorded: {recorded:?}"));
        let _ = std::fs::remove_file(&pid_file);

        assert!(
            is_gone(grandchild, 40).await,
            "grandchild {grandchild} survived the timeout"
        );
    }

    #[tokio::test]
    async fn test_timeout_does_not_truncate_a_command_that_finishes() {
        // Draining both pipes concurrently with the wait is what keeps a chatty
        // command from blocking on a full pipe buffer and timing out falsely.
        let payload = json!({});
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            "head -c 200000 /dev/zero | tr '\\0' 'x'".to_string(),
        ];

        let result = execute_command(&payload, &command, 10_000, &MessageEnv::default())
            .await
            .unwrap_or_else(|_| panic!("expected success"))
            .unwrap_or_else(|| panic!("expected Some"));

        let output = result.stdout_payload["output"].as_str().unwrap_or_default();
        assert_eq!(output.len(), 200_000, "output was truncated");
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

    #[test]
    fn test_error_to_json_failed() {
        let err = ExecError::Failed {
            exit_code: 1,
            stderr: "bad input".to_string(),
            command: "my-cmd".to_string(),
        };
        let json = error_to_json(&err, &json!({}));
        assert_eq!(json["error"]["exit_code"], 1);
        assert_eq!(json["error"]["stderr"], "bad input");
        assert_eq!(json["error"]["command"], "my-cmd");
    }

    /// One of each variant, for the tests that assert identity survives all of
    /// them rather than only the one that happened to be written first.
    fn every_error_variant() -> Vec<ExecError> {
        vec![
            ExecError::Failed {
                exit_code: 1,
                stderr: "boom".to_string(),
                command: "my-cmd".to_string(),
            },
            ExecError::Timeout {
                command: "my-cmd".to_string(),
            },
            ExecError::SpawnFailed {
                error: "no such file".to_string(),
                command: "my-cmd".to_string(),
            },
            ExecError::StdinFailed {
                error: "broken pipe".to_string(),
                command: "my-cmd".to_string(),
            },
        ]
    }

    #[test]
    fn test_inbound_identity_survives_every_error_variant() {
        // A failure event that cannot say which work failed cannot be joined,
        // attributed, or routed to a terminal.
        let payload = json!({"issue": 42, "workspace": "wt-7"});

        for err in every_error_variant() {
            let json = error_to_json(&err, &payload);

            assert_eq!(json["issue"], 42, "identity lost for this variant");
            assert_eq!(json["workspace"], "wt-7", "identity lost for this variant");
            assert_eq!(json["error"]["command"], "my-cmd");
            assert!(
                !json["error"]["stderr"].is_null(),
                "every variant must explain itself"
            );
        }
    }

    #[test]
    fn test_a_join_key_in_the_payload_is_readable_on_the_error_event() {
        // The acceptance criterion from the issue: a join keyed on a payload
        // field can match a failure event.
        let payload = json!({"run_id": "run_9", "step": "judge"});
        let err = ExecError::Timeout {
            command: "claude -p".to_string(),
        };

        let json = error_to_json(&err, &payload);

        assert_eq!(json["run_id"], "run_9");
        assert_eq!(json["step"], "judge");
    }

    #[test]
    fn test_non_object_payload_is_carried_under_input() {
        // There are no keys to spread, so identity goes somewhere addressable
        // rather than being dropped.
        let err = ExecError::Timeout {
            command: "my-cmd".to_string(),
        };

        let json = error_to_json(&err, &json!("just a string"));

        assert_eq!(json["input"], "just a string");
        assert_eq!(json["error"]["command"], "my-cmd");
    }

    #[test]
    fn test_reserved_error_key_wins_over_an_inbound_one() {
        let payload = json!({"id": 3, "error": "an earlier step's error"});
        let err = ExecError::Failed {
            exit_code: 2,
            stderr: "boom".to_string(),
            command: "my-cmd".to_string(),
        };

        let json = error_to_json(&err, &payload);

        assert_eq!(json["id"], 3, "other identity fields still survive");
        assert_eq!(json["error"]["exit_code"], 2);
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

    #[test]
    fn test_error_to_json_timeout() {
        let err = ExecError::Timeout {
            command: "slow-cmd".to_string(),
        };
        let json = error_to_json(&err, &json!({}));
        assert!(json["error"]["exit_code"].is_null());
        assert_eq!(json["error"]["stderr"], "process timed out");
    }
}
