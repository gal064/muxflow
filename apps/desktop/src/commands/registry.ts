export type Platform = "mac" | "linux";

export interface CommandContext {
  canMutate: boolean;
  /** Whether any host offered by the New workspace dialog can create one. */
  canCreateWorkspace: boolean;
  hasSession: boolean;
  hasWindow: boolean;
  hasPane: boolean;
  hasTab: boolean;
  canMoveSessionUp: boolean;
  canMoveSessionDown: boolean;
  canMoveTabLeft: boolean;
  canMoveTabRight: boolean;
  /** Whether Settings is showing a picked saved host the store will part with. */
  hasHostProfile: boolean;
  /** Whether the sidebar is currently filtered to pinned workspaces. */
  pinnedOnly: boolean;
  /**
   * Row commands (`requires: "row"`) published by whichever row surface holds
   * the row the user last pointed at. See `rowCommands.ts`: the publishing
   * surface is the only thing that knows whether "Stage" applies to its current
   * row, so its answer *is* the availability rule rather than an input to one.
   */
  rowCommands: readonly CommandId[];
  run(commandId: CommandId, target?: CommandTarget): void | Promise<void>;
}

export type CommandTarget =
  | { kind: "session"; id: string; scope: HostScopeToken }
  | { kind: "terminalTab"; id: string; scope: HostScopeToken }
  | { kind: "appTab"; id: string; scope: HostScopeToken }
  | { kind: "pane"; id: string; scope: HostScopeToken }
  /** The active pane/tab close surface captured by an open menu. */
  | { kind: "focusedSurface"; paneId: string; scope: HostScopeToken };

/** ⌘1–9 workspaces and ⌃1–9 tabs, the cmux keymap's positional selectors. */
export type IndexDigit = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9;
export type WorkspaceSelectCommandId = `workspace.select${IndexDigit}`;
export type TabSelectCommandId = `tab.select${IndexDigit}`;

export type CommandId =
  | "commands.show"
  | "workspaces.switch"
  | "shortcuts.configure"
  | "settings.show"
  | "host.delete"
  | "view.toggleSidebar" | "view.togglePanel" | "view.showFiles" | "view.showGit"
  | "focus.workspaces" | "focus.tabs" | "focus.back" | "focus.forward"
  | "tab.previous" | "tab.next"
  | "agents.jumpUnread" | "agents.toggleSort"
  | "session.new" | "session.rename" | "session.moveLeft" | "session.moveRight" | "session.close"
  | "workspaces.showPinnedOnly" | "workspaces.showAll"
  | WorkspaceSelectCommandId
  | "window.new" | "window.rename" | "window.moveLeft" | "window.moveRight" | "window.close"
  | TabSelectCommandId
  | "pane.splitRight" | "pane.splitDown" | "pane.focusLeft" | "pane.focusRight"
  | "pane.focusUp" | "pane.focusDown" | "pane.resizeLeft" | "pane.resizeRight"
  | "pane.resizeUp" | "pane.resizeDown" | "pane.zoom" | "pane.close"
  | "terminal.copy" | "terminal.paste" | "terminal.search" | "terminal.scrollBottom"
  | RowCommandId;

/**
 * Actions that need a row as their subject. The Explorer, Git and the agents
 * list publish these for whichever of their rows the user last pointed at, so
 * that the palette can reach the actions that used to be per-row buttons —
 * 11.4's rule is that a removed button becomes a palette command *and* a
 * context-menu item *and* a bindable shortcut, and these were only the last two.
 */
export type RowCommandId =
  | "files.open" | "files.rename" | "files.move" | "files.duplicate" | "files.download"
  | "files.delete" | "files.newFile" | "files.newFolder" | "files.refresh"
  | "git.openDiff" | "git.stage" | "git.unstage" | "git.discard"
  | "agents.focusRow" | "agents.renameRow" | "agents.resumeRow";

