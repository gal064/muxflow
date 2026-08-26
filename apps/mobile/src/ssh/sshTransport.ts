// A `Transport` (src/protocol/Transport.ts) over the `muxflow-ssh` native
// module: one exec channel running `muxflow-host bridge --stdio`, one
// `connectionId` per lane (§11.1 opens a second one for file bodies).
//
// The remote command is the desktop's, byte for byte — see
// `apps/desktop/src-tauri/src/connection/transport.rs`, which passes
// `$HOME/.local/bin/muxflow-host bridge --stdio` to ssh as a single argument
// and lets the login shell expand `$HOME`. sshj's `exec` hands the string to
// sshd the same way, so no `sh -lc` wrapper is added here.

import { base64ToBytes, bytesToBase64 } from "./base64";
import { describeTransportClose, firstStderrLine, type HostAddress } from "../features/hosts/errorMatrix";
import { muxflowSsh, type MuxflowSsh, type SshCloseReason, type SshEvent, type SshTarget } from "./MuxflowSsh";
import { TransportDialError, type Transport, type TransportClose, type TransportCloseReason } from "../protocol/Transport";

/** Verbatim from the desktop's SSH bridge command. */
export const BRIDGE_COMMAND = "$HOME/.local/bin/muxflow-host bridge --stdio";

/** What the §9.10 dialog is asked to decide. */
export interface HostKeyPrompt {
  connectionId: string;
  target: SshTarget;
  algorithm: string;
  fingerprintSha256: string;
}

export interface SshTransportOptions {
  connectionId: string;
  target: SshTarget;
  /** The fingerprint this host is already pinned to, or null for trust-on-first-use. */
  trustedHostKeyFingerprint: string | null;
  /** Defaults to the app-wide facade. */
  ssh?: MuxflowSsh;
  command?: string;
  /**
   * Resolves true to trust the presented key (§9.10 "Trust"), false to refuse
   * it. Absent means "never trust", which is what a lane that should have
   * inherited a pin wants.
   */
  onHostKey?: (prompt: HostKeyPrompt) => Promise<boolean>;
  /** Every close, including the one that fails the dial, with §12 copy attached. */
  onClose?: (close: TransportClose) => void;
  /** Used only for the §12 copy; carries no port when absent. */
  hostAddress?: HostAddress;
  log?: (line: string) => void;
}

const CLOSE_REASONS: Record<SshCloseReason, TransportCloseReason> = {
  hostKeyNotTrusted: "hostKeyNotTrusted",
  hostKeyMismatch: "hostKeyMismatch",
  authFailed: "authFailed",
  connectFailed: "connectFailed",
  exited: "exited",
  closedByClient: "localClose",
  networkLost: "networkLost",
};

/**
 * Opens one bridge channel. Resolves once the remote command is running
 * (`connected`), rejects with a `TransportDialError` carrying the §12 row when
 * the channel closes first.
 */
