import { invoke } from "@tauri-apps/api/core";
import { useCallback, type Dispatch, type SetStateAction } from "react";
import { helperConnectionKey, type HelperInstallReport, type HelperUpgradeAction, type HelperUpgradeState } from "../features/shell/helperUpgrade";
import { profileIdForSshConnection } from "../features/shell/hostProfiles";
import type { HostScopeToken } from "../features/shell/hostScope";
import type { ConnectionSpec, HostProfile, PersistedProfiles } from "./types";

interface HostSettingsActionsOptions {
  /** Makes a saved host the one on screen; see `useAppConnectionController`. */
  activateHost(profileId: string): void;
  clearActiveSelection(): void;
  connection: ConnectionSpec;
  connectionMode: ConnectionSpec["mode"];
  currentScope: HostScopeToken;
  dispatchHelper: Dispatch<HelperUpgradeAction>;
  helperState: HelperUpgradeState;
  probeHelper(): void;
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

  /**
   * Saves the host and makes it the one a restart reconnects to. Saving alone
   * no longer moves that pointer: hosts are saved to be shown beside the
   * active one as often as to become it.
   */
  const saveActiveProfile = useCallback((profile: HostProfile) => invoke("save_host_profile", { profile })
    .then(() => invoke("set_last_profile_id", { profileId: profile.id }))
    .catch((error) => options.setStatus(String(error))), [options]);

  const connect = useCallback(() => {
    options.resetHost();
    options.clearActiveSelection();
    if (options.connectionMode === "local") {
      const profile: HostProfile = {
        ...options.profiles.find((item) => item.id === "local"),
        id: "local", label: "Local", connection: { mode: "local" }, shown: true,
      };
      options.setConnection(profile.connection);
      options.setSelectedProfileId(profile.id);
      options.setConnectionEpoch((value) => value + 1);
      options.setProfiles((current) => current.some((item) => item.id === profile.id)
        ? current.map((item) => item.id === profile.id ? profile : item)
        : [profile, ...current]);
      void saveActiveProfile(profile);
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
    const profile: HostProfile = { ...edited, id: profileId, label: target, connection, shown: true };
    options.setConnection(connection);
    options.setConnectionEpoch((value) => value + 1);
    options.setSelectedProfileId(profile.id);
    // An edit keeps its place in the list, exactly as the store keeps it: the
    // picker is a list of machines, and a machine that jumped to the bottom
    // every time its address was corrected would read as a different one.
    options.setProfiles((current) => edited
      ? current.map((item) => item.id === profile.id ? profile : item)
      : [...current.filter((item) => item.id !== profile.id), profile]);
    void saveActiveProfile(profile);
    options.setStatus(`Connecting to ${target}…`);
  }, [options, saveActiveProfile]);

  /**
   * Moves the app onto a saved host and shows it in the form. The host's own
   * link keeps whatever it already holds — a host shown beside the active one
   * has a live bridge, and switching to it must neither restart that bridge
   * nor wipe what it has said.
   */
  const switchHostProfile = useCallback((profile: HostProfile) => {
    options.activateHost(profile.id);
    options.setConnectionMode(profile.connection.mode);
    if (profile.connection.mode === "ssh") {
      options.setSshTarget(profile.connection.target);
      options.setSshConfigPath(profile.connection.configPath ?? "");
    }
  }, [options]);

  return {
    confirmHelperInstall, connect, deleteSavedProfile, probeHelper: options.probeHelper, selectProfile, switchHostProfile,
  };
}
