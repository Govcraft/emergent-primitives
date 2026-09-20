# jev-handler

Subscribe to events, ask [TypeSafe System One](https://docs.typesafe.ai) (Jev) a fixed set of typed questions about each payload, and publish the answers with calibrated confidence. Use it when a pipeline step is a judgment rather than a transformation: is this message unwanted, which folder fits, how urgent is it.

**Publishes:** `jev.answered`, `jev.error` (both configurable)

The handler makes no decision about the answers. Thresholds, confidence bands and failure handling are `exec-handler` routers running `jq` selectors over what it publishes, so the policy lives in your `emergent.toml`.

## Installation

```bash
emergent marketplace install jev-handler
```

Or download from [GitHub Releases](https://github.com/Govcraft/emergent-primitives/releases). Needs Emergent engine 0.14.0 or later.

## The API key

The key is read from the `TYPESAFE_API_KEY` environment variable and from nowhere else. There is deliberately no `--api-key` flag: a key on a command line is a key in the process table and in `emergent.toml`. The key never appears in a log line, in a published payload, or in `Debug` output. The engine forwards its own environment to every primitive, so export the variable before starting the engine. Without it the handler exits at startup with `TYPESAFE_API_KEY is not set`.

## Configuration

### CLI Arguments

| Argument | Default | Description |
|----------|---------|-------------|
| `-s, --subscribe` | required, repeatable | Message types to subscribe to |
| `--questions` | required | Path to the questions file, `.toml` or `.json` |
| `--publish-as` | `jev.answered` | Message type for an answered message |
| `-e, --error-as` | `jev.error` | Message type for a failed message |
| `--model` | `jev-latest` | Model alias or pinned model id |
| `--state-pointer` | whole payload | JSON pointer to the part of the payload sent as `state` |
| `-t, --timeout` | `120000` | Total budget for one message in ms, retries included |
| `--request-timeout` | `30000` | Timeout for a single HTTP attempt in ms |
| `--max-concurrent` | `1` | Maximum messages in flight at once |
| `--max-attempts` | `4` | Total attempts per message, including the first |
| `--retry-base-ms` | `500` | First backoff step; doubled per attempt |
| `--retry-max-delay-ms` | `30000` | Ceiling on a computed backoff |
| `--max-retry-after-ms` | `60000` | Ceiling on a server-supplied `Retry-After` |
| `--endpoint` | `https://api.typesafe.ai/v1/systemone` | Evaluation endpoint |

### emergent.toml

```toml
[[handlers]]
name = "judge"
path = "~/.local/share/emergent/primitives/bin/jev-handler"
args = [
    "-s", "mail.fetched",
    "--questions", "./questions.toml",
    "--state-pointer", "/body",
    "--max-concurrent", "4",
]
subscribes = ["mail.fetched"]
publishes = ["jev.answered", "jev.error"]
```

### The questions file

One file, read once at startup, asked about every message in a single request. There are three question types:

```toml
# noul: a yes/no judgement. The answer is a bare probability in 0..=1.
[questions.lure]
type = "noul"
instructions = "Does this message try to get the reader to click a link or reply with information?"

# choice: one option out of a defined set, at least two options.
[questions.kind]
type = "choice"
instructions = "What kind of message is this?"
criteria = { phish = "Credential theft under a false identity.", cold_pitch = "Unsolicited sales.", personal = "Ordinary correspondence." }

# score: a position along an ordered rubric. The answer may land between levels.
[questions.pressure]
type = "score"
instructions = "How much time pressure does the message apply?"
criteria = ["No deadline is expressed.", "A soft deadline.", "Act now or lose access."]
```

A full example is in [`examples/questions.toml`](examples/questions.toml). The file is validated at startup, before the engine is connected, and unknown keys are rejected rather than ignored.

## Events

### jev.answered

The answers in the API's exact JSON shape, with the inbound payload nested under `input`:

```json
{
  "input": {"issue": 42, "subject": "Action required"},
  "answers": {
    "lure": {"type": "noul", "noul": 0.93},
    "kind": {"type": "choice", "choice": "phish", "confidence": 0.99,
             "probabilities": {"phish": 0.99, "cold_pitch": 0.0, "personal": 0.01}}
  },
  "usage": {"input_tokens": 391, "output_tokens": 34},
  "model": "jev-1.13.0",
  "request_id": "req_2f8c1d..."
}
```

### jev.error

The inbound payload with the failure under a reserved `error` key. A payload that is not a JSON object is carried under `input` instead.

```json
{"issue": 42,
 "error": {"kind": "rate_limited", "status": 429, "attempts": 4,
           "message": "rate limited by the API (HTTP 429) after 4 attempt(s)",
           "endpoint": "https://api.typesafe.ai/v1/systemone", "model": "jev-latest",
           "request_id": "req_2f8c1d...", "body": "...", "detail": null}}
```

`error.kind` is one of `auth`, `billing`, `invalid_request`, `rate_limited`, `server_error`, `transport`, `timeout`, `bad_response`, `answer_contract` or `state_not_found`. A `billing` failure (`402`, out of API credit) means the item is fine and succeeds unchanged once credit is added, so hold and requeue it rather than quarantining it.

## Examples

### Route on confidence

```toml
[[handlers]]
name = "route-confident"
path = "~/.local/share/emergent/primitives/bin/exec-handler"
args = ["-s", "jev.answered", "--publish-as", "triage.confident",
        "--", "jq", "-c", "select(.answers.kind.confidence >= 0.9)"]
subscribes = ["jev.answered"]
publishes = ["triage.confident"]

[[handlers]]
name = "route-review"
path = "~/.local/share/emergent/primitives/bin/exec-handler"
args = ["-s", "jev.answered", "--publish-as", "triage.needs_review",
        "--", "jq", "-c", "select(.answers.kind.confidence < 0.9)"]
subscribes = ["jev.answered"]
publishes = ["triage.needs_review"]
```

### A complete topology

The engine repository ships a runnable example that reads a message from disk, judges it and prints the confidence band: [`config/examples/jev-triage/`](https://github.com/Govcraft/emergent/tree/main/config/examples/jev-triage).

## More

The [jev-handler section of the repository README](../../README.md#jev-handler) has the full `error.kind` table with what to do for each kind, the three-router failure block, and the notes on concurrency, timeouts and `Retry-After`.
