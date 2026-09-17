import { useRef, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import { useTransientDrag } from "../workspaces/transientDrag";
import { PANEL_MIN_WIDTH, type PanelSurface } from "./types";

interface RightPanelProps {
  surface: PanelSurface;
  files: ReactNode;
  git: ReactNode;
  onSurface(surface: PanelSurface): void;
  /** Current width in CSS pixels, already clamped against the window. */
  width: number;
  /** The cap the caller applies — half the window. */
  maxWidth: number;
  onWidth(width: number): void;
}

const SURFACES: readonly { id: PanelSurface; label: string }[] = [
  { id: "files", label: "Files" },
  { id: "git", label: "Git" },
];

/**
 * One surface on the right — 300px by default, drag-resizable from its left
 * edge — with Files and Git as two segments of it.
 *
 * It is closed by default and reserves nothing when closed — the caller does
 * not render it at all — which is the difference between this and the panel it
 * replaces, a 258px column that was always there whether or not it had
 * anything to say. Agents are not in here; they live in the sidebar.
 */
export function RightPanel(props: RightPanelProps) {
  const container = useRef<HTMLElement>(null);
  const [displayedWidth, startWidthDrag] = useTransientDrag(props.width, props.onWidth);

  const selectRelative = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const surface: PanelSurface = props.surface === "files" ? "git" : "files";
    props.onSurface(surface);
    window.requestAnimationFrame(() => document.getElementById(panelTabId(surface))?.focus());
  };

  return <aside
    aria-label="Files and Git"
    className="right-panel"
    ref={container}
    style={{ "--panel-width": `${displayedWidth}px` } as CSSProperties}
  >
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

    {/* The panel's own width, dragged from its left edge — so ArrowLeft grows
        it and ArrowRight shrinks it, the mirror of the sidebar's handle. The
        cap is applied by the caller, which is the only thing that knows the
        window. */}
    <div
      aria-label="Resize the panel"
      aria-orientation="vertical"
      aria-valuemax={Math.round(props.maxWidth)}
      aria-valuemin={PANEL_MIN_WIDTH}
      aria-valuenow={Math.round(displayedWidth)}
      className="panel-resize"
      onKeyDown={(event) => {
        const delta = event.key === "ArrowLeft" ? 16 : event.key === "ArrowRight" ? -16 : 0;
        if (!delta) return;
        event.preventDefault();
        props.onWidth(displayedWidth + delta);
      }}
      onPointerDown={(event) => {
        startWidthDrag(event, (pointer) => Math.max(
          PANEL_MIN_WIDTH,
          Math.min(props.maxWidth, (container.current?.getBoundingClientRect().right ?? 0) - pointer.clientX),
        ));
      }}
      role="separator"
      tabIndex={0}
    />
  </aside>;
}

export function panelTabId(surface: PanelSurface): string {
  return `panel-tab-${surface}`;
}

export function panelPanelId(surface: PanelSurface): string {
  return `panel-surface-${surface}`;
}
