# Emergent Primitives

Official marketplace primitives for the [Emergent](https://github.com/Govcraft/emergent) workflow engine.

## Available Primitives

| Name | Kind | Description |
|------|------|-------------|
| [`http-source`](primitives/http-source/) | source | HTTP webhook receiver |
| [`http-sink`](primitives/http-sink/) | sink | Outbound HTTP requests |
| [`exec-source`](primitives/exec-source/) | source | Shell command executor |
| [`exec-handler`](primitives/exec-handler/) | handler | Pipe event payloads through any executable |
| [`console-sink`](primitives/console-sink/) | sink | Output message payloads to stdout |

The [topology-viewer](https://github.com/Govcraft/emergent) sink ships with the engine repository.

## Installation

Install via the Emergent marketplace CLI:

```bash
emergent marketplace install http-source
emergent marketplace install exec-handler
emergent marketplace install console-sink
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

### http-sink

Make outbound HTTP requests from consumed events.

```bash
http-sink --base-url https://api.example.com --timeout 30
```

**Arguments:**
- `--base-url`, `-u`: Base URL for requests (env: `HTTP_BASE_URL`)
- `--timeout`, `-t`: Request timeout in seconds (default: 30)
- `--retries`, `-r`: Retry attempts (default: 3)
- `--auth-header`: Authorization header (env: `HTTP_AUTH_HEADER`)

**Message Payload:**
```json
{
  "url": "/endpoint",
  "method": "POST",
  "headers": { "Content-Type": "application/json" },
  "body": { "data": "value" }
}
```

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

Subscribe to events, pipe payloads through an executable, and publish the results.

```bash
exec-handler --publish-as processed.result
```

**Arguments:**
- `--publish-as`, `-p`: Message type for successful output (default: exec.output)
- `--error-as`, `-e`: Message type for error output (default: exec.error)
- `--timeout`, `-t`: Per-execution timeout in milliseconds (default: 30000)

**Subscribes:** `*` (configurable via TOML)
**Publishes:** `exec.output`, `exec.error`

### console-sink

Output message payloads to stdout.

```bash
console-sink --subscribe timer.tick --pretty --timestamp
```

**Arguments:**
- `--subscribe`, `-s`: Message types to subscribe to (can be repeated)
- `--pretty`, `-p`: Pretty-print JSON output (env: `CONSOLE_SINK_PRETTY`)
- `--timestamp`, `-t`: Include timestamps (env: `CONSOLE_SINK_TIMESTAMP`)

**Subscribes:** `*` (configurable via TOML or `--subscribe` flag)

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

1. Tag the commit: `git tag v0.3.12`
2. Push the tag: `git push origin v0.3.12`

The workflow will:
- Build for Linux (x86_64, aarch64), macOS (x86_64, aarch64)
- Create archives (tar.gz)
- Generate SHA256 checksums
- Upload to GitHub Releases

## License

MIT
