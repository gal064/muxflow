export type Platform = "mac" | "linux";

export interface CommandContext {
  canMutate: boolean;
  hasSession: boolean;
  hasWindow: boolean;
  hasPane: boolean;
  hasTab: boolean;
  canMoveSessionUp: boolean;
  canMoveSessionDown: boolean;
  canMoveTabLeft: boolean;
  canMoveTabRight: boolean;
  run(commandId: CommandId, target?: CommandTarget): void | Promise<void>;
}

export type CommandTarget =
  | { kind: "session"; id: string }
  | { kind: "terminalTab"; id: string }
  | { kind: "appTab"; id: string }
  | { kind: "pane"; id: string };

/** ⌘1–9 workspaces and ⌃1–9 tabs, the cmux keymap's positional selectors. */
export type IndexDigit = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9;
export type WorkspaceSelectCommandId = `workspace.select${IndexDigit}`;
export type TabSelectCommandId = `tab.select${IndexDigit}`;

export type CommandId =
  | "commands.show"
  | "workspaces.switch"
  | "shortcuts.configure"
  | "settings.show"
  | "view.toggleSidebar" | "view.togglePanel" | "view.showFiles" | "view.showGit"
  | "focus.workspaces" | "focus.tabs" | "focus.back" | "focus.forward"
  | "tab.previous" | "tab.next"
  | "agents.jumpUnread" | "agents.toggleSort"
  | "session.new" | "session.rename" | "session.moveLeft" | "session.moveRight" | "session.close"
  | WorkspaceSelectCommandId
  | "window.new" | "window.rename" | "window.moveLeft" | "window.moveRight" | "window.close"
  | TabSelectCommandId
  | "pane.splitRight" | "pane.splitDown" | "pane.focusLeft" | "pane.focusRight"
  | "pane.focusUp" | "pane.focusDown" | "pane.resizeLeft" | "pane.resizeRight"
  | "pane.resizeUp" | "pane.resizeDown" | "pane.zoom" | "pane.close"
  | "terminal.copy" | "terminal.paste" | "terminal.search" | "terminal.scrollBottom";

export interface CommandDefinition {
  id: CommandId;
  title: string;
  group: "Application" | "View" | "Agents" | "Workspace" | "Terminal tab" | "Pane" | "Terminal";
  defaults?: Partial<Record<Platform, string>>;
  mutates?: boolean;
  requires?: "session" | "window" | "pane" | "tab";
  destructive?: boolean;
  /**
   * Positional selectors (⌘4, ⌃7) are muscle memory, not things anyone searches
   * for by name. They stay in the registry — one registry is the rule, and the
   * shortcut editor must be able to rebind them — but eighteen near-identical
   * rows would bury every command the palette exists to surface.
   */
  paletteHidden?: boolean;
}

const DIGITS: readonly IndexDigit[] = [1, 2, 3, 4, 5, 6, 7, 8, 9];

/**
 * ⌘1–9 selects workspaces and ⌃1–9 selects tabs on macOS. On Linux ⌘ is the
 * window manager's Super key, so the same pair becomes Alt+N / Ctrl+N; they
 * must not both collapse onto Ctrl+N, which is why this is not a straight
 * Meta→Ctrl substitution like the rest of the keymap.
 */
const workspaceSelectCommands: readonly CommandDefinition[] = DIGITS.map((digit) => ({
  id: `workspace.select${digit}` as WorkspaceSelectCommandId,
  title: `Go to workspace ${digit}`,
  group: "Workspace",
  defaults: { mac: `Meta+${digit}`, linux: `Alt+${digit}` },
  paletteHidden: true,
}));

const tabSelectCommands: readonly CommandDefinition[] = DIGITS.map((digit) => ({
  id: `tab.select${digit}` as TabSelectCommandId,
  title: `Go to tab ${digit}`,
  group: "Terminal tab",
  defaults: { mac: `Ctrl+${digit}`, linux: `Ctrl+${digit}` },
  requires: "session",
  paletteHidden: true,
}));

