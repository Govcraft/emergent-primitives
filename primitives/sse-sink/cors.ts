/**
 * Which browser origins may read the stream.
 *
 * A browser lets a page on another origin read a response only when the
 * response names that origin in `Access-Control-Allow-Origin`. The stream has
 * no authentication, so that header is the one thing between a web page open
 * on the same machine and every event in the pipeline. It is sent only for
 * the origins the operator listed with `--allow-origin`.
 *
 * Both the parsing of the list and the decision for one request are pure
 * functions: no server is needed to test them.
 * @module
 */

/** The flag, repeatable: one origin per use, or `*`. */
export const ALLOW_ORIGIN_FLAG = "--allow-origin";

/** The origins allowed to read the stream from a browser. */
export type AllowedOrigins =
  /** No `--allow-origin`: no page on another origin. */
  | { readonly kind: "none" }
  /** `--allow-origin '*'`: every page, the behavior before the flag existed. */
  | { readonly kind: "any" }
  /** The origins listed, each in the spelling a browser sends. */
  | { readonly kind: "list"; readonly origins: readonly string[] };

/** The outcome of parsing: the allowed origins, or the one thing wrong. */
export type ParsedOrigins =
  | { readonly ok: true; readonly allowed: AllowedOrigins }
  | { readonly ok: false; readonly error: string };

/**
 * An origin in the one spelling a browser sends in `Origin`: scheme, host and
 * port, lower case, the scheme's default port left out. A value with anything
 * more (a path, a query, credentials) is refused rather than trimmed, because
 * it suggests the operator expects the rest to count, and it never does. So is
 * a `*` inside a host: origins are compared whole, there are no patterns, and
 * `https://*.app.example` would be listed and never match.
 */
function parseOrigin(value: string): string | null {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  if (url.hostname.includes("*")) return null;
  return url.href === `${url.origin}/` ? url.origin : null;
}

/**
 * Parse every `--allow-origin` value. `*` anywhere in the list allows every
 * origin. A listed origin is kept once, in the order first written.
 */
export function parseAllowedOrigins(values: readonly string[]): ParsedOrigins {
  const origins: string[] = [];
  let any = false;

  for (const value of values) {
    if (value === "*") {
      any = true;
      continue;
    }
    const origin = parseOrigin(value);
    if (origin === null) {
      return {
        ok: false,
        error: `Invalid ${ALLOW_ORIGIN_FLAG} "${value}": expected * or an ` +
          "http or https origin with no path, such as https://app.example " +
          "or http://localhost:3000",
      };
    }
    if (!origins.includes(origin)) origins.push(origin);
  }

  if (any) return { ok: true, allowed: { kind: "any" } };
  if (origins.length === 0) return { ok: true, allowed: { kind: "none" } };
  return { ok: true, allowed: { kind: "list", origins } };
}

/**
 * The value of `Access-Control-Allow-Origin` for one request, or `null` for no
 * header. `requestOrigin` is the request's `Origin` header, `null` when it has
 * none. A listed origin is echoed back, never the whole list: the header holds
 * one origin or `*`.
 */
export function allowOrigin(
  requestOrigin: string | null,
  allowed: AllowedOrigins,
): string | null {
  switch (allowed.kind) {
    case "none":
      return null;
    case "any":
      return "*";
    case "list":
      return requestOrigin !== null && allowed.origins.includes(requestOrigin)
        ? requestOrigin
        : null;
  }
}

/**
 * The response headers that carry the decision. With a list the reply differs
 * by `Origin`, allowed or not, so a cache must be told with `Vary`.
 */
export function corsHeaders(
  requestOrigin: string | null,
  allowed: AllowedOrigins,
): Record<string, string> {
  const headers: Record<string, string> = {};
  if (allowed.kind === "list") headers["vary"] = "Origin";
  const value = allowOrigin(requestOrigin, allowed);
  if (value !== null) headers["access-control-allow-origin"] = value;
  return headers;
}

/** The startup log line saying who may read the stream from a browser. */
export function describeAllowedOrigins(allowed: AllowedOrigins): string {
  switch (allowed.kind) {
    case "none":
      return `no page on another origin (name one with ${ALLOW_ORIGIN_FLAG})`;
    case "any":
      return "every origin (*)";
    case "list":
      return allowed.origins.join(", ");
  }
}