export interface CommandDefinition {
  id: CommandId;
  title: string;
  group: "Application" | "View" | "Agents" | "Workspace" | "Terminal tab" | "Pane" | "Terminal"
    | "Files" | "Source control";
  defaults?: Partial<Record<Platform, string>>;
  mutates?: boolean;
  requires?: "session" | "window" | "pane" | "tab" | "row" | "hostProfile";
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
  // `requires: "hostProfile"` for the same reason `requires: "row"` exists: the
  // surface that owns the picker is the only thing that knows which host is
  // picked and whether the store will part with it. And deliberately not
  // `mutates`, which is about the *tmux server* being writable — a saved host is
  // a local preference that stays deletable while the host is unreachable. No
  // `destructive` either: in this registry that flag routes a command through
  // the tmux confirmation builder, and this one carries its own dialog, exactly
  // as `files.delete` and `git.discard` do.
  { id: "host.delete", title: "Delete the selected saved host…", group: "Application", requires: "hostProfile" },
  { id: "view.toggleSidebar", title: "Toggle sidebar", group: "View", defaults: { mac: "Meta+B", linux: "Ctrl+B" } },
  // ⌘L, not ⌥⌘B. The Option-modified letter below is exactly the class of
  // binding that macOS rewrites into a different glyph, and the panel is a
  // twice-a-minute toggle that had a binding nobody could press. While a
  // terminal pane is focused this now takes ⌘L before the shell sees it —
  // the same trade ⌘B already makes for the sidebar.
  { id: "view.togglePanel", title: "Toggle right panel", group: "View", defaults: { mac: "Meta+L", linux: "Ctrl+L" } },
  { id: "view.showFiles", title: "Show Files", group: "View", defaults: { mac: "Meta+Shift+E", linux: "Ctrl+Shift+E" } },
  { id: "view.showGit", title: "Show Source Control", group: "View", defaults: { mac: "Meta+Shift+G", linux: "Ctrl+Shift+G" } },
  { id: "focus.workspaces", title: "Focus workspace list", group: "View" },
  { id: "focus.tabs", title: "Focus tab strip", group: "View" },
  // The titlebar's Back and Forward arrows run these same commands; the
  // history they walk covers terminal and document tabs alike.
  { id: "focus.back", title: "Back", group: "View", defaults: { mac: "Meta+[", linux: "Ctrl+[" } },
  { id: "focus.forward", title: "Forward", group: "View", defaults: { mac: "Meta+]", linux: "Ctrl+]" } },
  { id: "agents.jumpUnread", title: "Jump to the agent that needs you", group: "Agents", defaults: { mac: "Meta+Shift+U", linux: "Ctrl+Shift+U" } },
  { id: "agents.toggleSort", title: "Cycle agent ordering (priority → workspace → pinned)", group: "Agents" },
  // Kept adjacent to the rest of the Agents group: the palette prints a group
  // heading whenever the group changes down the list, so a group split across
  // two places in this array would print its heading twice.
  { id: "agents.focusRow", title: "Focus the selected agent's pane", group: "Agents", requires: "row" },
  { id: "agents.renameRow", title: "Rename the selected agent…", group: "Agents", requires: "row" },
  { id: "agents.resumeRow", title: "Resume the selected agent", group: "Agents", requires: "row" },
  { id: "session.new", title: "New workspace", group: "Workspace", defaults: { mac: "Meta+N", linux: "Ctrl+Shift+N" }, mutates: true },
  { id: "session.rename", title: "Rename workspace", group: "Workspace", mutates: true, requires: "session" },
  { id: "session.moveLeft", title: "Move workspace up", group: "Workspace", mutates: true, requires: "session" },
  { id: "session.moveRight", title: "Move workspace down", group: "Workspace", mutates: true, requires: "session" },
  // The sidebar's filter, as two commands rather than one toggle: the palette
  // has no dynamic titles, and "Toggle pinned workspaces" would leave the user
  // guessing which way it goes. Only the one that applies is ever offered.
  // Neither `mutates`: this is a view decision and sends tmux nothing.
  { id: "workspaces.showPinnedOnly", title: "Show pinned workspaces only", group: "Workspace" },
  { id: "workspaces.showAll", title: "Show all workspaces", group: "Workspace" },
  { id: "session.close", title: "Close workspace…", group: "Workspace", mutates: true, requires: "session", destructive: true },
  // With the rest of the Terminal tab group, not up beside the View commands:
  // the palette's headings assume one contiguous run per group, and these two
  // sitting apart printed a second "Terminal tab" heading.
  { id: "tab.previous", title: "Previous tab", group: "Terminal tab", defaults: { mac: "Meta+Shift+[", linux: "Ctrl+Shift+[" }, requires: "tab" },
  { id: "tab.next", title: "Next tab", group: "Terminal tab", defaults: { mac: "Meta+Shift+]", linux: "Ctrl+Shift+]" }, requires: "tab" },
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
  { id: "pane.zoom", title: "Toggle pane zoom", group: "Pane", defaults: { mac: "Meta+E", linux: "Ctrl+Shift+Enter" }, mutates: true, requires: "pane" },
  { id: "pane.close", title: "Close pane…", group: "Pane", mutates: true, requires: "pane", destructive: true },
  { id: "terminal.copy", title: "Copy terminal selection", group: "Terminal", defaults: { mac: "Meta+C", linux: "Ctrl+Shift+C" }, requires: "pane" },
  { id: "terminal.paste", title: "Paste into terminal", group: "Terminal", defaults: { mac: "Meta+V", linux: "Ctrl+Shift+V" }, requires: "pane" },
  { id: "terminal.search", title: "Find in terminal", group: "Terminal", defaults: { mac: "Meta+F", linux: "Ctrl+Shift+F" }, requires: "pane" },
  { id: "terminal.scrollBottom", title: "Scroll terminal to bottom", group: "Terminal", requires: "pane" },
  // Row commands. `destructive` is deliberately absent from the two that
  // destroy something: in this registry that flag means "route through the tmux
  // confirmation builder", and these two already carry their own confirmation
  // in the surface that owns them (the Explorer's delete dialog, Git's discard
  // dialog). Marking them here would produce a second, tmux-shaped prompt for a
  // filesystem action. The palette still reaches the same guarded path.
  { id: "files.open", title: "Open the selected file", group: "Files", requires: "row" },
  { id: "files.rename", title: "Rename the selected file…", group: "Files", requires: "row" },
  { id: "files.move", title: "Move the selected file…", group: "Files", requires: "row" },
  { id: "files.duplicate", title: "Duplicate the selected file…", group: "Files", requires: "row" },
  { id: "files.download", title: "Download the selected file…", group: "Files", requires: "row" },
  { id: "files.delete", title: "Delete the selected file…", group: "Files", requires: "row" },
  { id: "files.newFile", title: "New file in the Explorer…", group: "Files", requires: "row" },
  { id: "files.newFolder", title: "New folder in the Explorer…", group: "Files", requires: "row" },
  { id: "files.refresh", title: "Refresh the file tree", group: "Files", requires: "row" },
  { id: "git.openDiff", title: "Open the selected change's diff", group: "Source control", requires: "row" },
  { id: "git.stage", title: "Stage the selected change", group: "Source control", requires: "row" },
  { id: "git.unstage", title: "Unstage the selected change", group: "Source control", requires: "row" },
  { id: "git.discard", title: "Discard the selected change…", group: "Source control", requires: "row" },
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
  // A row command is available exactly while a row surface publishes it. That
  // one check replaces every proxy condition — connection state, panel
  // visibility, whether the row is a submodule — because the surface applied
  // all of them before publishing, and a second, weaker copy of those rules
  // here is how the palette and the context menu would drift apart.
  if (command.requires === "row") return context.rowCommands.includes(command.id);
  if (command.requires === "hostProfile") return context.hasHostProfile;
  // The filter's two commands are each other's inverse, so exactly one of them
  // is ever the thing to run; offering both would put a no-op in the palette.
  if (command.id === "workspaces.showPinnedOnly") return !context.pinnedOnly;
  if (command.id === "workspaces.showAll") return context.pinnedOnly;
  // Unlike every other ambient mutation, workspace creation has an explicit
  // host picker and is available when any shown host is writable, even if the
  // host currently on screen is not.
  if (command.id === "session.new") return context.canCreateWorkspace;
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

/** The binding a command would have with the keymap emptied, by id. */
export function defaultShortcutFor(commandId: CommandId, platform: Platform): string | undefined {
  const command = commandRegistry.find((item) => item.id === commandId);
  return command && shortcutFor(command, platform, {});
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
 * The character a key produces with no modifier that rewrites it.
 *
 * `event.key` reports what the keystroke *typed*, and two modifiers change what
 * that is:
 *
 * - **Shift.** ⌘⇧[ arrives as `{` and never matched a binding written
 *   `Meta+Shift+[`, which is how the cmux keymap's tab shortcuts came to be
 *   drawn in the palette while doing nothing at all.
 * - **Option, on macOS.** ⌥B arrives as `∫`, ⌥N as `˜`, ⌥E as `´`. So
 *   `Meta+Alt+B` — ⌥⌘B, the right panel, straight out of the plan's keymap —
 *   normalized to `Meta+Alt+∫` and could never fire either. Measured on the
 *   packaged app: the titlebar toggle worked, the shortcut did nothing.
 *
 * `event.code` names the physical key, so both cases are answered the same way:
 * when a rewriting modifier is held, resolve the key from the code. Letters and
 * digits get their own arms because `KeyB` → `B` and `Digit4` → `4` are the
 * whole mapping; only the US punctuation row needs a table. `event.key` remains
 * the fallback for any layout `code` does not describe.
 */
const UNSHIFTED_BY_CODE: Record<string, string> = {
  BracketLeft: "[", BracketRight: "]", Semicolon: ";", Quote: "'", Backquote: "`",
  Comma: ",", Period: ".", Slash: "/", Backslash: "\\", Minus: "-", Equal: "=",
};

/** What a physical key means when a modifier has rewritten what it typed. */
export function keyFromCode(code: string): string | undefined {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  return UNSHIFTED_BY_CODE[code];
}

/** A modified top-row digit, including WebKit events whose `code` is blank. */
function positionalDigitFromEvent(event: Pick<KeyboardEvent, "altKey" | "code" | "ctrlKey" | "key" | "keyCode" | "metaKey">): string | undefined {
  if (!event.altKey && !event.ctrlKey && !event.metaKey) return undefined;
  if (/^Digit[0-9]$/.test(event.code)) return event.code.slice(5);
  if (/^[0-9]$/.test(event.key)) return event.key;
  // WKWebView can pair a Control-character `key` with no physical `code`, but
  // its legacy code still identifies the top number row. Keep this fallback
  // deliberately narrower than numpad and unmodified keys.
  return event.keyCode >= 48 && event.keyCode <= 57 ? String(event.keyCode - 48) : undefined;
}

export function shortcutFromEvent(event: KeyboardEvent): string {
  const parts = [event.ctrlKey && "Ctrl", event.altKey && "Alt", event.shiftKey && "Shift", event.metaKey && "Meta"]
    .filter((part): part is string => Boolean(part));
  // WebKit can report a control character in `key` for Control-number while
  // xterm's hidden textarea has focus. Positional selectors are physical digit
  // keys, so resolve that row from `code` under Ctrl/Meta too. Other Control
  // keys continue through `event.key` and are not intercepted unless a command
  // actually binds them.
  const positionalDigit = positionalDigitFromEvent(event);
  const rewritten = positionalDigit ?? (event.shiftKey || event.altKey ? keyFromCode(event.code) : undefined);
  const raw = rewritten ?? event.key;
  const key = raw.length === 1 ? raw.toUpperCase() : raw;
  if (!["Control", "Alt", "Shift", "Meta"].includes(key)) parts.push(key);
  return normalizeShortcut(parts.join("+"));
}

export function commandForKeyboardEvent(
  event: KeyboardEvent,
  platform: Platform,
  overrides: ShortcutOverrides,
): CommandDefinition | undefined {
  // WebKit sometimes labels a non-composing modified digit with legacy 229.
  // A real composition still wins; only a physically identified selector may
  // bypass the legacy-code half of the composition guard.
  if (event.isComposing || (event.keyCode === 229 && positionalDigitFromEvent(event) === undefined)) return undefined;
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

export function globalShortcutAllowed(
  event: Pick<KeyboardEvent, "altKey" | "code" | "ctrlKey" | "key" | "keyCode" | "metaKey" | "target">,
  overlayOpen: boolean,
  commandId?: CommandId,
): boolean {
  if (overlayOpen) return false;
  // Positional navigation is global by design: switching a workspace or tab
  // must work while Monaco or another ordinary editor owns focus. Modal
  // surfaces still win above, including the shortcut recorder itself.
  if (positionalDigitFromEvent(event)
    && commandId
    && (selectionIndex(commandId, "workspace.select") || selectionIndex(commandId, "tab.select"))) return true;
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

/**
 * Migrates a keymap written by builds that allowed conflicting bindings.
 *
 * Only persisted overrides are removed; canonical defaults are never guessed
 * away. Removing one override can reveal that command's default and therefore
 * another collision, so repair continues until the active keymap is unique.
 */
export function repairShortcutCollisions(
  platform: Platform,
  overrides: ShortcutOverrides,
  onDisplaced: (bindings: readonly { commandId: CommandId; shortcut: string }[]) => void = () => undefined,
): ShortcutOverrides {
  const repaired = { ...overrides };
  const displaced: { commandId: CommandId; shortcut: string }[] = [];
  let changed = false;
  while (true) {
    const collision = shortcutCollisions(platform, repaired)[0];
    if (!collision) {
      if (displaced.length > 0) onDisplaced(displaced);
      return changed ? repaired : overrides;
    }
    const explicit = collision.commandIds.filter((commandId) =>
      Object.prototype.hasOwnProperty.call(repaired, commandId) && repaired[commandId] !== null);
    // One explicit customization wins over defaults introduced later. If two
    // old customizations conflict, preserve the first registry binding and
    // report the one that must be disabled so the repair is recoverable.
    const keep = explicit[0];
    for (const commandId of collision.commandIds) {
      if (commandId === keep) continue;
      const prior = repaired[commandId];
      // A command with no override loses its *default* here, and the explicit
      // null that records that survives every later load. Reporting only the
      // customizations someone typed left the rest disabled with no trace of
      // what they used to be, so name the default that was taken away too.
      const lost = typeof prior === "string" ? prior : defaultShortcutFor(commandId, platform);
      if (lost) displaced.push({ commandId, shortcut: lost });
      repaired[commandId] = null;
      changed = true;
    }
    if (!keep && collision.commandIds.length > 0) {
      // Defensive only: platform defaults are asserted collision-free.
      repaired[collision.commandIds[0]] = null;
      changed = true;
    }
  }
}

export function currentPlatform(userAgent = navigator.userAgent): Platform {
  return /Mac|iPhone|iPad/.test(userAgent) ? "mac" : "linux";
}
import type { HostScopeToken } from "../features/shell/hostScope";
