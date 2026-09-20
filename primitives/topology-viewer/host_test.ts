import { assertEquals } from "@std/assert";
import {
  decideHost,
  decideRequestHost,
  describeAllowedHosts,
  type HostDecision,
  misdirected,
  namedHosts,
  parseAllowedHosts,
} from "./host.ts";

const LISTED = ["app.example", "viewer.internal"];

Deno.test("decideHost table", () => {
  // (Host header, bound address, --allow-host list, decision)
  const cases: [string | null, string, readonly string[], HostDecision][] = [
    // Bound to loopback, nothing listed: the names that are always its own.
    ["localhost", "127.0.0.1", [], "accept"],
    ["localhost:8080", "127.0.0.1", [], "accept"],
    ["127.0.0.1:8080", "127.0.0.1", [], "accept"],
    ["127.0.0.1", "127.0.0.1", [], "accept"],
    ["[::1]:8080", "127.0.0.1", [], "accept"],
    ["[::1]", "::1", [], "accept"],
    // The rebinding request: the attacker's name, whatever port.
    ["attacker.example", "127.0.0.1", [], "refuse"],
    ["attacker.example:8080", "127.0.0.1", [], "refuse"],
    ["attacker.example:8080", "::1", [], "refuse"],
    ["attacker.example:8080", "192.168.1.20", [], "refuse"],
    ["attacker.example:8080", "0.0.0.0", [], "refuse"],
    ["attacker.example:8080", "::", [], "refuse"],
    ["attacker.example:8080", "0.0.0.0", LISTED, "refuse"],
    // Names dressed up as the accepted ones.
    ["localhost.attacker.example", "127.0.0.1", [], "refuse"],
    ["attacker.localhost", "127.0.0.1", [], "refuse"],
    ["127.0.0.1.attacker.example", "127.0.0.1", [], "refuse"],
    ["app.example.attacker.example", "127.0.0.1", LISTED, "refuse"],
    ["xapp.example", "127.0.0.1", LISTED, "refuse"],
    ["127.0.0.1.", "127.0.0.1", [], "accept"],
    ["127.1", "127.0.0.1", [], "refuse"],
    ["256.0.0.1", "127.0.0.1", [], "refuse"],
    ["1.2.3.4.5", "127.0.0.1", [], "refuse"],
    ["0x7f.0.0.1", "127.0.0.1", [], "refuse"],
    ["[::1", "127.0.0.1", [], "refuse"],
    ["[attacker.example]", "127.0.0.1", [], "refuse"],
    ["[zz::1]", "127.0.0.1", [], "refuse"],
    // An address cannot be rebound, so any IP literal is accepted, bound to
    // it or not: a port forward or a container publishes a loopback server
    // under the machine's address.
    ["192.168.1.20:8080", "192.168.1.20", [], "accept"],
    ["192.168.1.20:9000", "127.0.0.1", [], "accept"],
    ["192.168.1.20:8080", "0.0.0.0", [], "accept"],
    ["10.0.0.5", "0.0.0.0", [], "accept"],
    ["[fe80::1]:8080", "::", [], "accept"],
    ["[2001:db8::1]:8080", "0.0.0.0", [], "accept"],
    ["[::ffff:192.168.1.20]:8080", "::", [], "accept"],
    // `localhost` is accepted whatever the bind: a container bound to 0.0.0.0
    // with a published port is opened as http://localhost:8080.
    ["localhost:8080", "0.0.0.0", [], "accept"],
    ["localhost:8080", "::", [], "accept"],
    ["localhost:8080", "192.168.1.20", [], "accept"],
    // A machine name is a name like any other, exposed or not.
    ["myhost.local:8080", "0.0.0.0", [], "refuse"],
    ["myhost:8080", "0.0.0.0", [], "refuse"],
    ["myhost.local:8080", "0.0.0.0", ["myhost.local"], "accept"],
    // Listed names: what a reverse proxy that forwards its own name sends.
    ["app.example", "127.0.0.1", LISTED, "accept"],
    ["app.example:443", "127.0.0.1", LISTED, "accept"],
    ["viewer.internal:8080", "0.0.0.0", LISTED, "accept"],
    ["APP.Example", "127.0.0.1", LISTED, "accept"],
    ["app.example.", "127.0.0.1", LISTED, "accept"],
    ["app.example.:8080", "127.0.0.1", LISTED, "accept"],
    ["LOCALHOST:8080", "127.0.0.1", [], "accept"],
    ["localhost.", "127.0.0.1", [], "accept"],
    // Bound to a name: that name is its own.
    ["sink.internal:8080", "sink.internal", [], "accept"],
    ["sink.internal:8080", "Sink.Internal", [], "accept"],
    ["other.internal:8080", "sink.internal", [], "refuse"],
    // Not one host and an optional port.
    [null, "127.0.0.1", [], "refuse"],
    ["", "127.0.0.1", [], "refuse"],
    [":8080", "127.0.0.1", [], "refuse"],
    ["localhost:80abc", "127.0.0.1", [], "refuse"],
    ["localhost:8080:1", "127.0.0.1", [], "refuse"],
    ["localhost, attacker.example", "127.0.0.1", [], "refuse"],
    ["attacker.example, localhost", "127.0.0.1", [], "refuse"],
    ["localhost attacker.example", "127.0.0.1", [], "refuse"],
    ["localhost/attacker.example", "127.0.0.1", [], "refuse"],
    ["attacker.example@localhost", "127.0.0.1", [], "refuse"],
    ["[::1]x", "127.0.0.1", [], "refuse"],
    // An empty port is allowed by the grammar.
    ["localhost:", "127.0.0.1", [], "accept"],
  ];

  for (const [header, bound, allowed, expected] of cases) {
    assertEquals(
      decideHost(header, bound, allowed),
      expected,
      `Host ${header}, bound ${bound}, listed ${allowed.join(" ")}`,
    );
  }
});

