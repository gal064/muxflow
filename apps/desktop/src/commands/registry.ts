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

export type CommandId =
  | "commands.show"
  | "shortcuts.configure"
  | "view.toggleExplorer" | "view.toggleAgents" | "view.showExplorer" | "view.showGit"
  | "focus.workspaces" | "focus.tabs"
  | "session.new" | "session.rename" | "session.moveLeft" | "session.moveRight" | "session.close"
  | "window.new" | "window.rename" | "window.moveLeft" | "window.moveRight" | "window.close"
  | "pane.splitRight" | "pane.splitDown" | "pane.focusLeft" | "pane.focusRight"
  | "pane.focusUp" | "pane.focusDown" | "pane.resizeLeft" | "pane.resizeRight"
  | "pane.resizeUp" | "pane.resizeDown" | "pane.zoom" | "pane.close"
  | "terminal.copy" | "terminal.paste" | "terminal.search" | "terminal.scrollBottom";

export interface CommandDefinition {
  id: CommandId;
  title: string;
  group: "Application" | "View" | "Workspace" | "Terminal tab" | "Pane" | "Terminal";
  defaults?: Partial<Record<Platform, string>>;
  mutates?: boolean;
  requires?: "session" | "window" | "pane" | "tab";
  destructive?: boolean;
}

export const commandRegistry: readonly CommandDefinition[] = [
  { id: "commands.show", title: "Show command palette", group: "Application", defaults: { mac: "Meta+Shift+P", linux: "Ctrl+Shift+P" } },
  { id: "shortcuts.configure", title: "Configure keyboard shortcuts", group: "Application" },
  { id: "view.toggleExplorer", title: "Toggle Explorer/Git sidebar", group: "View", defaults: { mac: "Meta+B", linux: "Ctrl+B" } },
  { id: "view.toggleAgents", title: "Toggle agent sidebar", group: "View", defaults: { mac: "Meta+Shift+A", linux: "Ctrl+Shift+A" } },
  { id: "view.showExplorer", title: "Show Explorer", group: "View", defaults: { mac: "Meta+Shift+E", linux: "Ctrl+Shift+E" } },
  { id: "view.showGit", title: "Show Source Control", group: "View", defaults: { mac: "Meta+Shift+G", linux: "Ctrl+Shift+G" } },
  { id: "focus.workspaces", title: "Focus workspace rail", group: "View" },
  { id: "focus.tabs", title: "Focus tab strip", group: "View" },
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
];

export type ShortcutOverrides = Partial<Record<CommandId, string | null>>;
export type CommandSurface = "palette" | "menu" | "toolbar" | "context";

const toolbarCommandIds: readonly CommandId[] = [
  "commands.show", "view.toggleExplorer", "view.toggleAgents", "session.new", "session.rename", "session.close", "window.new",
  "pane.splitRight", "pane.splitDown", "pane.zoom",
];

export function commandsForSurface(surface: CommandSurface): readonly CommandDefinition[] {
  if (surface === "toolbar") return toolbarCommandIds.map((id) => commandRegistry.find((command) => command.id === id)!);
  return commandRegistry;
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

export function shortcutFromEvent(event: KeyboardEvent): string {
  const parts = [event.ctrlKey && "Ctrl", event.altKey && "Alt", event.shiftKey && "Shift", event.metaKey && "Meta"]
    .filter((part): part is string => Boolean(part));
  const key = event.key.length === 1 ? event.key.toUpperCase() : event.key;
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
