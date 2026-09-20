#!/usr/bin/env -S deno run --allow-env --allow-read --allow-write --allow-net
/**
 * Topology Viewer Sink - Real-time workflow visualization.
 *
 * Subscribes to system lifecycle events and serves a D3.js-based
 * force-directed graph visualization via HTTP/SSE.
 *
 * Usage:
 *   deno run --allow-env --allow-read --allow-write --allow-net main.ts --port 8080
 *
 * The page is served on 127.0.0.1 unless `--host` says otherwise: it shows
 * every primitive's name, topics, state and PID, so exposing it is a decision.
 * Whatever the address, a request is answered only when the host it names is
 * an IP address, `localhost`, or a name listed with `--allow-host`
 * (repeatable): that is what stops a DNS rebinding page, and it is what a
 * reverse proxy that forwards its own name needs listed.
 *
 * `--allow-write` is for the engine socket: Deno asks for read and write
 * access to a Unix socket path before it will connect to it.
 *
 * The SDK automatically handles system.shutdown for graceful shutdown.
 */

import { EmergentSink } from "jsr:@govcraft/emergent@0.13.0";
import type { SystemEventPayload } from "jsr:@govcraft/emergent@0.13.0";
import { bindFailure, listenUrl, parseListenArgs } from "./args.ts";
import type { ListenOptions } from "./args.ts";
import {
  ALLOW_HOST_FLAG,
  describeAllowedHosts,
  parseAllowedHosts,
} from "./host.ts";
import {
  ENGINE_NODE_ID,
  parseTopologyResponse,
  TopologyGraph,
} from "./graph.ts";
import { EVENT_STREAM_HEADERS, handleRequest, singleFlight } from "./http.ts";
import type { EnginePrimitive, TopologyNode } from "./types.ts";

const USAGE =
  "Usage: topology-viewer [--host HOST] [--port PORT] [--allow-host NAME]...";

// Parse the arguments, or say what is wrong with them in one line and exit.
function optionsOrExit(): ListenOptions & { allowedHosts: readonly string[] } {
  const parsed = parseListenArgs(Deno.args, [ALLOW_HOST_FLAG]);
  if (!parsed.ok) {
    console.error(`${parsed.error}. ${USAGE}`);
    Deno.exit(1);
  }
  const hosts = parseAllowedHosts(parsed.repeated[ALLOW_HOST_FLAG]);
  if (!hosts.ok) {
    console.error(`${hosts.error}. ${USAGE}`);
    Deno.exit(1);
  }
  return { ...parsed.options, allowedHosts: hosts.hosts };
}

// Get current script directory for static file serving
const scriptDir = new URL(".", import.meta.url).pathname;

// Content types for static files
const contentTypes: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
};

// Read a static file
async function readStaticFile(filename: string): Promise<Response> {
  const ext = filename.substring(filename.lastIndexOf("."));
  const contentType = contentTypes[ext] ?? "application/octet-stream";

  try {
    const content = await Deno.readTextFile(`${scriptDir}static/${filename}`);
    return new Response(content, {
      headers: { "Content-Type": contentType },
    });
  } catch {
    return new Response("Not Found", { status: 404 });
  }
}

// Start the server, or say in one line why the address could not be bound.
function serveOrExit(
  options: Deno.ServeTcpOptions,
  handler: Deno.ServeHandler,
): Deno.HttpServer {
  try {
    return Deno.serve(options, handler);
  } catch (err) {
    console.error(
      bindFailure(
        options.hostname ?? "",
        options.port ?? 0,
        errorMessage(err),
      ),
    );
    Deno.exit(1);
  }
}

// Create SSE response
function createSSEStream(graph: TopologyGraph): Response {
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      graph.registerSSEClient(controller);
    },
    cancel() {
      // Controller cleanup happens in graph.broadcast when write fails
    },
  });

  return new Response(stream, { headers: EVENT_STREAM_HEADERS });
}

// Delay helper
function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// How long the engine gets to answer a topology request
const TOPOLOGY_TIMEOUT_MS = 5000;

// How often the engine topology is re-read while connected
const TOPOLOGY_REFRESH_MS = 5000;

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

// Read the current topology from the engine HTTP API. The engine sets
// EMERGENT_API_PORT for every primitive it starts.
async function fetchEngineTopology(): Promise<EnginePrimitive[]> {
  const apiPort = Deno.env.get("EMERGENT_API_PORT") ?? "8891";
  const url = `http://127.0.0.1:${apiPort}/api/topology`;
  const resp = await fetch(url, {
    signal: AbortSignal.timeout(TOPOLOGY_TIMEOUT_MS),
  });
  if (!resp.ok) {
    await resp.body?.cancel();
    throw new Error(`GET ${url} answered ${resp.status}`);
  }
  return parseTopologyResponse(await resp.json());
}

// Load the engine topology into the graph, or record why it could not be
// read so the page does not present a partial graph as the whole topology.
async function refreshFromEngine(
  name: string,
  graph: TopologyGraph,
): Promise<void> {
  try {
    graph.handleTopologyRefresh(await fetchEngineTopology());
  } catch (err) {
    console.log(`[${name}] Topology request failed: ${errorMessage(err)}`);
    graph.handleTopologyFailure(errorMessage(err));
  }
}

