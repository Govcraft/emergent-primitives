# http-source

Receive HTTP webhooks and emit events. Supports optional HMAC-SHA256 signature validation for secure webhook endpoints.

**Publishes:** `http.request`

## Installation

```bash
emergent marketplace install http-source
```

Or download from [GitHub Releases](https://github.com/Govcraft/emergent-primitives/releases).

## Configuration

### CLI Arguments

| Argument | Environment Variable | Default | Description |
|----------|---------------------|---------|-------------|
| `-p, --port` | `HTTP_SOURCE_PORT` | `8080` | Port to listen on |
| `--host` | `HTTP_SOURCE_HOST` | `0.0.0.0` | Host to bind to |
| `--path` | `HTTP_SOURCE_PATH` | `/` | Path to accept requests on |
| `--secret` | `HTTP_SOURCE_SECRET` | — | HMAC secret for signature validation |

### emergent.toml

```toml
[[sources]]
name = "http-source"
path = "http-source"
args = ["--port", "8080", "--path", "/webhook"]
enabled = true
publishes = ["http.request"]
```

## Events

### http.request

Emitted for each incoming HTTP request.

```json
{
  "method": "POST",
  "path": "/",
  "headers": {
    "content-type": "application/json",
    "x-signature": "sha256=..."
  },
  "body": "{\"event\": \"push\", \"ref\": \"refs/heads/main\"}",
  "remote_addr": null
}
```

## Signature Validation

When `--secret` is provided, requests must include an `X-Signature` header with an HMAC-SHA256 signature of the request body:

```
X-Signature: sha256=<hex-encoded-hmac>
```

Requests with missing or invalid signatures return `401 Unauthorized`.

## Examples

### Basic webhook receiver

```bash
http-source --port 3000
```

### With signature validation

```bash
http-source --port 8080 --secret "my-webhook-secret"
```

### Custom path

```bash
http-source --port 8080 --path "/api/webhook"
```

### TOML: GitHub webhook receiver

```toml
[[sources]]
name = "github-webhook"
path = "http-source"
args = ["--port", "8080", "--path", "/github", "--secret", "${GITHUB_WEBHOOK_SECRET}"]
enabled = true
publishes = ["http.request"]
```

## Testing

Send a test webhook:

```bash
curl -X POST http://localhost:8080/ \
  -H "Content-Type: application/json" \
  -d '{"event": "test", "data": "hello"}'
```

With signature:

```bash
BODY='{"event": "test"}'
SIG=$(echo -n "$BODY" | openssl dgst -sha256 -hmac "my-secret" | cut -d' ' -f2)
curl -X POST http://localhost:8080/ \
  -H "Content-Type: application/json" \
  -H "X-Signature: sha256=$SIG" \
  -d "$BODY"
```
