# topology-viewer

A sink that draws the running topology as a live D3.js force-directed graph. It
reads the engine's `GET /api/topology`, follows the `system.*` lifecycle events,
and serves one page that updates over server-sent events.

**Subscribes:** `system.started.*`, `system.stopped.*`, `system.error.*`,
`system.response.topology`

## Installation

```bash
emergent marketplace install topology-viewer
```

Or download from
[GitHub Releases](https://github.com/Govcraft/emergent-primitives/releases).

## Configuration

### CLI Arguments

| Argument       | Default     | Description                                                                                                                                 |
| -------------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `--host`       | `127.0.0.1` | Address the page and its API are served on                                                                                                  |
| `-p, --port`   | `8080`      | Port the page and its API are served on                                                                                                     |
| `--allow-host` | none        | A host name the viewer answers to besides IP addresses and `localhost`, such as the public name of a reverse proxy. Repeat it for each name |

Each takes `--flag value` or `--flag=value`. An argument the viewer does not
know, a missing value, a port outside 1 to 65535, or an `--allow-host` that is
not a bare host name prints one line and exits `1`, so a misspelled `--host`
never leaves the page on a different address than the one asked for.

### Who can reach the page

From primitives 0.12.0 the viewer listens on `127.0.0.1` unless `--host` says
otherwise. The page shows every primitive's name, topics, state and PID, and
`POST /api/refresh` makes the viewer do work, so putting it on the network is a
decision. The startup line prints the address that was bound:

```text
[topology-viewer] HTTP server listening on http://127.0.0.1:8080/
```

**This is a behavior change.** On 0.11.0 and earlier there was no `--host`, the
viewer listened on every interface (`0.0.0.0`), and the startup line said
`http://localhost:8080` regardless. If you open the viewer from another machine,
or run it in a container with a published port, add `--host 0.0.0.0`:

```toml
args = ["--host", "0.0.0.0", "--port", "8080"]
```

The viewer has no authentication. Prefer a specific interface address
(`--host 192.168.1.20`) or a reverse proxy over `0.0.0.0` on a machine that is
on a network you do not control. `--host ::1` and `--host ::` select IPv6. An
address the machine does not have, or a port already in use, prints one line
(`Cannot listen on http://203.0.113.1:8080/: ...`) and exits `1`.

### Which host names it answers to

From primitives 0.12.0 the viewer looks at the host every request names and
answers `421 Misdirected Request` unless it is one of:

- an IP address (`127.0.0.1:8080`, `192.168.1.20:8080`, `[::1]:8080`), whatever
  address the viewer is bound to,
- `localhost`,
- the name given to `--host`, when that is a name,
- a name listed with `--allow-host` (repeatable).

The port is not compared. The startup line says which hosts are answered:

```text
[topology-viewer] HTTP server listening on http://127.0.0.1:8080/, answering to any IP address, localhost, app.example
```

This is what stops DNS rebinding. Binding `127.0.0.1` keeps other machines out
and the absence of `Access-Control-Allow-Origin` keeps other origins out, but a
page on `attacker.example` can re-resolve its own name to `127.0.0.1`, and from
then on the browser treats the viewer as that page's own origin. The one thing
the page cannot choose is the `Host` the browser sends, which stays
`attacker.example`. An address cannot be rebound and `localhost` is not the
attacker's to resolve, so those are always answered; every other name has to be
one you listed.

**This is a behavior change.** On 0.11.0 and earlier the `Host` was never looked
at. A viewer reached by IP address or as `localhost` needs no change, and that
covers a published container port and an SSH port forward. What needs
`--allow-host` is a name:

| You open the page as                                                                                                                               | Needs                                              |
| -------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------- |
| `http://127.0.0.1:8080`, `http://localhost:8080`, `http://192.168.1.20:8080`                                                                       | nothing                                            |
| `http://myhost:8080`, `http://myhost.local:8080`                                                                                                   | `--allow-host myhost`, `--allow-host myhost.local` |
| `https://app.example` through a reverse proxy that forwards the name it was asked for (Caddy by default, nginx with `proxy_set_header Host $host`) | `--allow-host app.example`                         |
| the same through a proxy that sends the upstream address as `Host` (nginx by default)                                                              | nothing                                            |

This holds whatever `--host` is: `--host 0.0.0.0` exposes the viewer to other
machines and does not make it answer to their names for it. A value is a bare
host name, stored lower case and in its ASCII form. A scheme, a port, a path or
a `*` is refused with one line and exit `1`: names are compared whole, there are
no patterns, so `--allow-host '*.app.example'` would be listed and never match.
`attacker.localhost` and other names under `localhost` are names like any other
and need listing.

### Which pages can read it

From primitives 0.12.0 no response carries `Access-Control-Allow-Origin`. The
page, `GET /events`, `GET /api/topology` and `POST /api/refresh` are one origin,
and a page needs no permission to read its own origin, so nothing about the
viewer changes.

**This is a behavior change** for anything else. On 0.11.0 and earlier
`GET /api/topology` and `GET /events` answered with
`Access-Control-Allow-Origin: *`, so any web page open in a browser on the same
machine could read the topology, `127.0.0.1` or not. A page on another origin
that read them is now refused by the browser, and there is no option to allow
it: put the viewer and that page behind one reverse proxy so they share an
origin. Programs that are not browsers (curl, scripts) are unaffected, the
header only ever instructed browsers.

The engine sets `EMERGENT_API_PORT` for every primitive it starts. The viewer
reads the topology from `http://127.0.0.1:$EMERGENT_API_PORT/api/topology`, and
falls back to port `8891` when the variable is absent.

### emergent.toml

```toml
[[sinks]]
name = "topology-viewer"
path = "topology-viewer"
args = ["--port", "8080"]
enabled = true
subscribes = ["system.started.*", "system.stopped.*", "system.error.*", "system.response.topology"]
```

### Running from source

```toml
[[sinks]]
name = "topology-viewer"
path = "deno"
args = ["run", "--allow-env", "--allow-read", "--allow-write", "--allow-net", "/path/to/primitives/topology-viewer/main.ts", "--port", "8080"]
enabled = true
subscribes = ["system.started.*", "system.stopped.*", "system.error.*", "system.response.topology"]
```

`--allow-write` is not optional. Deno asks for read and write access to a Unix
socket path before it connects to it, so without the flag the page is served but
the viewer never reaches the engine.

## HTTP endpoints

Everything is served by the viewer itself, on `--port`.

| Method and path                   | Reply                                                                                                |
| --------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `GET /` , `/app.js`, `/style.css` | The page                                                                                             |
| `GET /events`                     | Server-sent events: `topology:full`, `node:added`, `node:updated`, `edges:updated`, `health:updated` |
| `GET /api/topology`               | The state the viewer currently holds: `nodes`, `edges`, `health`                                     |
| `POST /api/refresh`               | Re-reads the engine topology now, then replies with the fresh state                                  |

### When the graph is re-read

While it is connected to the engine the viewer re-reads the topology every 5
seconds, and applies each lifecycle event as it arrives. `POST /api/refresh` is
the same re-read on demand, and it is what the page's **Refresh** button calls.

```bash
curl -s -X POST http://localhost:8080/api/refresh
```

```json
{
  "nodes": [
    {
      "id": "emergent-engine",
      "kind": "source",
      "status": "running",
      "publishes": [
        "system.started.*",
        "system.stopped.*",
        "system.error.*",
        "system.shutdown"
      ],
      "subscribes": [],
      "pid": 4242
    },
    {
      "id": "topology-viewer",
      "kind": "sink",
      "status": "running",
      "publishes": [],
      "subscribes": [
        "system.started.*",
        "system.stopped.*",
        "system.error.*",
        "system.response.topology"
      ],
      "pid": 4250
    }
  ],
  "edges": [
    {
      "source": "emergent-engine",
      "target": "topology-viewer",
      "messageType": "system.started.*"
    }
  ],
  "health": { "status": "ok" }
}
```

| Status | Meaning                                                                                                                                                     |
| ------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `200`  | The engine answered. The body is the fresh state                                                                                                            |
| `502`  | The engine read failed or timed out (5 seconds). The body is still the full state, the last graph the viewer held, and `health.detail` says what went wrong |
| `405`  | Any method but `POST`. Refresh makes the viewer do work, so a prefetch or a crawler issuing `GET` does not trigger it                                       |

Requests that overlap, with each other or with the 5 second re-read, share one
engine request. Like every reply of the viewer it carries no
`Access-Control-Allow-Origin` header.

From primitives 0.12.0 the button calls `POST /api/refresh`. On 0.11.0 and
earlier it first called a `/refresh` URL on a hard-coded `localhost` port, the
address of a `topology-api` example source that nothing in this repository
serves. Every click made a cross-origin request to a port the operator never
configured, logged a network error, and only then fell back to
`GET /api/topology`, which reports the held state without re-reading anything.

### `health`

An empty or partial graph is reported, not drawn as if it were the whole
topology. The page shows `health.detail` in a banner whenever the status is not
`ok`.

| `health.status` | Meaning                                                                                |
| --------------- | -------------------------------------------------------------------------------------- |
| `ok`            | The engine answered and at least one primitive is known                                |
| `pending`       | The engine has not answered yet                                                        |
| `empty`         | The engine answered and reports no primitive but itself                                |
| `degraded`      | The topology request failed or timed out, or the viewer is not connected to the engine |

## Development

```bash
deno fmt --check
deno lint
deno check main.ts
deno test -A
```

`args.ts` parses the arguments as a pure function; it is the same file as
sse-sink's, byte for byte, and collects the repeatable flags a caller names,
which for the viewer is `--allow-host`. `host.ts` parses those names and decides
`(Host header, bound address, allowed hosts) -> accept | refuse`, pure, tested
in `host_test.ts`, and also the same file as sse-sink's. `graph.ts` holds the
graph state and the pure functions behind it. `http.ts` holds the request
handling, written against an interface so `http_test.ts` drives it with plain
`Request` objects and no listening socket. `main.ts` is the shell: arguments,
the engine connection, and `Deno.serve`.
