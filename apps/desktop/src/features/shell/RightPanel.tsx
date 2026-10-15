import { useRef, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import { useTransientDrag } from "../workspaces/transientDrag";
import { PANEL_MIN_WIDTH, type PanelSurface } from "./types";

interface RightPanelProps {
  surface: PanelSurface;
  files: ReactNode;
  git: ReactNode;
  ports: ReactNode;
  /** Enabled panel extensions, after the built-in segments. */
  extensions?: readonly { id: string; label: string; body: ReactNode }[];
  onSurface(surface: PanelSurface): void;
  /** Current width in CSS pixels, already clamped against the window. */
  width: number;
  /** The cap the caller applies — half the window. */
  maxWidth: number;
  onWidth(width: number): void;
}

const BUILT_IN_SURFACES: readonly { id: PanelSurface; label: string }[] = [
  { id: "files", label: "Files" },
  { id: "git", label: "Git" },
  { id: "ports", label: "Ports" },
];

/**
 * One surface on the right — 300px by default, drag-resizable from its left
 * edge — with Files, Git and Ports as segments of it.
 *
 * It is closed by default and reserves nothing when closed — the caller does
 * not render it at all — which is the difference between this and the panel it
 * replaces, a 258px column that was always there whether or not it had
 * anything to say. Agents are not in here; they live in the sidebar.
 */
export function RightPanel(props: RightPanelProps) {
  const container = useRef<HTMLElement>(null);
  const [displayedWidth, startWidthDrag] = useTransientDrag(props.width, props.onWidth);
  const extensions = props.extensions ?? [];
  const SURFACES = [...BUILT_IN_SURFACES, ...extensions];
  // A surface that is not showable, such as an extension just turned off, is Files.
  const surface = SURFACES.some((candidate) => candidate.id === props.surface) ? props.surface : "files";
  const body = surface === "files" ? props.files
    : surface === "git" ? props.git
      : surface === "ports" ? props.ports
        : extensions.find((extension) => extension.id === surface)?.body;

  const selectRelative = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const index = SURFACES.findIndex((candidate) => candidate.id === surface);
    const step = event.key === "ArrowRight" ? 1 : SURFACES.length - 1;
    const next = SURFACES[(index + step) % SURFACES.length].id;
    props.onSurface(next);
    window.requestAnimationFrame(() => document.getElementById(panelTabId(next))?.focus());
  };

  return <aside
    aria-label="Files, Git and Ports"
    className="right-panel"
    ref={container}
    style={{ "--panel-width": `${displayedWidth}px` } as CSSProperties}
  >
    <div aria-label="Panel surface" className="panel-tabs" role="tablist">
      {SURFACES.map((candidate) => <button
        aria-controls={panelPanelId(candidate.id)}
        aria-selected={surface === candidate.id}
        className={surface === candidate.id ? "panel-tab active" : "panel-tab"}
        id={panelTabId(candidate.id)}
        key={candidate.id}
        onClick={() => props.onSurface(candidate.id)}
        onKeyDown={selectRelative}
        role="tab"
        tabIndex={surface === candidate.id ? 0 : -1}
        type="button"
      >{candidate.label}</button>)}
    </div>
    <div
      aria-labelledby={panelTabId(surface)}
      className="panel-body"
      id={panelPanelId(surface)}
      role="tabpanel"
      tabIndex={0}
    >{body}</div>

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
