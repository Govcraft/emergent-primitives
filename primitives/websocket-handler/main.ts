#!/usr/bin/env -S deno run --allow-env --allow-read --allow-write --allow-net
/**
 * WebSocket Handler: bidirectional WebSocket bridge for Emergent pipelines.
 *
 * Fully message-driven: inert until it receives a connect message.
 *
 * Subscribe messages:
 *   {prefix}.connect     open a WebSocket to the URL in the payload
 *   {prefix}.send        send payload as a frame to the open WebSocket
 *   {prefix}.disconnect  close the connection
 *
 * Publish messages:
 *   {prefix}.connected     connection established
 *   {prefix}.frame         incoming WebSocket frame
 *   {prefix}.closed        connection ended because the handler was asked to
 *                          end it (disconnect message, a newer connect message,
 *                          handler shutdown). Do not reconnect.
 *   {prefix}.disconnected  connection ended and nobody asked (peer closed it,
 *                          network dropped it, or it never opened). Reconnect
 *                          if you want it back.
 *   {prefix}.error         connection or send error (diagnostic, not terminal)
 *
 * Every connection publishes exactly one of `closed` and `disconnected`. Both
 * carry { url, code, reason, was_clean, cause, opened, error } and are caused
 * by the connect message that opened the connection. The rules live in
 * lifecycle.ts as pure functions.
 *
 * Usage:
 *   websocket-handler --prefix ws
 *
 * The prefix defaults to "ws". When running under the engine, message types
 * are resolved from EMERGENT_SUBSCRIBES and EMERGENT_PUBLISHES env vars
 * using suffix matching (see message_types.ts).
 *
 * @module
 */

import { createMessage, EmergentHandler } from "jsr:@govcraft/emergent@0.13.0";
import {
  type CloseObservation,
  type CloseRequester,
  type ConnectionState,
  endConnection,
  markError,
  markOpened,
  newConnection,
  requestClose,
} from "./lifecycle.ts";
import { resolvePublishTypes, resolveSubscribeTypes } from "./message_types.ts";

// ============================================================================
// CLI Argument Parsing
// ============================================================================

function parseArgs(): { prefix: string } {
  const args = Deno.args;
  let prefix = "ws";

  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--prefix" && args[i + 1]) {
      prefix = args[i + 1];
      i++;
    }
  }

  return { prefix };
}

// ============================================================================
// Environment
// ============================================================================

/** An env var, or undefined when it is unset or env access is not granted. */
function readEnv(name: string): string | undefined {
  try {
    return Deno.env.get(name);
  } catch {
    return undefined;
  }
}

// ============================================================================
// Startup
// ============================================================================

const { prefix } = parseArgs();
const subTypes = resolveSubscribeTypes(prefix, readEnv("EMERGENT_SUBSCRIBES"));
const pubTypes = resolvePublishTypes(prefix, readEnv("EMERGENT_PUBLISHES"));

const handler = await EmergentHandler.connect(
  readEnv("EMERGENT_NAME") ?? "websocket_handler",
);

// ============================================================================
// WebSocket Management
// ============================================================================

/**
 * How long shutdown waits for peers to answer the close frame before the
 * handler reports the connection closed anyway. The engine sends SIGKILL
 * `shutdown_grace_ms` (default 2000) after SIGTERM, so this stays well inside.
 */
const SHUTDOWN_CLOSE_DEADLINE_MS = 1000;

/** One socket and everything known about it. */
interface Connection {
  state: ConnectionState;
  readonly ws: WebSocket;
  /** Resolves once the terminal event has been handed to the engine. */
  readonly settled: Promise<void>;
  readonly settle: () => void;
}

/** The connection that send and disconnect messages act on. */
let current: Connection | null = null;

/** Every connection that has not published its terminal event yet. */
const live = new Set<Connection>();

/** Publish, reporting a failure on stderr instead of throwing. */
async function publish(
  messageType: string,
  causedBy: string,
  payload: Record<string, unknown>,
): Promise<void> {
  try {
    await handler.publish(
      createMessage(messageType).causedBy(causedBy).payload(payload),
    );
  } catch (e) {
    console.error(`websocket-handler: could not publish ${messageType}:`, e);
  }
}

/** Decode a frame: JSON when it parses, the raw text otherwise, base64 for binary. */
function decodeFrame(raw: unknown): unknown {
  if (typeof raw === "string") {
    try {
      return JSON.parse(raw);
    } catch {
      return raw;
    }
  }
  if (raw instanceof ArrayBuffer) {
    return btoa(String.fromCharCode(...new Uint8Array(raw)));
  }
  return String(raw);
}

