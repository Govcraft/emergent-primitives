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
 * Whatever the address, a request is answered only when the host it names is
 * an IP address, `localhost`, or a name listed with `--allow-host`
 * (repeatable): that is what stops a DNS rebinding page, and it is what a
 * reverse proxy that forwards its own name needs listed.
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
  describeAllowedOrigins,
  parseAllowedOrigins,
} from "./cors.ts";
import {
  ALLOW_HOST_FLAG,
  describeAllowedHosts,
  parseAllowedHosts,
} from "./host.ts";
import { clientStream, handleRequest } from "./http.ts";
import type { Clients, SinkRules } from "./http.ts";

// ============================================================================
// CLI
// ============================================================================

const USAGE = "Usage: sse-sink [--host HOST] [--port PORT] " +
  "[--allow-origin ORIGIN]... [--allow-host NAME]...";

// Parse the arguments, or say what is wrong with them in one line and exit.
function optionsOrExit(): ListenOptions & { rules: SinkRules } {
  const parsed = parseListenArgs(Deno.args, [
    ALLOW_ORIGIN_FLAG,
    ALLOW_HOST_FLAG,
  ]);
  if (!parsed.ok) {
    console.error(`${parsed.error}. ${USAGE}`);
    Deno.exit(1);
  }
  const origins = parseAllowedOrigins(parsed.repeated[ALLOW_ORIGIN_FLAG]);
  if (!origins.ok) {
    console.error(`${origins.error}. ${USAGE}`);
    Deno.exit(1);
  }
  const hosts = parseAllowedHosts(parsed.repeated[ALLOW_HOST_FLAG]);
  if (!hosts.ok) {
    console.error(`${hosts.error}. ${USAGE}`);
    Deno.exit(1);
  }
  return {
    ...parsed.options,
    rules: {
      bound: parsed.options.host,
      allowedHosts: hosts.hosts,
      allowedOrigins: origins.allowed,
    },
  };
}

// ============================================================================
// SSE Server
// ============================================================================

const clients: Clients = new Set();
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

// ============================================================================
// Main
// ============================================================================

const { host, port, rules } = optionsOrExit();

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
    handler: (req) =>
      handleRequest(
        req,
        {
          openStream: () => clientStream(clients),
          clientCount: () => clients.size,
        },
        rules,
      ),
    // Log the address that was bound, not the one that was asked for.
    onListen: (addr) =>
      console.error(
        `[sse-sink] Listening on ${
          listenUrl(addr.hostname, addr.port, "/events")
        }, answering to ${
          describeAllowedHosts(host, rules.allowedHosts)
        }, readable from a browser by ${
          describeAllowedOrigins(rules.allowedOrigins)
        }`,
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
