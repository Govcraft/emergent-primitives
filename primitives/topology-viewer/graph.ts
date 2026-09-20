/**
 * Topology graph state management.
 * @module
 */

import type {
  EnginePrimitive,
  NodeStatus,
  PrimitiveKind,
  SnapshotState,
  SSEMessage,
  TopologyEdge,
  TopologyHealth,
  TopologyNode,
  TopologyState,
} from "./types.ts";
import type { SystemEventPayload } from "jsr:@govcraft/emergent@0.14.0";

/** The name the engine reports itself under in topology responses. */
export const ENGINE_NODE_ID = "emergent-engine";

/**
 * Return the literal prefix of a terminal-wildcard topic.
 *
 * "tick.*" yields "tick." and "*" yields "". A topic with no trailing "*"
 * yields null, and so does one whose prefix contains a "*", because that topic
 * is not a usable selector. Same rule as `patternPrefix` in the Emergent SDK.
 */
export function patternPrefix(topic: string): string | null {
  if (!topic.endsWith("*")) return null;
  const prefix = topic.slice(0, -1);
  if (prefix.includes("*")) return null;
  return prefix;
}

/**
 * Test a message type against one subscription topic, using the rule the
 * engine routes by. Same rule as `topicMatches` in the Emergent SDK, which the
 * tests hold this function to whenever the SDK in use exports it.
 *   - "timer.tick" matches only "timer.tick"
 *   - "timer.*" matches every type that starts with "timer."
 *   - "*" matches everything
 *   - "system.*.error" has a misplaced wildcard and matches nothing
 */
export function topicMatches(topic: string, messageType: string): boolean {
  const prefix = patternPrefix(topic);
  if (prefix === null) return !topic.includes("*") && topic === messageType;
  return messageType.startsWith(prefix);
}

/**
 * Decide whether a declared publication can reach a subscription topic.
 *
 * A literal publication is a message type, so this is `topicMatches`. The
 * engine declares its own publications as families ("system.started.*"
 * stands for one type per primitive), and a family reaches a subscription
 * when the two can share a message type.
 */
export function publicationReaches(
  published: string,
  subscribed: string,
): boolean {
  if (!published.includes("*")) return topicMatches(subscribed, published);
  const family = patternPrefix(published);
  if (family === null) return false;
  const selector = patternPrefix(subscribed);
  if (selector === null) {
    return !subscribed.includes("*") && subscribed.startsWith(family);
  }
  return family.startsWith(selector) || selector.startsWith(family);
}

/**
 * Compute edges from publish/subscribe declarations: one edge per publisher,
 * subscriber and published type the subscriber would be delivered.
 */
export function computeEdges(nodes: readonly TopologyNode[]): TopologyEdge[] {
  const edges: TopologyEdge[] = [];

  for (const pub of nodes) {
    for (const messageType of pub.publishes) {
      for (const sub of nodes) {
        if (pub.id === sub.id) continue;
        if (sub.subscribes.some((s) => publicationReaches(messageType, s))) {
          edges.push({ source: pub.id, target: sub.id, messageType });
        }
      }
    }
  }

  return edges;
}

/**
 * Order nodes the same way however they were learned: the engine first, then
 * by name. The engine lists primitives in no fixed order, and an unordered
 * list would read as a change every time it was re-read.
 */
export function sortNodes(nodes: readonly TopologyNode[]): TopologyNode[] {
  return [...nodes].sort((a, b) => {
    if (a.id === ENGINE_NODE_ID || b.id === ENGINE_NODE_ID) {
      return a.id === b.id ? 0 : a.id === ENGINE_NODE_ID ? -1 : 1;
    }
    return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
  });
}

/** Map an engine lifecycle state to the status the page draws. */
export function statusFromEngineState(state: string): NodeStatus {
  switch (state) {
    case "running":
    case "external":
      return "running";
    case "failed":
      return "error";
    default:
      return "stopped";
  }
}

