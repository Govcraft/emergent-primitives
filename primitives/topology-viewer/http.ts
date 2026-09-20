/**
 * The viewer's HTTP surface, kept apart from `Deno.serve` so it can be
 * exercised with a plain `Request` and no listening socket.
 *
 * `decideRoute` is the pure part: method and path in, a decision out.
 * `handleRequest` carries the decision out against whatever it is handed for
 * the graph state, the static files, the event stream and the engine re-read,
 * which in the binary are the real ones and in a test are stand-ins.
 *
 * No response carries `Access-Control-Allow-Origin`. The page, its event
 * stream and its API are one origin, which needs no permission to read itself,
 * and the viewer has no authentication: the header would only let any other
 * web page open in the same browser read the topology.
 *
 * Every request is first asked which host it names (`host.ts`), and one that
 * names a host the viewer does not know as its own is answered `421` before
 * any route is looked at: that is what a DNS rebinding page sends.
 * @module
 */

import { decideRequestHost, misdirected } from "./host.ts";
import type { TopologyHealth, TopologyState } from "./types.ts";

/** Static files the viewer serves, by request path. */
const STATIC_FILES: Readonly<Record<string, string>> = {
  "/": "index.html",
  "/app.js": "app.js",
  "/style.css": "style.css",
};

/** Headers of the `/events` stream the page listens to. */
export const EVENT_STREAM_HEADERS: Readonly<Record<string, string>> = {
  "Content-Type": "text/event-stream",
  "Cache-Control": "no-cache",
  Connection: "keep-alive",
};

/** Path of the on-demand refresh endpoint. */
export const REFRESH_PATH = "/api/refresh";

/** What the viewer does with one request. */
export type RouteDecision =
  | { readonly kind: "static"; readonly file: string }
  | { readonly kind: "events" }
  | { readonly kind: "topology" }
  | { readonly kind: "refresh" }
  | { readonly kind: "methodNotAllowed"; readonly allow: string }
  | { readonly kind: "notFound" };

/**
 * Decide what a request is for.
 *
 * The refresh endpoint makes the viewer do work, so it answers `POST` only: a
 * link prefetch or a crawler issuing `GET` must not trigger an engine read.
 * The read-only routes keep answering every method, as they always have.
 */
export function decideRoute(method: string, path: string): RouteDecision {
  if (path === REFRESH_PATH) {
    return method === "POST"
      ? { kind: "refresh" }
      : { kind: "methodNotAllowed", allow: "POST" };
  }
  if (path === "/events") return { kind: "events" };
  if (path === "/api/topology") return { kind: "topology" };
  const file = STATIC_FILES[path];
  return file === undefined ? { kind: "notFound" } : { kind: "static", file };
}

/**
 * HTTP status for a refresh reply.
 *
 * The viewer stands between the caller and the engine here. When the engine
 * read failed the reply is `502`, so a script can tell a fresh topology from a
 * stale one without parsing the body. The body carries the state either way,
 * and its `health.detail` says what went wrong.
 */
export function refreshStatus(health: TopologyHealth): number {
  return health.status === "degraded" ? 502 : 200;
}

/**
 * Wrap `run` so that overlapping calls share one run.
 *
 * A caller arriving while a run is in flight gets that run's promise. The
 * next call after it settles starts a new one. This keeps a burst of refresh
 * requests, or a request landing on top of the interval re-read, from sending
 * the engine a burst of topology requests.
 */
export function singleFlight<T>(run: () => Promise<T>): () => Promise<T> {
  let inFlight: Promise<T> | null = null;
  return () => {
    inFlight ??= run().finally(() => {
      inFlight = null;
    });
    return inFlight;
  };
}

/** What `handleRequest` needs from the rest of the viewer. */
export interface ViewerHandlers {
  /** The current graph state. */
  readonly state: () => TopologyState;
  /** Re-read the engine topology into the graph, once. Never rejects. */
  readonly refresh: () => Promise<void>;
  /** Serve one file from `static/`. */
  readonly readStatic: (file: string) => Promise<Response>;
  /** Open a server-sent event stream. */
  readonly openEvents: () => Response;
}

/** The hosts the viewer answers to. */
export interface HostRules {
  /** The address or name given to `--host`. */
  readonly bound: string;
  /** The parsed `--allow-host` list. */
  readonly allowed: readonly string[];
}

function jsonResponse(
  body: unknown,
  status: number,
  headers: Record<string, string> = {},
): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json", ...headers },
  });
}

/** Answer one request. */
export async function handleRequest(
  req: Request,
  handlers: ViewerHandlers,
  hosts: HostRules,
): Promise<Response> {
  const host = decideRequestHost(
    req.headers.get("host"),
    req.url,
    hosts.bound,
    hosts.allowed,
  );
  if (host === "refuse") return misdirected();

  const decision = decideRoute(req.method, new URL(req.url).pathname);

  switch (decision.kind) {
    case "static":
      return await handlers.readStatic(decision.file);
    case "events":
      return handlers.openEvents();
    case "topology":
      return jsonResponse(handlers.state(), 200);
    case "refresh": {
      await handlers.refresh();
      const state = handlers.state();
      return jsonResponse(state, refreshStatus(state.health), {
        "Cache-Control": "no-store",
      });
    }
    case "methodNotAllowed":
      return new Response("Method Not Allowed", {
        status: 405,
        headers: { Allow: decision.allow },
      });
    case "notFound":
      return new Response("Not Found", { status: 404 });
  }
}
