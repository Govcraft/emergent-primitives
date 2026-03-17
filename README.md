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

**Publishes:** `exec.output`, `exec.error`, `exec.exit`

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
- `-- <command> [args...]`: The command to execute

**Subscribes:** configurable via `--subscribe`
**Publishes:** `exec.output`, `exec.error` (configurable)

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
- `-- <command> [args...]`: The command to execute

**Subscribes:** configurable via `--subscribe`

## Shared Code

The `exec-common` crate provides the core command execution logic shared by `exec-handler` and `exec-sink`: payload-to-stdin piping, timeout handling, JSON output parsing, and structured error types.

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
