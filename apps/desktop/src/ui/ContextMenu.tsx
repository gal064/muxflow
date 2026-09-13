import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { Icon } from "./Icon";

export interface ContextMenuItem {
  id: string;
  label: string;
  disabled?: boolean;
  /** Uses the app's single destructive treatment; there is only one red. */
  destructive?: boolean;
  /** Already rendered in platform notation by the caller. */
  shortcut?: string;
  shortcutLabel?: string;
  /**
   * Makes the item one option of a set rather than a command: defined turns the
   * row into a `menuitemradio` carrying `aria-checked` and a check column, so a
   * menu that *picks* something can say which one is picked. Left undefined —
   * every menu but the strip's all-tabs list — the row is a plain `menuitem`
   * and nothing about its markup changes.
   */
  checked?: boolean;
  /**
   * A connection phase drawn as a dot before the label — the host menu's way
   * of showing how each shown host is doing without a row for each.
   */
  dot?: "connected" | "connecting" | "reconnecting" | "resyncing" | "disconnected";
  run(): void;
}

export interface ContextMenuAnchor {
  x: number;
  y: number;
  /** The opener's width, for combobox-style menus that match their field. */
  width?: number;
}

/**
 * The keyboard's way of saying "right-click".
 *
 * Every row action in this app lives in a context menu, so without this a
 * keyboard-only user could not rename a file, stage a change, or resume an
 * agent at all. `Shift+F10` is the cross-platform convention and works on a
 * Mac keyboard through Fn+F10; the dedicated `ContextMenu` key exists on most
 * PC keyboards.
 */
export function isContextMenuKey(event: Pick<KeyboardEvent, "key" | "shiftKey">): boolean {
  return event.key === "ContextMenu" || (event.shiftKey && event.key === "F10");
}

/** Where a keyboard-opened menu goes: under the row that has focus. */
export function anchorForElement(element: Element): ContextMenuAnchor {
  const box = element.getBoundingClientRect();
  return { x: Math.round(box.left + 8), y: Math.round(box.bottom), width: Math.round(box.width) };
}

/**
 * How many menus are open, so the shell can suppress global shortcuts while one
 * is. Every row action in the app lives in a menu now; with one open, ⌘W has to
 * mean "the menu", not "the tab behind it".
 */
let openMenus = 0;
const openMenuListeners = new Set<(open: boolean) => void>();

export function useContextMenusOpen(): boolean {
  const [open, setOpen] = useState(openMenus > 0);
  useEffect(() => {
    openMenuListeners.add(setOpen);
    setOpen(openMenus > 0);
    return () => { openMenuListeners.delete(setOpen); };
  }, []);
  return open;
}

interface ContextMenuProps {
  label: string;
  anchor: ContextMenuAnchor;
  items: readonly (ContextMenuItem | "separator")[];
  /** Make the menu at least as wide as its opener, like a select popup. */
  matchAnchorWidth?: boolean;
  onClose(): void;
  children?: ReactNode;
}

