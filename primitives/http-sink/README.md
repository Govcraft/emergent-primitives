# http-sink

Make outbound HTTP requests from consumed events. Supports configurable retries, timeouts, and authentication.

**Subscribes:** Configured via `emergent.toml`

## Installation

```bash
emergent marketplace install http-sink
```

Or download from [GitHub Releases](https://github.com/Govcraft/emergent-primitives/releases).

## Configuration

### CLI Arguments

| Argument | Environment Variable | Default | Description |
|----------|---------------------|---------|-------------|
| `-b, --base-url` | `HTTP_SINK_BASE_URL` | — | Base URL to prepend to relative paths |
| `-t, --timeout` | `HTTP_SINK_TIMEOUT` | `30` | Request timeout in seconds |
| `-r, --retries` | `HTTP_SINK_RETRIES` | `3` | Number of retries on failure |
| `--auth-header` | `HTTP_SINK_AUTH_HEADER` | — | Authorization header value |

### emergent.toml

```toml
[[sinks]]
name = "http-sink"
path = "http-sink"
args = ["--base-url", "https://api.example.com", "--timeout", "60"]
enabled = true
subscribes = ["order.created", "user.registered"]
```

## Message Payload

The sink expects messages with payloads containing:

| Field | Required | Description |
|-------|----------|-------------|
| `url` | Yes* | Full target URL |
| `path` | Yes* | Relative path (combined with `--base-url`) |
| `method` | No | HTTP method (default: `POST`) |
| `headers` | No | Additional headers object |
| `body` | No | Request body (defaults to entire payload) |

*Either `url` or `path` is required.

### Example Payloads

Full URL:
```json
{
  "url": "https://api.example.com/webhook",
  "method": "POST",
  "body": {"event": "order.created", "order_id": "12345"}
}
```

Relative path (with `--base-url`):
```json
{
  "path": "/v1/events",
  "method": "POST",
  "headers": {"X-Custom-Header": "value"},
  "body": {"data": "payload"}
}
```

## Examples

### Basic usage

```bash
http-sink --base-url "https://api.example.com"
```

### With authentication

```bash
http-sink \
  --base-url "https://api.example.com" \
  --auth-header "Bearer eyJhbGc..."
```

### Custom timeout and retries

```bash
http-sink \
  --base-url "https://slow-api.example.com" \
  --timeout 120 \
  --retries 5
```

### TOML: Forward events to Slack

```toml
[[sinks]]
name = "slack-forwarder"
path = "http-sink"
args = ["--base-url", "https://hooks.slack.com/services/XXX/YYY/ZZZ"]
enabled = true
subscribes = ["alert.*", "error.*"]
```

### TOML: API integration with auth

```toml
[[sinks]]
name = "api-sync"
path = "http-sink"
args = [
  "--base-url", "https://api.internal.com",
  "--auth-header", "Bearer ${API_TOKEN}",
  "--timeout", "60",
  "--retries", "5"
]
enabled = true
subscribes = ["user.created", "order.completed"]
```

## Retry Behavior

On failure, the sink retries with exponential backoff:
- Attempt 1: immediate
- Attempt 2: 100ms delay
- Attempt 3: 200ms delay
- Attempt 4: 300ms delay

Retries occur for both network errors and non-2xx status codes.
