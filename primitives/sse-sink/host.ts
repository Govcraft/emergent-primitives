/**
 * Which `Host` a request may name.
 *
 * Binding to 127.0.0.1 keeps other machines out, and sending no
 * `Access-Control-Allow-Origin` keeps other origins out. Neither stops DNS
 * rebinding: a page on `attacker.example` re-resolves its own name to
 * 127.0.0.1, and from then on the browser treats this server as that page's
 * own origin. The one thing the attacker cannot choose is the name the browser
 * puts in `Host`, which stays `attacker.example`. So a server with no
 * authentication answers only to the names it knows to be its own.
 *
 * Rebinding needs a name the attacker controls, so the rule is about names.
 * An IP literal is never one (a browser sends the address it connected to),
 * and neither is `localhost`. Every other name is refused unless it is the
 * name the server was bound to or one listed with `--allow-host`, which is
 * what a reverse proxy that forwards its public name needs.
 *
 * The decision and the parsing of the list are pure functions. topology-viewer
 * and sse-sink each carry this file and its test, for the reason given in
 * `args.ts`. Keep the two copies identical.
 * @module
 */

/** The flag, repeatable: one host name per use. */
export const ALLOW_HOST_FLAG = "--allow-host";

/** What to do with one request. */
export type HostDecision = "accept" | "refuse";

/** The outcome of parsing: the listed names, or the one thing wrong. */
export type ParsedHosts =
  | { readonly ok: true; readonly hosts: readonly string[] }
  | { readonly ok: false; readonly error: string };

/** A dotted quad, the only IPv4 spelling a browser puts in `Host`. */
function isIpv4(name: string): boolean {
  const parts = name.split(".");
  return parts.length === 4 &&
    parts.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255);
}

/** A bracketed IPv6 address, as `Host` and a URL both write it. */
function isBracketedIpv6(name: string): boolean {
  if (!/^\[[0-9a-f:.]+\]$/.test(name)) return false;
  try {
    return new URL(`http://${name}/`).hostname.length > 0;
  } catch {
    return false;
  }
}

/** Lower case, and without the one trailing dot of a fully qualified name. */
function canonicalName(name: string): string {
  const lower = name.toLowerCase();
  return lower.endsWith(".") ? lower.slice(0, -1) : lower;
}

/**
 * The host of a `Host` value, without its port: `app.example:8080` gives
 * `app.example`, `[::1]:8080` gives `[::1]`. `null` when the value is not one
 * host and an optional numeric port, which covers two `Host` headers joined by
 * a comma.
 */
function hostOf(header: string): string | null {
  const match = /^(\[[^\]]*\]|[^:\s,/]+)(?::\d*)?$/.exec(header);
  return match === null ? null : canonicalName(match[1]);
}

/**
 * Parse every `--allow-host` value: a host name, with no scheme, port or path.
 * It is stored the way a browser sends it, lower case and in its ASCII form.
 * IP addresses never need listing, and there are no patterns: a `*` would be
 * listed and never match.
 */
export function parseAllowedHosts(values: readonly string[]): ParsedHosts {
  const hosts: string[] = [];

  for (const value of values) {
    const host = parseHostName(value);
    if (host === null) {
      return {
        ok: false,
        error: `Invalid ${ALLOW_HOST_FLAG} "${value}": expected a host name ` +
          "with no scheme, port or path, such as app.example",
      };
    }
    if (!hosts.includes(host)) hosts.push(host);
  }

  return { ok: true, hosts };
}

function parseHostName(value: string): string | null {
  if (value.length === 0 || value.startsWith("-")) return null;
  if (/[\s/?#@:*,\\[\]]/.test(value)) return null;
  try {
    const host = canonicalName(new URL(`http://${value}/`).hostname);
    return host.length > 0 ? host : null;
  } catch {
    return null;
  }
}

/**
 * Accept or refuse one request by the host it names.
 *
 * `hostHeader` is the request's `Host` value, `null` when it has none. `bound`
 * is the address or name the server listens on, as given to `--host`.
 * `allowed` is the parsed `--allow-host` list.
 *
 * The bound address only adds something when it is a name: every IP literal is
 * accepted whatever the server is bound to, since a request can reach a
 * loopback server under another address through a port forward or a container,
 * and no address can be rebound.
 */
export function decideHost(
  hostHeader: string | null,
  bound: string,
  allowed: readonly string[],
): HostDecision {
  if (hostHeader === null) return "refuse";
  const host = hostOf(hostHeader);
  if (host === null) return "refuse";

  const known = isIpv4(host) || isBracketedIpv6(host) ||
    host === "localhost" || host === canonicalName(bound) ||
    allowed.includes(host);
  return known ? "accept" : "refuse";
}

/**
 * Every host one request names. Over HTTP/1.1 that is the `Host` header, and
 * the request line too when it carries a whole URL. Over HTTP/2 there is no
 * `Host` header and the URL holds the authority. All of them must be accepted.
 */
export function namedHosts(
  hostHeader: string | null,
  requestUrl: string,
): readonly string[] {
  const fromUrl = new URL(requestUrl).host;
  return hostHeader === null || hostHeader === fromUrl
    ? [fromUrl]
    : [hostHeader, fromUrl];
}

/** Decide for a whole request: refused if any host it names is refused. */
export function decideRequestHost(
  hostHeader: string | null,
  requestUrl: string,
  bound: string,
  allowed: readonly string[],
): HostDecision {
  return namedHosts(hostHeader, requestUrl)
      .every((host) => decideHost(host, bound, allowed) === "accept")
    ? "accept"
    : "refuse";
}

/** The reply to a refused request: 421, and what the operator can do. */
export function misdirected(): Response {
  return new Response(
    "Misdirected Request: this server does not answer to the host name in " +
      `the request. If the name is its own, list it with ${ALLOW_HOST_FLAG}.\n`,
    {
      status: 421,
      headers: { "Content-Type": "text/plain; charset=utf-8" },
    },
  );
}

/** The startup log line saying which hosts are answered. */
export function describeAllowedHosts(
  bound: string,
  allowed: readonly string[],
): string {
  const boundName = canonicalName(bound);
  const names = [
    "localhost",
    ...(decideHost(boundName, "", []) === "accept" ? [] : [boundName]),
    ...allowed,
  ];
  return `any IP address, ${[...new Set(names)].join(", ")}`;
}
