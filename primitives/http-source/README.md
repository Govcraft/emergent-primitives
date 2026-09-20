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
| `--path` | `HTTP_SOURCE_PATH` | `/` | Route to accept requests on |
| `--secret` | `HTTP_SOURCE_SECRET` | — | HMAC secret for signature validation |
| `--trust-forwarded-for` | — | off | Report the caller from `X-Forwarded-For` instead of the socket peer |

`--trust-forwarded-for` is a flag with no environment variable, deliberately: a
bool read from the environment treats `VAR=false` as "set", which is exactly the
wrong default for a flag that relaxes a trust boundary.

### emergent.toml

```toml
[[sources]]
name = "http-source"
path = "http-source"
args = ["--port", "8080", "--path", "/webhook"]
enabled = true
publishes = ["http.request"]
```

## Routing

`--path` is an **exact route, not a prefix**. `--path /inject` serves `/inject`
and returns `404` for `/inject/extra`. To accept a family of paths, use axum's
capture syntax:

| `--path` | Matches | Published `path` |
|----------|---------|------------------|
| `/inject` | `/inject` | `/inject` |
| `/hook/{id}` | `/hook/42` | `/hook/42` |
| `/hook/{*rest}` | `/hook/a/b/c` | `/hook/a/b/c` |

The published `path` is always the path the **client requested**, never the
pattern `--path` was configured with.

### An invalid `--path`

After primitives 0.11.0 the route is checked before the source connects to the
engine or binds its port. A value the router would refuse prints one line that
names the value and the rule, and the process exits with status `1`:

```text
$ http-source --path bad-no-slash
Invalid --path "bad-no-slash": a route path must start with '/'
```

On 0.11.0 and earlier the same value reached axum's `Router::route`, which
panics: the source died at spawn with a backtrace and exit status `101`, and
`/api/topology` reported only `Exited with status: 101`.

All but the last two rows are rules that axum 0.8 and its matchit router
enforce:

| `--path` | Refused because |
|----------|-----------------|
| *(empty)* | a route path cannot be empty, use `/` for the root |
| `webhook` | a route path must start with `/` |
| `/hook/:id`, `/hook/*rest` | a segment cannot start with `:` or `*`, the capture syntax before axum 0.8. Write `{id}` and `{*rest}` |
| `/hook/{id`, `/hook/id}` | unbalanced braces. Write `{{` and `}}` for literal braces |
| `/hook/{}`, `/hook/{*}` | a capture needs a name |
| `/hook/{a/b}`, `/hook/{a*b}` | a capture name cannot contain `/` or `*` |
| `/hook/{id}.json`, `/hook/{a}{b}` | a capture must end its path segment. A literal prefix is fine: `/hook/v-{id}` |
| `/hook/{*rest}/more` | a `{*wildcard}` must be the end of the path |
| 26 or more `{name}` captures | the router holds at most 25 named captures per route |
| `/hook?x=1`, `/hook#top` | a route path cannot contain `?` or `#`. See below |
| `/hook with space`, `/hük` | a request carries these characters percent-encoded, and the route must be spelled the same way. See below |

The `?` and `#` row is not a router rule. axum registers `/hook?x=1` without
complaint and then matches nothing: the router is shown only the path of a
request, which ends where the query string starts, and a fragment never leaves
the client. The source would start, report healthy, and answer `404` to every
request. The query string is not part of the route. Configure `--path /hook`,
and every request to `/hook?x=1` is accepted and published with
`"path": "/hook"` and `"query": "x=1"`, ready for a downstream
`select(.query == "x=1")`. After primitives 0.11.0 such a path is refused at
startup like the others. A `?` or `#` inside a capture name (`/hook/{id?}`) is
only part of the name and is accepted.

The last row is the same kind of rule. The router compares the route with the
request path as it arrived, still percent-encoded, and never decodes either
side. A client asked for `/hük` sends `/h%C3%BCk`, which the route `/hük` does
not equal, so that source too would start, report healthy, and answer `404` to
every request. After primitives 0.11.0 the path is refused and the message
shows the spelling to configure:

