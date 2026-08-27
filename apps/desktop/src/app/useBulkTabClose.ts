import { useCallback, type MutableRefObject } from "react";
import { editorFlushRegistry } from "../features/files/editorFlushRegistry";
import { sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";
import {
  agentPresenceIsCurrent,
  bulkCloseCompleteStatus,
  bulkCloseOutcomeStatus,
  tabsEligibleAtBulkCloseCommit,
  type AgentPresenceSnapshot,
  type CombinedTab,
} from "../features/shell/model";
import type { AppOwnedTab } from "../features/shell/types";
import type { AuthoritativePrecondition, TmuxAction, TmuxActionResult } from "../features/tmux/actions";
import type { TmuxSnapshot } from "./types";

interface BulkTabCloseOptions {
  agentPresenceRef: MutableRefObject<AgentPresenceSnapshot>;
  closeAppTab(tab: AppOwnedTab, scope: HostScopeToken): void;
  hostScopeRef: MutableRefObject<HostScopeToken>;
  performAction(
    action: TmuxAction,
    precondition?: AuthoritativePrecondition,
  ): Promise<TmuxActionResult | undefined>;
  setStatus(status: string): void;
  snapshotRef: MutableRefObject<TmuxSnapshot>;
  workspaceAppTabs: readonly AppOwnedTab[];
}

/**
 * Closes a settled set of tabs after its one confirmation.
 *
 * Saving happens once, before any mutation. Terminal closes use an identity-only
 * precondition and re-check protected agents immediately before each destructive
 * request.
 */
export function useBulkTabClose(options: BulkTabCloseOptions) {
  const {
    agentPresenceRef, closeAppTab, hostScopeRef, performAction, setStatus, snapshotRef, workspaceAppTabs,
  } = options;
  return useCallback(async (
    tabs: readonly CombinedTab[],
    scope: HostScopeToken,
    protectAgents: boolean,
  ): Promise<void> => {
    if (!sameHostConnection(scope, hostScopeRef.current)) {
      setStatus("Closing those tabs was cancelled because its host scope changed.");
      return;
    }
    try {
      await editorFlushRegistry.flushAll();
    } catch (error) {
      if (sameHostConnection(scope, hostScopeRef.current)) {
        setStatus(`Could not close those tabs because an editor did not save: ${String(error)}`);
      }
      return;
    }
    // The connection, not the topology generation: an agent animating its
    // title advances the generation several times a second, and a set captured
    // before a confirmation would never survive to the commit.
    if (!sameHostConnection(scope, hostScopeRef.current) || !scope.serverIdentity) {
      setStatus("Closing those tabs was cancelled because the terminal layout changed.");
      return;
    }

    const terminalTabs = tabs.filter(
      (tab): tab is Extract<CombinedTab, { kind: "terminal" }> => tab.kind === "terminal",
    );
    let closed = 0;
    let failed = 0;
    let skipped = 0;
    for (const [index, tab] of terminalTabs.entries()) {
      // The first close can take long enough for a newly detected agent to
      // protect a later tab in the same batch. A tab held back this way is
      // neither a success nor a failure, and counting it as neither is what
      // made the whole close silently do nothing.
      if (tabsEligibleAtBulkCloseCommit(
        [tab], protectAgents, agentPresenceRef.current,
      ).length === 0) {
        skipped += 1;
        continue;
      }
      const terminalWindow = snapshotRef.current.windows.find((item) => item.id === tab.id);
      if (!terminalWindow) continue;

      // Every close advances the generation once for dispatch and again for the
      // tmux notification. Guard the server identity but not that moving
      // generation, or every close after the first can be rejected as stale.
      const result = await performAction({
        kind: "closeWindow",
        sessionId: terminalWindow.sessionId,
        windowId: terminalWindow.id,
        confirmed: true,
      }, { serverIdentity: scope.serverIdentity, generation: 0 });
      if (!sameHostConnection(scope, hostScopeRef.current)) return;
      if (!result) {
        failed += 1;
        continue;
      }
      closed += 1;

      if (protectAgents && index < terminalTabs.length - 1) {
        // Absence in the previous agent snapshot proves nothing after tmux has
        // advanced: another client may have split an agent into a survivor.
        const deadline = Date.now() + 2_000;
        while (sameHostConnection(scope, hostScopeRef.current)
          && !agentPresenceIsCurrent(agentPresenceRef.current, result.topologyGeneration)
          && Date.now() < deadline) {
          await new Promise((resolve) => globalThis.setTimeout(resolve, 16));
        }
        if (!sameHostConnection(scope, hostScopeRef.current)) return;
        if (!agentPresenceIsCurrent(agentPresenceRef.current, result.topologyGeneration)) {
          setStatus(
            "Stopped closing tabs because current agent status was unavailable; remaining terminals were left open.",
          );
          return;
        }
      }
    }

    const outcome = bulkCloseOutcomeStatus(closed, failed, skipped);
    if (outcome) setStatus(outcome);
    // A survivor of either kind leaves the set incomplete, and closing the
    // editors of a set that kept its terminals is the half-close nobody asked
    // for.
    if (failed > 0 || skipped > 0) return;

    // Local document tabs are lossless after the shared editor flush, but
    // commit them only once the stale-sensitive terminal transaction has been
    // admitted. A rejected first terminal close must leave the set untouched.
    if (!sameHostConnection(scope, hostScopeRef.current)) return;
    let closedAppTabs = 0;
    for (const tab of tabs) {
      if (tab.kind !== "app") continue;
      const appTab = workspaceAppTabs.find((item) => item.id === tab.id);
      if (!appTab) continue;
      closeAppTab(appTab, scope);
      closedAppTabs += 1;
    }

    // The receipt for a close that asked nothing first. Emitted last, once the
    // whole set is actually gone, so the number is what happened rather than
    // what was attempted.
    const complete = bulkCloseCompleteStatus(closed + closedAppTabs);
    if (complete) setStatus(complete);
  }, [agentPresenceRef, closeAppTab, hostScopeRef, performAction, setStatus, snapshotRef, workspaceAppTabs]);
}
