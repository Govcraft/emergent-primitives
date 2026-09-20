import { assertEquals } from "jsr:@std/assert@1";
import {
  decideRoute,
  EVENT_STREAM_HEADERS,
  handleRequest,
  type HostRules,
  refreshStatus,
  type RouteDecision,
  singleFlight,
  type ViewerHandlers,
} from "./http.ts";
import type { TopologyHealth, TopologyState } from "./types.ts";

const OK_STATE: TopologyState = {
  nodes: [{
    id: "timer",
    kind: "source",
    status: "running",
    publishes: ["timer.tick"],
    subscribes: [],
  }],
  edges: [],
  health: { status: "ok" },
};

const DEGRADED_STATE: TopologyState = {
  nodes: [],
  edges: [],
  health: { status: "degraded", detail: "The engine did not answer." },
};

/** Handlers that record what was asked of them. */
function recorder(states: TopologyState[]): {
  handlers: ViewerHandlers;
  calls: string[];
} {
  const calls: string[] = [];
  let current = states[0];
  const handlers: ViewerHandlers = {
    state: () => {
      calls.push("state");
      return current;
    },
    refresh: () => {
      calls.push("refresh");
      // A refresh moves the graph on to the next state, when there is one.
      current = states[1] ?? current;
      return Promise.resolve();
    },
    readStatic: (file) => {
      calls.push(`static:${file}`);
      return Promise.resolve(new Response(file));
    },
    openEvents: () => {
      calls.push("events");
      return new Response("stream");
    },
  };
  return { handlers, calls };
}

// The tests reach the viewer as viewer.test, a name it has been told is its own.
const HOSTS: HostRules = { bound: "127.0.0.1", allowed: ["viewer.test"] };

function request(method: string, path: string): Request {
  return new Request(`http://viewer.test${path}`, { method });
}

Deno.test("decideRoute table", () => {
  const cases: [string, string, RouteDecision][] = [
    ["GET", "/", { kind: "static", file: "index.html" }],
    ["GET", "/app.js", { kind: "static", file: "app.js" }],
    ["GET", "/style.css", { kind: "static", file: "style.css" }],
    ["GET", "/events", { kind: "events" }],
    ["GET", "/api/topology", { kind: "topology" }],
    ["POST", "/api/refresh", { kind: "refresh" }],
    // Refresh does work, so only POST reaches it.
    ["GET", "/api/refresh", { kind: "methodNotAllowed", allow: "POST" }],
    ["HEAD", "/api/refresh", { kind: "methodNotAllowed", allow: "POST" }],
    ["OPTIONS", "/api/refresh", { kind: "methodNotAllowed", allow: "POST" }],
    ["post", "/api/refresh", { kind: "methodNotAllowed", allow: "POST" }],
    // The read-only routes answer every method, as before.
    ["POST", "/api/topology", { kind: "topology" }],
    // Paths are exact.
    ["POST", "/api/refresh/", { kind: "notFound" }],
    ["GET", "/refresh", { kind: "notFound" }],
    ["GET", "/static/app.js", { kind: "notFound" }],
    ["GET", "/../main.ts", { kind: "notFound" }],
    ["GET", "/constructor", { kind: "notFound" }],
    ["GET", "", { kind: "notFound" }],
  ];

  for (const [method, path, expected] of cases) {
    assertEquals(decideRoute(method, path), expected, `${method} ${path}`);
  }
});

Deno.test("refreshStatus is 502 only when the engine read failed", () => {
  const cases: [TopologyHealth, number][] = [
    [{ status: "ok" }, 200],
    [{ status: "empty", detail: "no primitives" }, 200],
    [{ status: "pending", detail: "waiting" }, 200],
    [{ status: "degraded", detail: "timed out" }, 502],
  ];

  for (const [health, expected] of cases) {
    assertEquals(refreshStatus(health), expected, health.status);
  }
});

