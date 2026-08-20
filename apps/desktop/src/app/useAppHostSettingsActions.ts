import { invoke } from "@tauri-apps/api/core";
import { useCallback, type Dispatch, type SetStateAction } from "react";
import {
  helperConnectionKey,
  type HelperInstallReport,
  type HelperUpgradeAction,
  type HelperUpgradeState,
  type RemoteHelperProbe,
} from "../features/shell/helperUpgrade";
import { profileIdForSshConnection } from "../features/shell/hostProfiles";
import type { HostScopeToken } from "../features/shell/hostScope";
import type { ConnectionSpec, HostProfile, PersistedProfiles } from "./types";

interface HostSettingsActionsOptions {
  clearActiveSelection(): void;
  connection: ConnectionSpec;
  connectionMode: ConnectionSpec["mode"];
  currentScope: HostScopeToken;
  dispatchHelper: Dispatch<HelperUpgradeAction>;
  helperState: HelperUpgradeState;
  profiles: readonly HostProfile[];
  resetHost(): void;
  scopeIsCurrent(scope: HostScopeToken): boolean;
  /** The saved host the form is editing; empty is the new-host form. */
  selectedProfileId: string;
  setConnection: Dispatch<SetStateAction<ConnectionSpec>>;
  setConnectionDetail: Dispatch<SetStateAction<string>>;
  setConnectionEpoch: Dispatch<SetStateAction<number>>;
  setConnectionMode: Dispatch<SetStateAction<ConnectionSpec["mode"]>>;
  setProfiles: Dispatch<SetStateAction<HostProfile[]>>;
  setSelectedProfileId: Dispatch<SetStateAction<string>>;
  setSshConfigPath: Dispatch<SetStateAction<string>>;
  setSshTarget: Dispatch<SetStateAction<string>>;
  setStatus(message: string): void;
  sshConfigPath: string;
  sshTarget: string;
}