```text
$ http-source --path '/hook with space'
Invalid --path "/hook with space": a route path cannot contain ' ' unencoded, a request carries it percent-encoded and the route must be spelled the same way: "/hook%20with%20space"
```

The characters refused are the ones no request path carries as written: ASCII
control characters, space, `<`, `>`, a backtick (the server answers `400` to
each before routing), and anything outside ASCII (clients encode it). The
encoded spelling is not registered on your behalf, for two reasons. The
published `path` is the encoded one, so a downstream
`select(.path == "/h%C3%BCk")` has to be written that way and the config should
say the same. And escapes are compared byte for byte: `--path /h%C3%BCk`
matches `/h%C3%BCk` and not `/h%c3%bck`. Browsers and curl send uppercase hex;
for a client that may not, use a capture (`/hooks/{name}`) and filter on the
published `path`. Capture names are never part of a request, so
`/hook/{naïve}` is accepted as written.

Repeating a capture name (`/{id}/{id}`) is accepted: the router allows it, and
this source never extracts captures, it publishes the concrete path.

## Events

### http.request

Emitted for each incoming HTTP request.

```json
{
  "method": "POST",
  "path": "/hook/42",
  "query": "retry=1",
  "headers": {
    "content-type": "application/json",
    "x-signature": "sha256=..."
  },
  "body": { "event": "push", "ref": "refs/heads/main" },
  "remote_addr": "203.0.113.7"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `method` | string | Request method, uppercase |
| `path` | string | Requested path, **without** the query string |
| `query` | string or null | Raw, undecoded query string; `null` when the request had none |
| `headers` | object | Header names lowercased; headers whose value is not valid UTF-8 are dropped |
| `body` | any | Parsed JSON when the body is JSON, otherwise the body as a string |
| `remote_addr` | string | Caller's IP address, no port |

#### Why `query` is separate from `path`

Topologies route on `path` by equality, for example an `exec-handler` running
`select(.path == "/inject")`. Folding `?a=1` into `path` would make that match
for one caller and miss for the next, silently. Keeping them apart means `path`
is stable per endpoint and `query` is available in one piece.

`query` is passed through exactly as it arrived. Percent decoding, repeated keys
and bare flags have no single right answer, so the decision is left to whoever
consumes it.

#### What `remote_addr` contains

`remote_addr` is an IP address with no port. The peer's source port identifies a
connection rather than a caller, and including it only in some deployments would
make the published shape depend on configuration.

By default it is the socket peer: the other end of the TCP connection. That
cannot be forged by the client, but behind a reverse proxy it is the proxy, so
every request looks like it came from the same machine.

With `--trust-forwarded-for`, the **leftmost** entry of the `X-Forwarded-For`
header is used instead, which is the original client each hop prepends. Entries
may be bare IPs or carry a port (`203.0.113.7:443`, `[2001:db8::1]:443`); the
port is dropped either way.

If the header is absent, empty, or its leftmost entry is not an IP address, the
socket peer is reported. A malformed leftmost entry does **not** fall through to
the next hop, because the next hop is a proxy and reporting a proxy as the
caller would be a wrong answer that looks right.

> **`X-Forwarded-For` is client-supplied.** On a directly exposed port, enabling
> `--trust-forwarded-for` lets any caller name itself, which defeats logging,
> rate limiting and allow-listing built on this field. Enable it only when a
> reverse proxy in front of this port overwrites the header.

## Signature Validation

When `--secret` is provided, requests must include an `X-Signature` header with an HMAC-SHA256 signature of the request body:

```
X-Signature: sha256=<hex-encoded-hmac>
```

Requests with missing or invalid signatures return `401 Unauthorized` and publish nothing.

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

### Behind a reverse proxy

```bash
http-source --port 8080 --path /inject --trust-forwarded-for
```

### One endpoint per tenant, routed downstream on `path`

```bash
http-source --port 8080 --path "/tenant/{id}"
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
curl -X POST "http://localhost:8080/?source=manual" \
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
