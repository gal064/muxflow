import { keyboardEventIsComposing, type Platform } from "../../commands/registry";
import type { TerminalRenderer } from "./TerminalRenderer";

const SHELL_COMMANDS = new Set([
  "bash", "csh", "dash", "elvish", "fish", "ksh", "ksh93", "nu", "pwsh",
  "sh", "tcsh", "xonsh", "zsh",
]);

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
  if (context.platform !== "mac" || context.alternateScreen || !SHELL_COMMANDS.has(command)
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
  container: HTMLElement;
  renderer: Pick<TerminalRenderer, "getSelection" | "hasSelection" | "onSelectionChange">;
  enabled: () => boolean;
  write: (text: string) => void | Promise<void>;
  onError: (error: unknown) => void;
}

/**
 * Copies only a selection changed by the pointer gesture that just ended.
 *
 * Looking only at `hasSelection()` on mouseup is insufficient: xterm retains a
 * prior selection across several unrelated clicks, which could overwrite a
 * newer clipboard value. Its selection-change event is the authoritative
 * distinction between such a click and a gesture that finalized selection.
 */
export function installTerminalCopyOnSelect({
  container,
  renderer,
  enabled,
  write,
  onError,
}: CopyOnSelectOptions): () => void {
  const ownerDocument = container.ownerDocument;
  let gestureActive = false;
  let selectionChanged = false;
  let disposed = false;

  const selectionDisposable = renderer.onSelectionChange(() => {
    if (gestureActive) selectionChanged = true;
  });
  const finishGesture = () => {
    ownerDocument.removeEventListener("mouseup", finishGesture, true);
    const completedGesture = gestureActive;
    // xterm completes its pointer handling in this same mouseup dispatch.
    queueMicrotask(() => {
      const shouldCopy = completedGesture && selectionChanged;
      gestureActive = false;
      selectionChanged = false;
      if (!shouldCopy) return;
      if (disposed) return;
      void copyCompletedTerminalSelection(renderer, enabled(), write).catch(onError);
    });
  };
  const beginGesture = (event: MouseEvent) => {
    if (event.button !== 0 && event.button !== 2) return;
    ownerDocument.removeEventListener("mouseup", finishGesture, true);
    gestureActive = true;
    selectionChanged = false;
    ownerDocument.addEventListener("mouseup", finishGesture, true);
  };

  container.addEventListener("mousedown", beginGesture, true);
  return () => {
    disposed = true;
    gestureActive = false;
    container.removeEventListener("mousedown", beginGesture, true);
    ownerDocument.removeEventListener("mouseup", finishGesture, true);
    selectionDisposable();
  };
}
