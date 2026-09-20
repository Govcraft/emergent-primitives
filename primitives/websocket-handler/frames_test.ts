import { assertEquals } from "@std/assert";
import { bytesToBase64, decodeFrame } from "./frames.ts";

Deno.test("decodeFrame: text frames parse as JSON, else stay text", () => {
  // [raw, expected]
  const cases: [string, unknown][] = [
    ['{"type":"hello","n":1}', { type: "hello", n: 1 }],
    ["[1,2]", [1, 2]],
    ["42", 42],
    ["plain text", "plain text"],
    ["{not json", "{not json"],
    ["", ""],
  ];
  for (const [raw, expected] of cases) {
    assertEquals(decodeFrame(raw), expected);
  }
});

Deno.test("decodeFrame: binary frames become base64", () => {
  assertEquals(decodeFrame(new Uint8Array([1, 2, 3]).buffer), "AQID");
  assertEquals(decodeFrame(new ArrayBuffer(0)), "");
});

Deno.test("bytesToBase64: a frame larger than the call stack allows", () => {
  const bytes = new Uint8Array(1_000_000);
  for (let i = 0; i < bytes.length; i++) bytes[i] = i % 251;

  const decoded = Uint8Array.from(
    atob(bytesToBase64(bytes)),
    (c) => c.charCodeAt(0),
  );
  assertEquals(decoded, bytes);
});

Deno.test("bytesToBase64: chunk boundaries do not corrupt the encoding", () => {
  for (const size of [0x7fff, 0x8000, 0x8001, 0x10000]) {
    const bytes = new Uint8Array(size).fill(0xab);
    const decoded = Uint8Array.from(
      atob(bytesToBase64(bytes)),
      (c) => c.charCodeAt(0),
    );
    assertEquals(decoded, bytes, `size ${size}`);
  }
});
