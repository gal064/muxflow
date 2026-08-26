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
  canMutate: boolean;
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
