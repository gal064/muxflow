// Base64 for the SSH byte pipe. The native module speaks base64 in both
// directions (§6.1); everything above it speaks `Uint8Array`.
//
// Hermes and Node both provide `atob`/`btoa`, which are far faster than a JS
// loop on the terminal-output path; the pure fallback keeps this module usable
// on a runtime that does not.

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/** Chunked so `String.fromCharCode(...)` never overflows the argument stack. */
const CHUNK = 0x8000;

const nativeBtoa = typeof globalThis.btoa === "function" ? globalThis.btoa.bind(globalThis) : undefined;
const nativeAtob = typeof globalThis.atob === "function" ? globalThis.atob.bind(globalThis) : undefined;

export function bytesToBase64(bytes: Uint8Array): string {
  if (bytes.length === 0) return "";
  if (nativeBtoa) {
    let binary = "";
    for (let offset = 0; offset < bytes.length; offset += CHUNK) {
      binary += String.fromCharCode(...bytes.subarray(offset, offset + CHUNK));
    }
    return nativeBtoa(binary);
  }
  let out = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const a = bytes[i] as number;
    const b = bytes[i + 1];
    const c = bytes[i + 2];
    out += ALPHABET[a >> 2];
    out += ALPHABET[((a & 3) << 4) | ((b ?? 0) >> 4)];
    out += b === undefined ? "=" : ALPHABET[((b & 15) << 2) | ((c ?? 0) >> 6)];
    out += c === undefined ? "=" : ALPHABET[c & 63];
  }
  return out;
}

export function base64ToBytes(base64: string): Uint8Array {
  if (base64.length === 0) return new Uint8Array(0);
  if (nativeAtob) {
    const binary = nativeAtob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i) & 0xff;
    return bytes;
  }
  const clean = base64.replace(/[^A-Za-z0-9+/]/g, "");
  const bytes = new Uint8Array((clean.length * 3) >> 2);
  let out = 0;
  for (let i = 0; i < clean.length; i += 4) {
    const a = ALPHABET.indexOf(clean[i] as string);
    const b = ALPHABET.indexOf(clean[i + 1] ?? "A");
    const c = clean[i + 2] === undefined ? -1 : ALPHABET.indexOf(clean[i + 2] as string);
    const d = clean[i + 3] === undefined ? -1 : ALPHABET.indexOf(clean[i + 3] as string);
    bytes[out++] = (a << 2) | (b >> 4);
    if (c >= 0) bytes[out++] = ((b & 15) << 4) | (c >> 2);
    if (d >= 0) bytes[out++] = ((c & 3) << 6) | d;
  }
  return bytes.subarray(0, out);
}
