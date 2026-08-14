import { useEffect, useRef, type ReactNode } from "react";

export interface ContextMenuItem {
  id: string;
  label: string;
  disabled?: boolean;
  /** Uses the app's single destructive treatment; there is only one red. */
  destructive?: boolean;
  /** Already rendered in platform notation by the caller. */
  shortcut?: string;
  shortcutLabel?: string;
  run(): void;
}

export interface ContextMenuAnchor {
  x: number;
  y: number;
}

interface ContextMenuProps {
  label: string;
  anchor: ContextMenuAnchor;
  items: readonly (ContextMenuItem | "separator")[];
  onClose(): void;
  children?: ReactNode;
}

/** Desktop scale, per the token table: 13px text, 24px rows. */
const ROW_HEIGHT = 24;
const ESTIMATED_WIDTH = 200;

/**
 * One context menu for the whole app.
 *
 * Before this there were seven bespoke `<details>` menus with different widths,
 * row heights and keyboard behavior, and per-row action clusters that appeared
 * on hover. Everything that used to be a hover button is an item in here or a
 * command in the palette.
 *
 * It is a real menu for the keyboard too: arrows move, Home/End jump, Escape
 * closes and returns focus to whatever opened it, and it closes on outside
 * pointer-down rather than on blur — blur alone would swallow the click that
 * was meant to be a selection.
 */
export function ContextMenu(props: ContextMenuProps) {
  const container = useRef<HTMLDivElement>(null);
  const opener = useRef<Element | null>(null);

  useEffect(() => {
    opener.current = document.activeElement;
    container.current?.querySelector<HTMLButtonElement>("button:not([disabled])")?.focus();
    const onPointerDown = (event: PointerEvent) => {
      if (!container.current?.contains(event.target as Node)) props.onClose();
    };
    // Capture, so a pointer-down on any surface closes the menu before that
    // surface reacts to it.
    document.addEventListener("pointerdown", onPointerDown, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      if (opener.current instanceof HTMLElement && document.contains(opener.current)) opener.current.focus();
    };
  }, []);

  const move = (from: HTMLElement, delta: number | "first" | "last") => {
    const buttons = [...(container.current?.querySelectorAll<HTMLButtonElement>("button:not([disabled])") ?? [])];
    if (buttons.length === 0) return;
    const index = buttons.indexOf(from as HTMLButtonElement);
    const next = delta === "first" ? 0
      : delta === "last" ? buttons.length - 1
      : (index + delta + buttons.length) % buttons.length;
    buttons[next]?.focus();
  };

  const rows = props.items.length;
  return <div
    aria-label={props.label}
    className="context-menu"
    onKeyDown={(event) => {
      if (event.key === "Escape") { event.stopPropagation(); props.onClose(); return; }
      const target = event.target as HTMLElement;
      if (event.key === "ArrowDown") { event.preventDefault(); move(target, 1); }
      else if (event.key === "ArrowUp") { event.preventDefault(); move(target, -1); }
      else if (event.key === "Home") { event.preventDefault(); move(target, "first"); }
      else if (event.key === "End") { event.preventDefault(); move(target, "last"); }
    }}
    ref={container}
    role="menu"
    style={{
      // Kept inside the viewport rather than clipped by it; a menu opened near
      // the bottom-right of the window is the common case, not the exception.
      left: Math.max(4, Math.min(props.anchor.x, window.innerWidth - ESTIMATED_WIDTH - 4)),
      top: Math.max(4, Math.min(props.anchor.y, window.innerHeight - rows * ROW_HEIGHT - 12)),
    }}
  >
    {props.items.map((item, index) => item === "separator"
      ? <hr aria-hidden="true" key={`separator-${index}`} />
      : <button
        className={item.destructive ? "context-menu-item destructive" : "context-menu-item"}
        // Stable identity for tests and for anything that needs to point at a
        // specific item; the label is user-facing text and will change.
        data-menu-item={item.id}
        disabled={item.disabled}
        key={item.id}
        onClick={() => { item.run(); props.onClose(); }}
        role="menuitem"
        type="button"
      >
        <span>{item.label}</span>
        {item.shortcut && <kbd aria-label={item.shortcutLabel}>{item.shortcut}</kbd>}
      </button>)}
    {props.children}
  </div>;
}
