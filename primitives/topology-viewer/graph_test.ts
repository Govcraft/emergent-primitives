import { assertEquals, assertThrows } from "@std/assert";
import * as sdk from "@govcraft/emergent";
import {
  assessHealth,
  computeEdges,
  ENGINE_NODE_ID,
  nodeFromPrimitive,
  parseTopologyResponse,
  patternPrefix,
  publicationReaches,
  sortNodes,
  statusFromEngineState,
  topicMatches,
  TopologyGraph,
} from "./graph.ts";
import type { EnginePrimitive, TopologyNode } from "./types.ts";

function node(
  id: string,
  publishes: string[],
  subscribes: string[],
): TopologyNode {
  return { id, kind: "handler", status: "running", publishes, subscribes };
}

function primitive(name: string, state = "running"): EnginePrimitive {
  return { name, kind: "source", state, publishes: [], subscribes: [] };
}

const ENGINE: EnginePrimitive = {
  name: ENGINE_NODE_ID,
  kind: "source",
  state: "running",
  publishes: ["system.started.*", "system.shutdown"],
  subscribes: [],
  pid: 42,
  error: null,
};

// [topic, messageType, delivered]
const ROUTING: Array<[string, string, boolean]> = [
  ["timer.tick", "timer.tick", true],
  ["timer.tick", "timer.tock", false],
  ["timer.tick", "timer.tick.extra", false],
  ["timer.*", "timer.tick", true],
  ["timer.*", "timer.tick.extra", true],
  ["timer.*", "timer.", true],
  ["timer.*", "timer", false],
  ["timer.*", "timers.tick", false],
  ["timer*", "timers.tick", true],
  ["*", "timer.tick", true],
  ["*", "", true],
  ["system.*.error", "system.ticker.error", false],
  ["system.*.error", "system.*.error", false],
  ["*.tick", "timer.tick", false],
  ["timer.**", "timer.tick", false],
  ["", "", true],
  ["", "timer.tick", false],
];

Deno.test("patternPrefix accepts only a single terminal wildcard", () => {
  assertEquals(patternPrefix("timer.*"), "timer.");
  assertEquals(patternPrefix("*"), "");
  assertEquals(patternPrefix("timer.tick"), null);
  assertEquals(patternPrefix("system.*.error"), null);
  assertEquals(patternPrefix("timer.**"), null);
});

Deno.test("topicMatches routes the way the engine does", () => {
  for (const [topic, messageType, delivered] of ROUTING) {
    assertEquals(
      topicMatches(topic, messageType),
      delivered,
      `topic '${topic}' against '${messageType}'`,
    );
  }
});

// SDK releases before 0.14.0 predate the topic helpers, so this is skipped when
// the SDK in use does not export them. On the pinned 0.14.0 it runs.
const sdkTopicMatches = (sdk as Record<string, unknown>).topicMatches;

Deno.test({
  name: "topicMatches agrees with the SDK's topicMatches",
  ignore: typeof sdkTopicMatches !== "function",
  fn() {
    const reference = sdkTopicMatches as typeof topicMatches;
    for (const [topic, messageType] of ROUTING) {
      assertEquals(
        topicMatches(topic, messageType),
        reference(topic, messageType),
        `topic '${topic}' against '${messageType}'`,
      );
    }
  },
});

Deno.test("publicationReaches treats a published wildcard as a family", () => {
  // Literal publications follow topicMatches
  assertEquals(publicationReaches("timer.tick", "timer.*"), true);
  assertEquals(publicationReaches("timer.tick", "*"), true);
  assertEquals(publicationReaches("timer.tick", "timer.*.x"), false);
  // The engine declares "system.started.*" for one type per primitive
  assertEquals(
    publicationReaches("system.started.*", "system.started.*"),
    true,
  );
  assertEquals(
    publicationReaches("system.started.*", "system.started.ticker"),
    true,
  );
  assertEquals(publicationReaches("system.started.*", "system.*"), true);
  assertEquals(publicationReaches("system.started.*", "*"), true);
  assertEquals(
    publicationReaches("system.started.*", "system.started.a.*"),
    true,
  );
  assertEquals(
    publicationReaches("system.started.*", "system.stopped.*"),
    false,
  );
  assertEquals(
    publicationReaches("system.started.*", "system.shutdown"),
    false,
  );
  assertEquals(
    publicationReaches("system.started.*", "system.*.ticker"),
    false,
  );
  assertEquals(publicationReaches("system.*.x", "*"), false);
});

