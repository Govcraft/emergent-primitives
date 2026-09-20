/**
 * The sink's HTTP surface, kept apart from `Deno.serve` so it can be exercised
 * with a plain `Request` and no listening socket.
 *
 * Two questions are asked of every request before it is served. Which host
 * does it name (`host.ts`): one the sink does not know as its own is answered
 * `421`, which is what a DNS rebinding page sends. And, for the stream, which
 * origin is asking (`cors.ts`): only a listed one is named in the reply.
 * @module
 */

import { corsHeaders } from "./cors.ts";
import type { AllowedOrigins } from "./cors.ts";
import { decideRequestHost, misdirected } from "./host.ts";

/** What the operator allowed on the command line. */
export interface SinkRules {
  /** The address or name given to `--host`. */
  readonly bound: string;
  /** The parsed `--allow-host` list. */
  readonly allowedHosts: readonly string[];
  /** The parsed `--allow-origin` list. */
  readonly allowedOrigins: AllowedOrigins;
}

/** What the HTTP surface needs from the rest of the sink. */
export interface SinkHandlers {
  /** Open one client's event stream. */
  readonly openStream: () => ReadableStream<Uint8Array>;
  /** How many clients are connected. */
  readonly clientCount: () => number;
}

/** The connected clients: one stream controller each. */
export type Clients = Set<ReadableStreamDefaultController<Uint8Array>>;

/**
 * One client's event stream, in `clients` for as long as it is open. `cancel`
 * is handed the reason the stream closed, not the controller, so the
 * controller is kept from `start`.
 */
export function clientStream(clients: Clients): ReadableStream<Uint8Array> {
  let client: ReadableStreamDefaultController<Uint8Array> | undefined;
  return new ReadableStream<Uint8Array>({
    start(controller) {
      client = controller;
      clients.add(controller);
    },
    cancel() {
      if (client !== undefined) clients.delete(client);
    },
  });
}

/** Answer one request. */
export function handleRequest(
  req: Request,
  handlers: SinkHandlers,
  rules: SinkRules,
): Response {
  const host = decideRequestHost(
    req.headers.get("host"),
    req.url,
    rules.bound,
    rules.allowedHosts,
  );
  if (host === "refuse") return misdirected();

  const path = new URL(req.url).pathname;

  if (path === "/events") {
    return new Response(handlers.openStream(), {
      headers: {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
        "connection": "keep-alive",
        ...corsHeaders(req.headers.get("origin"), rules.allowedOrigins),
      },
    });
  }

  if (path === "/health") {
    return new Response(
      JSON.stringify({ ok: true, clients: handlers.clientCount() }),
      { headers: { "content-type": "application/json" } },
    );
  }

  return new Response("Not Found", { status: 404 });
}
