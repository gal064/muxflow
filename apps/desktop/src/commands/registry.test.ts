import { describe, expect, it } from "vitest";
import {
  commandAvailable,
  commandForKeyboardEvent,
  commandRegistry,
  commandsForSurface,
  globalShortcutAllowed,
  isSafeShortcut,
  keyFromCode,
  keyboardEventIsComposing,
  normalizeShortcut,
  selectionIndex,
  shortcutFromEvent,
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
  hasHostProfile: true,
  rowCommands: [],
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
    // ⌘L, off Option entirely: ⌥⌘B was drawn in the palette and could never
    // fire, because macOS types `∫` for ⌥B.
    expect(shortcut("view.togglePanel", "mac")).toBe("Meta+L");
    expect(shortcut("view.togglePanel", "linux")).toBe("Ctrl+L");
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

  it("resolves an Option-modified binding, which macOS rewrites into a different glyph", () => {
    // ⌥⌘B. macOS types `∫` for ⌥B, so matching on `event.key` normalized this
    // to `Meta+Alt+∫` and the right panel's shortcut did nothing at all —
    // measured on the packaged app, where the titlebar toggle worked and the
    // key did not. Same defect class as ⌘⇧[, one modifier over.
    const event = {
      key: "∫", code: "KeyB", ctrlKey: false, altKey: true, shiftKey: false, metaKey: true,
      isComposing: false, keyCode: 0,
    } as KeyboardEvent;
    expect(shortcutFromEvent(event)).toBe("Alt+Meta+B");
    // ⌥⌘B is no longer a default — the panel moved to ⌘L precisely because
    // Option-modified letters are this fragile — but a user who binds one by
    // hand must still have it resolve.
    expect(commandForKeyboardEvent(event, "mac", { "view.togglePanel": "Meta+Alt+B" })?.id).toBe("view.togglePanel");
    // A layout `code` cannot describe still falls back to what was typed.
    expect(keyFromCode("KeyB")).toBe("B");
    expect(keyFromCode("Digit4")).toBe("4");
    expect(keyFromCode("IntlBackslash")).toBeUndefined();
  });

  it("resolves macOS Control-number from the physical digit while a terminal has focus", () => {
    const controlCharacter = {
      key: "\u0004", code: "Digit4", ctrlKey: true, altKey: false, shiftKey: false, metaKey: false,
      isComposing: false, keyCode: 52,
    } as KeyboardEvent;
    expect(shortcutFromEvent(controlCharacter)).toBe("Ctrl+4");
    expect(commandForKeyboardEvent(controlCharacter, "mac", {})?.id).toBe("tab.select4");

    const missingPhysicalCode = { ...controlCharacter, code: "" } as KeyboardEvent;
    expect(shortcutFromEvent(missingPhysicalCode)).toBe("Ctrl+4");
    expect(commandForKeyboardEvent(missingPhysicalCode, "mac", {})?.id).toBe("tab.select4");

    const legacyCompositionCode = { ...controlCharacter, keyCode: 229 } as KeyboardEvent;
    expect(commandForKeyboardEvent(legacyCompositionCode, "mac", {})?.id).toBe("tab.select4");

    const legacyCompositionWithOnlyTypedDigit = { ...controlCharacter, key: "4", code: "", keyCode: 229 } as KeyboardEvent;
    expect(commandForKeyboardEvent(legacyCompositionWithOnlyTypedDigit, "mac", {})?.id).toBe("tab.select4");

    const missingCommandCode = {
      key: "\u0002", code: "", ctrlKey: false, altKey: false, shiftKey: false, metaKey: true,
      isComposing: false, keyCode: 50,
    } as KeyboardEvent;
    expect(shortcutFromEvent(missingCommandCode)).toBe("Meta+2");
    expect(commandForKeyboardEvent(missingCommandCode, "mac", {})?.id).toBe("workspace.select2");

    // Only the nine registered selectors are consumed. A neighbouring terminal
    // Control sequence remains terminal input rather than an application key.
    const controlZero = { ...controlCharacter, key: "\u0000", code: "Digit0", keyCode: 48 } as KeyboardEvent;
    expect(commandForKeyboardEvent(controlZero, "mac", {})).toBeUndefined();
  });

  it("resolves every default binding back from the keystroke that produces it", () => {
    // The gap this closes: nothing round-tripped a binding through a real
    // KeyboardEvent, so `Meta+Shift+[` sat in the registry, rendered as ⌘⇧[ in
    // the palette, and could never fire — a shifted `[` arrives as `{`.
    const CODE_BY_KEY: Record<string, string> = {
      "[": "BracketLeft", "]": "BracketRight", ",": "Comma", ".": "Period", "/": "Slash",
      ";": "Semicolon", "'": "Quote", "`": "Backquote", "\\": "Backslash", "-": "Minus", "=": "Equal",
    };
    const SHIFTED: Record<string, string> = { "[": "{", "]": "}", ",": "<", ".": ">", "/": "?", "=": "+", "-": "_" };
    for (const platform of ["mac", "linux"] as const) {
      for (const command of commandRegistry) {
        const shortcut = shortcutFor(command, platform, {});
        if (!shortcut) continue;
        const parts = shortcut.split("+");
        const key = parts.at(-1)!;
        const shift = parts.includes("Shift");
        const event = {
          ctrlKey: parts.includes("Ctrl"),
          altKey: parts.includes("Alt"),
          shiftKey: shift,
          metaKey: parts.includes("Meta"),
          // What a browser actually reports: the typed glyph, plus the physical
          // key that produced it.
          key: shift && SHIFTED[key] ? SHIFTED[key] : key,
          code: CODE_BY_KEY[key] ?? (key.length === 1 ? `Key${key.toUpperCase()}` : key),
          isComposing: false,
          keyCode: 0,
        } as KeyboardEvent;
        expect(commandForKeyboardEvent(event, platform, {})?.id, `${command.id} on ${platform} (${shortcut})`)
          .toBe(command.id);
      }
    }
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
    const event = (editable: boolean, terminal: boolean, key = "1", code = "Digit1") => ({
      target: target(editable, terminal) as unknown as EventTarget,
      key, code, keyCode: key.charCodeAt(0), ctrlKey: true, metaKey: false,
    });
    expect(globalShortcutAllowed(event(true, false), false)).toBe(false);
    expect(globalShortcutAllowed(event(true, false), false, "workspace.select1")).toBe(true);
    expect(globalShortcutAllowed(event(true, false), false, "tab.select1")).toBe(true);
    expect(globalShortcutAllowed(event(true, false), true, "tab.select1")).toBe(false);
    // A selector rebound to an editing chord does not steal that chord merely
    // because the resolved command happens to be positional.
    expect(globalShortcutAllowed(event(true, false, "x", "KeyX"), false, "tab.select1")).toBe(false);
    expect(globalShortcutAllowed(event(true, true), false)).toBe(true);
    expect(globalShortcutAllowed(event(false, false), true)).toBe(false);
  });

  it("has collision-free platform defaults and detects user override collisions", () => {
    // A duplicate binding silently disables *both* commands — the resolver
    // takes a match only when there is exactly one — so this has to stay empty
    // as bindings move around.
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
    // `host.delete` destroys something too, and is deliberately not marked: in
    // this registry the flag routes a command through the *tmux* confirmation
    // builder, and a saved host has no server identity or topology generation to
    // capture. It carries its own dialog, exactly as `files.delete` does.
    expect(commandRegistry.find((command) => command.id === "host.delete")!.destructive).toBeUndefined();
  });

  it("keeps a saved host deletable while the tmux server it names is unreachable", () => {
    const remove = commandRegistry.find((command) => command.id === "host.delete")!;
    // A saved host is a local preference. Gating it on `canMutate` — which is
    // about the *host's* tmux server being writable — would make the host you
    // cannot reach the one you cannot remove.
    expect(commandAvailable(remove, context({ canMutate: false }))).toBe(true);
    // And it is offered only while the surface that shows the picker says so.
    expect(commandAvailable(remove, context({ hasHostProfile: false }))).toBe(false);
  });

  it("offers a row command only while a row surface publishes it", () => {
    const stage = commandRegistry.find((command) => command.id === "git.stage")!;
    const rename = commandRegistry.find((command) => command.id === "files.rename")!;
    // Nothing focused in a row surface: the palette shows them unavailable
    // rather than pretending it knows which row is meant.
    expect(commandAvailable(stage, context())).toBe(false);
    expect(commandAvailable(rename, context())).toBe(false);
    expect(commandAvailable(stage, context({ rowCommands: ["git.stage"] }))).toBe(true);
    // The publishing surface is the whole rule. It already applied read-only
    // state, submodule state and panel visibility before publishing, so a
    // second copy of those conditions here could only disagree with it.
    expect(commandAvailable(stage, context({ canMutate: false, rowCommands: ["git.stage"] }))).toBe(true);
    expect(commandAvailable(rename, context({ rowCommands: ["git.stage"] }))).toBe(false);
  });

  it("keeps every row action reachable from the palette, and never as a tmux confirmation", () => {
    const rowCommands = commandRegistry.filter((command) => command.requires === "row");
    // Every removed per-row button, as a searchable command.
    expect(rowCommands.map((command) => command.id)).toEqual([
      "agents.focusRow", "agents.renameRow", "agents.resumeRow",
      "files.open", "files.rename", "files.move", "files.duplicate", "files.download",
      "files.delete", "files.newFile", "files.newFolder", "files.refresh",
      "git.openDiff", "git.stage", "git.unstage", "git.discard",
    ]);
    expect(rowCommands.every((command) => !command.paletteHidden)).toBe(true);
    // `destructive` here means "confirm as a tmux action". Deleting a file and
    // discarding a diff confirm inside the surface that owns them; marking them
    // would produce a second, wrongly-worded prompt.
    expect(rowCommands.some((command) => command.destructive)).toBe(false);
    // No defaults: every free chord is spent, and these are bindable in the
    // shortcut editor like anything else in the one registry.
    expect(rowCommands.some((command) => command.defaults)).toBe(false);
    // The palette prints a group heading whenever the group changes going down
    // the registry, so each group has to occupy one contiguous run.
    const groupRuns = commandsForSurface("palette").reduce<string[]>(
      (runs, command) => runs.at(-1) === command.group ? runs : [...runs, command.group], [],
    );
    expect(groupRuns).toEqual([...new Set(groupRuns)]);
  });

  it("closes app-owned tabs while disconnected but never sends terminal-window close while frozen", () => {
    const close = commandRegistry.find((command) => command.id === "window.close")!;
    expect(commandAvailable(close, context({ canMutate: false, hasWindow: false, hasPane: false, hasTab: true }))).toBe(true);
    expect(commandAvailable(close, context({ canMutate: false, hasWindow: true, hasTab: true }))).toBe(false);
  });
});
