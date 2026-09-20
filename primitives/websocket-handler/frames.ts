/**
 * Frame decoding for websocket-handler, as pure functions.
 *
 * @module
 */

/** Bytes converted per String.fromCharCode call, far below the argument limit. */
const BASE64_CHUNK_BYTES = 0x8000;

/**
 * Base64 of `bytes`. The bytes are converted in chunks because spreading a
 * large frame into one String.fromCharCode call overflows the call stack
 * (measured at about 125 KB).
 */
export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += BASE64_CHUNK_BYTES) {
    binary += String.fromCharCode(
      ...bytes.subarray(i, i + BASE64_CHUNK_BYTES),
    );
  }
  return btoa(binary);
}

/**
 * The `data` field of a frame event: parsed JSON when a text frame parses, the
 * raw text otherwise, and base64 for a binary frame.
 *
 * Binary frames arrive as ArrayBuffer only when the socket's binaryType is
 * "arraybuffer", which main.ts sets.
 */
export function decodeFrame(raw: unknown): unknown {
  if (typeof raw === "string") {
    try {
      return JSON.parse(raw);
    } catch {
      return raw;
    }
  }
  if (raw instanceof ArrayBuffer) return bytesToBase64(new Uint8Array(raw));
  return String(raw);
}
