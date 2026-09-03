// The byte pipe HostConnection speaks over. The SSH module (§6) implements it
// for a real host; tests implement it in memory or over a child process.

/** Why a transport closed, mirroring the SSH module's `closed` reasons (§6.2, §12). */
export type TransportCloseReason =
  | "exited"
  | "networkLost"
  | "connectFailed"
  | "authFailed"
  | "hostKeyMismatch"
  | "hostKeyNotTrusted"
  | "localClose";

export interface TransportClose {
  reason: TransportCloseReason;
  /** Remote command exit code when `reason` is `exited`. */
  exitCode?: number;
  /** First stderr line or a driver message, for the strip/toast. */
  message?: string;
}

export interface Transport {
  write(bytes: Uint8Array): void;
  onData(listener: (chunk: Uint8Array) => void): void;
  onClosed(listener: (close: TransportClose) => void): void;
  close(): void;
}

/** Thrown by a dial that could not produce a transport at all. */
export class TransportDialError extends Error {
  constructor(readonly close: TransportClose) {
    super(close.message ?? close.reason);
    this.name = "TransportDialError";
  }
}