/** Ask a connection to close on behalf of `requester`. */
function closeConnection(conn: Connection, requester: CloseRequester): void {
  conn.state = requestClose(conn.state, requester);
  conn.ws.close();
}

/**
 * End a connection and publish its terminal event. Only the first call
 * publishes. A later call waits for that publish, so shutdown never closes the
 * engine connection underneath it.
 */
async function finishConnection(
  conn: Connection,
  observed: CloseObservation | null,
): Promise<void> {
  const { state, event } = endConnection(conn.state, observed);
  conn.state = state;
  if (current === conn) current = null;
  if (event === null) return await conn.settled;

  await publish(pubTypes[event.kind], state.connectId, { ...event.payload });
  live.delete(conn);
  conn.settle();
}

async function handleConnect(
  msgId: string,
  payload: { url: string },
): Promise<void> {
  if (current) closeConnection(current, "reconnect");

  const url = payload.url;
  let ws: WebSocket;
  try {
    ws = new WebSocket(url);
  } catch (e) {
    // No socket was created, so there is no connection to end: error only.
    const error = e instanceof Error ? e.message : String(e);
    await publish(pubTypes.error, msgId, { url, error });
    return;
  }

  const { promise: settled, resolve: settle } = Promise.withResolvers<void>();
  const conn: Connection = {
    state: newConnection(url, msgId),
    ws,
    settled,
    settle,
  };
  current = conn;
  live.add(conn);

  ws.onopen = async () => {
    conn.state = markOpened(conn.state);
    await publish(pubTypes.connected, msgId, { url });
  };

  ws.onmessage = async (event: MessageEvent) => {
    await publish(pubTypes.frame, msgId, { data: decodeFrame(event.data) });
  };

  ws.onerror = async (event: Event) => {
    const error = event instanceof ErrorEvent
      ? event.message
      : "WebSocket error";
    conn.state = markError(conn.state, error);
    await publish(pubTypes.error, msgId, { url, error });
  };

  ws.onclose = async (event: CloseEvent) => {
    await finishConnection(conn, {
      code: event.code,
      reason: event.reason,
      wasClean: event.wasClean,
    });
  };
}

async function handleSend(msgId: string, payload: unknown): Promise<void> {
  if (!current || current.ws.readyState !== WebSocket.OPEN) {
    await publish(pubTypes.error, msgId, {
      url: current?.state.url ?? null,
      error: "No active WebSocket connection",
    });
    return;
  }

  const data = typeof payload === "string" ? payload : JSON.stringify(payload);
  current.ws.send(data);
}

function handleDisconnect(): void {
  // The close event publishes the terminal event.
  if (current) closeConnection(current, "disconnect");
}

/**
 * Close every connection for shutdown and wait until each has published its
 * terminal event. A peer that does not answer the close frame in time is
 * reported closed without a close observation.
 */
async function closeAllForShutdown(): Promise<void> {
  const pending = [...live];
  if (pending.length === 0) return;
  for (const conn of pending) closeConnection(conn, "shutdown");

  let timer: number | undefined;
  const deadline = new Promise<void>((resolve) => {
    timer = setTimeout(resolve, SHUTDOWN_CLOSE_DEADLINE_MS);
  });
  await Promise.race([
    Promise.all(pending.map((conn) => conn.settled)),
    deadline,
  ]);
  clearTimeout(timer);

  await Promise.all(pending.map((conn) => finishConnection(conn, null)));
}

// ============================================================================
// Message Loop
// ============================================================================

const stream = await handler.subscribe([
  subTypes.connect,
  subTypes.send,
  subTypes.disconnect,
]);

// A signal ends the message loop. The engine's system.shutdown does the same
// inside the SDK. Either way the code after the loop closes the connections.
const stopLoop = () => stream.close();
Deno.addSignalListener("SIGTERM", stopLoop);
Deno.addSignalListener("SIGINT", stopLoop);

for await (const msg of stream) {
  const msgType = msg.messageType;

  if (msgType === subTypes.connect) {
    const payload = msg.payloadAs<{ url: string }>();
    await handleConnect(msg.id, payload);
  } else if (msgType === subTypes.send) {
    await handleSend(msg.id, msg.payload);
  } else if (msgType === subTypes.disconnect) {
    handleDisconnect();
  }
}

// Publish every terminal event before the engine connection goes away.
await closeAllForShutdown();
handler.close();

// A socket whose peer never answers the close frame would keep the event loop
// alive until the engine sends SIGKILL. Every terminal event is published, so
// there is nothing left to wait for.
Deno.exit(0);
