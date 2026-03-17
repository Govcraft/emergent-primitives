#!/usr/bin/env -S deno run --allow-env --allow-read --allow-write --allow-net
/**
 * WebSocket Handler — bidirectional WebSocket bridge for Emergent pipelines.
 *
 * Fully message-driven: inert until it receives a connect message.
 *
 * Subscribe messages:
 *   {prefix}.connect  — open a WebSocket to the URL in the payload
 *   {prefix}.send     — send payload as a frame to the open WebSocket
 *   {prefix}.disconnect — close the connection
 *
 * Publish messages:
 *   {prefix}.connected — connection established
 *   {prefix}.frame    — incoming WebSocket frame
 *   {prefix}.closed   — connection closed
 *   {prefix}.error    — connection or send error
 *
 * Usage:
 *   websocket-handler --prefix ws
 *
 * The prefix defaults to "ws". When running under the engine, message types
 * are resolved from EMERGENT_SUBSCRIBES and EMERGENT_PUBLISHES env vars
 * using suffix matching.
 *
 * @module
 */

import { EmergentHandler, createMessage } from "jsr:@govcraft/emergent@0.13.0";

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
// Message Type Resolution
// ============================================================================

interface SubscribeTypes {
  connect: string;
  send: string;
  disconnect: string;
}

interface PublishTypes {
  connected: string;
  frame: string;
  closed: string;
  error: string;
}

/** Resolve subscribe types from EMERGENT_SUBSCRIBES or prefix. */
function resolveSubscribeTypes(prefix: string): SubscribeTypes {
  try {
    const envSubs = Deno.env.get("EMERGENT_SUBSCRIBES");
    if (envSubs) {
      const types = envSubs.split(",").map((t) => t.trim()).filter((t) =>
        t.length > 0
      );
      return {
        connect: types.find((t) => t.endsWith(".connect")) ??
          `${prefix}.connect`,
        send: types.find((t) => t.endsWith(".send")) ?? `${prefix}.send`,
        disconnect: types.find((t) => t.endsWith(".disconnect")) ??
          `${prefix}.disconnect`,
      };
    }
  } catch {
    // Env not available
  }
  return {
    connect: `${prefix}.connect`,
    send: `${prefix}.send`,
    disconnect: `${prefix}.disconnect`,
  };
}

/** Resolve publish types from EMERGENT_PUBLISHES or prefix. */
function resolvePublishTypes(prefix: string): PublishTypes {
  try {
    const envPubs = Deno.env.get("EMERGENT_PUBLISHES");
    if (envPubs) {
      const types = envPubs.split(",").map((t) => t.trim()).filter((t) =>
        t.length > 0
      );
      return {
        connected: types.find((t) => t.endsWith(".connected")) ??
          `${prefix}.connected`,
        frame: types.find((t) => t.endsWith(".frame")) ?? `${prefix}.frame`,
        closed: types.find((t) => t.endsWith(".closed")) ??
          `${prefix}.closed`,
        error: types.find((t) => t.endsWith(".error")) ?? `${prefix}.error`,
      };
    }
  } catch {
    // Env not available
  }
  return {
    connected: `${prefix}.connected`,
    frame: `${prefix}.frame`,
    closed: `${prefix}.closed`,
    error: `${prefix}.error`,
  };
}

// ============================================================================
// WebSocket Management
// ============================================================================

let currentWs: WebSocket | null = null;
let currentUrl: string | null = null;

// deno-lint-ignore prefer-const
let handler: EmergentHandler;
// deno-lint-ignore prefer-const
let pubTypes: PublishTypes;

/** The message ID of the connect command that opened the current connection. */
let connectCausationId: string | null = null;

