import { assertEquals } from "@std/assert";
import {
  type Clients,
  clientStream,
  handleRequest,
  type SinkHandlers,
  type SinkRules,
} from "./http.ts";

const RULES: SinkRules = {
  bound: "127.0.0.1",
  allowedHosts: [],
  allowedOrigins: { kind: "list", origins: ["https://app.example"] },
};

// Stand-ins that record what the HTTP surface asked of the sink.
function recorder(): { handlers: SinkHandlers; calls: string[] } {
  const calls: string[] = [];
  const handlers: SinkHandlers = {
    openStream: () => {
      calls.push("stream");
      return new ReadableStream<Uint8Array>();
    },
    clientCount: () => {
      calls.push("count");
      return 2;
    },
  };
  return { handlers, calls };
}

function request(host: string, path: string, origin?: string): Request {
  return new Request(`http://${host}${path}`, {
    headers: origin === undefined
      ? { Host: host }
      : { Host: host, Origin: origin },
  });
}

Deno.test("the routes, asked under the sink's own address", async () => {
  const { handlers, calls } = recorder();

  const events = handleRequest(
    request("127.0.0.1:8080", "/events"),
    handlers,
    RULES,
  );
  assertEquals(events.status, 200);
  assertEquals(events.headers.get("content-type"), "text/event-stream");
  await events.body?.cancel();

  const health = handleRequest(
    request("127.0.0.1:8080", "/health"),
    handlers,
    RULES,
  );
  assertEquals(await health.json(), { ok: true, clients: 2 });

  const missing = handleRequest(
    request("127.0.0.1:8080", "/nope"),
    handlers,
    RULES,
  );
  assertEquals(missing.status, 404);
  await missing.body?.cancel();

  assertEquals(calls, ["stream", "count"]);
});

Deno.test("a request naming a foreign host is answered 421 and opens no stream", async () => {
  // What a rebinding page sends: its own name, on the sink's port or not.
  const foreign = ["attacker.example", "attacker.example:8080", "localhost.x"];

  for (const path of ["/events", "/health", "/nope"]) {
    for (const host of foreign) {
      const { handlers, calls } = recorder();

      const resp = handleRequest(request(host, path), handlers, RULES);

      assertEquals(resp.status, 421, `${path} as ${host}`);
      assertEquals(calls, [], `${path} as ${host}: no client is registered`);
      await resp.body?.cancel();
    }
  }
});

Deno.test("a refused host gets no origin header, listed origin or not", async () => {
  const { handlers } = recorder();

  const resp = handleRequest(
    request("attacker.example", "/events", "https://app.example"),
    handlers,
    RULES,
  );

  assertEquals(resp.status, 421);
  assertEquals(resp.headers.get("access-control-allow-origin"), null);
  await resp.body?.cancel();
});

Deno.test("the hosts the sink knows as its own reach the stream", async () => {
  const cases: [string, string, string[], number][] = [
    ["127.0.0.1:8080", "127.0.0.1", [], 200],
    ["localhost:8080", "127.0.0.1", [], 200],
    ["[::1]:8080", "::1", [], 200],
    ["192.168.1.20:8080", "0.0.0.0", [], 200],
    ["localhost:8080", "0.0.0.0", [], 200],
    ["myhost.local:8080", "0.0.0.0", [], 421],
    ["myhost.local:8080", "0.0.0.0", ["myhost.local"], 200],
    ["events.example", "127.0.0.1", ["events.example"], 200],
    ["events.example", "127.0.0.1", [], 421],
  ];

  for (const [host, bound, allowedHosts, status] of cases) {
    const { handlers } = recorder();

    const resp = handleRequest(request(host, "/events"), handlers, {
      ...RULES,
      bound,
      allowedHosts,
    });

    assertEquals(resp.status, status, `${host} bound ${bound}`);
    await resp.body?.cancel();
  }
});

Deno.test("the origin decision still rides on the stream, and only there", async () => {
  const cases: [string, string | undefined, string | null][] = [
    ["/events", "https://app.example", "https://app.example"],
    ["/events", "https://evil.example", null],
    ["/events", undefined, null],
    ["/health", "https://app.example", null],
  ];

  for (const [path, origin, expected] of cases) {
    const { handlers } = recorder();

    const resp = handleRequest(
      request("127.0.0.1:8080", path, origin),
      handlers,
      RULES,
    );

    assertEquals(
      resp.headers.get("access-control-allow-origin"),
      expected,
      `${path} from ${origin}`,
    );
    await resp.body?.cancel();
  }
});

Deno.test("a client is counted while its stream is open and dropped when it closes", async () => {
  const clients: Clients = new Set();

  const first = clientStream(clients);
  const second = clientStream(clients);
  assertEquals(clients.size, 2);

  await first.cancel("client went away");
  assertEquals(clients.size, 1, "the closed client is dropped at once");

  // The one that is left is the one still open: it takes an event.
  const [open] = clients;
  open.enqueue(new TextEncoder().encode("data: {}\n\n"));
  const chunk = await second.getReader().read();
  assertEquals(new TextDecoder().decode(chunk.value), "data: {}\n\n");
});
