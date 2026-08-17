export interface RemoteHelperProbe {
  operatingSystem: string;
  architecture: string;
  tmuxVersion: string;
  gitVersion: string;
  installed: boolean;
  helperVersion?: string;
  compatible: boolean;
  /** The host runs a newer helper than this app carries, so the app is the side that must move. */
  appOutdated?: boolean;
  /** The helper version this app ships, for explaining an `appOutdated` refusal. */
  expectedHelperVersion?: string;
  digest?: string;
  remotePath: string;
}

export interface HelperInstallReport {
  ok: boolean;
  message: string;
  rollback: "restored" | "failed" | "notNeeded";
}

export type HelperOperation = "install" | "upgrade";

export type HelperUpgradeState =
  | { phase: "idle" }
  | { phase: "probing"; connectionKey: string }
  | { phase: "ready"; connectionKey: string; probe: RemoteHelperProbe }
  | { phase: "confirming"; connectionKey: string; probe: RemoteHelperProbe; operation: HelperOperation }
  | { phase: "upgrading"; connectionKey: string; probe: RemoteHelperProbe; operation: HelperOperation }
  | { phase: "succeeded"; connectionKey: string; message: string; operation: HelperOperation }
  | { phase: "failed"; connectionKey: string; message: string; rollback: "restored" | "failed" | "notNeeded"; probe?: RemoteHelperProbe };

export type HelperUpgradeAction =
  | { type: "probe"; connectionKey: string }
  | { type: "probeSucceeded"; connectionKey: string; probe: RemoteHelperProbe }
  | { type: "probeFailed"; connectionKey: string; message: string }
  | { type: "requestUpgrade" }
  | { type: "cancelUpgrade" }
  | { type: "upgrade" }
  | { type: "upgradeSucceeded"; connectionKey: string; message: string }
  | { type: "upgradeFailed"; connectionKey: string; message: string; rollback: "restored" | "failed" | "notNeeded" }
  | { type: "reset" };

export const initialHelperUpgradeState: HelperUpgradeState = { phase: "idle" };

export function helperUpgradeReducer(state: HelperUpgradeState, action: HelperUpgradeAction): HelperUpgradeState {
  switch (action.type) {
    case "reset": return initialHelperUpgradeState;
    case "probe": return { phase: "probing", connectionKey: action.connectionKey };
    case "probeSucceeded": return state.phase === "probing" && state.connectionKey === action.connectionKey
      ? { phase: "ready", connectionKey: action.connectionKey, probe: action.probe } : state;
    case "probeFailed": return state.phase === "probing" && state.connectionKey === action.connectionKey
      ? { phase: "failed", connectionKey: action.connectionKey, message: action.message, rollback: "notNeeded" } : state;
    case "requestUpgrade": return state.phase === "ready" ? { phase: "confirming", connectionKey: state.connectionKey, probe: state.probe, operation: state.probe.installed ? "upgrade" : "install" } : state;
    case "cancelUpgrade": return state.phase === "confirming" ? { phase: "ready", connectionKey: state.connectionKey, probe: state.probe } : state;
    case "upgrade": return state.phase === "confirming" ? { ...state, phase: "upgrading" } : state;
    case "upgradeSucceeded": return state.phase === "upgrading" && state.connectionKey === action.connectionKey
      ? { phase: "succeeded", connectionKey: action.connectionKey, message: action.message, operation: state.operation } : state;
    case "upgradeFailed": {
      if (state.phase !== "upgrading" || state.connectionKey !== action.connectionKey) return state;
      return {
        phase: "failed",
        connectionKey: action.connectionKey,
        message: action.message,
        rollback: action.rollback,
        probe: state.probe,
      };
    }
  }
}

export function helperConnectionKey(connection: { mode: "local" } | { mode: "ssh"; profileId: string; target: string; configPath?: string }): string {
  return connection.mode === "local" ? "local" : `${connection.profileId}\0${connection.target}\0${connection.configPath ?? ""}`;
}
