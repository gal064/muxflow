import type { IDisposable } from "@xterm/xterm";

/** One terminal application may replace at most 1 MiB of clipboard text. */
export const MAX_OSC52_CLIPBOARD_BYTES = 1024 * 1024;

const OSC52_SELECTION = /^[cpsq0-7]*$/u;
const BASE64_PAYLOAD = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u;

/**
 * Decodes only OSC 52 writes. Queries (`?`), malformed data, non-UTF-8 text,
 * empty writes and payloads above the clipboard bound are all refused.
 */
export function decodeOsc52ClipboardWrite(data: string): string | undefined {
  const separator = data.indexOf(";");
  if (separator < 0) return undefined;
  const selection = data.slice(0, separator);
  const encoded = data.slice(separator + 1);
  if (!OSC52_SELECTION.test(selection) || !encoded || encoded === "?" || !BASE64_PAYLOAD.test(encoded)) {
    return undefined;
  }
  const padding = encoded.endsWith("==") ? 2 : encoded.endsWith("=") ? 1 : 0;
  const decodedBytes = (encoded.length / 4) * 3 - padding;
  if (decodedBytes > MAX_OSC52_CLIPBOARD_BYTES) return undefined;
  try {
    const binary = atob(encoded);
    // `atob` accepts base64 with non-zero discarded padding bits. Re-encoding
    // makes the accepted form canonical instead of treating that as valid.
    if (btoa(binary) !== encoded) return undefined;
    const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return undefined;
  }
}

type Osc52Parser = {
  registerOscHandler(identifier: number, handler: (data: string) => boolean): IDisposable;
};

/** Installs a consumed, write-only OSC 52 bridge on an xterm-compatible parser. */
export function installOsc52ClipboardWrite(
  parser: Osc52Parser,
  write: (text: string) => void | Promise<void>,
  onError: (error: unknown) => void = () => undefined,
): IDisposable {
  return parser.registerOscHandler(52, (data) => {
    const text = decodeOsc52ClipboardWrite(data);
    if (text !== undefined) void Promise.resolve().then(() => write(text)).catch(onError);
    // Always consume OSC 52, including rejected reads and malformed writes.
    return true;
  });
}
