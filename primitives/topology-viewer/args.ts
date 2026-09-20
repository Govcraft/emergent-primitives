/**
 * Command line arguments: where to listen.
 *
 * Parsing is a pure function of the argument list. It reports a problem as a
 * value, and the caller decides to print it and exit, so every branch can be
 * tested without a process to kill.
 *
 * topology-viewer and sse-sink each carry this file and its test. Every Deno
 * primitive is checked and compiled from its own directory, so the file is
 * copied rather than shared. Keep the two copies identical.
 * @module
 */

/** The default bind address: loopback, so exposing the server is a decision. */
export const DEFAULT_HOST = "127.0.0.1";

/** The default port. */
export const DEFAULT_PORT = 8080;

/** Where the HTTP server listens. */
export interface ListenOptions {
  /** Address to bind, without brackets even for IPv6. */
  readonly host: string;
  /** Port to bind. */
  readonly port: number;
}

/** Every value given for each repeatable flag, in the order written. */
export type RepeatedValues = Readonly<Record<string, readonly string[]>>;

/** The outcome of parsing: the options, or the one thing that is wrong. */
export type ParsedArgs =
  | {
    readonly ok: true;
    readonly options: ListenOptions;
    readonly repeated: RepeatedValues;
  }
  | { readonly ok: false; readonly error: string };

/** One flag and its value, from `--flag value`, `--flag=value` or `-f value`. */
function splitFlag(
  args: readonly string[],
  index: number,
): { flag: string; value: string | undefined; consumed: number } {
  const arg = args[index];
  const equals = arg.startsWith("--") ? arg.indexOf("=") : -1;
  if (equals >= 0) {
    return {
      flag: arg.slice(0, equals),
      value: arg.slice(equals + 1),
      consumed: 1,
    };
  }
  return { flag: arg, value: args[index + 1], consumed: 2 };
}

function parsePort(value: string): number | null {
  if (!/^\d+$/.test(value)) return null;
  const port = Number(value);
  return port >= 1 && port <= 65535 ? port : null;
}

/**
 * A host is taken as given, minus the brackets of `[::1]`. Whether it names an
 * address this machine has is for the bind to say. What is refused here is a
 * value that is plainly not a host: nothing, whitespace, or the next flag,
 * which is what `--host --port 80` hands over.
 */
function parseHost(value: string): string | null {
  const host = value.startsWith("[") && value.endsWith("]")
    ? value.slice(1, -1)
    : value;
  if (host.length === 0 || host.startsWith("-") || /\s/.test(host)) return null;
  return host;
}

/**
 * Parse `--host` and `--port` (or `-p`), plus the flags named in `repeatable`.
 *
 * A repeated `--host` or `--port` takes its last value. A flag named in
 * `repeatable` is one only some primitives have (sse-sink passes
 * `--allow-origin`, topology-viewer passes none): every value it is given is
 * collected under its name, in order and unexamined, for the caller to parse.
 * Anything else is an error rather than ignored: a misspelled `--host` must not
 * quietly leave the server on a different address than the operator asked for.
 */
export function parseListenArgs(
  args: readonly string[],
  repeatable: readonly string[] = [],
): ParsedArgs {
  let host = DEFAULT_HOST;
  let port = DEFAULT_PORT;
  const repeated: Record<string, string[]> = Object.fromEntries(
    repeatable.map((name) => [name, []]),
  );

  for (let index = 0; index < args.length;) {
    const { flag, value, consumed } = splitFlag(args, index);
    const collects = repeatable.includes(flag);

    if (!collects && flag !== "--host" && flag !== "--port" && flag !== "-p") {
      return { ok: false, error: `Unknown argument "${args[index]}"` };
    }
    if (value === undefined) {
      return { ok: false, error: `${flag} needs a value` };
    }

    if (collects) {
      repeated[flag].push(value);
    } else if (flag === "--host") {
      const parsed = parseHost(value);
      if (parsed === null) {
        return {
          ok: false,
          error: `Invalid --host "${value}": expected an IP address or a ` +
            "host name, such as 127.0.0.1, 0.0.0.0 or ::1",
        };
      }
      host = parsed;
    } else {
      const parsed = parsePort(value);
      if (parsed === null) {
        return {
          ok: false,
          error:
            `Invalid --port "${value}": expected a whole number from 1 to 65535`,
        };
      }
      port = parsed;
    }

    index += consumed;
  }

  return { ok: true, options: { host, port }, repeated };
}

/**
 * The one line printed when the address cannot be bound: the address is in use,
 * or `--host` names an address this machine does not have.
 */
export function bindFailure(
  host: string,
  port: number,
  reason: string,
): string {
  return `Cannot listen on ${listenUrl(host, port)}: ${reason}`;
}

/**
 * The URL a bound address is reached at, for the startup log line. An IPv6
 * address needs brackets in a URL; `path` starts with `/`.
 */
export function listenUrl(host: string, port: number, path = "/"): string {
  const authority = host.includes(":") ? `[${host}]` : host;
  return `http://${authority}:${port}${path}`;
}
