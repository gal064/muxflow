// The process-local diagnostic flight recorder. Every mobile subsystem writes
// through this one sink, and the Connection sheet exposes this same store.
// Nothing is persisted or uploaded: process death intentionally clears it.

import { createStore, type StoreApi } from "zustand/vanilla";

export const LOG_EVENT_CAPACITY = 1_000;
export const LOG_BYTE_CAPACITY = 256 * 1024;
/** Bounds accidental native/host error dumps before they enter the recorder. */
export const LOG_EVENT_CHAR_CAPACITY = 4_096;

export interface LogState {
  lines: string[];
  /** UTF-8 bytes occupied by `lines.join("\n")`. */
  bytes: number;
}

export interface LogActions {
  /** Appends one redacted, timestamped event and returns the stored line. */
  append(line: string): string;
  clear(): void;
}

export type LogStore = StoreApi<LogState & LogActions>;

export interface LogStoreOptions {
  eventCapacity?: number;
  byteCapacity?: number;
  now?: () => number;
}

const SECRET_VALUE = "<redacted>";

/**
 * Central last-line defense for copied diagnostics. Producers must still avoid
 * user content entirely; this catches common credential/key shapes in errors.
 */
export function redactSecrets(value: string): string {
  return value
    .replace(/-----BEGIN [^-\r\n]*(?:PRIVATE|PUBLIC) KEY-----[\s\S]*?-----END [^-\r\n]*(?:PRIVATE|PUBLIC) KEY-----/gi, SECRET_VALUE)
    .replace(/-----BEGIN [^-\r\n]*(?:PRIVATE|PUBLIC) KEY-----[\s\S]*/gi, SECRET_VALUE)
    .replace(/\b(ssh-ed25519|ssh-rsa|ecdsa-sha2-[a-z0-9-]+)\s+[A-Za-z0-9+/=]{60,}/gi, "$1 …")
    .replace(/\bsk-(?:proj-)?[A-Za-z0-9_-]{16,}/g, SECRET_VALUE)
    .replace(/\bgh[pousr]_[A-Za-z0-9]{20,}/g, SECRET_VALUE)
    .replace(/\bAKIA[A-Z0-9]{16}\b/g, SECRET_VALUE)
    .replace(/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/g, SECRET_VALUE)
    .replace(/\b(Bearer)\s+[A-Za-z0-9._~+/=-]+/gi, `$1 ${SECRET_VALUE}`)
    .replace(/(["'](?:[a-z][a-z0-9_.-]*[_.-])?(?:authorization|credential|password|passwd|passphrase|private[_-]?key|public[_-]?key|api[_-]?key|client[_-]?secret|secret[_-]access[_-]key|access[_-]?token|refresh[_-]?token|secret|token|prompt|transcript|terminal[_-]?(?:output|contents?)|raw[_-]?audio|audio[_-]?(?:data|base64))["']\s*:\s*)(?:"[^"]*"|'[^']*'|[^\s,;}]+)/gi, `$1"${SECRET_VALUE}"`)
    .replace(/\b((?:[a-z][a-z0-9_.-]*[_.-])?(?:authorization|credential|password|passwd|passphrase|private[_-]?key|public[_-]?key|api[_-]?key|client[_-]?secret|secret[_-]access[_-]key|access[_-]?token|refresh[_-]?token|secret|token|prompt|transcript|terminal[_-]?(?:output|contents?)|raw[_-]?audio|audio[_-]?(?:data|base64)))\s*([=:])\s*(?:"[^"]*"|'[^']*'|[^\s,;]+)/gi, `$1$2${SECRET_VALUE}`)
    .replace(/\b([a-z][a-z0-9+.-]*:\/\/[^\s:/@]+:)[^\s@]+@/gi, `$1${SECRET_VALUE}@`);
}

function singleLine(value: string): string {
  const redacted = redactSecrets(value).replace(/[\r\n]+/g, " ↩ ").trim();
  if (redacted.length <= LOG_EVENT_CHAR_CAPACITY) return redacted;
  return `${redacted.slice(0, LOG_EVENT_CHAR_CAPACITY - 1)}…`;
}

function utf8Bytes(value: string): number {
  // Hermes versions without TextEncoder still need an exact byte bound. Count
  // UTF-8 directly and treat unpaired surrogates as the 3-byte replacement
  // character, matching TextEncoder.
  let bytes = 0;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code < 0x80) bytes += 1;
    else if (code < 0x800) bytes += 2;
    else if (code >= 0xd800 && code <= 0xdbff && index + 1 < value.length) {
      const low = value.charCodeAt(index + 1);
      if (low >= 0xdc00 && low <= 0xdfff) {
        bytes += 4;
        index += 1;
      } else {
        bytes += 3;
      }
    } else {
      bytes += 3;
    }
  }
  return bytes;
}

export function createLogStore(options: LogStoreOptions = {}): LogStore {
  const eventCapacity = Math.max(1, Math.floor(options.eventCapacity ?? LOG_EVENT_CAPACITY));
  const byteCapacity = Math.max(1, Math.floor(options.byteCapacity ?? LOG_BYTE_CAPACITY));
  const now = options.now ?? Date.now;
  let sequence = 0;

  return createStore<LogState & LogActions>((set) => ({
    lines: [],
    bytes: 0,
    append(line) {
      sequence += 1;
      const body = singleLine(line.startsWith("[muxflow]") ? line : `[muxflow] ${line}`);
      const entry = `${new Date(now()).toISOString()} #${String(sequence).padStart(6, "0")} ${body}`;
      set((state) => {
        let lines = [...state.lines, entry];
        let bytes = state.bytes + (state.lines.length > 0 ? 1 : 0) + utf8Bytes(entry);
        let removed = 0;
        while (removed < lines.length && (lines.length - removed > eventCapacity || bytes > byteCapacity)) {
          bytes -= utf8Bytes(lines[removed]!) + (lines.length - removed > 1 ? 1 : 0);
          removed += 1;
        }
        if (removed > 0) lines = lines.slice(removed);
        return { lines, bytes: Math.max(0, bytes) };
      });
      return entry;
    },
    clear() {
      set({ lines: [], bytes: 0 });
    },
  }));
}

export const logStore: LogStore = createLogStore();

/** Console output and the visible/copyable recorder always receive the same redacted line. */
export function log(line: string): void {
  console.log(logStore.getState().append(line));
}

/**
 * The buffer as one copyable block, optionally preceded by copy-time snapshot
 * lines. Events are newest-first so a downstream paste limit preserves the
 * failure closest to the moment Copy was tapped.
 */
export function logText(state: LogState = logStore.getState(), header: readonly string[] = []): string {
  return [...header.map(singleLine), ...[...state.lines].reverse()].join("\n");
}

export function logLines(): readonly string[] {
  return logStore.getState().lines;
}
