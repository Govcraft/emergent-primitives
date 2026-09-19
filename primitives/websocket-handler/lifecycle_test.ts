import { assertEquals } from "jsr:@std/assert@1";
import {
  ABNORMAL_CLOSE_CODE,
  type CloseObservation,
  type CloseRequester,
  type ConnectionState,
  endConnection,
  type EndingCause,
  markError,
  markOpened,
  newConnection,
  normalizeCloseCode,
  requestClose,
  terminalEvent,
  type TerminalKind,
  terminalKind,
} from "./lifecycle.ts";

const SOCKET_URL = "wss://example.test/socket";
const CONNECT_ID = "msg_connect";

/** One thing that happens to a connection before it ends. */
type Step =
  | { open: true }
  | { error: string }
  | { request: CloseRequester };

function replay(steps: Step[]): ConnectionState {
  let state = newConnection(SOCKET_URL, CONNECT_ID);
  for (const step of steps) {
    if ("open" in step) state = markOpened(state);
    else if ("error" in step) state = markError(state, step.error);
    else state = requestClose(state, step.request);
  }
  return state;
}

const clean = (code: number, reason = ""): CloseObservation => ({
  code,
  reason,
  wasClean: true,
});
/** What Deno reports when the socket ends with no close frame. */
const DENO_ABNORMAL: CloseObservation = {
  code: 0,
  reason: "",
  wasClean: false,
};
const OPEN: Step = { open: true };

interface EndingCase {
  name: string;
  steps: Step[];
  observed: CloseObservation | null;
  kind: TerminalKind;
  cause: EndingCause;
  code: number;
  wasClean: boolean;
  error?: string;
}

const ENDINGS: EndingCase[] = [
  {
    name: "peer sends a close frame with an application code",
    steps: [OPEN],
    observed: clean(4001, "bye"),
    kind: "disconnected",
    cause: "remote_close",
    code: 4001,
    wasClean: true,
  },
  {
    name: "peer sends a normal close: still not asked for, so disconnected",
    steps: [OPEN],
    observed: clean(1000),
    kind: "disconnected",
    cause: "remote_close",
    code: 1000,
    wasClean: true,
  },
  {
    name: "TCP drops: error fires before close, one disconnected event",
    steps: [OPEN, { error: "Unexpected EOF" }],
    observed: DENO_ABNORMAL,
    kind: "disconnected",
    cause: "connection_lost",
    code: ABNORMAL_CLOSE_CODE,
    wasClean: false,
    error: "Unexpected EOF",
  },
  {
    name: "abnormal close reported as 1006 with no error event",
    steps: [OPEN],
    observed: { code: 1006, reason: "", wasClean: false },
    kind: "disconnected",
    cause: "connection_lost",
    code: ABNORMAL_CLOSE_CODE,
    wasClean: false,
  },
  {
    name: "connection refused: never opened",
    steps: [{ error: "tcp connect error" }],
    observed: DENO_ABNORMAL,
    kind: "disconnected",
    cause: "connect_failed",
    code: ABNORMAL_CLOSE_CODE,
    wasClean: false,
    error: "tcp connect error",
  },
  {
    name: "disconnect message, peer answers the close frame",
    steps: [OPEN, { request: "disconnect" }],
    observed: clean(1005),
    kind: "closed",
    cause: "disconnect",
    code: 1005,
    wasClean: true,
  },
  {
    name: "disconnect message, peer drops TCP instead of answering",
    steps: [OPEN, { request: "disconnect" }, { error: "Unexpected EOF" }],
    observed: DENO_ABNORMAL,
    kind: "closed",
    cause: "disconnect",
    code: ABNORMAL_CLOSE_CODE,
    wasClean: false,
    error: "Unexpected EOF",
  },
  {
    name: "disconnect message while still connecting",
    steps: [{ request: "disconnect" }, { error: "closed before open" }],
    observed: DENO_ABNORMAL,
    kind: "closed",
    cause: "disconnect",
    code: ABNORMAL_CLOSE_CODE,
    wasClean: false,
    error: "closed before open",
  },
  {
    name: "a newer connect message replaces the connection",
    steps: [OPEN, { request: "reconnect" }],
    observed: clean(1005),
    kind: "closed",
    cause: "reconnect",
    code: 1005,
    wasClean: true,
  },
  {
    name: "handler shutdown, peer answers the close frame",
    steps: [OPEN, { request: "shutdown" }],
    observed: clean(1005),
    kind: "closed",
    cause: "shutdown",
    code: 1005,
    wasClean: true,
  },
  {
    name: "handler shutdown, peer never answers before the deadline",
    steps: [OPEN, { request: "shutdown" }],
    observed: null,
    kind: "closed",
    cause: "shutdown",
    code: ABNORMAL_CLOSE_CODE,
    wasClean: false,
  },
  {
    name: "the first close request wins",
    steps: [OPEN, { request: "disconnect" }, { request: "shutdown" }],
    observed: clean(1005),
    kind: "closed",
    cause: "disconnect",
    code: 1005,
    wasClean: true,
  },
  {
    name:
      "a close request after the socket failed does not make it intentional",
    steps: [OPEN, { error: "Unexpected EOF" }, { request: "shutdown" }],
    observed: DENO_ABNORMAL,
    kind: "disconnected",
    cause: "connection_lost",
    code: ABNORMAL_CLOSE_CODE,
    wasClean: false,
    error: "Unexpected EOF",
  },
  {
    name: "only the first error is reported",
    steps: [OPEN, { error: "first" }, { error: "second" }],
    observed: DENO_ABNORMAL,
    kind: "disconnected",
    cause: "connection_lost",
    code: ABNORMAL_CLOSE_CODE,
    wasClean: false,
    error: "first",
  },
];

