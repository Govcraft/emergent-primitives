import { assertEquals } from "@std/assert";
import {
  allowOrigin,
  corsHeaders,
  describeAllowedOrigins,
  parseAllowedOrigins,
} from "./cors.ts";
import type { AllowedOrigins } from "./cors.ts";

const NONE: AllowedOrigins = { kind: "none" };
const ANY: AllowedOrigins = { kind: "any" };
const LIST: AllowedOrigins = {
  kind: "list",
  origins: ["https://app.example", "http://localhost:3000"],
};

Deno.test("allowOrigin table", () => {
  const cases: [string | null, AllowedOrigins, string | null][] = [
    // Nothing configured: no header, whoever asks.
    [null, NONE, null],
    ["https://app.example", NONE, null],
    ["https://evil.example", NONE, null],
    // `*` on request: every origin, and a request with none.
    [null, ANY, "*"],
    ["https://evil.example", ANY, "*"],
    // A list: the asking origin is echoed when it is listed.
    ["https://app.example", LIST, "https://app.example"],
    ["http://localhost:3000", LIST, "http://localhost:3000"],
    [null, LIST, null],
    ["https://evil.example", LIST, null],
    // An origin is scheme, host and port: each one counts.
    ["http://app.example", LIST, null],
    ["https://app.example:8443", LIST, null],
    ["http://localhost", LIST, null],
    ["http://localhost:3001", LIST, null],
    ["https://sub.app.example", LIST, null],
    ["https://app.example.evil.example", LIST, null],
    // A sandboxed frame or a file sends the word null, which is never listed.
    ["null", LIST, null],
    ["null", NONE, null],
    // Compared as sent: a browser sends lower case and no trailing slash.
    ["https://app.example/", LIST, null],
    ["HTTPS://APP.EXAMPLE", LIST, null],
    ["", LIST, null],
  ];

  for (const [origin, allowed, expected] of cases) {
    assertEquals(
      allowOrigin(origin, allowed),
      expected,
      `${origin} against ${allowed.kind}`,
    );
  }
});

Deno.test("corsHeaders table", () => {
  const cases: [string | null, AllowedOrigins, Record<string, string>][] = [
    [null, NONE, {}],
    ["https://evil.example", NONE, {}],
    ["https://evil.example", ANY, { "access-control-allow-origin": "*" }],
    [null, ANY, { "access-control-allow-origin": "*" }],
    [
      "https://app.example",
      LIST,
      {
        "vary": "Origin",
        "access-control-allow-origin": "https://app.example",
      },
    ],
    // Refused, and a cache must still not hand this reply to a listed origin.
    ["https://evil.example", LIST, { "vary": "Origin" }],
    [null, LIST, { "vary": "Origin" }],
  ];

  for (const [origin, allowed, expected] of cases) {
    assertEquals(
      corsHeaders(origin, allowed),
      expected,
      `${origin} against ${allowed.kind}`,
    );
  }
});

Deno.test("parseAllowedOrigins accepts table", () => {
  const cases: [string[], AllowedOrigins][] = [
    [[], NONE],
    [["*"], ANY],
    [["https://app.example"], {
      kind: "list",
      origins: ["https://app.example"],
    }],
    [["https://app.example", "http://localhost:3000"], LIST],
    // `*` wins wherever it stands.
    [["https://app.example", "*"], ANY],
    [["*", "https://app.example"], ANY],
    // Stored in the spelling a browser sends, so that it can match.
    [["https://app.example/"], {
      kind: "list",
      origins: ["https://app.example"],
    }],
    [["HTTPS://App.Example"], {
      kind: "list",
      origins: ["https://app.example"],
    }],
    [["https://app.example:443", "http://localhost:80"], {
      kind: "list",
      origins: ["https://app.example", "http://localhost"],
    }],
    [["http://[::1]:3000", "http://127.0.0.1:3000"], {
      kind: "list",
      origins: ["http://[::1]:3000", "http://127.0.0.1:3000"],
    }],
    // A browser sends an international name in its ASCII form.
    [["https://münchen.example"], {
      kind: "list",
      origins: ["https://xn--mnchen-3ya.example"],
    }],
    // Listed twice, kept once.
    [["https://app.example", "https://app.example/"], {
      kind: "list",
      origins: ["https://app.example"],
    }],
  ];

  for (const [values, allowed] of cases) {
    assertEquals(
      parseAllowedOrigins(values),
      { ok: true, allowed },
      values.join(" "),
    );
  }
});

Deno.test("parseAllowedOrigins rejects table", () => {
  const cases = [
    "",
    "app.example",
    "localhost:3000",
    "//app.example",
    "https://",
    "https://app.example/app",
    "https://app.example/?x=1",
    "https://app.example/#top",
    "https://user@app.example",
    "https://*.app.example/path",
    // There are no patterns: a wildcard host would be listed and never match.
    "https://*.app.example",
    "https://*",
    "http://localhost:*",
    "https://app.example:99999",
    "https://app.example https://b.example",
    "https://app.example,https://b.example",
    "null",
    "file:///home/me/page.html",
    "ftp://app.example",
    "chrome-extension://abcdefgh",
    // What `--allow-origin --port 80` hands over.
    "--port",
    "**",
  ];

  for (const value of cases) {
    const error = `Invalid --allow-origin "${value}": expected * or an http ` +
      "or https origin with no path, such as https://app.example or " +
      "http://localhost:3000";
    assertEquals(parseAllowedOrigins([value]), { ok: false, error }, value);
    // One bad value refuses the whole list, wherever it stands.
    assertEquals(
      parseAllowedOrigins(["https://ok.example", value, "*"]),
      { ok: false, error },
      value,
    );
  }
});

Deno.test("every origin that parses is one allowOrigin can match", () => {
  const written = [
    "https://app.example/",
    "HTTPS://App.Example:443",
    "http://LOCALHOST:3000",
    "http://[::1]:8080/",
  ];
  const sent = [
    "https://app.example",
    "https://app.example",
    "http://localhost:3000",
    "http://[::1]:8080",
  ];

  written.forEach((value, index) => {
    const parsed = parseAllowedOrigins([value]);
    assertEquals(parsed.ok, true, value);
    if (parsed.ok) {
      assertEquals(
        allowOrigin(sent[index], parsed.allowed),
        sent[index],
        value,
      );
    }
  });
});

Deno.test("describeAllowedOrigins says who may read the stream", () => {
  assertEquals(
    describeAllowedOrigins(NONE),
    "no page on another origin (name one with --allow-origin)",
  );
  assertEquals(describeAllowedOrigins(ANY), "every origin (*)");
  assertEquals(
    describeAllowedOrigins(LIST),
    "https://app.example, http://localhost:3000",
  );
});
