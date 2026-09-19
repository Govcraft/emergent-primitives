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
| [`jev-handler`](primitives/jev-handler/) | handler | Ask TypeSafe System One typed questions about event payloads |
| [`websocket-handler`](primitives/websocket-handler/) | handler | Bidirectional WebSocket bridge: connect, send and receive frames, and learn how each connection ended |

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
- `--host`: Host to bind (default: 0.0.0.0)
- `--path`: Exact route to accept requests on, axum capture syntax allowed (default: /)
- `--secret`: HMAC-SHA256 secret for signature validation (env: `HTTP_SOURCE_SECRET`)
- `--trust-forwarded-for`: Report `remote_addr` from `X-Forwarded-For` instead of the socket peer. Off by default; only safe behind a proxy that overwrites the header

**Publishes:** `http.request`

The payload carries `method`, the requested `path` (without the query string),
`query` (raw, or `null`), `headers`, `body`, and `remote_addr` (the caller's IP,
no port). See [the primitive's README](primitives/http-source/) for why `query`
is a separate field and which address `remote_addr` reports.

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

### stream-runner

Emit a JSON collection one item at a time, releasing the next item only once
the previous one has been acknowledged downstream. The acknowledgement is just
another event: whatever topic the last stage of the loop publishes.

```bash
# Stream transactions one at a time, acking on the classifier's output
stream-runner \
    --load-topic      batch.load \
    --publish-as      txn.raw \
    --ack-topic       txn.entry \
    --items-key       transactions \
    --ack-key         txn_id \
    --ack-timeout-ms  30000 \
    --on-timeout      skip
```

**Arguments:**
- `--load-topic`: Event carrying the JSON collection to stream (default: `stream.load`)
- `--publish-as`: Topic on which to emit each item (default: `stream.item`)
- `--ack-topic`: Topic that advances the stream (default: `stream.ack`)
- `--end-topic`: Topic published when the run finishes (default: `stream.end`)
- `--rejected-topic`: Topic published when a load is dropped (default: `stream.rejected`)
- `--timed-out-topic`: Topic published when an ack does not arrive in time (default: `stream.item-timed-out`)
- `--items-key`: Object key holding the array, ignored when the payload is a bare array (default: `items`)
- `--ack-key`: Field that must agree between an item and its ack (default: unset, any message advances)
- `--ack-timeout-ms`: How long an item may wait for its ack (default: unset, it waits forever)
- `--on-timeout`: `skip` the item or `end` the run when an ack times out (default: `skip`, needs `--ack-timeout-ms`)

**Subscribes:** `--load-topic` and `--ack-topic`
**Publishes:** `stream.item`, `stream.end`, `stream.rejected`, `stream.item-timed-out` (all configurable)

The four published types are resolved positionally from `EMERGENT_PUBLISHES`,
so the order of `publishes` in a config is load-bearing: item, end, rejected,
timed out.

```toml
[[handlers]]
name = "batch-runner"
path = "~/.local/share/emergent/primitives/bin/stream-runner"
args = [
  "--load-topic", "batch.load",
  "--publish-as", "txn.raw",
  "--ack-topic", "txn.entry",
  "--items-key", "transactions",
  "--ack-key", "txn_id",
  "--ack-timeout-ms", "30000",
]
subscribes = ["batch.load", "txn.entry"]
publishes = ["txn.raw", "batch.done", "batch.rejected", "batch.stalled"]
```

#### Matching acks to items

Without `--ack-key`, any message on the ack topic advances the stream. That is
the original behaviour and it is kept, but it means a duplicate ack, a late ack
from an earlier batch, or an ack for an item that something else injected all
release the next item early, putting two items in flight.

With `--ack-key <field>`, an ack advances the stream only when `ack[field]`
equals the same field on the item in flight. Anything else is **ignored and
logged at WARN**, with both sides of the comparison in the log line. It is
deliberately not published as an event: a mismatched ack is not a dropped
input, it is a message addressed to an item that is no longer in flight, and a
retrying downstream can produce them without bound. The item that actually lost
its acknowledgement is covered by `--ack-timeout-ms`, which does publish.

The field has to be present and non-null on both sides. Absence does not match
absence, or an item without the field would be advanced by any message at all,
which is the behaviour the flag exists to remove. A collection of bare strings
therefore cannot use `--ack-key`.

#### When an ack never arrives

Without `--ack-timeout-ms` an item waits forever, and because a load that
arrives mid stream is rejected, one unrouted error topic downstream can retire
the runner until it is restarted.

`--ack-timeout-ms` bounds the wait. On expiry the item is published on the
timed out topic and `--on-timeout` decides what happens to the run: `skip`
abandons the item and emits the next one, `end` abandons the whole run and
publishes the end event with `incomplete: true`.

Pair `--ack-timeout-ms` with `--ack-key`. Without the key, an ack that arrives
after its item timed out is indistinguishable from the current item's ack, so it
advances the stream a second time. With the key it is recognised as stale and
ignored. Internally each emitted item carries a generation and a timer fires for
the generation it was armed for, so a timer that fires just after its item was
acknowledged is a no op rather than a second advance.

#### What it publishes

Each item is published verbatim, so downstream sees exactly what was in the
collection. The other three carry a fixed shape:

```json
// stream.end
{"count": 3, "total": 3, "timed_out": 0, "incomplete": false}

// stream.rejected, reason "busy"
{"reason": "busy",
 "detail": "a load arrived while item 2 of 5 was in flight",
 "hint": null,
 "items_key": "transactions",
 "in_flight": {"index": 1, "total": 5},
 "payload": {"transactions": ["..."]}}

// stream.rejected, reason "bad_shape"
{"reason": "bad_shape",
 "detail": "object has no key 'transactions'",
 "hint": "payload looks like an exec-source envelope; unwrap it before the load topic, ...",
 "items_key": "transactions",
 "in_flight": null,
 "payload": {"command": "list-batch", "stdout": "...", "exit_code": 0}}

// stream.item-timed-out
{"reason": "ack_timeout", "action": "skip", "timeout_ms": 30000,
 "index": 1, "total": 5, "item": {"txn_id": "t-2"}}
```

`count` is how many items were emitted and `total` how many the collection
held; they differ only when a run ended early. The dropped load is nested under
`payload` rather than spread, so a router can replay it verbatim without having
to strip the rejection's own keys back off. `hint` is populated for the most
common wrong shape, the raw `exec-source` envelope `{command, stdout,
exit_code}` reaching the load topic, which otherwise looks like nothing
happening at all.

#### Routing a rejection is topology, not code

The primitive publishes the two reasons and makes no decision about them. A
`jq` selector splits them, exactly as it splits `jev-handler`'s error kinds:

```toml
# A collection that cannot be streamed at all. Park it for a human: retrying
# the same payload fails the same way.
[[handlers]]
name = "quarantine-bad-batches"
path = "~/.local/share/emergent/primitives/bin/exec-handler"
args = ["-s", "batch.rejected", "--publish-as", "batch.quarantined",
        "--", "jq", "-c", "select(.reason == \"bad_shape\")"]
subscribes = ["batch.rejected"]
publishes = ["batch.quarantined"]

# A collection that arrived at a busy moment. The payload is intact, so park it
# and replay it when the runner is free. Do not wire this straight back to the
# load topic: the stream is still busy and the rejection would loop.
[[handlers]]
name = "park-busy-batches"
path = "~/.local/share/emergent/primitives/bin/exec-handler"
args = ["-s", "batch.rejected", "--publish-as", "batch.parked",
        "--", "jq", "-c", "select(.reason == \"busy\") | .payload"]
subscribes = ["batch.rejected"]
publishes = ["batch.parked"]
```

An item that timed out is the same story: `batch.stalled` carries `.item`, so a
router can send it back through the work loop on its own or hand it to a human,
without the run it came from being stuck behind it.

### jev-handler

Subscribe to events, ask the [TypeSafe System One](https://docs.typesafe.ai) API
a fixed set of typed questions about each payload, and publish the answers.

```bash
TYPESAFE_API_KEY=... jev-handler -s email.received --questions ./questions.toml
```

**Arguments:**
- `--subscribe`, `-s`: Message types to subscribe to (required, repeatable)
- `--questions`: Path to the questions file, `.toml` or `.json` (required)
- `--publish-as`: Message type for an answered message (default: `jev.answered`)
- `--error-as`, `-e`: Message type for a failed message (default: `jev.error`)
- `--model`: Model alias or pinned model id (default: `jev-latest`)
- `--state-pointer`: JSON pointer to the part of the payload sent as `state` (default: the whole payload)
- `--timeout`, `-t`: Total budget for one message in milliseconds, retries included (default: 120000)
- `--request-timeout`: Timeout for a single HTTP attempt in milliseconds (default: 30000)
- `--max-concurrent`: Maximum messages in flight at once (default: 1)
- `--max-attempts`: Total attempts per message, including the first (default: 4)
- `--retry-base-ms`: First backoff step; doubled per attempt (default: 500)
- `--retry-max-delay-ms`: Ceiling on a computed backoff (default: 30000)
- `--max-retry-after-ms`: Ceiling on a server-supplied `Retry-After` (default: 60000)
- `--endpoint`: Evaluation endpoint (default: `https://api.typesafe.ai/v1/systemone`)

**Subscribes:** configurable via `--subscribe`
**Publishes:** `jev.answered`, `jev.error` (configurable)

The API key is read from `TYPESAFE_API_KEY` and nowhere else. There is
deliberately no `--api-key` flag: a key on a command line is a key in the
process table and in `emergent.toml`. The key never appears in a log line, in a
published payload, or in `Debug` output.

#### The questions file

One file, read once at startup, asked about every message. It mirrors the
TypeSafe request's `questions` map one-to-one, so the vendor's documentation
describes this file too:

```toml
# noul — a yes/no judgement; the answer is a bare probability in 0..=1.
# `criteria` is optional and may only use the keys "true" and "false".
[questions.lure]
type = "noul"
instructions = "Does this message try to get the reader to click a link or reply with information?"
criteria = { "true" = "There is something concrete to act on.", "false" = "The message is informational only." }

# choice — one option out of a defined set; at least two options.
[questions.kind]
type = "choice"
instructions = "What kind of message is this?"
criteria = { phish = "Credential theft under a false identity.", cold_pitch = "Unsolicited sales.", vendor_notice = "A legitimate operational notice.", personal = "Ordinary correspondence." }

# score — a position along an ordered rubric; order is the meaning, and the
# answer may land between levels.
[questions.pressure]
type = "score"
instructions = "How much time pressure does the message apply?"
criteria = ["No deadline is expressed.", "A soft deadline.", "Act now or lose access."]
```

The same file may be written as JSON; the extension picks the parser and both
produce an identical question set. A full example with all three types is in
[`primitives/jev-handler/examples/questions.toml`](primitives/jev-handler/examples/questions.toml).

Validation happens at startup, before the engine is connected, so a misconfigured
handler never appears healthy in a topology. Unknown keys are rejected rather
than ignored — a `critera` typo would otherwise leave a choice question with no
options on every message. Question ids are restricted to
`[A-Za-z_][A-Za-z0-9_]*` so a downstream router can always write
`.answers.kind` without quoting.

#### What it publishes

A successful message publishes the answers **in the API's exact JSON shape**,
with the inbound payload nested under `input`:

```json
{
  "input": {"issue": 42, "subject": "Action required"},
  "answers": {
    "lure": {"type": "noul", "noul": 0.93},
    "kind": {"type": "choice", "choice": "phish", "confidence": 0.99,
             "probabilities": {"phish": 0.99, "cold_pitch": 0.0, "vendor_notice": 0.01, "personal": 0.0}},
    "pressure": {"type": "score", "score": 2.0, "confidence": 1.0,
                 "legend": {"0": "No deadline is expressed.", "1": "A soft deadline.", "2": "Act now or lose access."},
                 "probabilities": {"0": 0.0, "1": 0.0, "2": 1.0}}
  },
  "usage": {"input_tokens": 391, "output_tokens": 34},
  "model": "jev-1.13.0",
  "request_id": "req_2f8c1d..."
}
```

The shape is passed through untouched because that is what downstream `jq`
routers select on. `input` is nested rather than spread: `answers`, `usage`, and
`model` would collide with ordinary payload keys.

Failures publish an error event carrying the inbound payload, following the same
rule as `exec-handler`: the details go under a reserved `error` key with the
inbound fields spread alongside, and a payload that is not a JSON object is
carried under `input` instead.

```json
{"issue": 42,
 "error": {"kind": "rate_limited", "status": 429, "attempts": 4,
           "message": "rate limited by the API (HTTP 429) after 4 attempt(s)",
           "endpoint": "https://api.typesafe.ai/v1/systemone", "model": "jev-latest",
           "request_id": "req_2f8c1d...", "body": "…", "detail": null}}
```

`error.kind` is the contract a router selects on:

| `error.kind` | What happened | What to do with the item |
|---|---|---|
| `auth` | The credentials were rejected (`401`) | Page a human; the key is wrong or revoked |
| `billing` | The organization is out of API credit (`402`) | **Page a human, then hold and requeue the item — do not quarantine it.** The request was well-formed and the item is fine; it succeeds unchanged once credit is added |
| `invalid_request` | The request body was rejected (`422`, or another `4xx` that is not `401`, `402`, or `429`) | Quarantine; the questions file is wrong and retrying would fail the same way |
| `rate_limited` | The attempt budget was spent on `429`s | Requeue |
| `server_error` | The API failed or was overloaded (`5xx`, including `529`) | Requeue |
| `transport` | DNS, connection, TLS, or a per-attempt timeout | Requeue |
| `timeout` | The whole-message `--timeout` budget elapsed | Requeue |
| `bad_response` | A `2xx` body that is not the documented envelope | Page a human; the vendor's contract moved |
| `answer_contract` | A well-formed response that does not answer the questions asked | Page a human; the vendor's contract moved |
| `state_not_found` | `--state-pointer` did not resolve in the payload | Quarantine; the upstream payload shape is wrong |

The API's own `detail` is surfaced as structured JSON rather than buried in the
body string, in both the shapes it arrives in: the array a `422` sends, so a
router can see which question was rejected, and the object a `402` sends —
`{"error_type": "billing_error", "message": "…"}`.

Error bodies are truncated to 2 KiB and may echo the state that was sent. That
is the operator's own data rather than a secret, but it does land in the event
store.

#### Routing is topology, not code

This primitive publishes exactly two message types and makes no decision about
the answers. Confidence banding, thresholds, and fan-out are `exec-handler`
blocks running `jq` selectors over the published `answers` — which is why the
answers must keep the vendor's shape.

```toml
# Ask the questions. One POST per inbound message.
[[handlers]]
name = "triage-judge"
path = "~/.local/share/emergent/primitives/bin/jev-handler"
args = [
    "-s", "email.received",
    "--questions", "/etc/emergent/triage.toml",
    "--state-pointer", "/body",
    "--max-concurrent", "4",
]
subscribes = ["email.received"]
publishes = ["jev.answered", "jev.error"]
env = { TYPESAFE_API_KEY = "sk-..." }

# Route on confidence. The three selectors are mutually exclusive and cover
# every value in 0..=1, so exactly one of them republishes each message and
# nothing is silently dropped.
[[handlers]]
name = "route-confident"
path = "~/.local/share/emergent/primitives/bin/exec-handler"
args = ["-s", "jev.answered", "--publish-as", "triage.confident",
        "--", "jq", "-c", "select(.answers.kind.confidence >= 0.9)"]
subscribes = ["jev.answered"]
publishes = ["triage.confident"]

[[handlers]]
name = "route-uncertain"
path = "~/.local/share/emergent/primitives/bin/exec-handler"
args = ["-s", "jev.answered", "--publish-as", "triage.uncertain",
        "--", "jq", "-c", "select(.answers.kind.confidence >= 0.6 and .answers.kind.confidence < 0.9)"]
subscribes = ["jev.answered"]
publishes = ["triage.uncertain"]

[[handlers]]
name = "route-review"
path = "~/.local/share/emergent/primitives/bin/exec-handler"
args = ["-s", "jev.answered", "--publish-as", "triage.needs_review",
        "--", "jq", "-c", "select(.answers.kind.confidence < 0.6)"]
subscribes = ["jev.answered"]
publishes = ["triage.needs_review"]

# Failures route on kind, not on confidence. The three routers are mutually
# exclusive and together exhaustive: the last one matches by negation, so a kind
# added in a later release lands somewhere instead of vanishing.

# The item is at fault, or the questions are: retrying fails the same way.
[[handlers]]
name = "route-unanswerable"
path = "~/.local/share/emergent/primitives/bin/exec-handler"
args = ["-s", "jev.error", "--publish-as", "triage.quarantined",
        "--", "jq", "-c", "select(.error.kind | IN(\"invalid_request\", \"state_not_found\"))"]
subscribes = ["jev.error"]
publishes = ["triage.quarantined"]

# The item is fine and only a human can unblock it: a rejected key, an empty
# credit balance, or a vendor contract that moved. Hold it, do not quarantine.
[[handlers]]
name = "route-blocked"
path = "~/.local/share/emergent/primitives/bin/exec-handler"
args = ["-s", "jev.error", "--publish-as", "triage.blocked",
        "--", "jq", "-c", "select(.error.kind | IN(\"auth\", \"billing\", \"bad_response\", \"answer_contract\"))"]
subscribes = ["jev.error"]
publishes = ["triage.blocked"]

# Everything else is transient (rate_limited, server_error, transport, timeout)
# or not yet known to this topology. Requeue it.
[[handlers]]
name = "route-transient"
path = "~/.local/share/emergent/primitives/bin/exec-handler"
args = ["-s", "jev.error", "--publish-as", "triage.requeued",
        "--", "jq", "-c", "select(.error.kind | IN(\"invalid_request\", \"state_not_found\", \"auth\", \"billing\", \"bad_response\", \"answer_contract\") | not)"]
subscribes = ["jev.error"]
publishes = ["triage.requeued"]
```

A subscriber on `triage.blocked` pages a human; one on `triage.requeued`
republishes the original event after a delay, with an attempt count in the
payload and a guard that ends the loop.

A `jq -c 'select(...)'` that matches nothing writes nothing and exits 0, which
`exec-handler` treats as a filter and drops — so a band that does not apply
publishes nothing rather than an empty event. Changing a threshold is a config
edit, not a rebuild, and the raw judgements stay reusable because no primitive
has baked a policy into them.

#### Concurrency and rate limits

`--max-concurrent` is the real rate control against a rate-limited API, and it
defaults to 1 to match the other handlers. **A task waiting out a retry backoff
holds its slot**, so at 1 a `429` storm stalls the handler until the limit
clears. That is honest backpressure rather than a bug, but an operator who does
not expect it will read it as a hang; for an IO-bound API call a higher value is
usually right.

The two timeouts are separate on purpose. `--timeout` is the budget for one
whole message including every retry; `--request-timeout` bounds one HTTP
attempt. With a single knob, retries silently blow past the budget the operator
thought they set.

Retries cover `429`, `5xx` (including TypeSafe's `529 Overloaded`), and
transport failures. Every other `4xx` is fatal on the first attempt: a bad key
or a rejected questions file would fail identically on every one. A
`Retry-After` header is honoured but clamped to `--max-retry-after-ms`, since
otherwise a server can pin a concurrency slot for an hour with nothing in the
logs to explain it. Only the delta-seconds form of `Retry-After` is read; the
HTTP-date form falls back to computed backoff.

### websocket-handler

Hold one outbound WebSocket connection on behalf of a pipeline. The handler is
inert until it receives a connect message, so the URL can come from an earlier
step (Slack Socket Mode, for example, hands out a fresh URL per connection).

```bash
websocket-handler --prefix ws
```

**Arguments:**
- `--prefix`: Message type prefix (default: `ws`). Under the engine the types
  declared in the topology win, matched by their last segment, so
  `slack.connect` fills the `connect` role whatever the prefix is.

**Subscribes:**
- `{prefix}.connect`: open a connection to `url` in the payload. A connection
  that is already open is closed first.
- `{prefix}.send`: send the payload as one text frame (a string as is, anything
  else as JSON).
- `{prefix}.disconnect`: close the connection.

**Publishes:** `{prefix}.connected`, `{prefix}.frame`, `{prefix}.closed`,
`{prefix}.disconnected`, `{prefix}.error`

Every event of a connection is caused by the connect message that opened it, so
`causation_id` plus the `url` in the payload identify the connection.

`{prefix}.frame` carries one field, `data`: the parsed JSON when a text frame
parses, the raw text otherwise, and base64 for a binary frame.

#### How a connection ends

Every connection publishes exactly one terminal event, never both and never
twice:

| Event | Meaning | `cause` | Reconnect? |
|-------|---------|---------|------------|
| `{prefix}.closed` | The handler was asked to end it | `disconnect` (a disconnect message), `reconnect` (a newer connect message replaced it), `shutdown` (the handler is stopping) | No |
| `{prefix}.disconnected` | Nobody asked | `remote_close` (the peer sent a close frame), `connection_lost` (the connection dropped with no close frame), `connect_failed` (it never opened) | Yes, if you want it back |

Both carry the same payload:

```json
{
  "url": "wss://example.com/socket",
  "code": 1006,
  "reason": "",
  "was_clean": false,
  "cause": "connection_lost",
  "opened": true,
  "error": "Unexpected EOF"
}
```

- `code`, `reason`, `was_clean`: the WebSocket close code, close reason and
  whether the close handshake completed. An ending with no close frame is
  always reported as `1006`.
- `cause`: why it ended, as in the table above.
- `opened`: `false` when the connection failed before it was established.
- `error`: the first socket error seen on the connection, or `null`.

`{prefix}.error` is diagnostic, not terminal. A dropped connection publishes
`error` and then `disconnected`, so drive reconnection from `disconnected`
alone or the flow reconnects twice. An `error` with no terminal event after it
means no connection existed (a send with nothing connected, or a connect
message whose URL could not be parsed).

On shutdown the handler closes its connection, waits up to one second for the
peer to answer, and publishes `closed` before it leaves. A peer that does not
answer in time is reported with `code` 1006 and `was_clean` false.

Before `disconnected` existed, every ending published `{prefix}.closed`, so a
remote close could not be told apart from a requested one, and a handler
shutdown published nothing at all. A topology that reconnects on `closed`
should subscribe to `disconnected` instead.

#### Reconnecting

Declare `disconnected` next to the other types and route it to whatever
produces the next connect message:

```toml
[[handlers]]
name = "ws"
path = "~/.local/share/emergent/primitives/bin/websocket-handler"
args = ["--prefix", "ws"]
enabled = true
subscribes = ["ws.connect", "ws.send", "ws.disconnect"]
publishes = ["ws.connected", "ws.frame", "ws.closed", "ws.disconnected", "ws.error"]

# Fetch a fresh URL and publish it as ws.connect whenever the connection is lost.
[[handlers]]
name = "reconnect"
path = "~/.local/share/emergent/primitives/bin/exec-handler"
args = ["-s", "ws.disconnected", "--publish-as", "ws.connect", "--", "./fetch-ws-url.sh"]
enabled = true
subscribes = ["ws.disconnected"]
publishes = ["ws.connect"]
```

A topology written before `disconnected` existed does not declare it. The
handler then publishes it as the sibling of the declared `closed` type
(`slack.closed` gives `slack.disconnected`), so it stays under the prefix the
topology already routes on.

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

Its `error_payload` is the one place the error-event merge rule lives — reserved
`error` key, inbound payload spread alongside, non-objects under `input`. Any
primitive that publishes a failure applies that function rather than a second
copy of the rule that can drift, which is how `jev-handler` error events join on
the same keys as `exec-handler` ones.

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
