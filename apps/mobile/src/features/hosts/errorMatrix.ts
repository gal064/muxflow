// The error matrix (design.md §12), in one place: every close reason and every
// handshake refusal maps to one row here — the exact copy, whether it belongs
// in the connection strip or on a full screen, whether the app retries, and
// which button the user gets. `sshTransport` stamps the copy onto the
// `TransportClose` it reports, and the connection chrome renders the row.

import type { TransportClose } from "../../protocol/Transport";
import type { ConnectionState } from "../../store/sessionStore";

/** §12: a row is either a line in the 28 dp strip or a full screen on top of it. */
export type ConnectionErrorPresentation = "strip" | "fullScreen";

export type ConnectionErrorActionKind = "sshKey" | "reconnect" | "forgetHostKey";

export interface ConnectionErrorAction {
  kind: ConnectionErrorActionKind;
  label: string;
}

export interface ConnectionErrorInfo {
  presentation: ConnectionErrorPresentation;
  message: string;
  action?: ConnectionErrorAction;
  /** §7.2: true when the app reconnects on the backoff schedule by itself. */
  retryable: boolean;
}

export interface HostAddress {
  host: string;
  port: number;
}

/** §12 "Helper missing" (exit code 127). */
export const HELPER_MISSING_EXIT_CODE = 127;

const SSH_KEY_ACTION: ConnectionErrorAction = { kind: "sshKey", label: "Your SSH key" };
const RECONNECT_ACTION: ConnectionErrorAction = { kind: "reconnect", label: "Reconnect now" };
const FORGET_HOST_KEY_ACTION: ConnectionErrorAction = { kind: "forgetHostKey", label: "Forget host key" };

function where(host: HostAddress | undefined): string {
  return host ? `${host.host}:${host.port}` : "the host";
}

function named(host: HostAddress | undefined): string {
  return host?.host ?? "The host";
}

/** The first line of whatever the helper printed on stderr, if anything. */
export function firstStderrLine(text: string | undefined): string | undefined {
  const line = (text ?? "")
    .split("\n")
    .map((candidate) => candidate.trim())
    .find((candidate) => candidate.length > 0);
  return line === undefined || line.length === 0 ? undefined : line;
}

/** One §12 row for a transport that closed. */
export function describeTransportClose(
  close: TransportClose,
  host?: HostAddress,
): ConnectionErrorInfo {
  switch (close.reason) {
    case "connectFailed":
      return { presentation: "strip", retryable: true, message: `Couldn't reach ${where(host)}.` };
    case "authFailed":
      return {
        presentation: "fullScreen",
        retryable: false,
        action: SSH_KEY_ACTION,
        message: `${named(host)} rejected this phone's SSH login. Over Tailscale SSH, check the tailnet's SSH policy; otherwise add the key under Your SSH key to ~/.ssh/authorized_keys on the host.`,
      };
    case "hostKeyMismatch":
      // §9.10's second paragraph.
      return {
        presentation: "fullScreen",
        retryable: false,
        action: FORGET_HOST_KEY_ACTION,
        message: `The host key for ${named(host)} has changed. This can mean the machine was reinstalled — or that something is intercepting the connection. Connection refused.`,
      };
    case "hostKeyNotTrusted":
      // §12 has no row: this is the user answering "Cancel" in the §9.10 dialog.
      return {
        presentation: "strip",
        retryable: false,
        message: `The host key for ${named(host)} wasn't trusted. Connection refused.`,
      };
    case "exited":
      if (close.exitCode === HELPER_MISSING_EXIT_CODE) {
        return {
          presentation: "fullScreen",
          retryable: false,
          message:
            "muxflow-host isn't installed on this host. Install it from the Muxflow desktop app (Settings → Connection).",
        };
      }
      return {
        presentation: "strip",
        retryable: true,
        message:
          firstStderrLine(close.message) ??
          `The helper exited${close.exitCode === undefined ? "" : ` (code ${close.exitCode})`}.`,
      };
    case "networkLost":
      return { presentation: "strip", retryable: true, message: "Connection lost." };
    case "localClose":
      return { presentation: "strip", retryable: false, message: "Disconnected." };
  }
}

export interface ConnectionFailureInput {
  state: ConnectionState;
  /** `connection.message` from the session store. */
  message: string | undefined;
  /** The last close this connection saw, when the failure came from the transport. */
  close?: TransportClose | undefined;
  host?: HostAddress | undefined;
}

/**
 * What the chrome shows for a `failed` / `incompatible` connection. An
 * `incompatible` host carries the §7.3 message and a Reconnect button; a
 * `failed` one is diagnosed from its last close, falling back to whatever
 * message the state machine recorded.
 */
export function describeConnectionFailure(input: ConnectionFailureInput): ConnectionErrorInfo {
  if (input.state === "incompatible") {
    return {
      presentation: "fullScreen",
      retryable: false,
      action: RECONNECT_ACTION,
      message: input.message ?? "This host's Muxflow helper is not compatible with this app.",
    };
  }
  // A `localClose` in a failed state is the state machine closing the transport
  // after it decided the failure itself (`HostConnection.fail`), so it says
  // nothing about why; the message it recorded does.
  const close = input.close?.reason === "localClose" ? undefined : input.close;
  if (close) return describeTransportClose(close, input.host);
  return {
    presentation: "strip",
    retryable: false,
    message: input.message ?? "The connection failed.",
  };
}
