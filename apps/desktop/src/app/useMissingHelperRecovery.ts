import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, type Dispatch } from "react";
import {
  helperConnectionKey,
  type HelperUpgradeAction,
  type RemoteHelperProbe,
} from "../features/shell/helperUpgrade";
import type { TerminalEvent } from "../features/terminal/api";
import type { ConnectionSpec } from "./types";

interface RemoteHelperRecoveryOptions {
  connection: ConnectionSpec;
  connectionEpoch: number;
  dispatchHelper: Dispatch<HelperUpgradeAction>;
}

/**
 * Keeps the helper on an SSH host aligned with the one this desktop ships.
 *
 * A failed handshake may mean the helper is absent or incompatible. A successful
 * handshake proves only the protocol contract, not that the remote executable
 * has the same bytes as the packaged helper. Both paths ask the digest-aware
 * probe and drive the same consent dialog the Settings button drives, with the
 * same verified install, rollback and reconnect behind it.
 *
 * What this deliberately does *not* do is decide anything on a probe it could
 * not complete. An unreachable host, a refused key or a password prompt all
 * fail the probe too, and the original connection error is the honest thing to
 * leave on screen for those.
 */
export function useRemoteHelperRecovery(options: RemoteHelperRecoveryOptions) {
  const optionsRef = useRef(options);
  optionsRef.current = options;
  /**
   * The connection *and* the epoch, because the bridge supervisor retries in a
   * loop: one unreachable helper produces a handshake error per attempt, and
   * without this every one of them would open an SSH connection of its own to
   * ask the same question. An explicit reconnect — including the one an install
   * triggers — gets a new epoch; native in-place reconnects are re-armed from
   * their connection-state events below.
   */
  const attempted = useRef<{ connected?: string; handshakeFailure?: string }>({});
  const probeGeneration = useRef(0);
  const activeProbe = useRef<{
    connectionKey: string;
    generation: number;
    key: string;
    requestUpgrade: boolean;
    survivesTransportDrop: boolean;
  } | undefined>(undefined);
  const settledConnectionProbeKey = useRef<string | undefined>(undefined);
  const scopeKey = `${helperConnectionKey(options.connection)}:${options.connectionEpoch}`;
  const attemptScopeKey = useRef(scopeKey);
  if (attemptScopeKey.current !== scopeKey) {
    attemptScopeKey.current = scopeKey;
    attempted.current = {};
  }
  const scopeKeyRef = useRef(scopeKey);
  scopeKeyRef.current = scopeKey;
  useEffect(() => {
    settledConnectionProbeKey.current = undefined;
    const active = activeProbe.current;
    if (!active || active.key === scopeKey) return;
    activeProbe.current = undefined;
    probeGeneration.current += 1;
    options.dispatchHelper({ type: "abandonProbe", connectionKey: active.connectionKey });
  }, [options.dispatchHelper, scopeKey]);

  const probe = useCallback((
    connection: ConnectionSpec,
    reason: keyof typeof attempted.current | "manual",
    requestUpgrade: boolean,
  ) => {
    if (connection.mode !== "ssh") return;
    const connectionKey = helperConnectionKey(connection);
    const key = `${connectionKey}:${optionsRef.current.connectionEpoch}`;
    if (scopeKeyRef.current !== key) return;
    if (reason !== "manual") {
      if (attempted.current[reason] === key) return;
      attempted.current[reason] = key;
    }
    const active = activeProbe.current;
    if (active?.key === key) {
      const transportSupersedesRecovery = reason === "connected" && active.survivesTransportDrop;
      const recoverySupersedesTransport = reason === "handshakeFailure" && !active.survivesTransportDrop;
      if (transportSupersedesRecovery || recoverySupersedesTransport) {
        // Failed and settled transports are different observations. Whichever
        // lifecycle fact arrived second gets its own probe rather than reusing
        // an answer sampled for the transport it replaced.
        activeProbe.current = undefined;
        probeGeneration.current += 1;
      } else {
        // One SSH round trip can answer every caller. Preserve the strongest
        // intent so a Settings click cannot turn an automatic reconciliation
        // into an informational-only result while the first probe is pending.
        active.requestUpgrade ||= requestUpgrade;
        // A request associated with any live transport is invalid once that
        // transport drops. Manual callers may strengthen intent, never age.
        active.survivesTransportDrop &&= reason !== "connected";
        return;
      }
    }
    const generation = ++probeGeneration.current;
    const request = {
      connectionKey,
      generation,
      key,
      requestUpgrade,
      survivesTransportDrop: reason !== "connected",
    };
    activeProbe.current = request;
    settledConnectionProbeKey.current = undefined;
    // Begin before the round trip. On the successful-connection path this is
    // also the arbitration signal that keeps the separate agent-setup question
    // from opening underneath the helper confirmation.
    optionsRef.current.dispatchHelper({ type: "probe", connectionKey });
    void invoke<RemoteHelperProbe>("probe_remote_helper", { connection }).then((probe) => {
      // The probe is a round trip over SSH; a host switch or a reconnect in
      // that window makes its answer about a machine this app has left, and
      // raising an install dialog for that machine is the M13-E004 shape.
      if (scopeKeyRef.current !== key || probeGeneration.current !== generation
        || activeProbe.current !== request) return;
      activeProbe.current = undefined;
      settledConnectionProbeKey.current = request.survivesTransportDrop ? undefined : key;
      optionsRef.current.dispatchHelper({ type: "probeSucceeded", connectionKey, probe });
      if (request.requestUpgrade && (!probe.installed || !probe.compatible)) {
        // Straight through the same reducer path the Settings button walks, so
        // the confirmation, the install, the rollback and the reconnect after
        // it are one implementation rather than two.
        optionsRef.current.dispatchHelper({ type: "requestUpgrade" });
        return;
      }
    }).catch((error) => {
      if (scopeKeyRef.current !== key || probeGeneration.current !== generation
        || activeProbe.current !== request) return;
      activeProbe.current = undefined;
      settledConnectionProbeKey.current = request.survivesTransportDrop ? undefined : key;
      optionsRef.current.dispatchHelper({ type: "probeFailed", connectionKey, message: String(error) });
      // No connection detail here. On a failed handshake its original error is
      // more useful; on a live connection this secondary check must not make a
      // healthy terminal read as disconnected. Settings retains the failure.
    });
  }, []);

  const onConnectionStateChanged = useCallback((
    connection: ConnectionSpec,
    state: Extract<TerminalEvent, { kind: "connectionState" }>["state"],
  ) => {
    if (connection.mode !== "ssh") return;
    if (state === "connected") {
      probe(connection, "connected", true);
      return;
    }
    if (state === "resyncing") return;
    const connectionKey = helperConnectionKey(connection);
    const key = `${connectionKey}:${optionsRef.current.connectionEpoch}`;
    if (scopeKeyRef.current !== key) return;
    if (attempted.current.connected === key) attempted.current.connected = undefined;
    const active = activeProbe.current;
    if (active?.key === key && !active.survivesTransportDrop) {
      activeProbe.current = undefined;
      probeGeneration.current += 1;
      optionsRef.current.dispatchHelper({ type: "abandonProbe", connectionKey });
    }
    if (settledConnectionProbeKey.current === key) {
      settledConnectionProbeKey.current = undefined;
      optionsRef.current.dispatchHelper({ type: "invalidateConnectionProbe", connectionKey });
    }
  }, [probe]);

  return {
    onConnectionStateChanged,
    onHandshakeFailure: useCallback((connection: ConnectionSpec) => probe(connection, "handshakeFailure", true), [probe]),
    probeManually: useCallback(() => probe(optionsRef.current.connection, "manual", false), [probe]),
  };
}
