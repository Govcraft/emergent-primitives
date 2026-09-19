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

| Argument     | Default     | Description                     |
| ------------ | ----------- | ------------------------------- |
| `--host`     | `127.0.0.1` | Address the stream is served on |
| `-p, --port` | `8080`      | Port the stream is served on    |

Both take `--flag value` or `--flag=value`. An argument the sink does not know,
a missing value, or a port outside 1 to 65535 prints one line and exits `1`.

### emergent.toml

```toml
[[sinks]]
name = "sse-sink"
path = "sse-sink"
args = ["--port", "8080"]
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
`http://localhost:8080/events` regardless. The stream has no authentication and
answers any origin (`Access-Control-Allow-Origin: *`), so on every interface it
handed each event, payload and all, to anyone who could reach the port.

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

`args.ts` parses `--host` and `--port` as a pure function, tested in
`args_test.ts`. `main.ts` is the server and the engine connection.