async function handleConnect(
  msgId: string,
  payload: { url: string },
): Promise<void> {
  // Close existing connection if any
  if (currentWs) {
    currentWs.close();
    currentWs = null;
  }

  const url = payload.url;
  connectCausationId = msgId;

  try {
    const ws = new WebSocket(url);
    currentUrl = url;

    ws.onopen = async () => {
      currentWs = ws;
      try {
        await handler.publish(
          createMessage(pubTypes.connected)
            .causedBy(msgId)
            .payload({ url }),
        );
      } catch {
        // Publish failed — connection may be closing
      }
    };

    ws.onmessage = async (event: MessageEvent) => {
      let data: unknown;

      if (typeof event.data === "string") {
        // Text frame: try JSON parse, fall back to raw string
        try {
          data = JSON.parse(event.data);
        } catch {
          data = event.data;
        }
      } else if (event.data instanceof ArrayBuffer) {
        // Binary frame: base64 encode
        const bytes = new Uint8Array(event.data);
        data = btoa(String.fromCharCode(...bytes));
      } else {
        data = String(event.data);
      }

      try {
        await handler.publish(
          createMessage(pubTypes.frame)
            .causedBy(connectCausationId ?? msgId)
            .payload({ data }),
        );
      } catch {
        // Publish failed — handler may be shutting down
      }
    };

    ws.onclose = async (event: CloseEvent) => {
      try {
        await handler.publish(
          createMessage(pubTypes.closed)
            .causedBy(connectCausationId ?? msgId)
            .payload({
              url: currentUrl,
              code: event.code,
              reason: event.reason,
            }),
        );
      } catch {
        // Publish failed
      }
      currentWs = null;
      currentUrl = null;
      connectCausationId = null;
    };

    ws.onerror = async (event: Event) => {
      const errorMsg = event instanceof ErrorEvent
        ? event.message
        : "WebSocket error";
      try {
        await handler.publish(
          createMessage(pubTypes.error)
            .causedBy(connectCausationId ?? msgId)
            .payload({ url: currentUrl ?? url, error: errorMsg }),
        );
      } catch {
        // Publish failed
      }
    };
  } catch (e) {
    const errorMsg = e instanceof Error ? e.message : String(e);
    try {
      await handler.publish(
        createMessage(pubTypes.error)
          .causedBy(msgId)
          .payload({ url, error: errorMsg }),
      );
    } catch {
      // Publish failed
    }
  }
}

function handleSend(
  msgId: string,
  payload: unknown,
): void {
  if (!currentWs || currentWs.readyState !== WebSocket.OPEN) {
    handler.publish(
      createMessage(pubTypes.error)
        .causedBy(msgId)
        .payload({
          url: currentUrl,
          error: "No active WebSocket connection",
        }),
    ).catch(() => {});
    return;
  }

  const data = typeof payload === "string"
    ? payload
    : JSON.stringify(payload);
  currentWs.send(data);
}

function handleDisconnect(): void {
  if (currentWs) {
    currentWs.close();
    // onclose handler will publish the closed event
  }
}

// ============================================================================
// Main
// ============================================================================

const { prefix } = parseArgs();
const subTypes = resolveSubscribeTypes(prefix);
pubTypes = resolvePublishTypes(prefix);

const name = Deno.env.get("EMERGENT_NAME") ?? "websocket_handler";
handler = await EmergentHandler.connect(name);

const stream = await handler.subscribe([
  subTypes.connect,
  subTypes.send,
  subTypes.disconnect,
]);

// Graceful shutdown
const shutdown = () => {
  if (currentWs) {
    currentWs.close();
  }
  stream.close();
};

Deno.addSignalListener("SIGTERM", shutdown);
Deno.addSignalListener("SIGINT", shutdown);

// Message loop
for await (const msg of stream) {
  const msgType = msg.messageType;

  if (msgType === subTypes.connect) {
    const payload = msg.payloadAs<{ url: string }>();
    await handleConnect(msg.id, payload);
  } else if (msgType === subTypes.send) {
    handleSend(msg.id, msg.payload);
  } else if (msgType === subTypes.disconnect) {
    handleDisconnect();
  }
}

// Clean up
shutdown();
handler.close();