Deno.test("computeEdges draws terminal wildcards and skips misplaced ones", () => {
  const edges = computeEdges([
    node("timer", ["timer.tick", "timer.done"], []),
    node("exact", [], ["timer.tick"]),
    node("prefix", [], ["timer.*"]),
    node("all", [], ["*"]),
    node("misplaced", [], ["*.tick", "timer.*.tick"]),
    node("unrelated", [], ["clock.*"]),
  ]);

  assertEquals(edges, [
    { source: "timer", target: "exact", messageType: "timer.tick" },
    { source: "timer", target: "prefix", messageType: "timer.tick" },
    { source: "timer", target: "all", messageType: "timer.tick" },
    { source: "timer", target: "prefix", messageType: "timer.done" },
    { source: "timer", target: "all", messageType: "timer.done" },
  ]);
});

Deno.test("computeEdges draws one edge per type and never a self edge", () => {
  const edges = computeEdges([
    node("loop", ["a.out"], ["a.*", "a.out", "*"]),
    node("sink", [], ["a.*", "a.out"]),
  ]);
  assertEquals(edges, [
    { source: "loop", target: "sink", messageType: "a.out" },
  ]);
});

Deno.test("sortNodes puts the engine first and the rest in name order", () => {
  const ids = ["ticker", ENGINE_NODE_ID, "alpha", "topology"];
  const sorted = sortNodes(ids.map((id) => node(id, [], [])));
  assertEquals(sorted.map((n) => n.id), [
    ENGINE_NODE_ID,
    "alpha",
    "ticker",
    "topology",
  ]);
});

Deno.test("statusFromEngineState maps engine lifecycle states", () => {
  assertEquals(statusFromEngineState("running"), "running");
  assertEquals(statusFromEngineState("external"), "running");
  assertEquals(statusFromEngineState("failed"), "error");
  for (const state of ["configured", "starting", "stopping", "stopped", "?"]) {
    assertEquals(statusFromEngineState(state), "stopped");
  }
});

Deno.test("nodeFromPrimitive drops the engine's null pid and error", () => {
  const built = nodeFromPrimitive({
    ...primitive("ticker", "configured"),
    pid: null,
    error: null,
  });
  assertEquals(built, {
    id: "ticker",
    kind: "source",
    status: "stopped",
    publishes: [],
    subscribes: [],
    pid: undefined,
    error: undefined,
  });
});

Deno.test("parseTopologyResponse accepts an engine body and rejects others", () => {
  assertEquals(parseTopologyResponse({ primitives: [ENGINE] }), [ENGINE]);
  assertThrows(() => parseTopologyResponse(null), Error, "no primitives");
  assertThrows(() => parseTopologyResponse({ nodes: [] }), Error);
  assertThrows(
    () => parseTopologyResponse({ primitives: [{ name: "x" }] }),
    Error,
    "primitive 0 is malformed",
  );
  assertThrows(
    () =>
      parseTopologyResponse({
        primitives: [{ ...ENGINE, subscribes: [1] }],
      }),
    Error,
    "malformed",
  );
});

Deno.test("assessHealth reports pending, empty, degraded and ok", () => {
  const engineOnly = [nodeFromPrimitive(ENGINE)];
  const populated = [...engineOnly, nodeFromPrimitive(primitive("ticker"))];

  assertEquals(assessHealth([], { status: "pending" }).status, "pending");
  assertEquals(
    assessHealth(engineOnly, { status: "pending" }).status,
    "pending",
  );
  assertEquals(assessHealth(engineOnly, { status: "loaded" }).status, "empty");
  assertEquals(assessHealth(populated, { status: "loaded" }), { status: "ok" });
  assertEquals(assessHealth(populated, { status: "pending" }), {
    status: "ok",
  });

  const failed = assessHealth(populated, {
    status: "failed",
    reason: "signal timed out",
  });
  assertEquals(failed.status, "degraded");
  assertEquals(failed.detail?.includes("signal timed out"), true);
});

Deno.test("TopologyGraph reports an engine-only topology as empty", () => {
  const graph = new TopologyGraph();
  graph.addInitialNode(nodeFromPrimitive(ENGINE));
  assertEquals(graph.getFullState().health.status, "pending");

  graph.handleTopologyRefresh([ENGINE]);
  const state = graph.getFullState();
  assertEquals(state.nodes.map((n) => n.id), [ENGINE_NODE_ID]);
  assertEquals(state.health.status, "empty");
});

