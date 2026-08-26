import { Icon } from "../../ui/Icon";
import type { Platform } from "../../commands/registry";

interface TitleBarProps {
  platform: Platform;
  /** The workspace being shown, or undefined before the first snapshot. */
  workspaceName?: string;
  sidebarOpen: boolean;
  panelOpen: boolean;
  /** Agents waiting on a human; 0 renders no badge at all. */
  unread: number;
  /**
   * Whether the bell has somewhere to go. Not the same as `unread > 0`: a
   * waiting agent whose pane is gone still counts on the badge but cannot be
   * selected, and a bell that silently does nothing reads as broken.
   */
  canJump: boolean;
  canMutate: boolean;
  onToggleSidebar(): void;
  onTogglePanel(): void;
  onNewWorkspace(): void;
  onBell(): void;
}

/**
 * The only full-width bar: 38px on macOS, where it has to clear the native
 * traffic lights drawn over it, and 28px like every other bar elsewhere.
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
  const plural = props.unread === 1 ? "" : "s";
  // Three states, and the tooltip says the same thing the label does: there is
  // somewhere to go, there is nothing waiting, or something is waiting that the
  // bell cannot reach.
  const bellHint = props.canJump
    ? "Go to the next agent waiting (blocked first, then unread completed)"
    : props.unread > 0
      ? `${props.unread} agent${plural} waiting, none reachable right now`
      : "No agents waiting";
  const bellLabel = props.canJump
    ? `${props.unread} agent${plural} waiting; go to the next one`
    : bellHint;
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
    </div>
    <div className="titlebar-spacer" data-tauri-drag-region />
    <button
      aria-label={bellLabel}
      className="bar-button bar-button-badged"
      disabled={!props.canJump}
      onClick={props.onBell}
      title={bellHint}
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
