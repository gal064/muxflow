import { Icon } from "../../ui/Icon";
import type { Platform } from "../../commands/registry";

interface TitleBarProps {
  platform: Platform;
  /** The workspace being shown, or undefined before the first snapshot. */
  workspaceName?: string;
  /** `main*` — branch plus a marker when the worktree is dirty. */
  branch?: string;
  sidebarOpen: boolean;
  panelOpen: boolean;
  /** Agents waiting on a human; 0 renders no badge at all. */
  unread: number;
  canMutate: boolean;
  onToggleSidebar(): void;
  onTogglePanel(): void;
  onNewWorkspace(): void;
  onBell(): void;
}

/**
 * The only full-width bar, 28px like every other bar in the app.
 *
 * It carries four controls. The mock also draws back/forward arrows; those did
 * not survive, because the phase's own acceptance gate caps resting chrome and
 * "delete chrome instead of rearranging it" outranks a decorative pair of
 * arrows. Focus history is still there — as commands, on ⌘[ and ⌘].
 *
 * On macOS the OS titlebar is an overlay, so this bar draws underneath the
 * traffic lights and reserves room for them; the empty space is a drag region,
 * which is the only reason the window can still be moved.
 */
export function TitleBar(props: TitleBarProps) {
  return <header className={`titlebar ${props.platform === "mac" ? "titlebar-overlay" : ""}`} data-tauri-drag-region>
    <button
      aria-label="Toggle sidebar"
      aria-pressed={props.sidebarOpen}
      className="bar-button"
      onClick={props.onToggleSidebar}
      type="button"
    ><Icon name="sidebarLeft" /></button>
    <div className="titlebar-title" data-tauri-drag-region>
      <span className="titlebar-workspace">{props.workspaceName ?? "No workspace"}</span>
      {props.branch && <span className="titlebar-branch" title={`Git branch ${props.branch}`}>{props.branch}</span>}
    </div>
    <div className="titlebar-spacer" data-tauri-drag-region />
    <button
      aria-label={props.unread > 0
        ? `${props.unread} agent${props.unread === 1 ? "" : "s"} waiting; jump to the loudest`
        : "No agents waiting"}
      className="bar-button bar-button-badged"
      onClick={props.onBell}
      type="button"
    >
      <Icon name="bell" />
      {props.unread > 0 && <span aria-hidden="true" className="badge badge-corner">{props.unread > 99 ? "99+" : props.unread}</span>}
    </button>
    <button
      aria-label="New workspace"
      className="bar-button"
      disabled={!props.canMutate}
      onClick={props.onNewWorkspace}
      type="button"
    ><Icon name="plus" /></button>
    <button
      aria-label="Toggle right panel"
      aria-pressed={props.panelOpen}
      className="bar-button"
      onClick={props.onTogglePanel}
      type="button"
    ><Icon name="panelRight" /></button>
  </header>;
}