Deno.test("POST /api/refresh re-reads once, then replies with the fresh state", async () => {
  const { handlers, calls } = recorder([DEGRADED_STATE, OK_STATE]);

  const resp = await handleRequest(
    request("POST", "/api/refresh"),
    handlers,
    HOSTS,
  );

  assertEquals(calls, ["refresh", "state"]);
  assertEquals(resp.status, 200);
  assertEquals(resp.headers.get("Content-Type"), "application/json");
  assertEquals(resp.headers.get("Cache-Control"), "no-store");
  assertEquals(resp.headers.get("Access-Control-Allow-Origin"), null);
  assertEquals(await resp.json(), OK_STATE);
});

Deno.test("POST /api/refresh answers 502 with the state when the engine read failed", async () => {
  const { handlers } = recorder([OK_STATE, DEGRADED_STATE]);

  const resp = await handleRequest(
    request("POST", "/api/refresh"),
    handlers,
    HOSTS,
  );

  assertEquals(resp.status, 502);
  assertEquals(await resp.json(), DEGRADED_STATE);
});

Deno.test("GET /api/refresh is refused without touching the engine", async () => {
  const { handlers, calls } = recorder([OK_STATE]);

  const resp = await handleRequest(
    request("GET", "/api/refresh"),
    handlers,
    HOSTS,
  );

  assertEquals(resp.status, 405);
  assertEquals(resp.headers.get("Allow"), "POST");
  assertEquals(calls, []);
  await resp.body?.cancel();
});

Deno.test("GET /api/topology reports the state without a re-read", async () => {
  const { handlers, calls } = recorder([OK_STATE]);

  const resp = await handleRequest(
    request("GET", "/api/topology"),
    handlers,
    HOSTS,
  );

  assertEquals(calls, ["state"]);
  assertEquals(resp.status, 200);
  assertEquals(await resp.json(), OK_STATE);
});

Deno.test("no response allows another origin, whatever origin asks", async () => {
  const routes: [string, string, number][] = [
    ["GET", "/", 200],
    ["GET", "/app.js", 200],
    ["GET", "/style.css", 200],
    ["GET", "/events", 200],
    ["GET", "/api/topology", 200],
    ["POST", "/api/refresh", 200],
    ["GET", "/api/refresh", 405],
    ["OPTIONS", "/api/refresh", 405],
    ["GET", "/nope", 404],
  ];
  const origins = [null, "https://evil.example", "http://127.0.0.1:8080"];

  for (const [method, path, status] of routes) {
    for (const origin of origins) {
      const { handlers } = recorder([OK_STATE]);
      const req = new Request(`http://viewer.test${path}`, {
        method,
        headers: origin === null ? {} : { Origin: origin },
      });

      const resp = await handleRequest(req, handlers, HOSTS);

      const label = `${method} ${path} from ${origin}`;
      assertEquals(resp.status, status, label);
      assertEquals(
        resp.headers.get("Access-Control-Allow-Origin"),
        null,
        label,
      );
      await resp.body?.cancel();
    }
  }
});

Deno.test("the event stream headers name no other origin", () => {
  const names = Object.keys(EVENT_STREAM_HEADERS).map((n) => n.toLowerCase());
  assertEquals(names, ["content-type", "cache-control", "connection"]);
  assertEquals(EVENT_STREAM_HEADERS["Content-Type"], "text/event-stream");
});

Deno.test("static files, the event stream and unknown paths", async () => {
  const { handlers, calls } = recorder([OK_STATE]);

  const page = await handleRequest(request("GET", "/"), handlers, HOSTS);
  assertEquals(await page.text(), "index.html");
  const events = await handleRequest(
    request("GET", "/events"),
    handlers,
    HOSTS,
  );
  assertEquals(await events.text(), "stream");
  const missing = await handleRequest(request("GET", "/nope"), handlers, HOSTS);
  assertEquals(missing.status, 404);
  await missing.body?.cancel();

  assertEquals(calls, ["static:index.html", "events"]);
});

