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
  shortcutFor,
  shortcutCollisions,
  unsafeShortcutBindings,
  type CommandContext,
} from "./registry";

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

  it("makes every registered command reachable from both palette and Linux web menu", () => {
    const registered = commandRegistry.map((command) => command.id);
    expect(commandsForSurface("palette").map((command) => command.id)).toEqual(registered);
    expect(commandsForSurface("menu").map((command) => command.id)).toEqual(registered);
    expect(commandsForSurface("context").map((command) => command.id)).toEqual(registered);
    expect(commandsForSurface("toolbar").map((command) => command.id)).toContain("commands.show");
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
