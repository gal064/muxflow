/**
 * Typed facade over the `muxflow-ssh` native module (design doc §6.1).
 *
 * The native side emits one `onSshEvent` event discriminated by `type`; this module fans that out
 * into the `SshEvent` union, keeps writes ordered and chunked, and is the only place that knows the
 * native module exists.
 */

export interface SshTarget {
  /** Hostname or IP, as typed by the user. */
  host: string;
  /** Default 22. */
  port: number;
  user: string;
}

/** The complete set of close reasons the native module may report (design doc §6.1). */
export type SshCloseReason =
  | "hostKeyNotTrusted"
  | "hostKeyMismatch"
  | "authFailed"
  | "connectFailed"
  | "exited"
  | "closedByClient"
  | "networkLost";

export type SshEvent =
  /** Emitted only when the presented host key is not yet trusted. */
  | { type: "hostKey"; connectionId: string; algorithm: string; fingerprintSha256: string }
  | { type: "connected"; connectionId: string }
  /** stdout bytes from the remote command. */
  | { type: "data"; connectionId: string; base64: string }
  /** stderr text, utf-8, for diagnostics only. */
  | { type: "stderr"; connectionId: string; text: string }
  | { type: "closed"; connectionId: string; exitCode: number | null; reason: SshCloseReason };

export interface MuxflowSsh {
  /** ed25519; the private key never leaves native storage. Replaces any existing key. */
  generateKeyPair(): Promise<{ publicKeyOpenSsh: string }>;
  /** `"ssh-ed25519 AAAA... muxflow-mobile"`, or null when this phone has no key yet. */
  getPublicKey(): Promise<string | null>;
  deleteKeyPair(): Promise<void>;
  connect(
    connectionId: string,
    target: SshTarget,
    command: string,
    trustedHostKeyFingerprint: string | null,
  ): Promise<void>;
  /** Answers a `hostKey` event. */
  trustHostKey(connectionId: string, fingerprintSha256: string): Promise<void>;
  /** stdin bytes for the remote command. Writes on one connection are serialised. */
  write(connectionId: string, base64: string): Promise<void>;
  close(connectionId: string): Promise<void>;
  startForegroundService(title: string, body: string): Promise<void>;
  stopForegroundService(): Promise<void>;
  addListener(listener: (event: SshEvent) => void): () => void;
}

export interface NativeSubscription {
  remove(): void;
}

/** The shape the Kotlin module exposes. Only `MuxflowSsh.ts` and `modules/muxflow-ssh` use it. */
export interface NativeMuxflowSshModule {
  generateKeyPair(): Promise<{ publicKeyOpenSsh: string }>;
  getPublicKey(): Promise<string | null>;
  deleteKeyPair(): Promise<void>;
  connect(
    connectionId: string,
    target: SshTarget,
    command: string,
    trustedHostKeyFingerprint: string | null,
  ): Promise<void>;
  trustHostKey(connectionId: string, fingerprintSha256: string): Promise<void>;
  write(connectionId: string, base64: string): Promise<void>;
  close(connectionId: string): Promise<void>;
  startForegroundService(title: string, body: string): Promise<void>;
  stopForegroundService(): Promise<void>;
  addListener(
    eventName: "onSshEvent",
    listener: (payload: unknown) => void,
  ): NativeSubscription;
}

export const NATIVE_EVENT_NAME = "onSshEvent";

/**
 * Longest base64 payload handed to a single native `write`. A multiple of 4 so every chunk decodes
 * on its own: 65536 base64 characters carry exactly 48 KiB of stdin.
 */
export const WRITE_CHUNK_BASE64_CHARS = 65536;

const CLOSE_REASONS: ReadonlySet<string> = new Set<SshCloseReason>([
  "hostKeyNotTrusted",
  "hostKeyMismatch",
  "authFailed",
  "connectFailed",
  "exited",
  "closedByClient",
  "networkLost",
]);

/**
 * An unrecognised reason is reported as `networkLost`, the retryable default: §7.2 reconnects from
 * it, so a native module that grew a new reason degrades into a retry rather than a dead end.
 */
export function normalizeCloseReason(raw: unknown): SshCloseReason {
  if (typeof raw === "string" && CLOSE_REASONS.has(raw)) {
    return raw as SshCloseReason;
  }
  console.log(`[muxflow] unknown ssh close reason ${JSON.stringify(raw)}`);
  return "networkLost";
}

