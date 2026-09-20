# sse-sink

Push pipeline events to browsers and other HTTP clients as
[Server-Sent Events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events).
Every event the sink receives is broadcast to every client connected to
`/events`.

**Subscribes:** whatever the topology configures, `*` (everything) when nothing
is configured

## Installation

```bash
emergent marketplace install sse-sink
```

Or download from
[GitHub Releases](https://github.com/Govcraft/emergent-primitives/releases).

## Configuration

### CLI Arguments

| Argument         | Default     | Description                                                                                                                      |
| ---------------- | ----------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `--host`         | `127.0.0.1` | Address the stream is served on                                                                                                  |
| `-p, --port`     | `8080`      | Port the stream is served on                                                                                                     |
| `--allow-origin` | none        | A browser origin allowed to read the stream, such as `http://localhost:3000`. Repeat it for each origin. `*` allows every origin |

Each takes `--flag value` or `--flag=value`. An argument the sink does not know,
a missing value, a port outside 1 to 65535, or an `--allow-origin` that is not
`*` or an origin prints one line and exits `1`.

### emergent.toml

```toml
[[sinks]]
name = "sse-sink"
path = "sse-sink"
args = ["--port", "8080", "--allow-origin", "https://app.example"]
enabled = true
subscribes = ["order.created", "order.shipped"]
```

List the topics a browser should see in `subscribes`. Leaving it out subscribes
the sink to `*`, which is every event in the pipeline, payloads included.

### Who can reach the stream

After primitives 0.11.0 the sink listens on `127.0.0.1` unless `--host` says
otherwise. The startup line prints the address that was bound:

```text
[sse-sink] Listening on http://127.0.0.1:8080/events
```

**This is a behavior change.** On 0.11.0 and earlier there was no `--host`, the
sink listened on every interface (`0.0.0.0`), and the startup line said
`http://localhost:8080/events` regardless. The stream has no authentication, so
on every interface it handed each event, payload and all, to anyone who could
reach the port.

A browser on the same machine, and a reverse proxy in front of the sink, both
reach `127.0.0.1` and need no change. A browser on another machine, or a sink in
a container with a published port, needs the sink on a reachable address:

```toml
args = ["--host", "0.0.0.0", "--port", "8080"]
```

Prefer a specific interface address (`--host 192.168.1.20`), or a reverse proxy
that adds TLS and authentication, over `0.0.0.0` on a network you do not
control. `--host ::1` and `--host ::` select IPv6. An address the machine does
not have, or a port already in use, prints one line
(`Cannot listen on http://203.0.113.1:8080/: ...`) and exits `1`.

### Which pages can read the stream

Listening on `127.0.0.1` keeps other machines out. It does not keep out a web
page: any site open in a browser on the same machine can ask that browser to
connect to `http://127.0.0.1:8080/events`. What stops the page from reading the
reply is the browser's same-origin rule, which the server lifts by naming the
page's origin in `Access-Control-Allow-Origin`.

After primitives 0.11.0 the sink sends that header only for the origins listed
with `--allow-origin`, and the startup line says who they are:

```toml
args = ["--port", "8080", "--allow-origin", "http://localhost:3000", "--allow-origin", "https://app.example"]
```

```text
[sse-sink] Listening on http://127.0.0.1:8080/events, readable from a browser by http://localhost:3000, https://app.example
```

| `--allow-origin`      | A request from `https://app.example`                                  | A request from any other origin  |
| --------------------- | --------------------------------------------------------------------- | -------------------------------- |
| not given             | no header, the browser refuses the page                               | no header                        |
| `https://app.example` | `Access-Control-Allow-Origin: https://app.example` and `Vary: Origin` | `Vary: Origin` only              |
| `*`                   | `Access-Control-Allow-Origin: *`                                      | `Access-Control-Allow-Origin: *` |

**This is a behavior change.** On 0.11.0 and earlier the sink answered every
request with `Access-Control-Allow-Origin: *`, so every page in a local browser
could read every event. A page that reads the stream from another origin now
needs that origin listed. `--allow-origin '*'` restores the old behavior; use it
only for a stream that carries nothing you would mind a stranger's page reading.

An origin is a scheme, a host and a port, and all three count:
`http://localhost:3000`, `http://127.0.0.1:3000` and `https://localhost:3000`
are three origins. The value is stored the way a browser sends it, lower case
and without the scheme's default port, so `https://App.Example:443/` lists
`https://app.example`. A value with a path, a query or credentials, a scheme
other than `http` or `https`, the word `null`, and a wildcard host such as
`https://*.app.example` are refused: origins are compared whole, there are no
patterns.

The header is a rule for browsers and nothing more. curl, a proxy, or any
program that can reach the port reads the stream whatever the header says; that
is what `--host` and a reverse proxy with authentication are for. A proxy that
serves the stream under the page's own origin needs no `--allow-origin` at all.
`GET /health` never carries the header.

## HTTP endpoints

| Method and path | Reply                                          |
| --------------- | ---------------------------------------------- |
| `GET /events`   | The event stream                               |
| `GET /health`   | `{"ok": true, "clients": <connected clients>}` |

Each event is one `data:` line holding JSON:

```json
{
  "id": "msg_01m2xzp97xf419pf5gz4nnjmyb",
  "type": "order.created",
  "source": "http-source",
  "timestamp": 1789860193533,
  "payload": { "order": 42 }
}
```

From a page whose origin is listed with `--allow-origin`:

```js
const source = new EventSource("http://localhost:8080/events");
source.onmessage = (e) => console.log(JSON.parse(e.data));
```

## Development

```bash
deno fmt --check
deno lint
deno check main.ts
deno test -A
```

`args.ts` parses the arguments as a pure function, tested in `args_test.ts`. It
is the same file as topology-viewer's, byte for byte: it collects the values of
the repeatable flags a caller names and leaves their meaning to the caller, so
`--allow-origin` exists here and is an unknown argument there. `cors.ts` parses
the origins and decides the header for one request, both pure and tested in
`cors_test.ts`. `main.ts` is the server and the engine connection.
