declare const terminalBytesOwnership: unique symbol;

/** Bytes whose backing allocation belongs exclusively to the terminal path. */
export type OwnedTerminalBytes = Uint8Array<ArrayBuffer> & {
  readonly [terminalBytesOwnership]: true;
};

/** Establishes the single ownership-copy boundary for borrowed transport data. */
export function copyTerminalBytes(bytes: Uint8Array): OwnedTerminalBytes {
  const owned = new Uint8Array(bytes.byteLength);
  owned.set(bytes);
  return owned as OwnedTerminalBytes;
}

/** Marks a fresh allocation that has no external mutable owner. */
export function ownTerminalBytes(bytes: Uint8Array<ArrayBuffer>): OwnedTerminalBytes {
  return bytes as OwnedTerminalBytes;
}
