# exec-source

Execute shell commands and emit output as events. Supports one-time execution or repeated runs on an interval.

**Publishes:** `exec.output`, `exec.error`, `exec.exit`

## Installation

```bash
emergent marketplace install exec-source
```

Or download from [GitHub Releases](https://github.com/Govcraft/emergent-primitives/releases).

## Configuration

### CLI Arguments

| Argument | Environment Variable | Default | Description |
|----------|---------------------|---------|-------------|
| `-c, --command` | `EXEC_SOURCE_COMMAND` | required | Command to execute |
| `-a, --args` | `EXEC_SOURCE_ARGS` | — | Space-separated command arguments |
| `-i, --interval` | `EXEC_SOURCE_INTERVAL` | `0` | Repeat interval in milliseconds (0 = run once) |
| `-d, --working-dir` | `EXEC_SOURCE_WORKING_DIR` | — | Working directory for command |
| `-s, --shell` | `EXEC_SOURCE_SHELL` | — | Shell to use (e.g., `bash`, `sh`) |

### emergent.toml

```toml
[[sources]]
name = "exec-source"
path = "exec-source"  # or full path to binary
args = ["--command", "date", "--interval", "5000"]
enabled = true
publishes = ["exec.output", "exec.error", "exec.exit"]
```

## Events

### exec.output

Emitted when stdout is non-empty.

```json
{
  "command": "date",
  "stdout": "Sat Jan 18 15:30:00 CST 2026\n",
  "exit_code": 0
}
```

### exec.error

Emitted when stderr is non-empty.

```json
{
  "command": "cat /nonexistent",
  "stderr": "cat: /nonexistent: No such file or directory\n",
  "exit_code": 1
}
```

### exec.exit

Always emitted after command completes.

```json
{
  "command": "date",
  "exit_code": 0
}
```

## Examples

### Run once

```bash
exec-source --command "ls -la"
```

### Periodic execution

```bash
exec-source --command "df -h" --interval 60000
```

### With shell and working directory

```bash
exec-source \
  --shell bash \
  --command "git status" \
  --working-dir /path/to/repo
```

### TOML: Monitor disk space every minute

```toml
[[sources]]
name = "disk-monitor"
path = "exec-source"
args = ["--command", "df -h", "--interval", "60000"]
enabled = true
publishes = ["exec.output", "exec.exit"]
```