Deno.test("TopologyGraph fills from a topology response and recovers from failure", () => {
  const graph = new TopologyGraph();
  graph.addInitialNode(nodeFromPrimitive(ENGINE));

  graph.handleTopologyFailure("connection refused");
  assertEquals(graph.getFullState().health.status, "degraded");

  graph.handleTopologyRefresh([
    ENGINE,
    {
      ...primitive("viewer"),
      kind: "sink",
      subscribes: ["system.started.*"],
    },
    primitive("ticker", "failed"),
  ]);
  const state = graph.getFullState();
  assertEquals(state.health, { status: "ok" });
  assertEquals(state.nodes.map((n) => [n.id, n.status]), [
    [ENGINE_NODE_ID, "running"],
    ["ticker", "error"],
    ["viewer", "running"],
  ]);
  assertEquals(state.edges, [{
    source: ENGINE_NODE_ID,
    target: "viewer",
    messageType: "system.started.*",
  }]);

  // A primitive the engine no longer reports is dropped
  graph.handleTopologyRefresh([ENGINE, primitive("ticker")]);
  assertEquals(graph.getFullState().nodes.map((n) => n.id), [
    ENGINE_NODE_ID,
    "ticker",
  ]);
});

Deno.test("TopologyGraph keeps the seeded engine node when a response omits it", () => {
  const graph = new TopologyGraph();
  graph.addInitialNode(nodeFromPrimitive(ENGINE));
  graph.handleTopologyRefresh([primitive("ticker")]);
  assertEquals(graph.getFullState().nodes.map((n) => n.id), [
    ENGINE_NODE_ID,
    "ticker",
  ]);
});

/** Collect the SSE event names a graph sends to one client. */
function recordEvents(graph: TopologyGraph): string[] {
  const events: string[] = [];
  const decoder = new TextDecoder();
  new ReadableStream<Uint8Array>({
    start(controller) {
      const enqueue = controller.enqueue.bind(controller);
      controller.enqueue = (chunk) => {
        if (chunk) events.push(decoder.decode(chunk).split("\n")[0]);
        enqueue(chunk);
      };
      graph.registerSSEClient(controller);
    },
  });
  return events;
}

Deno.test("TopologyGraph broadcasts a refresh only when the state changed", () => {
  const graph = new TopologyGraph();
  const events = recordEvents(graph);
  assertEquals(events, ["event: topology:full"]);

  graph.handleTopologyRefresh([ENGINE, primitive("ticker")]);
  graph.handleTopologyRefresh([ENGINE, primitive("ticker")]);
  assertEquals(events, ["event: topology:full", "event: topology:full"]);

  // The engine lists primitives in no fixed order
  graph.handleTopologyRefresh([primitive("ticker"), ENGINE]);
  assertEquals(events.length, 2);

  graph.handleTopologyRefresh([ENGINE, primitive("ticker", "stopped")]);
  assertEquals(events.length, 3);
});

Deno.test("TopologyGraph applies lifecycle events and updates health", () => {
  const graph = new TopologyGraph();
  graph.handleTopologyRefresh([ENGINE]);
  const events = recordEvents(graph);

  graph.handleStarted({
    name: "ticker",
    kind: "source",
    pid: 7,
    publishes: ["tick.out"],
  });
  assertEquals(events.slice(1), [
    "event: node:added",
    "event: edges:updated",
    "event: health:updated",
  ]);
  assertEquals(graph.getFullState().health, { status: "ok" });

  graph.handleStopped({ name: "ticker", kind: "source" });
  const stopped = graph.getFullState().nodes.find((n) => n.id === "ticker");
  assertEquals([stopped?.status, stopped?.pid], ["stopped", undefined]);

  graph.handleError({ name: "ticker", kind: "source", error: "exit 1" });
  const failed = graph.getFullState().nodes.find((n) => n.id === "ticker");
  assertEquals([failed?.status, failed?.error], ["error", "exit 1"]);

  // The engine reports the same thing, so re-reading it is not a change
  const before = events.length;
  graph.handleTopologyRefresh([ENGINE, {
    ...primitive("ticker", "failed"),
    publishes: ["tick.out"],
    pid: null,
    error: "exit 1",
  }]);
  assertEquals(events.length, before);
});
