/**
 * Message type resolution for websocket-handler, as pure functions.
 *
 * Under the engine the handler is told its types through EMERGENT_SUBSCRIBES
 * and EMERGENT_PUBLISHES (comma separated). Each role is matched by suffix, so
 * a topology can use any prefix. A role the list does not name falls back to
 * `{prefix}.{role}`.
 *
 * @module
 */

/** Message types the handler subscribes to, by role. */
export interface SubscribeTypes {
  connect: string;
  send: string;
  disconnect: string;
}

/** Message types the handler publishes, by role. */
export interface PublishTypes {
  connected: string;
  frame: string;
  closed: string;
  disconnected: string;
  error: string;
}

/** Split a comma separated type list, dropping blanks. */
export function parseTypeList(list: string | undefined): string[] {
  return (list ?? "").split(",").map((t) => t.trim()).filter((t) =>
    t.length > 0
  );
}

/**
 * The declared type for `role`, or `{prefix}.{role}` when none is declared.
 *
 * The match is on the whole last segment, so `ws.disconnected` is never taken
 * for the `connected` role.
 */
export function resolveType(
  declared: string[],
  prefix: string,
  role: string,
): string {
  return declared.find((t) => t.endsWith(`.${role}`)) ?? `${prefix}.${role}`;
}

/**
 * `type` with its last segment replaced by `role`: the sibling of `slack.closed`
 * for role `disconnected` is `slack.disconnected`.
 */
export function siblingType(type: string, role: string): string {
  const dot = type.lastIndexOf(".");
  return dot < 0 ? role : `${type.slice(0, dot)}.${role}`;
}

/** Resolve subscribe types from the EMERGENT_SUBSCRIBES value, if any. */
export function resolveSubscribeTypes(
  prefix: string,
  envSubscribes?: string,
): SubscribeTypes {
  const declared = parseTypeList(envSubscribes);
  return {
    connect: resolveType(declared, prefix, "connect"),
    send: resolveType(declared, prefix, "send"),
    disconnect: resolveType(declared, prefix, "disconnect"),
  };
}

/**
 * Resolve publish types from the EMERGENT_PUBLISHES value, if any.
 *
 * `disconnected` is newer than the other roles, so a topology written before it
 * existed does not declare it. It then becomes the sibling of the `closed` type
 * rather than `{prefix}.disconnected`, which keeps both terminal events under
 * the prefix the topology already routes on.
 */
export function resolvePublishTypes(
  prefix: string,
  envPublishes?: string,
): PublishTypes {
  const declared = parseTypeList(envPublishes);
  const closed = resolveType(declared, prefix, "closed");
  return {
    connected: resolveType(declared, prefix, "connected"),
    frame: resolveType(declared, prefix, "frame"),
    closed,
    disconnected: declared.find((t) => t.endsWith(".disconnected")) ??
      siblingType(closed, "disconnected"),
    error: resolveType(declared, prefix, "error"),
  };
}