/** Build a graph node from a primitive in an engine topology response. */
export function nodeFromPrimitive(prim: EnginePrimitive): TopologyNode {
  return {
    id: prim.name,
    kind: prim.kind as PrimitiveKind,
    status: statusFromEngineState(prim.state),
    publishes: [...prim.publishes],
    subscribes: [...prim.subscribes],
    pid: prim.pid ?? undefined,
    error: prim.error ?? undefined,
  };
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((v) => typeof v === "string");
}

/**
 * Validate the body of an engine topology response.
 *
 * @throws {Error} If the body is not `{ primitives: [...] }` with a name,
 * kind, state, publishes and subscribes on every entry.
 */
export function parseTopologyResponse(body: unknown): EnginePrimitive[] {
  const primitives = (body as { primitives?: unknown } | null)?.primitives;
  if (!Array.isArray(primitives)) {
    throw new Error("topology response has no primitives array");
  }
  return primitives.map((entry: unknown, index) => {
    const prim = entry as Partial<EnginePrimitive> | null;
    if (
      typeof prim?.name !== "string" || typeof prim.kind !== "string" ||
      typeof prim.state !== "string" || !isStringArray(prim.publishes) ||
      !isStringArray(prim.subscribes)
    ) {
      throw new Error(`topology response primitive ${index} is malformed`);
    }
    return prim as EnginePrimitive;
  });
}

/**
 * Decide whether the graph can be trusted, so an empty or stale graph is
 * reported instead of being drawn as if it were the whole topology.
 */
export function assessHealth(
  nodes: readonly TopologyNode[],
  snapshot: SnapshotState,
): TopologyHealth {
  if (snapshot.status === "failed") {
    return {
      status: "degraded",
      detail: `The engine topology could not be read: ${snapshot.reason}. ` +
        "The graph may be missing primitives.",
    };
  }
  if (nodes.some((n) => n.id !== ENGINE_NODE_ID)) return { status: "ok" };
  if (snapshot.status === "pending") {
    return {
      status: "pending",
      detail: "Waiting for the engine to answer the topology request.",
    };
  }
  return {
    status: "empty",
    detail: "The engine reports no primitives besides itself.",
  };
}

/**
 * Manages the topology graph state and SSE broadcasting.
 */
export class TopologyGraph {
  private nodes: Map<string, TopologyNode> = new Map();
  private snapshot: SnapshotState = { status: "pending" };
  private sseClients: Set<ReadableStreamDefaultController<Uint8Array>> =
    new Set();
  private encoder = new TextEncoder();

  /**
   * Add an initial known node (e.g., the engine itself).
   * Used for nodes that don't generate system.started events.
   */
  addInitialNode(node: TopologyNode): void {
    if (!this.nodes.has(node.id)) {
      this.nodes.set(node.id, node);
    }
  }

  /**
   * Handle a system.started event.
   */
  handleStarted(payload: SystemEventPayload): void {
    const node: TopologyNode = {
      id: payload.name,
      kind: payload.kind as PrimitiveKind,
      status: "running",
      publishes: payload.publishes ? [...payload.publishes] : [],
      subscribes: payload.subscribes ? [...payload.subscribes] : [],
      pid: payload.pid,
    };

    const isNew = !this.nodes.has(payload.name);
    this.nodes.set(payload.name, node);

    this.broadcast({
      type: isNew ? "node:added" : "node:updated",
      data: node,
      timestamp: Date.now(),
    });
    this.broadcastDerived();
  }

  /**
   * Handle a system.stopped event.
   */
  handleStopped(payload: SystemEventPayload): void {
    const existing = this.nodes.get(payload.name);
    if (existing) {
      existing.status = "stopped";
      existing.pid = undefined;
      this.broadcast({
        type: "node:updated",
        data: existing,
        timestamp: Date.now(),
      });
      this.broadcastDerived();
    }
  }

