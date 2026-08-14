import { describe, expect, it } from "vitest";
import {
  commandAvailable,
  commandForKeyboardEvent,
  commandRegistry,
  commandsForSurface,
  globalShortcutAllowed,
  isSafeShortcut,
  keyboardEventIsComposing,
  normalizeShortcut,
  selectionIndex,
  shortcutFor,
  shortcutCollisions,
  unsafeShortcutBindings,
  type CommandContext,
} from "./registry";

const DIGITS = [1, 2, 3, 4, 5, 6, 7, 8, 9] as const;

const context = (overrides: Partial<CommandContext> = {}): CommandContext => ({
  canMutate: true, hasPane: true, hasSession: true, hasWindow: true, hasTab: true,
  canMoveSessionUp: true, canMoveSessionDown: true,
  canMoveTabLeft: true, canMoveTabRight: true,
  run: () => undefined, ...overrides,
});

describe("command registry", () => {
  it("uses conventional Linux shortcuts without a tmux prefix", () => {
    const split = commandRegistry.find((command) => command.id === "pane.splitRight")!;
    expect(shortcutFor(split, "linux", {})).toBe("Ctrl+Shift+D");
    const event = {
      key: "d", ctrlKey: true, shiftKey: true, altKey: false, metaKey: false,
    } as KeyboardEvent;
    expect(commandForKeyboardEvent(event, "linux", {})?.id).toBe("pane.splitRight");
  });

  it("never invokes application shortcuts while an IME composition is active or committing", () => {
    const base = { key: "p", ctrlKey: true, shiftKey: true, altKey: false, metaKey: false };
    const composing = { ...base, isComposing: true, keyCode: 80 } as KeyboardEvent;
    const webkitCommit = { ...base, isComposing: false, keyCode: 229 } as KeyboardEvent;
    expect(keyboardEventIsComposing(composing)).toBe(true);
    expect(keyboardEventIsComposing(webkitCommit)).toBe(true);
    expect(commandForKeyboardEvent(composing, "linux", {})).toBeUndefined();
    expect(commandForKeyboardEvent(webkitCommit, "linux", {})).toBeUndefined();
  });

  it("normalizes modifier order and honors overrides and disabled bindings", () => {
    expect(normalizeShortcut("Shift+Ctrl+d")).toBe("Ctrl+Shift+D");
    const create = commandRegistry.find((command) => command.id === "session.new")!;
    expect(shortcutFor(create, "linux", { "session.new": "Alt+N" })).toBe("Alt+N");
    expect(shortcutFor(create, "linux", { "session.new": null })).toBeUndefined();
  });

  it("freezes mutating commands while disconnected but leaves local terminal commands available", () => {
    const split = commandRegistry.find((command) => command.id === "pane.splitRight")!;
    const search = commandRegistry.find((command) => command.id === "terminal.search")!;
    expect(commandAvailable(split, context({ canMutate: false }))).toBe(false);
    expect(commandAvailable(search, context({ canMutate: false }))).toBe(true);
  });

  it("rejects bare printable bindings that would steal terminal input", () => {
    expect(isSafeShortcut("A")).toBe(false);
    expect(isSafeShortcut("Shift+A")).toBe(false);
    expect(isSafeShortcut("Ctrl+A")).toBe(true);
    expect(unsafeShortcutBindings({ "session.new": "A" })).toEqual(["session.new"]);
  });

  it("makes every registered command reachable, and only hides the positional selectors", () => {
    const registered = commandRegistry.map((command) => command.id);
    const searchable = commandRegistry.filter((command) => !command.paletteHidden).map((command) => command.id);
    expect(commandsForSurface("shortcuts").map((command) => command.id)).toEqual(registered);
    expect(commandsForSurface("palette").map((command) => command.id)).toEqual(searchable);
    expect(commandsForSurface("menu").map((command) => command.id)).toEqual(searchable);
    expect(commandsForSurface("context").map((command) => command.id)).toEqual(searchable);
    // Nothing but ⌘1–9 / ⌃1–9 may be kept out of the searchable surfaces.
    expect(commandRegistry.filter((command) => command.paletteHidden).map((command) => command.id).sort())
      .toEqual([...DIGITS.map((d) => `tab.select${d}`), ...DIGITS.map((d) => `workspace.select${d}`)].sort());
  });

  it("adopts the cmux keymap, with ⌘ mapped away from the Linux window manager", () => {
    const shortcut = (id: string, platform: "mac" | "linux") =>
      shortcutFor(commandRegistry.find((command) => command.id === id)!, platform, {});
    expect(shortcut("commands.show", "mac")).toBe("Meta+K");
    expect(shortcut("workspaces.switch", "mac")).toBe("Meta+P");
    expect(shortcut("session.new", "mac")).toBe("Meta+N");
    expect(shortcut("view.toggleSidebar", "mac")).toBe("Meta+B");
    expect(shortcut("view.togglePanel", "mac")).toBe("Meta+Alt+B");
    expect(shortcut("pane.splitRight", "mac")).toBe("Meta+D");
    expect(shortcut("pane.splitDown", "mac")).toBe("Meta+Shift+D");
    expect(shortcut("pane.zoom", "mac")).toBe("Meta+Shift+Enter");
    expect(shortcut("agents.jumpUnread", "mac")).toBe("Meta+Shift+U");
    expect(shortcut("workspace.select4", "mac")).toBe("Meta+4");
    expect(shortcut("tab.select4", "mac")).toBe("Ctrl+4");
    // Super is the compositor's on Linux, so workspaces move to Alt there and
    // the two positional families stay distinct.
    expect(shortcut("workspace.select4", "linux")).toBe("Alt+4");
    expect(shortcut("tab.select4", "linux")).toBe("Ctrl+4");
  });

  it("resolves a positional selector to its 1-based index", () => {
    expect(selectionIndex("workspace.select7", "workspace.select")).toBe(7);
    expect(selectionIndex("tab.select1", "tab.select")).toBe(1);
    expect(selectionIndex("tab.select1", "workspace.select")).toBeUndefined();
    expect(selectionIndex("session.new", "workspace.select")).toBeUndefined();
  });

  it("never steals shortcuts from forms or overlays but keeps xterm's hidden textarea routable", () => {
    const target = (editable: boolean, terminal: boolean) => ({
      closest: (selector: string) => selector.startsWith("input") ? (editable ? {} : null) : (terminal ? {} : null),
    });
    expect(globalShortcutAllowed({ target: target(true, false) as unknown as EventTarget }, false)).toBe(false);
    expect(globalShortcutAllowed({ target: target(true, true) as unknown as EventTarget }, false)).toBe(true);
    expect(globalShortcutAllowed({ target: target(false, false) as unknown as EventTarget }, true)).toBe(false);
  });

  it("has collision-free platform defaults and detects user override collisions", () => {
    expect(shortcutCollisions("linux", {})).toEqual([]);
    expect(shortcutCollisions("mac", {})).toEqual([]);
    expect(shortcutCollisions("linux", { "session.new": "Ctrl+Shift+T" })).toEqual([{
      shortcut: "Ctrl+Shift+T", commandIds: ["session.new", "window.new"],
    }]);
    const event = { key: "T", ctrlKey: true, shiftKey: true, altKey: false, metaKey: false } as KeyboardEvent;
    expect(commandForKeyboardEvent(event, "linux", { "session.new": "Ctrl+Shift+T" })).toBeUndefined();
  });

  it("disables relative window moves when there is no adjacent target", () => {
    const left = commandRegistry.find((command) => command.id === "window.moveLeft")!;
    const right = commandRegistry.find((command) => command.id === "window.moveRight")!;
    expect(commandAvailable(left, context({ canMoveTabLeft: false }))).toBe(false);
    expect(commandAvailable(right, context({ canMoveTabRight: false }))).toBe(false);
    expect(commandAvailable(left, context())).toBe(true);
  });

  it("shares move availability between terminal and app-owned tabs", () => {
    const left = commandRegistry.find((command) => command.id === "window.moveLeft")!;
    expect(commandAvailable(left, context({ canMutate: false, hasWindow: false, hasPane: false, hasTab: true }))).toBe(true);
    expect(commandAvailable(left, context({ canMutate: false, hasWindow: true, hasTab: true }))).toBe(false);
  });

  it("marks exactly the session, window, and pane close menu commands for confirmation", () => {
    expect(commandRegistry.filter((command) => command.destructive).map((command) => command.id)).toEqual([
      "session.close", "window.close", "pane.close",
    ]);
    expect(commandsForSurface("menu").filter((command) => command.destructive).map((command) => command.id)).toEqual([
      "session.close", "window.close", "pane.close",
    ]);
  });

  it("closes app-owned tabs while disconnected but never sends terminal-window close while frozen", () => {
    const close = commandRegistry.find((command) => command.id === "window.close")!;
    expect(commandAvailable(close, context({ canMutate: false, hasWindow: false, hasPane: false, hasTab: true }))).toBe(true);
    expect(commandAvailable(close, context({ canMutate: false, hasWindow: true, hasTab: true }))).toBe(false);
  });
});