export function openSshTransport(options: SshTransportOptions): Promise<Transport> {
  const ssh = options.ssh ?? muxflowSsh();
  const { connectionId, target } = options;
  const command = options.command ?? BRIDGE_COMMAND;
  const log = (line: string): void => options.log?.(`[muxflow] ssh.${connectionId} ${line}`);

  let dataListener: ((chunk: Uint8Array) => void) | undefined;
  let closedListener: ((close: TransportClose) => void) | undefined;
  /** stdout that arrived before the protocol client attached its listener. */
  let pendingData: Uint8Array[] = [];
  let pendingClose: TransportClose | undefined;
  let firstStderr: string | undefined;
  let connected = false;
  let settled = false;
  let closedByUs = false;
  let refusedHostKey = false;
  let unsubscribe = (): void => {};

  return new Promise<Transport>((resolve, reject) => {
    const finish = (raw: { reason: SshCloseReason; exitCode: number | null }): void => {
      unsubscribe();
      // The native module reports a host key the user refused as
      // `closedByClient`, because refusing it means closing the channel; the
      // JS side is the one that knows why, so it names the reason (§12).
      const reason: TransportCloseReason = refusedHostKey
        ? "hostKeyNotTrusted"
        : closedByUs
          ? "localClose"
          : CLOSE_REASONS[raw.reason];
      const base: TransportClose = {
        reason,
        ...(raw.exitCode === null ? {} : { exitCode: raw.exitCode }),
        ...(firstStderr === undefined ? {} : { message: firstStderr }),
      };
      // Every close carries the §12 copy for the row it lands in.
      const close: TransportClose = {
        ...base,
        message: describeTransportClose(base, options.hostAddress).message,
      };
      log(`closed reason=${close.reason} exit=${String(raw.exitCode)}`);
      options.onClose?.(close);
      if (!settled) {
        settled = true;
        reject(new TransportDialError(close));
        return;
      }
      if (closedListener) closedListener(close);
      else pendingClose = close;
    };

    unsubscribe = ssh.addListener((event: SshEvent) => {
      if (event.connectionId !== connectionId) return;
      switch (event.type) {
        case "hostKey": {
          log(`hostKey ${event.algorithm} ${event.fingerprintSha256}`);
          const decide = options.onHostKey?.({
            connectionId,
            target,
            algorithm: event.algorithm,
            fingerprintSha256: event.fingerprintSha256,
          }) ?? Promise.resolve(false);
          void decide.then(
            (trusted) => {
              if (settled && !connected) return; // the channel is already gone
              if (!trusted) {
                refusedHostKey = true;
                // Releases the verifier: the native side stops waiting for a
                // decision once nothing is left that would use the answer.
                void ssh.close(connectionId).catch(() => undefined);
                return;
              }
              ssh.trustHostKey(connectionId, event.fingerprintSha256).catch((error: unknown) => {
                log(`trustHostKey.failed ${describe(error)}`);
              });
            },
            (error: unknown) => {
              log(`hostKey.decision.failed ${describe(error)}`);
              refusedHostKey = true;
              void ssh.close(connectionId).catch(() => undefined);
            },
          );
          return;
        }
        case "connected":
          if (settled) return;
          connected = true;
          settled = true;
          log("connected");
          resolve(transport);
          return;
        case "data": {
          const bytes = base64ToBytes(event.base64);
          if (bytes.length === 0) return;
          if (dataListener) dataListener(bytes);
          else pendingData.push(bytes);
          return;
        }
        case "stderr":
          firstStderr ??= firstStderrLine(event.text);
          log(`stderr ${event.text.trim()}`);
          return;
        case "closed":
          finish(event);
      }
    });

    const transport: Transport = {
      write(bytes) {
        if (bytes.length === 0) return;
        ssh.write(connectionId, bytesToBase64(bytes)).catch((error: unknown) => {
          // A failed write means the channel is going away; its `closed` event
          // is what drives the state machine, so this only needs a log line.
          log(`write.failed ${describe(error)}`);
        });
      },
      onData(listener) {
        dataListener = listener;
        const buffered = pendingData;
        pendingData = [];
        for (const chunk of buffered) listener(chunk);
      },
      onClosed(listener) {
        closedListener = listener;
        if (pendingClose) {
          const close = pendingClose;
          pendingClose = undefined;
          listener(close);
        }
      },
      close() {
        if (closedByUs) return;
        closedByUs = true;
        ssh.close(connectionId).catch((error: unknown) => log(`close.failed ${describe(error)}`));
      },
    };

    ssh.connect(connectionId, target, command, options.trustedHostKeyFingerprint).catch((error: unknown) => {
      if (settled) return;
      settled = true;
      unsubscribe();
      const close: TransportClose = {
        reason: "connectFailed",
        message: describeTransportClose({ reason: "connectFailed" }, options.hostAddress).message,
      };
      log(`connect.failed ${describe(error)}`);
      options.onClose?.(close);
      reject(new TransportDialError(close));
    });
  });
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
