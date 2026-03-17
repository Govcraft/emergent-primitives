#!/usr/bin/env -S deno run --allow-env --allow-read --allow-write --allow-net
/**
 * SSE Sink — Push pipeline events to browsers via Server-Sent Events.
 *
 * Subscribes to configured message types and broadcasts each event to all
 * connected SSE clients on the /events endpoint.
 *
 * Usage:
 *   sse-sink --port 8080
 *
 * Connect from a browser:
 *   const source = new EventSource("http://localhost:8080/events");
 *   source.onmessage = (e) => console.log(JSON.parse(e.data));
 *
 * @module
 */

import { runSink } from "jsr:@govcraft/emergent@0.12.0";
import type { EmergentMessage } from "jsr:@govcraft/emergent@0.12.0";

// ============================================================================
// CLI
// ============================================================================

function parseArgs(): { port: number } {
  const args = Deno.args;
  let port = 8080;

  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--port" && args[i + 1]) {
      port = parseInt(args[i + 1], 10);
      if (isNaN(port) || port < 1 || port > 65535) {
        console.error("Invalid port number");
        Deno.exit(1);
      }
      i++;
    }
  }

  return { port };
}

// ============================================================================
// SSE Server
// ============================================================================

const clients = new Set<ReadableStreamDefaultController<Uint8Array>>();
const encoder = new TextEncoder();

function broadcast(msg: EmergentMessage): void {
  const data = JSON.stringify({
    id: msg.id,
    type: msg.messageType,
    source: msg.source,
    timestamp: msg.timestampMs,
    payload: msg.payload,
  });
  const message = encoder.encode(`data: ${data}\n\n`);
  for (const controller of clients) {
    try {
      controller.enqueue(message);
    } catch {
      clients.delete(controller);
    }
  }
}

function handleRequest(req: Request): Response {
  const url = new URL(req.url);

  if (url.pathname === "/events") {
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        clients.add(controller);
      },
      cancel(controller) {
        clients.delete(controller);
      },
    });

    return new Response(body, {
      headers: {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
        "connection": "keep-alive",
        "access-control-allow-origin": "*",
      },
    });
  }

  if (url.pathname === "/health") {
    return new Response(
      JSON.stringify({ ok: true, clients: clients.size }),
      { headers: { "content-type": "application/json" } },
    );
  }

  return new Response("Not Found", { status: 404 });
}

// ============================================================================
// Main
// ============================================================================

const { port } = parseArgs();

// Resolve subscribe types from EMERGENT_SUBSCRIBES env var
let subscribeTypes: string[];
try {
  const envSubs = Deno.env.get("EMERGENT_SUBSCRIBES");
  subscribeTypes = envSubs
    ? envSubs.split(",").map((t) => t.trim()).filter((t) => t.length > 0)
    : ["*"];
} catch {
  subscribeTypes = ["*"];
}

// Start SSE server
Deno.serve({ port, handler: handleRequest });
console.error(`[sse-sink] Listening on http://localhost:${port}/events`);

// Connect to engine and broadcast events
await runSink(undefined, subscribeTypes, (msg) => {
  broadcast(msg);
  return Promise.resolve();
});
