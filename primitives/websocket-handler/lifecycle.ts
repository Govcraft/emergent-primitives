/**
 * Connection lifecycle for websocket-handler, as pure functions.
 *
 * A connection starts when a connect message creates a socket and ends exactly
 * once. How it ends decides which terminal event the handler publishes:
 *
 *   closed        the handler was asked to close it (a disconnect message, a
 *                 newer connect message replacing it, or handler shutdown).
 *                 Consumers should not reconnect.
 *   disconnected  nobody asked: the peer closed it, the network dropped it, or
 *                 it never opened. Consumers may reconnect.
 *
 * Nothing here touches a socket, the clock or the engine, so every rule is
 * covered by table tests in lifecycle_test.ts. main.ts only feeds socket events
 * into these functions and publishes what they return.
 *
 * @module
 */

/** Who asked the handler to close a connection. */
export type CloseRequester = "disconnect" | "reconnect" | "shutdown";

/** Why a connection ended. The first three are intentional, the rest are not. */
export type EndingCause =
  | CloseRequester
  | "remote_close"
  | "connection_lost"
  | "connect_failed";

/** Which terminal event a connection publishes. */
export type TerminalKind = "closed" | "disconnected";

/** RFC 6455 close code for an ending with no close frame. */
export const ABNORMAL_CLOSE_CODE = 1006;

/** The fields of a WebSocket CloseEvent that the handler reports. */
export interface CloseObservation {
  readonly code: number;
  readonly reason: string;
  readonly wasClean: boolean;
}

/** Everything the handler knows about one connection. */
export interface ConnectionState {
  /** URL the connect message asked for. */
  readonly url: string;
  /** ID of the connect message, the causation ID of every event it produces. */
  readonly connectId: string;
  /** True once the socket reported open. */
  readonly opened: boolean;
  /** Set by the first close request that arrives while the socket is healthy. */
  readonly requestedBy: CloseRequester | null;
  /** First socket error seen, if any. */
  readonly error: string | null;
  /** True once the terminal event has been decided. */
  readonly ended: boolean;
}

/** Payload of both terminal events. `url`, `code` and `reason` predate it. */
export interface TerminalPayload {
  readonly url: string;
  readonly code: number;
  readonly reason: string;
  readonly was_clean: boolean;
  readonly cause: EndingCause;
  readonly opened: boolean;
  readonly error: string | null;
}

/** The one terminal event of a connection. */
export interface TerminalEvent {
  readonly kind: TerminalKind;
  readonly payload: TerminalPayload;
}

/** State of a connection whose socket was just created. */
export function newConnection(url: string, connectId: string): ConnectionState {
  return {
    url,
    connectId,
    opened: false,
    requestedBy: null,
    error: null,
    ended: false,
  };
}

/** The socket reported open. */
export function markOpened(state: ConnectionState): ConnectionState {
  return state.ended ? state : { ...state, opened: true };
}

/** The socket reported an error. The first one is kept: it is the root cause. */
export function markError(
  state: ConnectionState,
  message: string,
): ConnectionState {
  if (state.ended || state.error !== null) return state;
  return { ...state, error: message };
}

/**
 * The handler was asked to close the connection.
 *
 * The first request wins. A request that arrives after the socket already
 * failed is ignored, because that connection was lost, not closed: asking for a
 * close afterwards does not make the loss intentional.
 */
export function requestClose(
  state: ConnectionState,
  requester: CloseRequester,
): ConnectionState {
  if (state.ended || state.requestedBy !== null || state.error !== null) {
    return state;
  }
  return { ...state, requestedBy: requester };
}

/**
 * Deno reports an ending with no close frame as code 0. RFC 6455 and the
 * WebSocket standard reserve 1006 for it, so consumers see 1006.
 */
export function normalizeCloseCode(code: number): number {
  return code === 0 ? ABNORMAL_CLOSE_CODE : code;
}

/** Why the connection ended, given its history and the close observed. */
export function endingCause(
  state: ConnectionState,
  code: number,
): EndingCause {
  if (state.requestedBy !== null) return state.requestedBy;
  if (!state.opened) return "connect_failed";
  return code === ABNORMAL_CLOSE_CODE ? "connection_lost" : "remote_close";
}

/** Intentional endings publish `closed`, every other ending `disconnected`. */
export function terminalKind(cause: EndingCause): TerminalKind {
  switch (cause) {
    case "disconnect":
    case "reconnect":
    case "shutdown":
      return "closed";
    case "remote_close":
    case "connection_lost":
    case "connect_failed":
      return "disconnected";
  }
}

/**
 * The terminal event for a connection that ended with `observed`.
 *
 * `observed` is null when the handler stopped waiting for the close event (the
 * peer never answered the close frame before the handler had to exit). That is
 * reported as an abnormal close, because no close frame was received.
 */
export function terminalEvent(
  state: ConnectionState,
  observed: CloseObservation | null,
): TerminalEvent {
  const code = normalizeCloseCode(observed?.code ?? ABNORMAL_CLOSE_CODE);
  const cause = endingCause(state, code);
  return {
    kind: terminalKind(cause),
    payload: {
      url: state.url,
      code,
      reason: observed?.reason ?? "",
      was_clean: observed?.wasClean ?? false,
      cause,
      opened: state.opened,
      error: state.error,
    },
  };
}

/**
 * End a connection. Returns the event to publish the first time, and null on
 * every later call, so a connection can never publish two terminal events.
 */
export function endConnection(
  state: ConnectionState,
  observed: CloseObservation | null,
): { state: ConnectionState; event: TerminalEvent | null } {
  if (state.ended) return { state, event: null };
  return {
    state: { ...state, ended: true },
    event: terminalEvent(state, observed),
  };
}
