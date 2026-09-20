#!/usr/bin/env -S deno run --allow-env --allow-read --allow-write --allow-net
/**
 * SSE Sink — Push pipeline events to browsers via Server-Sent Events.
 *
 * Subscribes to configured message types and broadcasts each event to all
 * connected SSE clients on the /events endpoint.
 *
 * Usage:
 *   sse-sink --port 8080 --allow-origin http://localhost:3000
 *   sse-sink --host 0.0.0.0 --port 8080 --allow-origin https://app.example
 *
 * The stream is served on 127.0.0.1 unless `--host` says otherwise. It has no
 * authentication and by default carries every event in the pipeline, so
 * putting it on the network is a decision, not a default.
 *
 * So is letting a web page read it. A browser hands the stream to a page on
 * another origin only when the response allows that origin, and the response
 * does so only for the origins named with `--allow-origin` (repeatable, `*`
 * for every origin).
 *
 * Connect from a page served by http://localhost:3000:
 *   const source = new EventSource("http://localhost:8080/events");
 *   source.onmessage = (e) => console.log(JSON.parse(e.data));
 *
 * @module
 */

import { runSink } from "jsr:@govcraft/emergent@0.13.0";
import type { EmergentMessage } from "jsr:@govcraft/emergent@0.13.0";
import { bindFailure, listenUrl, parseListenArgs } from "./args.ts";
import type { ListenOptions } from "./args.ts";
import {
  ALLOW_ORIGIN_FLAG,
  corsHeaders,
  describeAllowedOrigins,
  parseAllowedOrigins,
} from "./cors.ts";
import type { AllowedOrigins } from "./cors.ts";

// ============================================================================
// CLI
// ============================================================================

const USAGE =
  "Usage: sse-sink [--host HOST] [--port PORT] [--allow-origin ORIGIN]...";

// Parse the arguments, or say what is wrong with them in one line and exit.
function optionsOrExit(): ListenOptions & { allowed: AllowedOrigins } {
  const parsed = parseListenArgs(Deno.args, [ALLOW_ORIGIN_FLAG]);
  if (!parsed.ok) {
    console.error(`${parsed.error}. ${USAGE}`);
    Deno.exit(1);
  }
  const origins = parseAllowedOrigins(parsed.repeated[ALLOW_ORIGIN_FLAG]);
  if (!origins.ok) {
    console.error(`${origins.error}. ${USAGE}`);
    Deno.exit(1);
  }
  return { ...parsed.options, allowed: origins.allowed };
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
        ...corsHeaders(req.headers.get("origin"), allowed),
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

const { host, port, allowed } = optionsOrExit();

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

// Start SSE server, or say in one line why the address could not be bound
try {
  Deno.serve({
    hostname: host,
    port,
    handler: handleRequest,
    // Log the address that was bound, not the one that was asked for.
    onListen: (addr) =>
      console.error(
        `[sse-sink] Listening on ${
          listenUrl(addr.hostname, addr.port, "/events")
        }, readable from a browser by ${describeAllowedOrigins(allowed)}`,
      ),
  });
} catch (err) {
  console.error(
    bindFailure(host, port, err instanceof Error ? err.message : String(err)),
  );
  Deno.exit(1);
}

// Connect to engine and broadcast events
await runSink(undefined, subscribeTypes, (msg) => {
  broadcast(msg);
  return Promise.resolve();
});
