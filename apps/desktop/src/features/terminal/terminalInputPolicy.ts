import { keyboardEventIsComposing, type Platform } from "../../commands/registry";
import type { TerminalRenderer } from "./TerminalRenderer";

type TerminalKeyEvent = Pick<KeyboardEvent,
  "altKey" | "code" | "ctrlKey" | "isComposing" | "key" | "keyCode" | "metaKey" | "shiftKey"
>;

export interface TerminalKeyContext {
  alternateScreen: boolean;
  applicationCursorKeys: boolean;
  currentCommand: string;
  platform: Platform;
}

/** Returns bytes for the terminal shortcuts Muxflow owns, or nothing. */
export function translateTerminalKey(event: TerminalKeyEvent, context: TerminalKeyContext): string | undefined {
  if (keyboardEventIsComposing(event)) return undefined;
  // xterm.js 6 has no Ctrl+/ mapping; legacy terminals alias Ctrl+/ and Ctrl+_
  // to 0x1f (US). Ctrl+_ is left to xterm, which already emits 0x1f.
  //
  // `code` is only a fallback for a key that reports no character at all:
  // `code` is US-positional, and on a QWERTZ layout `Slash` is the `-`/`_`
  // key, so matching it outright would send 0x1f for an unrelated Ctrl+-.
  const slashKey = event.key === "/" || (event.key.length !== 1 && event.code === "Slash");
  if (event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey && slashKey) {
    return "\u001f";
  }
  // Shift-Enter is what Ghostty sends for it: the CSI-u encoding, for every
  // app. Both agent composers read it as "insert a newline"; xterm.js on its
  // own sends a bare carriage return, which they read as submit. A
  // Control-J special case keyed on tmux's foreground command being `codex`
  // used to cover one composer and missed Claude Code, whose process name is
  // `claude` or `node` depending on how it was installed.
  if (event.key === "Enter" && event.shiftKey && !event.metaKey && !event.ctrlKey && !event.altKey) {
    return "\u001b[13;2u";
  }
  // Command-Arrow always means line start/end; the screen only picks the
  // encoding. Normal-screen shells and wrappers read the conventional readline
  // Control-A/Control-E, while full-screen programs — including the agent
  // composers, which run on the alternate screen — read Home/End, sent as SS3
  // when the program asked for application cursor keys (DECCKM) and as CSI
  // otherwise. A process-name allowlist made an ordinary wrapper silently lose
  // the shortcut and could never enumerate every interactive shell.
  if (context.platform !== "mac"
    || !event.metaKey || event.shiftKey || event.ctrlKey || event.altKey) return undefined;
  const cursorKeyPrefix = context.applicationCursorKeys ? "O" : "[";
  if (event.key === "ArrowLeft") return context.alternateScreen ? `\u001b${cursorKeyPrefix}H` : "\u0001";
  if (event.key === "ArrowRight") return context.alternateScreen ? `\u001b${cursorKeyPrefix}F` : "\u0005";
  return undefined;
}

/** Copies one finalized selection without allowing an empty click to clear the clipboard. */
export async function copyCompletedTerminalSelection(
  renderer: Pick<TerminalRenderer, "getSelection" | "hasSelection">,
  enabled: boolean,
  write: (text: string) => void | Promise<void>,
): Promise<boolean> {
  if (!enabled || !renderer.hasSelection()) return false;
  const selection = renderer.getSelection();
  if (!selection) return false;
  await write(selection);
  return true;
}

interface CopyOnSelectOptions {
  renderer: Pick<TerminalRenderer, "getSelection" | "hasSelection" | "onSelectionChange">;
  enabled: () => boolean;
  write: (text: string) => void | Promise<void>;
  onError: (error: unknown) => void;
}

/**
 * Copies xterm's current non-empty selection whenever xterm says it changed.
 * A click that merely leaves an old selection in place emits no change; a
 * click that clears it is rejected by `copyCompletedTerminalSelection`.
 */
export function installTerminalCopyOnSelect({
  renderer,
  enabled,
  write,
  onError,
}: CopyOnSelectOptions): () => void {
  return renderer.onSelectionChange(() => {
    void copyCompletedTerminalSelection(renderer, enabled(), write).catch(onError);
  });
}