for (const c of ENDINGS) {
  Deno.test(`ending: ${c.name}`, () => {
    const event = terminalEvent(replay(c.steps), c.observed);
    assertEquals(event, {
      kind: c.kind,
      payload: {
        url: SOCKET_URL,
        code: c.code,
        reason: c.observed?.reason ?? "",
        was_clean: c.wasClean,
        cause: c.cause,
        opened: c.steps.includes(OPEN),
        error: c.error ?? null,
      },
    });
  });
}

Deno.test("every ending publishes exactly one terminal event", () => {
  for (const c of ENDINGS) {
    const first = endConnection(replay(c.steps), c.observed);
    assertEquals(first.event?.kind, c.kind, c.name);
    assertEquals(first.state.ended, true, c.name);

    // A late close event, or the shutdown deadline after the close event.
    for (const late of [c.observed, null, clean(1000)]) {
      const again = endConnection(first.state, late);
      assertEquals(again.event, null, c.name);
      assertEquals(again.state, first.state, c.name);
    }
  }
});

Deno.test("an ended connection ignores later socket events and requests", () => {
  const { state: ended } = endConnection(replay([OPEN]), clean(1000));
  assertEquals(markError(ended, "late"), ended);
  assertEquals(requestClose(ended, "shutdown"), ended);
  assertEquals(
    markOpened(endConnection(replay([]), null).state).opened,
    false,
  );
});

Deno.test("transitions do not mutate the state they are given", () => {
  const start = newConnection(SOCKET_URL, CONNECT_ID);
  const snapshot = { ...start };
  markOpened(start);
  markError(start, "boom");
  requestClose(start, "disconnect");
  endConnection(start, null);
  assertEquals(start, snapshot);
});

Deno.test("the connect message ID travels with the state", () => {
  assertEquals(replay([OPEN]).connectId, CONNECT_ID);
});

// [cause, kind]
const KINDS: [EndingCause, TerminalKind][] = [
  ["disconnect", "closed"],
  ["reconnect", "closed"],
  ["shutdown", "closed"],
  ["remote_close", "disconnected"],
  ["connection_lost", "disconnected"],
  ["connect_failed", "disconnected"],
];

Deno.test("terminalKind: intentional causes close, the rest disconnect", () => {
  for (const [cause, kind] of KINDS) {
    assertEquals(terminalKind(cause), kind, cause);
  }
});

Deno.test("normalizeCloseCode: only Deno's 0 becomes 1006", () => {
  for (
    const [input, expected] of [
      [0, 1006],
      [1000, 1000],
      [1005, 1005],
      [1006, 1006],
      [4001, 4001],
    ]
  ) {
    assertEquals(normalizeCloseCode(input), expected);
  }
});
