import { keyboardEventIsComposing, type Platform } from "../../commands/registry";
import type { TerminalRenderer } from "./TerminalRenderer";

type TerminalKeyEvent = Pick<KeyboardEvent,
  "altKey" | "ctrlKey" | "isComposing" | "key" | "keyCode" | "metaKey" | "shiftKey"
>;

export interface TerminalKeyContext {
  alternateScreen: boolean;
  currentCommand: string;
  platform: Platform;
}

/** Returns bytes for the two terminal shortcuts Muxflow owns, or nothing. */
export function translateTerminalKey(event: TerminalKeyEvent, context: TerminalKeyContext): string | undefined {
  if (keyboardEventIsComposing(event)) return undefined;
  const command = context.currentCommand.split("/").at(-1)?.toLowerCase() ?? "";
  // tmux's current command is foreground evidence. Agent records are not:
  // process-tree discovery deliberately keeps suspended/background agents
  // present, so using a record here can steal Shift-Enter from the next app.
  if (command === "codex" && event.key === "Enter" && event.shiftKey
    && !event.metaKey && !event.ctrlKey && !event.altKey) {
    return "\n";
  }
  // The alternate screen is the behavioral boundary: full-screen TUIs own
  // their Command-Arrows, while normal-screen shells, wrappers, and agent
  // composers all use the terminal's conventional Control-A/Control-E line
  // controls. A process-name allowlist made an ordinary wrapper silently lose
  // the shortcut and could never enumerate every interactive shell.
  if (context.platform !== "mac" || context.alternateScreen
    || !event.metaKey || event.shiftKey || event.ctrlKey || event.altKey) return undefined;
  if (event.key === "ArrowLeft") return "\u0001";
  if (event.key === "ArrowRight") return "\u0005";
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