/** Desktop scale, per the token table: 13px text, 24px rows. */
const ROW_HEIGHT = 24;
/** Only used for the first paint, before the menu has a measured width. */
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
  // Menus are as wide as their widest label, which no constant can know. The
  // first paint uses an estimate; this corrects it before the browser draws.
  const [size, setSize] = useState({ width: ESTIMATED_WIDTH, height: props.items.length * ROW_HEIGHT + 12 });
  const anchorX = props.matchAnchorWidth ? props.anchor.x - 8 : props.anchor.x;
  useLayoutEffect(() => {
    const box = container.current?.getBoundingClientRect();
    if (!box) return;
    setSize((current) => current.width === box.width && current.height === box.height
      ? current
      : { width: box.width, height: box.height });
  }, [props.items]);

  useEffect(() => {
    openMenus += 1;
    for (const listener of openMenuListeners) listener(true);
    opener.current = document.activeElement;
    // Falling back to the menu itself is not a nicety. An open menu suppresses
    // every global shortcut (`openMenus` feeds `modalOpen`), and Escape is
    // handled by a listener on this container — so a menu whose items are all
    // disabled, which happens while disconnected, left focus outside it
    // and made ⌘K, ⌘P, ⌘B and ⌘, all dead with no keyboard way out.
    // A menu that *picks* something opens on the option that is already
    // picked, the way every platform's own picker does — so the strip's
    // all-tabs list lands the keyboard on the current tab rather than on
    // whatever happens to be first in the strip.
    const checked = container.current?.querySelector<HTMLButtonElement>('button[aria-checked="true"]:not([disabled])');
    const first = checked ?? container.current?.querySelector<HTMLButtonElement>("button:not([disabled])");
    (first ?? container.current)?.focus();
    const onPointerDown = (event: PointerEvent) => {
      if (!container.current?.contains(event.target as Node)) props.onClose();
    };
    // Capture, so a pointer-down on any surface closes the menu before that
    // surface reacts to it.
    document.addEventListener("pointerdown", onPointerDown, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      openMenus = Math.max(0, openMenus - 1);
      for (const listener of openMenuListeners) listener(openMenus > 0);
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

  return <div
    aria-label={props.label}
    className="context-menu"
    onKeyDown={(event) => {
      if (event.key === "Escape") { event.stopPropagation(); props.onClose(); return; }
      const target = event.target as HTMLElement;
      // Tab must not walk out of an open menu. Focus leaving while the menu
      // stayed mounted kept `openMenus` above zero — global shortcuts off,
      // Escape out of reach — with only the mouse left to recover.
      if (event.key === "Tab") { event.preventDefault(); move(target, event.shiftKey ? -1 : 1); return; }
      if (event.key === "ArrowDown") { event.preventDefault(); move(target, 1); }
      else if (event.key === "ArrowUp") { event.preventDefault(); move(target, -1); }
      else if (event.key === "Home") { event.preventDefault(); move(target, "first"); }
      else if (event.key === "End") { event.preventDefault(); move(target, "last"); }
    }}
    ref={container}
    role="menu"
    // Focusable only as the fallback target above; it never enters the tab order.
    tabIndex={-1}
    style={{
      // Kept inside the viewport rather than clipped by it; a menu opened near
      // the bottom-right of the window is the common case, not the exception.
      left: Math.max(4, Math.min(anchorX, window.innerWidth - size.width - 4)),
      minWidth: props.matchAnchorWidth && props.anchor.width ? Math.max(180, props.anchor.width) : undefined,
      top: Math.max(4, Math.min(props.anchor.y, window.innerHeight - size.height - 4)),
    }}
  >
    {props.items.map((item, index) => item === "separator"
      ? <hr aria-hidden="true" key={`separator-${index}`} />
      : <button
        aria-checked={item.checked}
        className={item.destructive ? "context-menu-item destructive" : "context-menu-item"}
        // Stable identity for tests and for anything that needs to point at a
        // specific item; the label is user-facing text and will change.
        data-menu-item={item.id}
        disabled={item.disabled}
        key={item.id}
        onClick={() => { item.run(); props.onClose(); }}
        role={item.checked === undefined ? "menuitem" : "menuitemradio"}
        type="button"
      >
        {item.checked === undefined
          ? <span>{item.label}</span>
          // The check column is reserved on every row of such a menu, not only
          // the checked one, so choosing a different row does not reflow the
          // labels under the pointer.
          : <span className="menu-item-label">
            <span aria-hidden="true" className="menu-item-check">{item.checked ? <Icon name="check" size={11} /> : null}</span>
            {item.dot && <span aria-hidden="true" className={`link-dot ${item.dot}`} />}
            {item.label}
          </span>}
        {item.shortcut && <kbd aria-label={item.shortcutLabel}>{item.shortcut}</kbd>}
      </button>)}
    {props.children}
  </div>;
}
