export interface HostScopeToken {
  hostProfileId: string;
  connectionKey: string;
  connectionEpoch: number;
  serverIdentity?: string;
  generation: number;
}

export function sameHostScope(left: HostScopeToken, right: HostScopeToken): boolean {
  return left.hostProfileId === right.hostProfileId
    && left.connectionKey === right.connectionKey
    && left.connectionEpoch === right.connectionEpoch
    && left.serverIdentity === right.serverIdentity
    && left.generation === right.generation;
}

/**
 * Dialogs may remain open while an unrelated tmux topology event advances the
 * generation. Keep the user's input only while the durable connection is the
 * same, then let the submitted action use the latest authoritative generation.
 * A profile switch, reconnect, helper/server restart, or tmux server identity
 * change still invalidates the dialog.
 */
export function sameHostConnection(left: HostScopeToken, right: HostScopeToken): boolean {
  return left.hostProfileId === right.hostProfileId
    && left.connectionKey === right.connectionKey
    && left.connectionEpoch === right.connectionEpoch
    && left.serverIdentity === right.serverIdentity;
}

/**
 * A first helper install begins before the bridge can report a server identity.
 * Let that one missing fact become known without treating the authorized host
 * as a different connection; every durable fact that existed at consent time
 * still has to match.
 */
export function sameHelperInstallConnection(left: HostScopeToken, right: HostScopeToken): boolean {
  return left.hostProfileId === right.hostProfileId
    && left.connectionKey === right.connectionKey
    && left.connectionEpoch === right.connectionEpoch
    && (left.serverIdentity === undefined || left.serverIdentity === right.serverIdentity);
}

/** Stable identity for work that may outlive topology-generation changes. */
export function hostConnectionKey(scope: HostScopeToken): string {
  return [scope.hostProfileId, scope.connectionKey, scope.connectionEpoch, scope.serverIdentity ?? ""].join("\0");
}