// Subscriptions for system lifecycle events and topology responses
const SUBSCRIPTIONS = [
  "system.started.*",
  "system.stopped.*",
  "system.error.*",
  "system.response.topology", // Responses to topology refresh requests
];

// Connect to engine with retry logic
async function connectWithRetry(
  name: string,
  graph: TopologyGraph,
  refresh: () => Promise<void>,
  maxRetries = 30,
  retryDelayMs = 2000,
): Promise<void> {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      console.log(
        `[${name}] Connecting to engine (attempt ${attempt}/${maxRetries})...`,
      );

      // Connect explicitly and subscribe with our own types
      // (not relying on engine config since we're externally managed)
      const sink = await EmergentSink.connect(name);

      try {
        // Subscribe first so no lifecycle event is lost while the topology
        // request is in flight. Wildcard delivery needs an engine and SDK
        // that route terminal wildcards, so the graph does not depend on it:
        // the engine topology is read now and re-read on an interval.
        console.log(`[${name}] Subscribing to: ${SUBSCRIPTIONS.join(", ")}`);
        const stream = await sink.subscribe(SUBSCRIPTIONS);
        await refresh();
        const refreshTimer = setInterval(refresh, TOPOLOGY_REFRESH_MS);

        try {
          for await (const msg of stream) {
            if (msg.messageType === "system.response.topology") {
              // Handle topology refresh response
              try {
                const primitives = parseTopologyResponse(
                  msg.payloadAs<unknown>(),
                );
                console.log(
                  `[${name}] Topology refresh: ${primitives.length} primitive(s)`,
                );
                graph.handleTopologyRefresh(primitives);
              } catch (err) {
                console.log(
                  `[${name}] Ignored topology response: ${errorMessage(err)}`,
                );
              }
            } else {
              const payload = msg.payloadAs<SystemEventPayload>();

              if (msg.messageType.startsWith("system.started.")) {
                console.log(
                  `[${name}] Started: ${payload.name} (${payload.kind}) pid=${payload.pid}`,
                );
                graph.handleStarted(payload);
              } else if (msg.messageType.startsWith("system.stopped.")) {
                console.log(`[${name}] Stopped: ${payload.name}`);
                graph.handleStopped(payload);
              } else if (msg.messageType.startsWith("system.error.")) {
                console.log(
                  `[${name}] Error: ${payload.name} - ${payload.error}`,
                );
                graph.handleError(payload);
              }
            }
          }
        } finally {
          clearInterval(refreshTimer);
          stream.close();
        }
      } finally {
        sink.close();
      }

      // Stream ended gracefully (shutdown)
      console.log(`[${name}] Engine connection closed`);
      return;
    } catch (err) {
      const errorMsg = errorMessage(err);
      console.log(`[${name}] Connection failed: ${errorMsg}`);
      graph.handleTopologyFailure(`not connected to the engine (${errorMsg})`);

      if (attempt < maxRetries) {
        console.log(`[${name}] Retrying in ${retryDelayMs / 1000}s...`);
        await delay(retryDelayMs);
      }
    }
  }

  console.error(`[${name}] Failed to connect after ${maxRetries} attempts`);
}

// Main entry point
async function main(): Promise<void> {
  const { host, port, allowedHosts } = optionsOrExit();
  const name = Deno.env.get("EMERGENT_NAME") ?? "topology-viewer";
  const graph = new TopologyGraph();

  // Add the engine as a known node (it doesn't emit system.started for itself)
  const engineNode: TopologyNode = {
    id: ENGINE_NODE_ID,
    kind: "source", // Engine acts as a source of system.* events
    status: "running",
    publishes: [
      "system.started.*",
      "system.stopped.*",
      "system.error.*",
      "system.shutdown",
    ],
    subscribes: [],
    pid: undefined, // Unknown from our perspective
  };
  graph.addInitialNode(engineNode);

  console.log(`[${name}] Starting topology viewer on ${host} port ${port}`);

  // One engine re-read at a time, shared by the interval and by
  // `POST /api/refresh`, so a click landing on top of a tick does not send the
  // engine two topology requests.
  const refresh = singleFlight(() => refreshFromEngine(name, graph));

  // Start HTTP server first (non-blocking)
  const server = serveOrExit(
    {
      hostname: host,
      port,
      // Log the address that was bound, not the one that was asked for.
      onListen: (addr) =>
        console.log(
          `[${name}] HTTP server listening on ${
            listenUrl(addr.hostname, addr.port)
          }, answering to ${describeAllowedHosts(host, allowedHosts)}`,
        ),
    },
    (req: Request): Promise<Response> =>
      handleRequest(req, {
        state: () => graph.getFullState(),
        refresh,
        readStatic: readStaticFile,
        openEvents: () => createSSEStream(graph),
      }, { bound: host, allowed: allowedHosts }),
  );

  // Connect to engine with retry (runs in background)
  await connectWithRetry(name, graph, refresh);

  console.log(`[${name}] Shutting down`);
  await server.shutdown();
}

if (import.meta.main) {
  main();
}
