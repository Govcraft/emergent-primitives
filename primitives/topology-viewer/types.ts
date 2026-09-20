/**
 * TypeScript interfaces for the Topology Viewer.
 * @module
 */

/** Status of a primitive node. */
export type NodeStatus = "running" | "stopped" | "error";

/** Kind of primitive in the Emergent system. */
export type PrimitiveKind = "source" | "handler" | "sink";

/** A node in the topology graph representing a primitive. */
export interface TopologyNode {
  /** Unique identifier (primitive name). */
  id: string;
  /** Type of primitive. */
  kind: PrimitiveKind;
  /** Current status. */
  status: NodeStatus;
  /** Message types this primitive publishes. */
  publishes: string[];
  /** Message types this primitive subscribes to. */
  subscribes: string[];
  /** Process ID if running. */
  pid?: number;
  /** Error message if in error state. */
  error?: string;
}

/** An edge in the topology graph representing a message flow. */
export interface TopologyEdge {
  /** Source node ID (publisher). */
  source: string;
  /** Target node ID (subscriber). */
  target: string;
  /** Message type flowing through this edge. */
  messageType: string;
}

/**
 * A primitive as the engine reports it, in `GET /api/topology` bodies and in
 * `system.response.topology` payloads.
 */
export interface EnginePrimitive {
  readonly name: string;
  readonly kind: string;
  readonly state: string;
  readonly publishes: readonly string[];
  readonly subscribes: readonly string[];
  readonly pid?: number | null;
  readonly error?: string | null;
}

/** Outcome of the most recent topology request to the engine. */
export type SnapshotState =
  | { readonly status: "pending" }
  | { readonly status: "loaded" }
  | { readonly status: "failed"; readonly reason: string };

/**
 * Whether the graph can be trusted.
 *   - "ok": the engine answered and at least one primitive is known
 *   - "pending": the engine has not answered yet
 *   - "empty": the engine answered and reports no primitive but itself
 *   - "degraded": the topology request failed or timed out
 */
export type HealthStatus = "ok" | "pending" | "empty" | "degraded";

/** Health of the graph, reported by the API and shown on the page. */
export interface TopologyHealth {
  status: HealthStatus;
  /** What is wrong, in words a person can act on. Absent when "ok". */
  detail?: string;
}

/** Complete topology state. */
export interface TopologyState {
  /** All nodes in the topology. */
  nodes: TopologyNode[];
  /** All edges in the topology. */
  edges: TopologyEdge[];
  /** Whether the nodes and edges above can be trusted. */
  health: TopologyHealth;
}

/** Server-sent event message wrapper. */
export interface SSEMessage<T = unknown> {
  /** Event type identifier. */
  type: string;
  /** Event payload. */
  data: T;
  /** Unix timestamp in milliseconds. */
  timestamp: number;
}