export const commandRegistry: readonly CommandDefinition[] = [
  { id: "commands.show", title: "Show command palette", group: "Application", defaults: { mac: "Meta+K", linux: "Ctrl+K" } },
  { id: "workspaces.switch", title: "Switch workspace…", group: "Application", defaults: { mac: "Meta+P", linux: "Ctrl+P" } },
  { id: "shortcuts.configure", title: "Configure keyboard shortcuts", group: "Application" },
  { id: "settings.show", title: "Settings", group: "Application", defaults: { mac: "Meta+,", linux: "Ctrl+," } },
  { id: "view.toggleSidebar", title: "Toggle sidebar", group: "View", defaults: { mac: "Meta+B", linux: "Ctrl+B" } },
  { id: "view.togglePanel", title: "Toggle right panel", group: "View", defaults: { mac: "Meta+Alt+B", linux: "Ctrl+Alt+B" } },
  { id: "view.showFiles", title: "Show Files", group: "View", defaults: { mac: "Meta+Shift+E", linux: "Ctrl+Shift+E" } },
  { id: "view.showGit", title: "Show Source Control", group: "View", defaults: { mac: "Meta+Shift+G", linux: "Ctrl+Shift+G" } },
  { id: "focus.workspaces", title: "Focus workspace list", group: "View" },
  { id: "focus.tabs", title: "Focus tab strip", group: "View" },
  // The mock draws these as titlebar arrows. They are commands instead: the
  // phase caps resting chrome, and two arrows that are usually both disabled
  // are the first thing that cap should spend.
  { id: "focus.back", title: "Back to the previous terminal", group: "View", defaults: { mac: "Meta+[", linux: "Ctrl+[" } },
  { id: "focus.forward", title: "Forward again", group: "View", defaults: { mac: "Meta+]", linux: "Ctrl+]" } },
  { id: "tab.previous", title: "Previous tab", group: "Terminal tab", defaults: { mac: "Meta+Shift+[", linux: "Ctrl+Shift+[" }, requires: "tab" },
  { id: "tab.next", title: "Next tab", group: "Terminal tab", defaults: { mac: "Meta+Shift+]", linux: "Ctrl+Shift+]" }, requires: "tab" },
  { id: "agents.jumpUnread", title: "Jump to the agent that needs you", group: "Agents", defaults: { mac: "Meta+Shift+U", linux: "Ctrl+Shift+U" } },
  { id: "agents.toggleSort", title: "Toggle agent ordering (grouped ⇄ priority)", group: "Agents" },
  { id: "session.new", title: "New workspace", group: "Workspace", defaults: { mac: "Meta+N", linux: "Ctrl+Shift+N" }, mutates: true },
  { id: "session.rename", title: "Rename workspace", group: "Workspace", mutates: true, requires: "session" },
  { id: "session.moveLeft", title: "Move workspace up", group: "Workspace", mutates: true, requires: "session" },
  { id: "session.moveRight", title: "Move workspace down", group: "Workspace", mutates: true, requires: "session" },
  { id: "session.close", title: "Close workspace…", group: "Workspace", mutates: true, requires: "session", destructive: true },
  { id: "window.new", title: "New terminal tab", group: "Terminal tab", defaults: { mac: "Meta+T", linux: "Ctrl+Shift+T" }, mutates: true, requires: "session" },
  { id: "window.rename", title: "Rename terminal tab", group: "Terminal tab", mutates: true, requires: "window" },
  { id: "window.moveLeft", title: "Move current tab left", group: "Terminal tab", requires: "tab" },
  { id: "window.moveRight", title: "Move current tab right", group: "Terminal tab", requires: "tab" },
  { id: "window.close", title: "Close current tab", group: "Terminal tab", defaults: { mac: "Meta+W", linux: "Ctrl+Shift+W" }, requires: "tab", destructive: true },
  { id: "pane.splitRight", title: "Split pane right", group: "Pane", defaults: { mac: "Meta+D", linux: "Ctrl+Shift+D" }, mutates: true, requires: "pane" },
  { id: "pane.splitDown", title: "Split pane down", group: "Pane", defaults: { mac: "Meta+Shift+D", linux: "Ctrl+Alt+Shift+D" }, mutates: true, requires: "pane" },
  { id: "pane.focusLeft", title: "Focus pane left", group: "Pane", defaults: { mac: "Meta+Alt+ArrowLeft", linux: "Alt+ArrowLeft" }, mutates: true, requires: "pane" },
  { id: "pane.focusRight", title: "Focus pane right", group: "Pane", defaults: { mac: "Meta+Alt+ArrowRight", linux: "Alt+ArrowRight" }, mutates: true, requires: "pane" },
  { id: "pane.focusUp", title: "Focus pane up", group: "Pane", defaults: { mac: "Meta+Alt+ArrowUp", linux: "Alt+ArrowUp" }, mutates: true, requires: "pane" },
  { id: "pane.focusDown", title: "Focus pane down", group: "Pane", defaults: { mac: "Meta+Alt+ArrowDown", linux: "Alt+ArrowDown" }, mutates: true, requires: "pane" },
  { id: "pane.resizeLeft", title: "Resize pane left", group: "Pane", defaults: { linux: "Ctrl+Shift+ArrowLeft" }, mutates: true, requires: "pane" },
  { id: "pane.resizeRight", title: "Resize pane right", group: "Pane", defaults: { linux: "Ctrl+Shift+ArrowRight" }, mutates: true, requires: "pane" },
  { id: "pane.resizeUp", title: "Resize pane up", group: "Pane", defaults: { linux: "Ctrl+Shift+ArrowUp" }, mutates: true, requires: "pane" },
  { id: "pane.resizeDown", title: "Resize pane down", group: "Pane", defaults: { linux: "Ctrl+Shift+ArrowDown" }, mutates: true, requires: "pane" },
  { id: "pane.zoom", title: "Toggle pane zoom", group: "Pane", defaults: { mac: "Meta+Shift+Enter", linux: "Ctrl+Shift+Enter" }, mutates: true, requires: "pane" },
  { id: "pane.close", title: "Close pane…", group: "Pane", mutates: true, requires: "pane", destructive: true },
  { id: "terminal.copy", title: "Copy terminal selection", group: "Terminal", defaults: { mac: "Meta+C", linux: "Ctrl+Shift+C" }, requires: "pane" },
  { id: "terminal.paste", title: "Paste into terminal", group: "Terminal", defaults: { mac: "Meta+V", linux: "Ctrl+Shift+V" }, requires: "pane" },
  { id: "terminal.search", title: "Find in terminal", group: "Terminal", defaults: { mac: "Meta+F", linux: "Ctrl+Shift+F" }, requires: "pane" },
  { id: "terminal.scrollBottom", title: "Scroll terminal to bottom", group: "Terminal", requires: "pane" },
  ...workspaceSelectCommands,
  ...tabSelectCommands,
];

