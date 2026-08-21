import { useCallback, type MutableRefObject } from "react";
import { editorFlushRegistry } from "../features/files/editorFlushRegistry";
import { sameHostConnection, sameHostScope, type HostScopeToken } from "../features/shell/hostScope";
import {
  agentPresenceIsCurrent,
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
 * Saving happens once, before any mutation. Terminal closes then advance one
 * authoritative precondition through the batch and re-check protected agents
 * immediately before each destructive request.
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
    if (!sameHostScope(scope, hostScopeRef.current) || !scope.serverIdentity) {
      setStatus("Closing those tabs was cancelled because the terminal layout changed.");
      return;
    }

    const terminalTabs = tabs.filter(
      (tab): tab is Extract<CombinedTab, { kind: "terminal" }> => tab.kind === "terminal",
    );
    let precondition: AuthoritativePrecondition = {
      serverIdentity: scope.serverIdentity,
      generation: scope.generation,
    };
    for (const [index, tab] of terminalTabs.entries()) {
      // The first close can take long enough for a newly detected agent to
      // protect a later tab in the same batch.
      if (tabsEligibleAtBulkCloseCommit(
        [tab], protectAgents, agentPresenceRef.current,
      ).length === 0) continue;
      const terminalWindow = snapshotRef.current.windows.find((item) => item.id === tab.id);
      if (!terminalWindow) continue;

      // The generation returned by one mutation is the precondition for the
      // next; React need not have published the corresponding snapshot yet.
      const result = await performAction({
        kind: "closeWindow",
        sessionId: terminalWindow.sessionId,
        windowId: terminalWindow.id,
        confirmed: true,
      }, precondition);
      if (!sameHostConnection(scope, hostScopeRef.current) || !result) return;
      const serverIdentity = hostScopeRef.current.serverIdentity;
      if (!serverIdentity) return;
      precondition = { serverIdentity, generation: result.topologyGeneration };

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

    // Local document tabs are lossless after the shared editor flush, but
    // commit them only once the stale-sensitive terminal transaction has been
    // admitted. A rejected first terminal close must leave the set untouched.
    if (!sameHostConnection(scope, hostScopeRef.current)) return;
    for (const tab of tabs) {
      if (tab.kind !== "app") continue;
      const appTab = workspaceAppTabs.find((item) => item.id === tab.id);
      if (appTab) closeAppTab(appTab, scope);
    }
  }, [agentPresenceRef, closeAppTab, hostScopeRef, performAction, setStatus, snapshotRef, workspaceAppTabs]);
}
