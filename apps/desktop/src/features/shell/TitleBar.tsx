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
  /** Whether at least one host offered by New workspace can create one. */
  canCreateWorkspace: boolean;
  /** Whether Back / Forward have somewhere that still exists to go. */
  canGoBack: boolean;
  canGoForward: boolean;
  onBack(): void;
  onForward(): void;
  onToggleSidebar(): void;
  onTogglePanel(): void;
  onNewWorkspace(): void;
  onBell(): void;
}

/**
 * The only full-width bar: 38px on macOS, where it has to clear the native
 * traffic lights drawn over it, and 28px like every other bar elsewhere.
 *
 * It carries six controls. Back and Forward walk the focus history — through
 * terminals and document tabs alike — and are disabled when nothing that
 * still exists lies in that direction; ⌘[ and ⌘] run the same commands.
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
    <button
      aria-label="Back"
      className="bar-button"
      disabled={!props.canGoBack}
      onClick={props.onBack}
      title={props.platform === "mac" ? "Back (⌘[)" : "Back (Ctrl+[)"}
      type="button"
    ><Icon name="arrowLeft" /></button>
    <button
      aria-label="Forward"
      className="bar-button"
      disabled={!props.canGoForward}
      onClick={props.onForward}
      title={props.platform === "mac" ? "Forward (⌘])" : "Forward (Ctrl+])"}
      type="button"
    ><Icon name="arrowRight" /></button>
    <div className="titlebar-title" data-tauri-drag-region>
      <span className="titlebar-workspace">{props.workspaceName ?? "No workspace"}</span>
    </div>
    <div className="titlebar-spacer" data-tauri-drag-region />
    <button
      aria-label={bellLabel}
      // `aria-disabled`, never `disabled`. A disabled button takes no mouse
      // events and holds no focus, so neither the tooltip nor the label would
      // reach anyone — and explaining itself is the entire point of the state.
      aria-disabled={!props.canJump || undefined}
      className="bar-button bar-button-badged"
      onClick={() => { if (props.canJump) props.onBell(); }}
      title={bellHint}
      type="button"
    >
      <Icon name="bell" />
      {props.unread > 0 && <span aria-hidden="true" className="badge badge-corner">{props.unread > 99 ? "99+" : props.unread}</span>}
    </button>
    <button
      aria-label="New workspace"
      className="bar-button"
      disabled={!props.canCreateWorkspace}
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
