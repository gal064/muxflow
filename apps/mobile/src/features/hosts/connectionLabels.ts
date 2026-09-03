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

/**
 * §9 spells the reconnecting strip `"Connection lost. Reconnecting in {n}s…"`,
 * and at 0 `"Reconnecting…"`. §12 asks the same 28 dp line to say
 * `"Couldn't reach {host}:{port}."` while it retries an unreachable host, so
 * the first sentence is the §12 close message when there is one and §9's
 * `"Connection lost."` — a connection that was up and went away — when there
 * is not. The countdown is the only place the two have to share; at 0 the line
 * is §9's exactly.
 */
export const CONNECTION_LOST = "Connection lost.";

export function reconnectingStripText(secondsLeft: number, reason: string = CONNECTION_LOST): string {
  if (secondsLeft <= 0) return "Reconnecting…";
  const why = reason.trim().length > 0 ? reason.trim() : CONNECTION_LOST;
  return `${why} Reconnecting in ${secondsLeft}s…`;
}

/** §6.3: the body of the ongoing notification while the host is up, and while it is being re-dialled. */
export function connectedNotificationText(label: string): string {
  return `Connected to ${label}`;
}

export function reconnectingNotificationText(label: string): string {
  return `Reconnecting to ${label}`;
}

/** §7.2's backoff, so the strip can count the same seconds down. */
export function backoffSeconds(attempt: number): number {
  return Math.min(2 ** Math.max(0, attempt - 1), 30);
}
