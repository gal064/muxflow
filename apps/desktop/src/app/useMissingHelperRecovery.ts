import { invoke } from "@tauri-apps/api/core";
import { useCallback, useRef, type Dispatch, type SetStateAction } from "react";
import { helperConnectionKey, type HelperUpgradeAction, type RemoteHelperProbe } from "../features/shell/helperUpgrade";
import type { ConnectionSpec } from "./types";

interface MissingHelperRecoveryOptions {
  connection: ConnectionSpec;
  connectionEpoch: number;
  dispatchHelper: Dispatch<HelperUpgradeAction>;
  setConnectionDetail: Dispatch<SetStateAction<string>>;
}

/**
 * Turns "this host has no helper" from a dead end into the install flow.
 *
 * Connecting to an SSH host that has never been set up produced one line in the
 * disconnected strip — `bash: …/muxflow-host: No such file or directory`,
 * reported as a handshake failure — and stopped there. Everything needed to fix
 * it already existed behind Settings, so the first-run path was "read a shell
 * error, guess that a helper is a thing, find the panel". The probe is asked
 * here instead, and its answer drives the same consent dialog the Settings
 * button drives, with the same install and the same reconnect behind it.
 *
 * What this deliberately does *not* do is decide anything on a probe it could
 * not complete. An unreachable host, a refused key or a password prompt all
 * fail the probe too, and the original connection error is the honest thing to
 * leave on screen for those.
 */
export function useMissingHelperRecovery(options: MissingHelperRecoveryOptions) {
  const optionsRef = useRef(options);
  optionsRef.current = options;
  /**
   * The connection *and* the epoch, because the bridge supervisor retries in a
   * loop: one unreachable helper produces a handshake error per attempt, and
   * without this every one of them would open an SSH connection of its own to
   * ask the same question. A reconnect — including the one the install itself
   * triggers — is a new epoch and therefore a new chance to ask.
   */
  const attempted = useRef<string>(undefined);
  const scopeKey = `${helperConnectionKey(options.connection)}:${options.connectionEpoch}`;
  const scopeKeyRef = useRef(scopeKey);
  scopeKeyRef.current = scopeKey;

  return useCallback((connection: ConnectionSpec) => {
    if (connection.mode !== "ssh") return;
    const connectionKey = helperConnectionKey(connection);
    const key = `${connectionKey}:${optionsRef.current.connectionEpoch}`;
    if (attempted.current === key) return;
    attempted.current = key;
    void invoke<RemoteHelperProbe>("probe_remote_helper", { connection }).then((probe) => {
      // The probe is a round trip over SSH; a host switch or a reconnect in
      // that window makes its answer about a machine this app has left, and
      // raising an install dialog for that machine is the M13-E004 shape.
      if (scopeKeyRef.current !== key) return;
      if (!probe.installed || (!probe.compatible && !probe.appOutdated)) {
        // Straight through the same reducer path the Settings button walks, so
        // the confirmation, the install, the rollback and the reconnect after
        // it are one implementation rather than two.
        optionsRef.current.dispatchHelper({ type: "probe", connectionKey });
        optionsRef.current.dispatchHelper({ type: "probeSucceeded", connectionKey, probe });
        optionsRef.current.dispatchHelper({ type: "requestUpgrade" });
        return;
      }
      if (!probe.compatible && probe.appOutdated) {
        // Nothing to offer: installing from here would replace the host's newer
        // helper with this app's older one. Said in the strip in the same words
        // Settings uses, because it is the same refusal.
        optionsRef.current.setConnectionDetail(
          `This host runs a newer helper (${probe.helperVersion}) than this app expects (${probe.expectedHelperVersion}). Update the app — installing from here would downgrade the host.`,
        );
      }
    }).catch(() => {
      // Deliberately silent. The connection error that brought us here is
      // already on screen and is the better description of an unreachable host.
    });
  }, []);
}