/** Owns Settings-driven connection/profile/helper mutations. */
export function useAppHostSettingsActions(options: HostSettingsActionsOptions) {
  const deleteSavedProfile = useCallback((profile: HostProfile) => {
    void invoke<PersistedProfiles>("delete_host_profile", { profileId: profile.id }).then((saved) => {
      options.setProfiles(saved.profiles);
      options.setSelectedProfileId("");
      options.setStatus(`Deleted the saved host ${profile.label}.`);
    }).catch((error) => options.setStatus(
      `Could not delete the saved host ${profile.label}: ${String(error)}`,
    ));
  }, [options]);

  const selectProfile = useCallback((profile: HostProfile | undefined) => {
    options.setSelectedProfileId(profile?.id ?? "");
    if (!profile) return;
    options.setConnectionMode(profile.connection.mode);
    if (profile.connection.mode === "ssh") {
      options.setSshTarget(profile.connection.target);
      options.setSshConfigPath(profile.connection.configPath ?? "");
    }
  }, [options]);

  const probeHelper = useCallback(async () => {
    if (options.connection.mode !== "ssh") return;
    const scope = options.currentScope;
    const connectionKey = helperConnectionKey(options.connection);
    options.dispatchHelper({ type: "probe", connectionKey });
    try {
      const probe = await invoke<RemoteHelperProbe>("probe_remote_helper", { connection: options.connection });
      if (!options.scopeIsCurrent(scope)) return;
      options.dispatchHelper({ type: "probeSucceeded", connectionKey, probe });
    } catch (error) {
      if (!options.scopeIsCurrent(scope)) return;
      options.dispatchHelper({ type: "probeFailed", connectionKey, message: String(error) });
    }
  }, [options]);

  const confirmHelperInstall = useCallback(async () => {
    const { connection, helperState } = options;
    if (connection.mode !== "ssh" || helperState.phase !== "confirming"
      || helperState.connectionKey !== helperConnectionKey(connection)) return;
    const connectionKey = helperState.connectionKey;
    const scope = options.currentScope;
    options.dispatchHelper({ type: "upgrade" });
    try {
      const report = await invoke<HelperInstallReport>("install_remote_helper", {
        connection,
        allowUpgrade: helperState.probe.installed,
      });
      if (!options.scopeIsCurrent(scope)) return;
      if (!report.ok) {
        options.dispatchHelper({
          type: "upgradeFailed", connectionKey, message: report.message, rollback: report.rollback,
        });
        return;
      }
      options.dispatchHelper({ type: "upgradeSucceeded", connectionKey, message: report.message });
      options.setConnectionDetail(
        `Remote helper ${helperState.probe.installed ? "upgraded" : "installed"}; reconnecting for a fresh authoritative snapshot.`,
      );
      options.setConnectionEpoch((value) => value + 1);
    } catch (error) {
      if (!options.scopeIsCurrent(scope)) return;
      options.dispatchHelper({
        type: "upgradeFailed", connectionKey, message: String(error), rollback: "notNeeded",
      });
    }
  }, [options]);

  const connect = useCallback(() => {
    options.resetHost();
    options.clearActiveSelection();
    if (options.connectionMode === "local") {
      const profile: HostProfile = { id: "local", label: "Local", connection: { mode: "local" } };
      options.setConnection(profile.connection);
      options.setSelectedProfileId(profile.id);
      options.setConnectionEpoch((value) => value + 1);
      void invoke("save_host_profile", { profile });
      options.setStatus("Discovering local tmux…");
      return;
    }
    const target = options.sshTarget.trim();
    if (!target) return options.setStatus("Enter an SSH host or config alias.");
    const configPath = options.sshConfigPath.trim();
    /**
     * The saved host being edited, if the picker is showing one.
     *
     * Connect used to derive the id from the form values alone, so correcting
     * one machine's address — a renamed alias, a moved config — connected to
     * the corrected host and left the original sitting in the list beside it,
     * as a second entry for the same machine that the user never asked for.
     * Keeping the picked id makes the same edit an edit: the backend upserts on
     * it, so the saved host moves with the form. A new host is the other
     * branch, where deriving the id is still right — and `profileIdForSsh-
     * Connection` is what stops two of *those* from being saved twice.
     */
    const edited = options.profiles.find((profile) => profile.id === options.selectedProfileId
      && profile.connection.mode === "ssh");
    const profileId = edited?.id ?? profileIdForSshConnection(options.profiles, target, configPath);
    const connection: ConnectionSpec = {
      mode: "ssh", profileId, target, ...(configPath ? { configPath } : {}),
    };
    const profile: HostProfile = { id: profileId, label: target, connection };
    options.setConnection(connection);
    options.setConnectionEpoch((value) => value + 1);
    options.setSelectedProfileId(profile.id);
    // An edit keeps its place in the list, exactly as the store keeps it: the
    // picker is a list of machines, and a machine that jumped to the bottom
    // every time its address was corrected would read as a different one.
    options.setProfiles((current) => edited
      ? current.map((item) => item.id === profile.id ? profile : item)
      : [...current.filter((item) => item.id !== profile.id), profile]);
    void invoke("save_host_profile", { profile }).catch((error) => options.setStatus(String(error)));
    options.setStatus(`Connecting to ${target}…`);
  }, [options]);

  const switchHostProfile = useCallback((profile: HostProfile) => {
    const connection: ConnectionSpec = profile.connection.mode === "ssh"
      ? { ...profile.connection, profileId: profile.connection.profileId || profile.id }
      : profile.connection;
    options.resetHost();
    options.clearActiveSelection();
    options.setConnection(connection);
    options.setConnectionMode(connection.mode);
    if (connection.mode === "ssh") {
      options.setSshTarget(connection.target);
      options.setSshConfigPath(connection.configPath ?? "");
    }
  }, [options]);

  return {
    confirmHelperInstall, connect, deleteSavedProfile, probeHelper, selectProfile, switchHostProfile,
  };
}