export type ShortcutOverrides = Partial<Record<CommandId, string | null>>;
/**
 * `palette` is the searchable surface; `menu` and `context` are the built
 * menus; `shortcuts` is the editor, which must reach everything bindable.
 */
export type CommandSurface = "palette" | "menu" | "context" | "shortcuts";

export function commandsForSurface(surface: CommandSurface): readonly CommandDefinition[] {
  if (surface === "shortcuts") return commandRegistry;
  return commandRegistry.filter((command) => !command.paletteHidden);
}

/** The 1-based position a positional selector refers to, if it is one. */
export function selectionIndex(commandId: CommandId, prefix: "workspace.select" | "tab.select"): number | undefined {
  if (!commandId.startsWith(prefix)) return undefined;
  const digit = Number.parseInt(commandId.slice(prefix.length), 10);
  return Number.isInteger(digit) && digit >= 1 && digit <= 9 ? digit : undefined;
}

export function commandAvailable(command: CommandDefinition, context: CommandContext): boolean {
  if (command.mutates && !context.canMutate) return false;
  if (command.id === "window.close" && context.hasWindow && !context.canMutate) return false;
  if (command.requires === "session" && !context.hasSession) return false;
  if (command.requires === "window" && !context.hasWindow) return false;
  if (command.requires === "pane" && !context.hasPane) return false;
  if (command.requires === "tab" && !context.hasTab) return false;
  if ((command.id === "window.moveLeft" || command.id === "window.moveRight") && context.hasWindow && !context.canMutate) return false;
  if (command.id === "window.moveLeft" && !context.canMoveTabLeft) return false;
  if (command.id === "window.moveRight" && !context.canMoveTabRight) return false;
  if (command.id === "session.moveLeft" && !context.canMoveSessionUp) return false;
  if (command.id === "session.moveRight" && !context.canMoveSessionDown) return false;
  return true;
}

export function shortcutFor(command: CommandDefinition, platform: Platform, overrides: ShortcutOverrides): string | undefined {
  const override = overrides[command.id];
  if (override === null) return undefined;
  if (override !== undefined) return isSafeShortcut(override) ? override : undefined;
  return command.defaults?.[platform];
}

export function normalizeShortcut(shortcut: string): string {
  const order = ["Ctrl", "Alt", "Shift", "Meta"];
  const parts = shortcut.split("+").filter(Boolean);
  const modifiers = order.filter((part) => parts.includes(part));
  const rawKey = parts.find((part) => !order.includes(part));
  const key = rawKey?.length === 1 ? rawKey.toUpperCase() : rawKey;
  return [...modifiers, key ?? ""].filter(Boolean).join("+");
}