  /**
   * Handle a system.error event.
   */
  handleError(payload: SystemEventPayload): void {
    const existing = this.nodes.get(payload.name);
    if (existing) {
      existing.status = "error";
      existing.error = payload.error;
      existing.pid = undefined;
      this.broadcast({
        type: "node:updated",
        data: existing,
        timestamp: Date.now(),
      });
    } else {
      const node: TopologyNode = {
        id: payload.name,
        kind: payload.kind as PrimitiveKind,
        status: "error",
        publishes: payload.publishes ? [...payload.publishes] : [],
        subscribes: payload.subscribes ? [...payload.subscribes] : [],
        pid: payload.pid,
        error: payload.error,
      };
      this.nodes.set(payload.name, node);
      this.broadcast({
        type: "node:added",
        data: node,
        timestamp: Date.now(),
      });
      this.broadcastDerived();
    }
  }

  /**
   * Replace the graph with an authoritative topology response from the
   * engine (`GET /api/topology` or `system.response.topology`). Clients are
   * told only when the state they were last sent has changed, so polling the
   * engine does not redraw an unchanged page.
   */
  handleTopologyRefresh(primitives: readonly EnginePrimitive[]): void {
    const before = JSON.stringify(this.getFullState());

    const engine = this.nodes.get(ENGINE_NODE_ID);
    this.nodes.clear();
    // Keep the seeded engine node if the response does not describe it
    if (engine) this.nodes.set(ENGINE_NODE_ID, engine);
    for (const prim of primitives) {
      this.nodes.set(prim.name, nodeFromPrimitive(prim));
    }
    this.snapshot = { status: "loaded" };

    this.broadcastFullIfChanged(before);
  }

  /**
   * Record that the engine topology could not be read, so the API and the
   * page stop presenting the graph as complete.
   */
  handleTopologyFailure(reason: string): void {
    const before = JSON.stringify(this.getFullState());
    this.snapshot = { status: "failed", reason };
    this.broadcastFullIfChanged(before);
  }

  /**
   * Get the full topology state.
   */
  getFullState(): TopologyState {
    const nodes = sortNodes(Array.from(this.nodes.values()));
    return {
      nodes,
      edges: computeEdges(nodes),
      health: assessHealth(nodes, this.snapshot),
    };
  }

  /**
   * Register an SSE client controller.
   */
  registerSSEClient(
    controller: ReadableStreamDefaultController<Uint8Array>,
  ): void {
    this.sseClients.add(controller);

    // Send full state to new client
    const state = this.getFullState();
    const message = this.formatSSE({
      type: "topology:full",
      data: state,
      timestamp: Date.now(),
    });

    try {
      controller.enqueue(this.encoder.encode(message));
    } catch {
      this.sseClients.delete(controller);
    }
  }

  /**
   * Unregister an SSE client controller.
   */
  unregisterSSEClient(
    controller: ReadableStreamDefaultController<Uint8Array>,
  ): void {
    this.sseClients.delete(controller);
  }

  /**
   * Format a message as SSE data.
   */
  private formatSSE<T>(event: SSEMessage<T>): string {
    return `event: ${event.type}\ndata: ${JSON.stringify(event.data)}\n\n`;
  }

  /**
   * Broadcast an event to all SSE clients.
   */
  broadcast<T>(event: SSEMessage<T>): void {
    const message = this.formatSSE(event);
    const data = this.encoder.encode(message);

    for (const controller of this.sseClients) {
      try {
        controller.enqueue(data);
      } catch {
        this.sseClients.delete(controller);
      }
    }
  }

  /**
   * Broadcast what is derived from the nodes: the edges and the health.
   */
  private broadcastDerived(): void {
    const state = this.getFullState();
    this.broadcast({
      type: "edges:updated",
      data: state.edges,
      timestamp: Date.now(),
    });
    this.broadcast({
      type: "health:updated",
      data: state.health,
      timestamp: Date.now(),
    });
  }

  /**
   * Broadcast the full state if it differs from a serialized earlier state.
   */
  private broadcastFullIfChanged(before: string): void {
    const state = this.getFullState();
    if (JSON.stringify(state) === before) return;
    this.broadcast({
      type: "topology:full",
      data: state,
      timestamp: Date.now(),
    });
  }

  /**
   * Get the number of connected SSE clients.
   */
  get clientCount(): number {
    return this.sseClients.size;
  }

  /**
   * Get the number of nodes.
   */
  get nodeCount(): number {
    return this.nodes.size;
  }
}
