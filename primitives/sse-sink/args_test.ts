import { assertEquals } from "jsr:@std/assert@1";
import {
  bindFailure,
  DEFAULT_HOST,
  DEFAULT_PORT,
  listenUrl,
  parseListenArgs,
} from "./args.ts";

Deno.test("the defaults are loopback and 8080", () => {
  assertEquals(DEFAULT_HOST, "127.0.0.1");
  assertEquals(DEFAULT_PORT, 8080);
  assertEquals(parseListenArgs([]), {
    ok: true,
    options: { host: "127.0.0.1", port: 8080 },
    repeated: {},
  });
});

Deno.test("parseListenArgs accepts table", () => {
  const cases: [string[], string, number][] = [
    [["--port", "9000"], "127.0.0.1", 9000],
    [["-p", "9000"], "127.0.0.1", 9000],
    [["--port=9000"], "127.0.0.1", 9000],
    [["--port", "1"], "127.0.0.1", 1],
    [["--port", "65535"], "127.0.0.1", 65535],
    [["--host", "0.0.0.0"], "0.0.0.0", 8080],
    [["--host=0.0.0.0"], "0.0.0.0", 8080],
    [["--host", "::"], "::", 8080],
    [["--host", "::1"], "::1", 8080],
    [["--host", "[::1]"], "::1", 8080],
    [["--host", "localhost"], "localhost", 8080],
    [["--host", "192.168.1.20", "--port", "8081"], "192.168.1.20", 8081],
    [["--port", "8081", "--host", "192.168.1.20"], "192.168.1.20", 8081],
    // A repeated flag takes its last value.
    [["--port", "1", "--port", "2"], "127.0.0.1", 2],
    [["--host", "0.0.0.0", "--host=127.0.0.1"], "127.0.0.1", 8080],
  ];

  for (const [args, host, port] of cases) {
    assertEquals(
      parseListenArgs(args),
      { ok: true, options: { host, port }, repeated: {} },
      args.join(" "),
    );
  }
});

Deno.test("parseListenArgs rejects table", () => {
  const cases: [string[], string][] = [
    [["--port"], "--port needs a value"],
    [["-p"], "-p needs a value"],
    [["--host"], "--host needs a value"],
    [
      ["--port", "0"],
      'Invalid --port "0": expected a whole number from 1 to 65535',
    ],
    [
      ["--port", "65536"],
      'Invalid --port "65536": expected a whole number from 1 to 65535',
    ],
    [
      ["--port", "80abc"],
      'Invalid --port "80abc": expected a whole number from 1 to 65535',
    ],
    [
      ["--port", "-1"],
      'Invalid --port "-1": expected a whole number from 1 to 65535',
    ],
    [
      ["--port", "8080.5"],
      'Invalid --port "8080.5": expected a whole number from 1 to 65535',
    ],
    [
      ["--port="],
      'Invalid --port "": expected a whole number from 1 to 65535',
    ],
    // `--host --port 80` must not bind a host named "--port".
    [
      ["--host", "--port", "80"],
      'Invalid --host "--port": expected an IP address or a host name, ' +
      "such as 127.0.0.1, 0.0.0.0 or ::1",
    ],
    [
      ["--host="],
      'Invalid --host "": expected an IP address or a host name, ' +
      "such as 127.0.0.1, 0.0.0.0 or ::1",
    ],
    [
      ["--host", "my host"],
      'Invalid --host "my host": expected an IP address or a host name, ' +
      "such as 127.0.0.1, 0.0.0.0 or ::1",
    ],
    [
      ["--host", "[]"],
      'Invalid --host "[]": expected an IP address or a host name, ' +
      "such as 127.0.0.1, 0.0.0.0 or ::1",
    ],
    // A misspelled flag is an error, never a silent default.
    [["--hots", "0.0.0.0"], 'Unknown argument "--hots"'],
    [["--hots=0.0.0.0"], 'Unknown argument "--hots=0.0.0.0"'],
    [["--port", "80", "extra"], 'Unknown argument "extra"'],
    [["-p=80"], 'Unknown argument "-p=80"'],
    [["--help"], 'Unknown argument "--help"'],
  ];

  for (const [args, error] of cases) {
    assertEquals(parseListenArgs(args), { ok: false, error }, args.join(" "));
  }
});

Deno.test("a repeatable flag collects every value in order", () => {
  const cases: [string[], string[]][] = [
    [[], []],
    [["--allow-origin", "https://a.example"], ["https://a.example"]],
    [["--allow-origin=https://a.example"], ["https://a.example"]],
    [
      [
        "--allow-origin",
        "https://b.example",
        "--allow-origin=https://a.example",
      ],
      ["https://b.example", "https://a.example"],
    ],
    [["--port", "9000", "--allow-origin", "*", "--host", "::1"], ["*"]],
    // The value is collected unexamined: what it means is the caller's rule.
    [["--allow-origin", "--port"], ["--port"]],
    [["--allow-origin="], [""]],
  ];

  for (const [args, values] of cases) {
    const parsed = parseListenArgs(args, ["--allow-origin"]);
    assertEquals(
      parsed.ok && parsed.repeated,
      { "--allow-origin": values },
      args.join(" "),
    );
  }
});

Deno.test("a repeatable flag does not disturb the listen options", () => {
  assertEquals(
    parseListenArgs(
      ["--host", "0.0.0.0", "--allow-origin", "*", "-p", "9000"],
      ["--allow-origin"],
    ),
    {
      ok: true,
      options: { host: "0.0.0.0", port: 9000 },
      repeated: { "--allow-origin": ["*"] },
    },
  );
});

Deno.test("a repeatable flag exists only for the caller that names it", () => {
  assertEquals(parseListenArgs(["--allow-origin", "*"]), {
    ok: false,
    error: 'Unknown argument "--allow-origin"',
  });
  assertEquals(parseListenArgs(["--allow-origin", "*"], ["--other"]), {
    ok: false,
    error: 'Unknown argument "--allow-origin"',
  });
  assertEquals(parseListenArgs(["--allow-origin"], ["--allow-origin"]), {
    ok: false,
    error: "--allow-origin needs a value",
  });
});

Deno.test("listenUrl prints the bound address, bracketing IPv6", () => {
  const cases: [string, number, string | undefined, string][] = [
    ["127.0.0.1", 8080, undefined, "http://127.0.0.1:8080/"],
    ["0.0.0.0", 9000, undefined, "http://0.0.0.0:9000/"],
    ["::1", 8080, undefined, "http://[::1]:8080/"],
    ["::", 8080, "/events", "http://[::]:8080/events"],
    ["localhost", 8080, "/events", "http://localhost:8080/events"],
  ];

  for (const [host, port, path, expected] of cases) {
    assertEquals(listenUrl(host, port, path), expected);
  }
});

Deno.test("bindFailure names the address and the reason on one line", () => {
  assertEquals(
    bindFailure("203.0.113.1", 8080, "Cannot assign requested address"),
    "Cannot listen on http://203.0.113.1:8080/: Cannot assign requested address",
  );
  assertEquals(
    bindFailure("::1", 8080, "Address already in use"),
    "Cannot listen on http://[::1]:8080/: Address already in use",
  );
});
