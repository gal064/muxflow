/**
 * Whether terminal *content* is mirrored into the accessibility tree.
 *
 * Phase 12 turned xterm's `screenReaderMode` off (P12-U002): it allocates a
 * string and dispatches an emitter event per printed codepoint and rewrites a
 * DOM mirror of every row on every render, which an agent TUI repainting at
 * 1 Hz pays thousands of times a second. That left terminal output unreadable
 * to a screen reader, and the fix was explicitly deferred to Phase 11's
 * settings surface, which now exists.
 *
 * It is a module-level value rather than a prop because a renderer reads it
 * once, at construction: making it reactive would mean tearing down and
 * rebuilding every live terminal — losing scrollback and forcing a reseed —
 * every time the checkbox moved. Panes pick the new value up as they are
 * created, and the setting says so.
 */
let mirrorTerminalContent = false;

export function setTerminalScreenReaderMode(enabled: boolean): void {
  mirrorTerminalContent = enabled;
}

export function terminalScreenReaderMode(): boolean {
  return mirrorTerminalContent;
}
