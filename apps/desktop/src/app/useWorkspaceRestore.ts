import { useEffect, useRef, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import { denormalizeSnapshot } from "../state/connectionReducer";
import type { HostLink } from "../state/hostLinks";
import { captureWorkspaceRestore, planWorkspaceRestore, sameWorkspaceRestore } from "../features/sessionRestore/model";
import type { PersistedAppState, WorkspaceRestoreHost } from "../features/shell/types";
import type { TmuxAction, TmuxActionResult } from "../features/tmux/actions";
import type { TmuxActionTarget } from "./useTmuxActionPerformer";

interface WorkspaceRestoreOptions {
  /** Nothing reads or writes the record before the saved state has loaded. */
  appStateLoaded: boolean;
  enabled: boolean;
  links: readonly HostLink[];
  /** The host whose facade a restored selection would land on. */
  activeHostProfileId: string;
  /** The live link for a profile, read again after the creates round-tripped. */
  linkFor(profileId: string): HostLink | undefined;
  appStateRef: MutableRefObject<PersistedAppState>;
  setAppState: Dispatch<SetStateAction<PersistedAppState>>;
  performAction(action: TmuxAction, target: TmuxActionTarget | undefined): Promise<TmuxActionResult | undefined>;
  /**
   * How an action reaches a host shown beside the active one. The sidebar's
   * rows use the same factory. Nothing for the host on screen, whose path is
   * the performer's default.
   */
  targetFor(profileId: string): TmuxActionTarget | undefined;
  /** The host on screen's own selection path, the one a sidebar click takes. */
  selectSessionOnActiveHost(sessionId: string): void;
}

/**
 * Remembering which workspaces each host had open, and reopening them when
 * that host's tmux server comes back without them.
 *
 * The whole feature is two rules over the same snapshot data:
 *
 * - **Capture**, after each host's first settled snapshot: fold what the
 *   snapshot says is open, and in which directories, into the persisted
 *   record. The debounced save and the close-time flush that already carry
 *   open document tabs carry this too.
 * - **Restore**, once per host link: ask for every remembered workspace whose
 *   *name* is absent from the snapshot (sessions the user closed are gone
 *   from the record, so they are never resurrected), and land on the
 *   remembered workspace on the host on screen.
 *
 * What it deliberately does not touch: a workspace that exists. tmux sessions
 * outlive this app by design, so an ordinary restart restores nothing but the
 * selection; the creates only fire against a server that lost them, after a
 * reboot or a `kill-server`. And capture never runs before that link's
 * restore has settled, because on a fresh server the first snapshot is the
 * empty one that would otherwise overwrite the very record the restore is
 * about to act on.
 */
export function useWorkspaceRestore(options: WorkspaceRestoreOptions): void {
  const {
    appStateLoaded, enabled, links, linkFor, appStateRef, setAppState, performAction, targetFor,
    selectSessionOnActiveHost,
  } = options;
  /** Per link: the connection epoch whose restore has run, and its in-flight promise. */
  const restoredEpochs = useRef(new Map<string, number>());
  const pendingRestores = useRef(new Map<string, Promise<void>>());
  // The host on screen, read when the restore *acts*, not when its effect ran:
  // the restore outlives several round trips, and the user can switch hosts
  // under it. A selection aimed at a host that is no longer on screen would
  // reach the wrong facade.
  const activeHostRef = useRef(options.activeHostProfileId);
  activeHostRef.current = options.activeHostProfileId;

  useEffect(() => {
    if (!appStateLoaded || !enabled) return;
    for (const link of links) {
      if (link.hostState.phase !== "connected" || !link.hostState.serverIdentity) continue;
      if (restoredEpochs.current.get(link.profileId) === link.connectionEpoch) continue;
      if (pendingRestores.current.has(link.profileId)) continue;
      restoredEpochs.current.set(link.profileId, link.connectionEpoch);
      const profileId = link.profileId;
      const run = (async () => {
        const remembered = appStateRef.current.workspaceRestore[profileId];
        const snapshot = denormalizeSnapshot(link.hostState);
        const target = () => targetFor(profileId);
        // Names created this run, so a selection can aim at one whose snapshot
        // has not landed yet: the create's ack is the earliest truth there is.
        const created = new Map<string, string>();
        for (const entry of planWorkspaceRestore(remembered, snapshot)) {
          const session = await performAction(
            { kind: "createSession", name: entry.name, ...(entry.directories[0] ? { directory: entry.directories[0] } : {}) },
            target(),
          );
          // A refusal (a remembered directory that is gone) leaves that one
          // workspace unrestored; the rest of the plan still runs, and the
          // action pipeline has already recorded the refusal.
          if (!session?.sessionId) continue;
          created.set(entry.name, session.sessionId);
          for (const directory of entry.directories.slice(1)) {
            await performAction(
              { kind: "createWindow", sessionId: session.sessionId, ...(directory ? { directory } : {}) },
              target(),
            );
          }
        }
        const selectedName = remembered?.selectedSessionName;
        if (!selectedName || profileId !== activeHostRef.current) return;
        const fresh = linkFor(profileId);
        const sessions = fresh
          ? denormalizeSnapshot(fresh.hostState).sessions
          : snapshot.sessions;
        const sessionId = sessions.find((session) => session.name === selectedName)?.id
          ?? created.get(selectedName);
        if (sessionId && fresh?.activeSessionId !== sessionId) selectSessionOnActiveHost(sessionId);
      })().finally(() => {
        pendingRestores.current.delete(profileId);
      });
      pendingRestores.current.set(profileId, run);
    }
  }, [appStateLoaded, enabled, links, appStateRef, linkFor, performAction, targetFor, selectSessionOnActiveHost]);

  useEffect(() => {
    if (!appStateLoaded || !enabled) return;
    const updates: [string, WorkspaceRestoreHost][] = [];
    for (const link of links) {
      if (link.hostState.phase !== "connected" || !link.hostState.serverIdentity) continue;
      // Not before this link's restore has settled. See the hook's comment.
      if (restoredEpochs.current.get(link.profileId) !== link.connectionEpoch) continue;
      if (pendingRestores.current.has(link.profileId)) continue;
      const snapshot = denormalizeSnapshot(link.hostState);
      const selectedSessionName = link.activeSessionId
        ? link.hostState.sessions[link.activeSessionId]?.name
        : undefined;
      const captured = captureWorkspaceRestore(snapshot, selectedSessionName);
      if (!sameWorkspaceRestore(appStateRef.current.workspaceRestore[link.profileId], captured)) {
        updates.push([link.profileId, captured]);
      }
    }
    if (updates.length === 0) return;
    setAppState((current) => {
      const workspaceRestore = { ...current.workspaceRestore };
      for (const [profileId, entry] of updates) {
        // A host with nothing open leaves no record at all, so the saved file
        // says "nothing remembered" the same way for a fresh install and for
        // someone who closed every workspace.
        if (entry.workspaces.length === 0 && entry.selectedSessionName === undefined) delete workspaceRestore[profileId];
        else workspaceRestore[profileId] = entry;
      }
      return { ...current, workspaceRestore };
    });
  }, [appStateLoaded, enabled, links, appStateRef, setAppState]);
}
