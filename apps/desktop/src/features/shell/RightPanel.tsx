import type { KeyboardEvent, ReactNode } from "react";
import type { PanelSurface } from "./types";

interface RightPanelProps {
  surface: PanelSurface;
  files: ReactNode;
  git: ReactNode;
  onSurface(surface: PanelSurface): void;
}

const SURFACES: readonly { id: PanelSurface; label: string }[] = [
  { id: "files", label: "Files" },
  { id: "git", label: "Git" },
];

/**
 * One 300px surface on the right, with Files and Git as two segments of it.
 *
 * It is closed by default and reserves nothing when closed — the caller does
 * not render it at all — which is the difference between this and the panel it
 * replaces, a 258px column that was always there whether or not it had
 * anything to say. Agents are not in here; they live in the sidebar.
 */
export function RightPanel(props: RightPanelProps) {
  const selectRelative = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const surface: PanelSurface = props.surface === "files" ? "git" : "files";
    props.onSurface(surface);
    window.requestAnimationFrame(() => document.getElementById(panelTabId(surface))?.focus());
  };

  return <aside aria-label="Files and Git" className="right-panel">
    <div aria-label="Panel surface" className="panel-tabs" role="tablist">
      {SURFACES.map((surface) => <button
        aria-controls={panelPanelId(surface.id)}
        aria-selected={props.surface === surface.id}
        className={props.surface === surface.id ? "panel-tab active" : "panel-tab"}
        id={panelTabId(surface.id)}
        key={surface.id}
        onClick={() => props.onSurface(surface.id)}
        onKeyDown={selectRelative}
        role="tab"
        tabIndex={props.surface === surface.id ? 0 : -1}
        type="button"
      >{surface.label}</button>)}
    </div>
    <div
      aria-labelledby={panelTabId(props.surface)}
      className="panel-body"
      id={panelPanelId(props.surface)}
      role="tabpanel"
      tabIndex={0}
    >{props.surface === "files" ? props.files : props.git}</div>
  </aside>;
}

export function panelTabId(surface: PanelSurface): string {
  return `panel-tab-${surface}`;
}

export function panelPanelId(surface: PanelSurface): string {
  return `panel-surface-${surface}`;
}