Deno.test("singleFlight shares an in-flight run and starts a new one after it", async () => {
  let runs = 0;
  const gates: (() => void)[] = [];
  const shared = singleFlight(() => {
    runs += 1;
    const run = runs;
    return new Promise<number>((resolve) => gates.push(() => resolve(run)));
  });

  const first = shared();
  const second = shared();
  assertEquals(runs, 1, "overlapping calls share one run");
  gates[0]();
  assertEquals(await Promise.all([first, second]), [1, 1]);

  const third = shared();
  assertEquals(runs, 2, "a call after the run settled starts a new one");
  gates[1]();
  assertEquals(await third, 2);
});

Deno.test("singleFlight starts a new run after a rejected one", async () => {
  let runs = 0;
  const shared = singleFlight(() => {
    runs += 1;
    return runs === 1
      ? Promise.reject(new Error("first run fails"))
      : Promise.resolve(runs);
  });

  const failure = await shared().then(() => null, (err: Error) => err.message);
  assertEquals(failure, "first run fails");
  assertEquals(await shared(), 2);
});

Deno.test("a request naming a foreign host is answered 421 before any route", async () => {
  const routes: [string, string][] = [
    ["GET", "/"],
    ["GET", "/app.js"],
    ["GET", "/events"],
    ["GET", "/api/topology"],
    ["POST", "/api/refresh"],
    ["GET", "/api/refresh"],
    ["GET", "/nope"],
  ];
  // What a rebinding page sends: its own name, on the viewer's port or not.
  const foreign = [
    "attacker.example",
    "attacker.example:8080",
    "viewer.test.x",
  ];

  for (const [method, path] of routes) {
    for (const host of foreign) {
      const { handlers, calls } = recorder([OK_STATE]);
      const req = new Request(`http://${host}${path}`, {
        method,
        headers: { Host: host },
      });

      const resp = await handleRequest(req, handlers, HOSTS);

      const label = `${method} ${path} as ${host}`;
      assertEquals(resp.status, 421, label);
      assertEquals(calls, [], `${label}: nothing is read, re-read or opened`);
      await resp.body?.cancel();
    }
  }
});

Deno.test("the hosts the viewer knows as its own reach the routes", async () => {
  const cases: [string, HostRules, number][] = [
    ["127.0.0.1:8080", { bound: "127.0.0.1", allowed: [] }, 200],
    ["localhost:8080", { bound: "127.0.0.1", allowed: [] }, 200],
    ["[::1]:8080", { bound: "::1", allowed: [] }, 200],
    ["192.168.1.20:8080", { bound: "0.0.0.0", allowed: [] }, 200],
    ["localhost:8080", { bound: "0.0.0.0", allowed: [] }, 200],
    ["myhost.local:8080", { bound: "0.0.0.0", allowed: [] }, 421],
    ["myhost.local:8080", { bound: "0.0.0.0", allowed: ["myhost.local"] }, 200],
    ["app.example", { bound: "127.0.0.1", allowed: ["app.example"] }, 200],
    ["app.example", { bound: "127.0.0.1", allowed: [] }, 421],
  ];

  for (const [host, hosts, status] of cases) {
    const { handlers } = recorder([OK_STATE]);
    const req = new Request(`http://${host}/api/topology`, {
      headers: { Host: host },
    });

    const resp = await handleRequest(req, handlers, hosts);

    assertEquals(resp.status, status, `${host} bound ${hosts.bound}`);
    await resp.body?.cancel();
  }
});

Deno.test("a Host header and a request URL that disagree are both checked", async () => {
  const { handlers, calls } = recorder([OK_STATE]);
  const req = new Request("http://attacker.example/api/topology", {
    headers: { Host: "localhost:8080" },
  });

  const resp = await handleRequest(req, handlers, HOSTS);

  assertEquals(resp.status, 421);
  assertEquals(calls, []);
  await resp.body?.cancel();
});
