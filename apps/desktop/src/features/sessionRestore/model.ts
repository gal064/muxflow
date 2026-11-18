import type { TmuxSnapshot } from "../../app/types";
import { orderedSessions } from "../shell/model";
import {
  MAX_RESTORE_TABS_PER_WORKSPACE, MAX_RESTORE_WORKSPACES_PER_HOST,
  type WorkspaceRestoreHost, type WorkspaceRestoreTab, type WorkspaceRestoreWorkspace,
} from "../shell/types";

/** The host's own ceiling on a tmux name (`validate_name` in the host's `command.rs`). */
const MAX_TMUX_NAME_LENGTH = 200;

/**
 * The workspaces a host had open, with the directory of every tab.
 *
 * Read off a snapshot, which is the only thing in this app that knows where a
 * pane sits: `currentPath` is tmux's `#{pane_current_path}`, so a `cd` lands
 * here as soon as the host's next snapshot does. A tab is captured by the
 * *active* pane's directory, the one the user was last looking at in that tab;
 * a split's other panes are deliberately not remembered, because recreating a
 * split tree is a different, larger feature and a half-restored one is worse
 * than an honest single terminal.
 *
 * A path tmux could not resolve (empty, or not absolute) restores at tmux's
 * own choice rather than at a guessed one, and a workspace name the host could
 * never create (control characters, or past its length ceiling) is dropped
 * rather than saved into a record whose restore is guaranteed to fail.
 */
export function captureWorkspaceRestore(
  snapshot: TmuxSnapshot,
  selectedSessionName: string | undefined,
): WorkspaceRestoreHost {
  const workspaces: WorkspaceRestoreWorkspace[] = [];
  for (const session of orderedSessions(snapshot.sessions)) {
    if (workspaces.length >= MAX_RESTORE_WORKSPACES_PER_HOST) break;
    const name = restorableName(session.name);
    if (!name) continue;
    const tabs: WorkspaceRestoreTab[] = [];
    const windows = snapshot.windows
      .filter((window) => window.sessionId === session.id)
      .sort((left, right) => left.index - right.index);
    for (const window of windows) {
      if (tabs.length >= MAX_RESTORE_TABS_PER_WORKSPACE) break;
      const pane = snapshot.panes.find((item) => item.windowId === window.id && item.active)
        ?? snapshot.panes.find((item) => item.windowId === window.id);
      const directory = pane ? restorableDirectory(pane.currentPath) : undefined;
      tabs.push(directory ? { directory } : {});
    }
    workspaces.push({ name, tabs });
  }
  const selected = selectedSessionName === undefined ? undefined : restorableName(selectedSessionName);
  return { workspaces, ...(selected ? { selectedSessionName: selected } : {}) };
}

/**
 * What to create on a host that is missing remembered workspaces.
 *
 * Matching is by name against the *live* snapshot, so this is safe to run
 * whenever it is convenient: workspaces that are already open (the ordinary
 * case, since tmux sessions outlive the app) come back planned as nothing,
 * and a second run after a first that half-succeeded creates only what is
 * still missing. The first directory rides on the session create (tmux gives
 * the session its first window); every later one is a `createWindow`.
 */
export interface WorkspaceRestorePlan {
  name: string;
  /** The first entry starts the session; the rest are the further tabs. */
  directories: (string | undefined)[];
}

export function planWorkspaceRestore(
  remembered: WorkspaceRestoreHost | undefined,
  snapshot: TmuxSnapshot,
): WorkspaceRestorePlan[] {
  if (!remembered) return [];
  const open = new Set(snapshot.sessions.map((session) => session.name));
  return remembered.workspaces
    .filter((workspace) => !open.has(workspace.name))
    .map((workspace) => ({
      name: workspace.name,
      directories: workspace.tabs.map((tab) => tab.directory),
    }));
}

/** Whether two captured records describe the same state, for write-skipping. */
export function sameWorkspaceRestore(
  left: WorkspaceRestoreHost | undefined,
  right: WorkspaceRestoreHost,
): boolean {
  if (left === undefined) return right.workspaces.length === 0 && right.selectedSessionName === undefined;
  return JSON.stringify(left) === JSON.stringify(right);
}

function restorableName(name: string): string | undefined {
  if (name === "" || name.length > MAX_TMUX_NAME_LENGTH || hasControlCharacter(name)) return undefined;
  return name;
}

/**
 * Only an absolute path is worth restoring. tmux reports an empty
 * `pane_current_path` when it cannot read one, and the host's create refuses
 * anything not absolute anyway, so remembering it would only guarantee a
 * refusal at restore time where tmux's own choice would have worked.
 */
function restorableDirectory(path: string): string | undefined {
  if (!path.startsWith("/") || hasControlCharacter(path)) return undefined;
  return path;
}

function hasControlCharacter(value: string): boolean {
  return /[\u0000-\u001f\u007f]/.test(value);
}