export function isSafeShortcut(shortcut: string): boolean {
  const normalized = normalizeShortcut(shortcut);
  const parts = normalized.split("+");
  const key = parts.at(-1) ?? "";
  const hasCommandModifier = parts.includes("Ctrl") || parts.includes("Alt") || parts.includes("Meta");
  return hasCommandModifier && key !== "" && !["Control", "Alt", "Shift", "Meta"].includes(key);
}

export function unsafeShortcutBindings(overrides: ShortcutOverrides): CommandId[] {
  return commandRegistry.flatMap((command) => {
    const value = overrides[command.id];
    return typeof value === "string" && !isSafeShortcut(value) ? [command.id] : [];
  });
}

/**
 * The character a punctuation key produces with no Shift held.
 *
 * `event.key` reports what the keystroke *typed*, so ⌘⇧[ arrives as `{` and
 * never matches a binding written `Meta+Shift+[` — which is how the cmux
 * keymap's ⌘⇧[ / ⌘⇧] tab shortcuts came to be drawn in the palette while doing
 * nothing at all. `event.code` names the physical key, so a Shift-modified
 * binding can be matched against the key rather than the glyph. Only the US
 * punctuation row needs this; letters and digits already round-trip, and the
 * fallback is `event.key` for any layout `code` does not describe.
 */
const UNSHIFTED_BY_CODE: Record<string, string> = {
  BracketLeft: "[", BracketRight: "]", Semicolon: ";", Quote: "'", Backquote: "`",
  Comma: ",", Period: ".", Slash: "/", Backslash: "\\", Minus: "-", Equal: "=",
};

export function shortcutFromEvent(event: KeyboardEvent): string {
  const parts = [event.ctrlKey && "Ctrl", event.altKey && "Alt", event.shiftKey && "Shift", event.metaKey && "Meta"]
    .filter((part): part is string => Boolean(part));
  const unshifted = event.shiftKey ? UNSHIFTED_BY_CODE[event.code] : undefined;
  const raw = unshifted ?? event.key;
  const key = raw.length === 1 ? raw.toUpperCase() : raw;
  if (!["Control", "Alt", "Shift", "Meta"].includes(key)) parts.push(key);
  return normalizeShortcut(parts.join("+"));
}

export function commandForKeyboardEvent(
  event: KeyboardEvent,
  platform: Platform,
  overrides: ShortcutOverrides,
): CommandDefinition | undefined {
  if (keyboardEventIsComposing(event)) return undefined;
  const pressed = shortcutFromEvent(event);
  const matches = commandRegistry.filter((command) => {
    const shortcut = shortcutFor(command, platform, overrides);
    return shortcut !== undefined && normalizeShortcut(shortcut) === pressed;
  });
  return matches.length === 1 ? matches[0] : undefined;
}

/**
 * Browsers disagree about whether the key event that commits an IME sequence
 * still has `isComposing` set. WebKit also reports the legacy 229 key code for
 * that event, so both signals must be honored before invoking an app shortcut.
 */
export function keyboardEventIsComposing(
  event: Pick<KeyboardEvent, "isComposing" | "keyCode">,
): boolean {
  return event.isComposing || event.keyCode === 229;
}

export function globalShortcutAllowed(event: Pick<KeyboardEvent, "target">, overlayOpen: boolean): boolean {
  if (overlayOpen) return false;
  const target = event.target as { closest?: (selector: string) => unknown } | null;
  if (!target?.closest) return true;
  const editable = target.closest("input, textarea, select, [contenteditable=true], [role=textbox]");
  return !editable || Boolean(target.closest("[data-terminal-surface]"));
}

export interface ShortcutCollision {
  shortcut: string;
  commandIds: CommandId[];
}

export function shortcutCollisions(platform: Platform, overrides: ShortcutOverrides): ShortcutCollision[] {
  const bindings = new Map<string, CommandId[]>();
  for (const command of commandRegistry) {
    const shortcut = shortcutFor(command, platform, overrides);
    if (!shortcut) continue;
    const normalized = normalizeShortcut(shortcut);
    bindings.set(normalized, [...(bindings.get(normalized) ?? []), command.id]);
  }
  return [...bindings.entries()]
    .filter(([, commandIds]) => commandIds.length > 1)
    .map(([shortcut, commandIds]) => ({ shortcut, commandIds }));
}

export function currentPlatform(userAgent = navigator.userAgent): Platform {
  return /Mac|iPhone|iPad/.test(userAgent) ? "mac" : "linux";
}