Deno.test("namedHosts finds every host a request names", () => {
  const cases: [string | null, string, string[]][] = [
    // HTTP/1.1: the URL is built from the header.
    ["localhost:8080", "http://localhost:8080/events", ["localhost:8080"]],
    // HTTP/2 has no Host header, the authority is in the URL.
    [null, "http://attacker.example/events", ["attacker.example"]],
    // A whole URL on the request line, and a Host that says otherwise.
    [
      "localhost:8080",
      "http://attacker.example/events",
      ["localhost:8080", "attacker.example"],
    ],
  ];

  for (const [header, url, expected] of cases) {
    assertEquals(namedHosts(header, url), expected, `${header} ${url}`);
  }
});

Deno.test("decideRequestHost refuses when any named host is refused", () => {
  const cases: [string | null, string, HostDecision][] = [
    ["localhost:8080", "http://localhost:8080/events", "accept"],
    ["attacker.example", "http://attacker.example/events", "refuse"],
    [null, "http://127.0.0.1:8080/events", "accept"],
    [null, "http://attacker.example/events", "refuse"],
    ["localhost:8080", "http://attacker.example/events", "refuse"],
    ["attacker.example", "http://localhost:8080/events", "refuse"],
    ["app.example", "http://localhost:8080/events", "accept"],
  ];

  for (const [header, url, expected] of cases) {
    assertEquals(
      decideRequestHost(header, url, "127.0.0.1", LISTED),
      expected,
      `${header} ${url}`,
    );
  }
});

Deno.test("parseAllowedHosts accepts table", () => {
  const cases: [string[], string[]][] = [
    [[], []],
    [["app.example"], ["app.example"]],
    [["app.example", "viewer.internal"], LISTED],
    [["myhost"], ["myhost"]],
    [["my_host.local"], ["my_host.local"]],
    // Stored in the spelling a browser sends, so that it can match.
    [["APP.Example"], ["app.example"]],
    [["app.example."], ["app.example"]],
    [["münchen.example"], ["xn--mnchen-3ya.example"]],
    // Listed twice, kept once.
    [["app.example", "App.Example."], ["app.example"]],
    // Never needed, and harmless.
    [["192.168.1.20"], ["192.168.1.20"]],
    [["localhost"], ["localhost"]],
  ];

  for (const [values, hosts] of cases) {
    assertEquals(
      parseAllowedHosts(values),
      { ok: true, hosts },
      values.join(" "),
    );
  }
});

Deno.test("parseAllowedHosts rejects table", () => {
  const cases = [
    "",
    ".",
    "https://app.example",
    "app.example:8080",
    "app.example/",
    "app.example/events",
    "user@app.example",
    "app.example?x=1",
    "app.example#top",
    "app example",
    "app.example,viewer.internal",
    // There are no patterns: these would be listed and never match.
    "*",
    "*.app.example",
    "[::1]",
    "a\\b",
    // What `--allow-host --port 80` hands over.
    "--port",
  ];

  for (const value of cases) {
    const error = `Invalid --allow-host "${value}": expected a host name ` +
      "with no scheme, port or path, such as app.example";
    assertEquals(parseAllowedHosts([value]), { ok: false, error }, value);
    assertEquals(
      parseAllowedHosts(["ok.example", value]),
      { ok: false, error },
      value,
    );
  }
});

Deno.test("every name that parses is one decideHost accepts", () => {
  const written = ["APP.Example.", "münchen.example", "my_host"];
  const sent = ["app.example:8080", "xn--mnchen-3ya.example", "my_host:80"];

  written.forEach((value, index) => {
    const parsed = parseAllowedHosts([value]);
    assertEquals(parsed.ok, true, value);
    if (parsed.ok) {
      assertEquals(
        decideHost(sent[index], "127.0.0.1", parsed.hosts),
        "accept",
      );
      assertEquals(decideHost(sent[index], "127.0.0.1", []), "refuse");
    }
  });
});

Deno.test("misdirected is a 421 that names the flag and echoes nothing", async () => {
  const resp = misdirected();
  assertEquals(resp.status, 421);
  assertEquals(resp.headers.get("Content-Type"), "text/plain; charset=utf-8");
  assertEquals(
    await resp.text(),
    "Misdirected Request: this server does not answer to the host name in " +
      "the request. If the name is its own, list it with --allow-host.\n",
  );
});

Deno.test("describeAllowedHosts says which hosts are answered", () => {
  assertEquals(
    describeAllowedHosts("127.0.0.1", []),
    "any IP address, localhost",
  );
  assertEquals(
    describeAllowedHosts("0.0.0.0", LISTED),
    "any IP address, localhost, app.example, viewer.internal",
  );
  assertEquals(
    describeAllowedHosts("Sink.Internal", ["app.example"]),
    "any IP address, localhost, sink.internal, app.example",
  );
  assertEquals(
    describeAllowedHosts("localhost", ["localhost"]),
    "any IP address, localhost",
  );
});
