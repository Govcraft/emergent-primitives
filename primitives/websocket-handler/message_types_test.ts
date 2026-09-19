import { assertEquals } from "jsr:@std/assert@1";
import {
  parseTypeList,
  resolvePublishTypes,
  resolveSubscribeTypes,
  siblingType,
} from "./message_types.ts";

Deno.test("parseTypeList: trims entries and drops blanks", () => {
  // [input, expected]
  const cases: [string | undefined, string[]][] = [
    [undefined, []],
    ["", []],
    [" , ,", []],
    ["ws.closed", ["ws.closed"]],
    [" ws.closed , ws.error,", ["ws.closed", "ws.error"]],
  ];
  for (const [input, expected] of cases) {
    assertEquals(parseTypeList(input), expected);
  }
});

Deno.test("publish types: prefix defaults when the engine declares nothing", () => {
  assertEquals(resolvePublishTypes("ws"), {
    connected: "ws.connected",
    frame: "ws.frame",
    closed: "ws.closed",
    disconnected: "ws.disconnected",
    error: "ws.error",
  });
  assertEquals(resolvePublishTypes("slack", "").closed, "slack.closed");
});

Deno.test("publish types: declared types win over the prefix", () => {
  assertEquals(
    resolvePublishTypes(
      "ws",
      "slack.connected,slack.frame,slack.closed,slack.disconnected,slack.error",
    ),
    {
      connected: "slack.connected",
      frame: "slack.frame",
      closed: "slack.closed",
      disconnected: "slack.disconnected",
      error: "slack.error",
    },
  );
});

Deno.test("publish types: disconnected is never taken for connected", () => {
  // Order matters: a loose suffix match would pick the first entry.
  const types = resolvePublishTypes("ws", "slack.disconnected,slack.connected");
  assertEquals(types.connected, "slack.connected");
  assertEquals(types.disconnected, "slack.disconnected");
});

Deno.test("publish types: an older topology gets the sibling of its closed type", () => {
  const types = resolvePublishTypes(
    "ws",
    "slack.connected,slack.frame,slack.closed,slack.error",
  );
  assertEquals(types.closed, "slack.closed");
  assertEquals(types.disconnected, "slack.disconnected");
});

Deno.test("subscribe types: disconnect is never taken for connect", () => {
  assertEquals(
    resolveSubscribeTypes("ws", "slack.disconnect,slack.connect,slack.send"),
    {
      connect: "slack.connect",
      send: "slack.send",
      disconnect: "slack.disconnect",
    },
  );
  assertEquals(resolveSubscribeTypes("ws"), {
    connect: "ws.connect",
    send: "ws.send",
    disconnect: "ws.disconnect",
  });
});

Deno.test("siblingType: replaces the last segment", () => {
  // [type, role, expected]
  const cases: [string, string, string][] = [
    ["ws.closed", "disconnected", "ws.disconnected"],
    ["slack.socket.closed", "disconnected", "slack.socket.disconnected"],
    ["closed", "disconnected", "disconnected"],
  ];
  for (const [type, role, expected] of cases) {
    assertEquals(siblingType(type, role), expected);
  }
});