/** Turns a raw native payload into a typed event, or null when it is not one we understand. */
export function normalizeSshEvent(payload: unknown): SshEvent | null {
  if (typeof payload !== "object" || payload === null) {
    return null;
  }
  const raw = payload as Record<string, unknown>;
  const connectionId = raw.connectionId;
  if (typeof connectionId !== "string") {
    return null;
  }
  switch (raw.type) {
    case "hostKey":
      if (typeof raw.algorithm !== "string" || typeof raw.fingerprintSha256 !== "string") {
        return null;
      }
      return {
        type: "hostKey",
        connectionId,
        algorithm: raw.algorithm,
        fingerprintSha256: raw.fingerprintSha256,
      };
    case "connected":
      return { type: "connected", connectionId };
    case "data":
      return typeof raw.base64 === "string"
        ? { type: "data", connectionId, base64: raw.base64 }
        : null;
    case "stderr":
      return typeof raw.text === "string" ? { type: "stderr", connectionId, text: raw.text } : null;
    case "closed":
      return {
        type: "closed",
        connectionId,
        exitCode: typeof raw.exitCode === "number" ? raw.exitCode : null,
        reason: normalizeCloseReason(raw.reason),
      };
    default:
      return null;
  }
}

/** Splits a base64 payload on 4-character boundaries so each chunk decodes independently. */
export function chunkBase64(base64: string): string[] {
  if (base64.length <= WRITE_CHUNK_BASE64_CHARS) {
    return base64.length === 0 ? [] : [base64];
  }
  const chunks: string[] = [];
  for (let offset = 0; offset < base64.length; offset += WRITE_CHUNK_BASE64_CHARS) {
    chunks.push(base64.slice(offset, offset + WRITE_CHUNK_BASE64_CHARS));
  }
  return chunks;
}

export function createMuxflowSsh(native: NativeMuxflowSshModule): MuxflowSsh {
  const listeners = new Set<(event: SshEvent) => void>();
  let subscription: NativeSubscription | null = null;
  // One promise chain per connection keeps writes ordered with at most one in flight (§6.2).
  const writeTails = new Map<string, Promise<unknown>>();

  const deliver = (payload: unknown): void => {
    const event = normalizeSshEvent(payload);
    if (event === null) {
      return;
    }
    if (event.type === "closed") {
      writeTails.delete(event.connectionId);
    }
    for (const listener of [...listeners]) {
      listener(event);
    }
  };

  const enqueueWrite = (connectionId: string, task: () => Promise<void>): Promise<void> => {
    const previous = writeTails.get(connectionId) ?? Promise.resolve();
    // `then(task, task)` so one failed write does not wedge the queue for the next one.
    const next = previous.then(task, task);
    writeTails.set(
      connectionId,
      next.catch(() => undefined),
    );
    return next;
  };

  return {
    generateKeyPair: () => native.generateKeyPair(),
    getPublicKey: () => native.getPublicKey(),
    deleteKeyPair: () => native.deleteKeyPair(),
    connect: (connectionId, target, command, trustedHostKeyFingerprint) =>
      native.connect(connectionId, target, command, trustedHostKeyFingerprint),
    trustHostKey: (connectionId, fingerprintSha256) =>
      native.trustHostKey(connectionId, fingerprintSha256),
    write: (connectionId, base64) =>
      enqueueWrite(connectionId, async () => {
        for (const chunk of chunkBase64(base64)) {
          await native.write(connectionId, chunk);
        }
      }),
    close: async (connectionId) => {
      writeTails.delete(connectionId);
      await native.close(connectionId);
    },
    startForegroundService: (title, body) => native.startForegroundService(title, body),
    stopForegroundService: () => native.stopForegroundService(),
    addListener: (listener) => {
      listeners.add(listener);
      if (subscription === null) {
        subscription = native.addListener(NATIVE_EVENT_NAME, deliver);
      }
      return () => {
        if (!listeners.delete(listener)) {
          return;
        }
        if (listeners.size === 0 && subscription !== null) {
          subscription.remove();
          subscription = null;
        }
      };
    },
  };
}

let instance: MuxflowSsh | null = null;

/** The app-wide facade, wired to the real native module on first use. */
export function muxflowSsh(): MuxflowSsh {
  if (instance === null) {
    // Required lazily so this module can be imported (and unit tested) without a native runtime.
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const native = require("../../modules/muxflow-ssh").default as NativeMuxflowSshModule;
    instance = createMuxflowSsh(native);
  }
  return instance;
}
