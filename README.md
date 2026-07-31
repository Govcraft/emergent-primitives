# Emergent Primitives

Official marketplace primitives for the [Emergent](https://github.com/Govcraft/emergent) workflow engine.

## Available Primitives

| Name | Kind | Description |
|------|------|-------------|
| [`http-source`](primitives/http-source/) | source | HTTP webhook receiver |
| [`exec-source`](primitives/exec-source/) | source | Execute shell commands and emit output as events |
| [`exec-handler`](primitives/exec-handler/) | handler | Pipe event payloads through any executable and publish results |
| [`exec-sink`](primitives/exec-sink/) | sink | Pipe event payloads through any executable (fire-and-forget) |
| [`stream-runner`](primitives/stream-runner/) | handler | Emit a JSON collection one item at a time, waiting for downstream ack before advancing |

The exec trio covers most use cases without writing code:

```bash
# Console output (replaces a dedicated console-sink)
exec-sink -s timer.tick -- jq .

# HTTP POST (replaces a dedicated http-sink)
exec-sink -s alert.fired -- curl -s -X POST -H "Content-Type: application/json" -d @- https://hooks.example.com

# File logging
exec-sink -s data.processed -- tee -a /var/log/events.jsonl
```

The [topology-viewer](https://github.com/Govcraft/emergent) sink ships with the engine repository.

## Installation

Install via the Emergent marketplace CLI:

```bash
emergent marketplace install http-source
emergent marketplace install exec-handler
emergent marketplace install exec-sink
```

Or download binaries directly from [GitHub Releases](https://github.com/Govcraft/emergent-primitives/releases).

## Usage

### http-source

Receive HTTP webhooks and emit `http.request` events.

```bash
http-source --port 8080 --path /webhook
```

**Arguments:**
- `--port`, `-p`: Port to listen on (default: 8080)
- `--host`, `-H`: Host to bind (default: 0.0.0.0)
- `--path`: URL path (default: /)
- `--secret`, `-s`: HMAC-SHA256 secret for signature validation (env: `HTTP_WEBHOOK_SECRET`)

**Publishes:** `http.request`

### exec-source

Execute shell commands and emit output events.

```bash
exec-source --command date --interval 5000
```

**Arguments:**
- `--command`, `-c`: Command to execute (required)
- `--args`, `-a`: Command arguments
- `--interval`, `-i`: Repeat interval in milliseconds
- `--working-dir`, `-w`: Working directory
- `--shell`, `-s`: Shell to use (default: sh)
- `--correlate`: Mint one correlation ID at startup and stamp it on every published message
- `--correlation-id`: Adopt an existing `cor_<uuid_v7>` instead of minting one (env: `EMERGENT_CORRELATION_ID`)

**Publishes:** `exec.output`, `exec.error`, `exec.exit`

A source is where a trail begins. `--correlate` stamps one correlation ID on
everything the source publishes; every downstream `exec-handler` carries it
forward, so the whole flow is one query against the event store's
`correlation_id` column. `--correlation-id` adopts an ID minted elsewhere —
this is how a run that spans more than one engine stays a single trail.

The command reads the same value from `EMERGENT_CORRELATION_ID`, so a shell
step can record the ID the event store will key on:

```bash
exec-source --correlate --shell sh \
  --command 'echo "{\"run\":\"$EMERGENT_CORRELATION_ID\"}" | tee run.json'
```

### exec-handler

Subscribe to events, pipe payloads through an executable, and publish results.

```bash
exec-handler -s timer.tick --publish-as data.transformed -- jq '.data | keys'
```

**Arguments:**
- `--subscribe`, `-s`: Message types to subscribe to (required, repeatable)
- `--publish-as`: Message type for successful output (default: `exec.output`)
- `--error-as`, `-e`: Message type for error output (default: `exec.error`)
- `--timeout`, `-t`: Per-execution timeout in milliseconds (default: 30000)
- `--kill-grace-ms`: Milliseconds a timed-out command may run after SIGTERM before SIGKILL (default: 5000)
- `--max-concurrent`: Maximum commands running at once (default: 1)
- `--silent-exit-codes`: Exit codes treated as a silent filter when stderr is empty (comma-separated, default: none)
- `-- <command> [args...]`: The command to execute

**Subscribes:** configurable via `--subscribe`
**Publishes:** `exec.output`, `exec.error` (configurable)

Published messages inherit the inbound message's `correlation_id` — a
transformation belongs to the same logical request as its input — alongside the
`causation_id` that links it to the specific message it came from.

At `--max-concurrent 1` (the default) messages are processed serially in
arrival order. Above 1, up to N commands run simultaneously — the right choice
for slow, IO-bound steps like an LLM call:

```bash
exec-handler -s slack.prompt --timeout 60000 --max-concurrent 4 -- claude -p "Answer this"
```

Outputs are then published as executions complete, not in arrival order; the
causation and correlation stamps keep interleaved trails intact. The
subscription stream is only pulled when a slot is free, so queued bursts
backpressure the engine rather than spawning unbounded processes, and in-flight
executions finish and publish before shutdown.

Every failure — non-zero exit, timeout, spawn failure — publishes an error
event. To use exit-code filtering (e.g. `jq -e 'select(...)'`, which drops a
message by exiting 1 with no stderr), list the filtering exit codes explicitly:

```bash
exec-handler -s log.line --silent-exit-codes 1 -- jq -e 'select(.level == "error")'
```

A listed exit code is silent only when stderr is empty; a command that writes
diagnostics before failing still publishes an error event.

Error events carry the inbound payload, so a failure can still be attributed to
the work that caused it. The failure details go under a reserved `error` key and
the inbound fields sit alongside them:

```json
{"issue": 42, "workspace": "wt-7", "error": {"exit_code": 1, "stderr": "...", "command": "..."}}
```

A downstream handler joining on `.issue` matches failure events the same way it
matches successful ones. A payload that is not a JSON object has nothing to
merge into, so it is carried under `input` instead; an inbound `error` key is
overwritten by the reserved one.

A timed-out command is terminated, not merely abandoned. Each command runs in
its own process group, so the timeout reclaims anything the command started —
the shell *and* the work it spawned. Termination is `SIGTERM`, then `SIGKILL` after
`--kill-grace-ms` (default 5000), giving a command holding real state the chance
to finish a write without letting one that ignores signals run forever.

### exec-sink

Subscribe to events and pipe payloads through an executable. Output is discarded (fire-and-forget).

```bash
# Pretty-print events to console
exec-sink -s timer.tick -- jq .

# POST to a webhook
exec-sink -s alert.fired -- curl -s -X POST -H "Content-Type: application/json" -d @- https://hooks.example.com

# Pipe through a custom script
exec-sink -s user.created -- ./scripts/send-welcome-email.sh
```

**Arguments:**
- `--subscribe`, `-s`: Message types to subscribe to (required, repeatable)
- `--timeout`, `-t`: Per-execution timeout in milliseconds (default: 30000)
- `--kill-grace-ms`: Milliseconds a timed-out command may run after SIGTERM before SIGKILL (default: 5000)
- `--max-concurrent`: Maximum commands running at once (default: 1)
- `--silent-exit-codes`: Exit codes not reported as failures (comma-separated, default: none)
- `-- <command> [args...]`: The command to execute

**Subscribes:** configurable via `--subscribe`

Concurrency works as in exec-handler: serial in arrival order at the default
of 1, up to N simultaneous commands above that, with backpressure on the
subscription stream and a full drain of in-flight commands before shutdown.

Sinks cannot publish, so failures are reported on stderr (captured in the
engine's primitive logs) tagged with the causing message ID. Exit codes listed
in `--silent-exit-codes` are not reported — for commands that use a non-zero
exit to mean something other than failure, e.g.
`--silent-exit-codes 1 -- grep -q ERROR`. Unlike exec-handler there is no
empty-stderr condition: the sink hands stderr through to the terminal rather
than capturing it. Timeouts and spawn failures are always reported.

Timeouts terminate the command's process group as in exec-handler, so a
fire-and-forget command that outlives its timeout is stopped rather than left
running unsupervised.

## Envelope Variables

Exec primitives pipe only the message *payload* to a command's stdin, so the
envelope is invisible to a jq filter or shell step. It arrives in the command's
environment instead:

| Variable | Source |
|----------|--------|
| `EMERGENT_MESSAGE_ID` | `message.id` |
| `EMERGENT_MESSAGE_TYPE` | `message.message_type` |
| `EMERGENT_MESSAGE_SOURCE` | `message.source` |
| `EMERGENT_CORRELATION_ID` | `message.correlation_id` |
| `EMERGENT_CAUSATION_ID` | `message.causation_id` |

`exec-handler` and `exec-sink` set all five from the message being handled;
`exec-source` sets `EMERGENT_CORRELATION_ID` alone, since it has no inbound
message. A field the message does not carry is **removed** from the command's
environment rather than left alone — the engine forwards its own environment
down to every command, so an ambient value would otherwise leak in and mislabel
the output.

This is what makes tracing identifiers usable from a zero-code pipeline without
smuggling them through the payload.

## Shared Code

The `exec-common` crate provides the core command execution logic shared by `exec-handler` and `exec-sink`: payload-to-stdin piping, process-group isolation and timeout termination, JSON output parsing, identity-preserving error payloads, and the `MessageEnv` envelope-to-environment mapping.

## Development

### Prerequisites

- Rust 2024 edition
- emergent-client SDK

### Building

```bash
cargo build --release
```

### Testing

```bash
cargo nextest run
```

### Linting

```bash
cargo clippy --all-targets -- -D warnings
```

## Release

Releases are automated via GitHub Actions. To create a new release:

1. Tag the commit: `git tag v0.4.0`
2. Push the tag: `git push origin v0.4.0`

The workflow will:
- Build for Linux (x86_64, aarch64), macOS (x86_64, aarch64)
- Create archives (tar.gz)
- Generate SHA256 checksums
- Upload to GitHub Releases

## License

MIT
