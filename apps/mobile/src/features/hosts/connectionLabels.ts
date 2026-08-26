// Human names for the connection states (design.md §9.8's `State` row) and the
// strip copy from §9 "Global chrome".

import type { ConnectionState } from "../../store/sessionStore";

export function connectionStateLabel(state: ConnectionState): string {
  switch (state) {
    case "idle":
      return "Not connected";
    case "sshConnecting":
      return "Connecting";
    case "awaitingHostKeyTrust":
      return "Waiting for host key trust";
    case "handshaking":
      return "Handshaking";
    case "connected":
      return "Connected";
    case "reconnecting":
      return "Reconnecting";
    case "failed":
      return "Failed";
    case "incompatible":
      return "Incompatible helper";
  }
}

/** §9: the strip's copy while connecting or reconnecting. */
export function connectingStripText(label: string): string {
  return `Connecting to ${label}…`;
}

export function reconnectingStripText(secondsLeft: number): string {
  return secondsLeft > 0 ? `Connection lost. Reconnecting in ${secondsLeft}s…` : "Reconnecting…";
}

/** §7.2's backoff, so the strip can count the same seconds down. */
export function backoffSeconds(attempt: number): number {
  return Math.min(2 ** Math.max(0, attempt - 1), 30);
}
