// @vitest-environment jsdom

import { describe, expect, it, vi } from "vitest";
import {
  decodeOsc52ClipboardWrite,
  installOsc52ClipboardWrite,
  MAX_OSC52_CLIPBOARD_BYTES,
} from "./osc52Clipboard";
import {
  copyCompletedTerminalSelection,
  installTerminalCopyOnSelect,
  translateTerminalKey,
  type TerminalKeyContext,
} from "./terminalInputPolicy";

const key = (overrides: Partial<KeyboardEvent> = {}) => ({
  altKey: false,
  code: "Enter",
  ctrlKey: false,
  isComposing: false,
  key: "Enter",
  keyCode: 13,
  metaKey: false,
  shiftKey: false,
  ...overrides,
}) as KeyboardEvent;

describe("terminal input translation", () => {
  const context = (overrides: Partial<TerminalKeyContext> = {}): TerminalKeyContext => ({
    alternateScreen: false,
    applicationCursorKeys: false,
    currentCommand: "zsh",
    platform: "mac",
    ...overrides,
  });
  const left = key({ key: "ArrowLeft", keyCode: 37, metaKey: true });
  const right = key({ key: "ArrowRight", keyCode: 39, metaKey: true });

  it("sends Control-/ as 0x1f, which xterm.js 6 does not map at all", () => {
    const slash = (overrides: Partial<KeyboardEvent> = {}) =>
      key({ code: "Slash", ctrlKey: true, key: "/", keyCode: 191, ...overrides });
    expect(translateTerminalKey(slash(), context())).toBe("\u001f");
    expect(translateTerminalKey(slash(), context({ platform: "linux" }))).toBe("\u001f");
    // Inside a TUI too: this is the one binding the shortcut exists for.
    expect(translateTerminalKey(slash(), context({ alternateScreen: true, currentCommand: "nvim" })))
      .toBe("\u001f");
    // A layout whose `/` reports no `key` still identifies by physical key.
    expect(translateTerminalKey(slash({ key: "Unidentified" }), context())).toBe("\u001f");
    // …but `code` is US-positional, so the physical fallback must not claim a
    // QWERTZ Ctrl+-, which sits where a US `/` does and reports its own key.
    expect(translateTerminalKey(slash({ key: "-", keyCode: 189 }), context())).toBeUndefined();

    // Everything with another modifier stays xterm's. Control-Shift-/ is
    // Control-? and Control-_ already produces 0x1f on its own.
    expect(translateTerminalKey(slash({ shiftKey: true }), context())).toBeUndefined();
    expect(translateTerminalKey(slash({ altKey: true }), context())).toBeUndefined();
    expect(translateTerminalKey(slash({ metaKey: true }), context())).toBeUndefined();
    expect(translateTerminalKey(key({ code: "Minus", ctrlKey: true, key: "_", keyCode: 189, shiftKey: true }), context()))
      .toBeUndefined();
    // Command-/ is an app shortcut, not terminal bytes.
    expect(translateTerminalKey(key({ code: "Slash", key: "/", keyCode: 191, metaKey: true }), context()))
      .toBeUndefined();
    // A composing IME still wins over every translation.
    expect(translateTerminalKey(slash({ isComposing: true }), context())).toBeUndefined();
  });

  it("turns Shift-Enter into Control-J only for Codex", () => {
    const shiftedEnter = key({ key: "Enter", shiftKey: true });
    expect(translateTerminalKey(shiftedEnter, context({ currentCommand: "codex" }))).toBe("\n");
    expect(translateTerminalKey(shiftedEnter, context({ currentCommand: "node" }))).toBeUndefined();
    expect(translateTerminalKey(shiftedEnter, context({ currentCommand: "zsh" }))).toBeUndefined();
    expect(translateTerminalKey(shiftedEnter, context({ alternateScreen: true, currentCommand: "vim" })))
      .toBeUndefined();
    expect(translateTerminalKey(key(), context({ currentCommand: "codex" }))).toBeUndefined();
  });

  it("sends the readline line controls on the normal screen", () => {
    expect(translateTerminalKey(left, context({ currentCommand: "/bin/zsh" }))).toBe("\u0001");
    expect(translateTerminalKey(right, context({ currentCommand: "fish" }))).toBe("\u0005");
    expect(translateTerminalKey(left, context({ currentCommand: "codex" }))).toBe("\u0001");
    expect(translateTerminalKey(right, context({ currentCommand: "shell-wrapper" }))).toBe("\u0005");
  });

  it("sends Home/End on the alternate screen, in the program's cursor key mode", () => {
    const alternate = { alternateScreen: true };
    expect(translateTerminalKey(left, context({ ...alternate, applicationCursorKeys: true }))).toBe("\u001bOH");
    expect(translateTerminalKey(right, context({ ...alternate, applicationCursorKeys: true }))).toBe("\u001bOF");
    expect(translateTerminalKey(left, context(alternate))).toBe("\u001b[H");
    expect(translateTerminalKey(right, context(alternate))).toBe("\u001b[F");
  });

  it("translates the event a packaged macOS Claude Code pane actually delivers", () => {
    // Measured in the packaged app. tmux reports Claude Code's foreground
    // command as its version-numbered binary, so the command never identifies
    // the program and must not affect the translation.
    const packaged = key({ key: "ArrowLeft", keyCode: 37, metaKey: true, isComposing: false });
    expect(translateTerminalKey(packaged, {
      alternateScreen: true,
      applicationCursorKeys: false,
      currentCommand: "2.1.238",
      platform: "mac",
    })).toBe("\u001b[H");
  });

  it("leaves other platforms and modified Command-Arrows alone", () => {
    expect(translateTerminalKey(left, context({ platform: "linux" }))).toBeUndefined();
    expect(translateTerminalKey(left, context({ alternateScreen: true, platform: "linux" }))).toBeUndefined();
    expect(translateTerminalKey(key({ key: "ArrowLeft" }), context())).toBeUndefined();
    for (const modifier of ["altKey", "ctrlKey", "shiftKey"] as const) {
      expect(translateTerminalKey(key({ key: "ArrowLeft", metaKey: true, [modifier]: true }), context()), modifier)
        .toBeUndefined();
      expect(
        translateTerminalKey(key({ key: "ArrowLeft", metaKey: true, [modifier]: true }), context({ alternateScreen: true })),
        modifier,
      ).toBeUndefined();
    }
  });

  it("never translates IME/composition events", () => {
    expect(translateTerminalKey(key({ shiftKey: true, isComposing: true }), context())).toBeUndefined();
    expect(translateTerminalKey(key({ key: "ArrowLeft", metaKey: true, keyCode: 229 }), context())).toBeUndefined();
    expect(translateTerminalKey(key({ key: "ArrowLeft", metaKey: true, keyCode: 229 }), context({ alternateScreen: true })))
      .toBeUndefined();
  });
});

describe("copy on select", () => {
  it("copies the completed selection verbatim when enabled", async () => {
    const write = vi.fn();
    const renderer = { hasSelection: () => true, getSelection: () => "two lines\nwith spaces  " };
    await expect(copyCompletedTerminalSelection(renderer, true, write)).resolves.toBe(true);
    expect(write).toHaveBeenCalledWith("two lines\nwith spaces  ");
  });

  it("does not touch the clipboard for an empty selection or when disabled", async () => {
    const write = vi.fn();
    await expect(copyCompletedTerminalSelection({ hasSelection: () => false, getSelection: () => "" }, true, write))
      .resolves.toBe(false);
    await expect(copyCompletedTerminalSelection({ hasSelection: () => true, getSelection: () => "kept" }, false, write))
      .resolves.toBe(false);
    expect(write).not.toHaveBeenCalled();
  });

  it("copies directly from xterm selection changes and reads the setting at event time", async () => {
    let notifySelectionChange: () => void = () => undefined;
    let selection = "";
    let enabled = false;
    const disposeSelection = vi.fn();
    const write = vi.fn();
    const renderer = {
      hasSelection: () => Boolean(selection),
      getSelection: () => selection,
      onSelectionChange: (listener: () => void) => {
        notifySelectionChange = listener;
        return disposeSelection;
      },
    };
    const dispose = installTerminalCopyOnSelect({
      renderer,
      enabled: () => enabled,
      write,
      onError: vi.fn(),
    });

    selection = "ignored while disabled";
    notifySelectionChange();
    await Promise.resolve();
    expect(write).not.toHaveBeenCalled();

    enabled = true;
    selection = "current selection";
    notifySelectionChange();
    await Promise.resolve();
    expect(write).toHaveBeenCalledOnce();
    expect(write).toHaveBeenCalledWith("current selection");

    selection = "";
    notifySelectionChange();
    await Promise.resolve();
    expect(write).toHaveBeenCalledOnce();

    dispose();
    expect(disposeSelection).toHaveBeenCalledOnce();
  });

  it("reports native clipboard failures from a selection change", async () => {
    let notifySelectionChange: () => void = () => undefined;
    const failure = new Error("clipboard denied");
    const onError = vi.fn();
    installTerminalCopyOnSelect({
      renderer: {
        hasSelection: () => true,
        getSelection: () => "selection",
        onSelectionChange: (listener: () => void) => {
          notifySelectionChange = listener;
          return () => undefined;
        },
      },
      enabled: () => true,
      write: () => Promise.reject(failure),
      onError,
    });
    notifySelectionChange();
    await Promise.resolve();
    await Promise.resolve();
    expect(onError).toHaveBeenCalledWith(failure);
  });
});

describe("write-only OSC 52 clipboard handling", () => {
  const encode = (value: string) => btoa(String.fromCharCode(...new TextEncoder().encode(value)));

  it("decodes bounded canonical UTF-8 writes", () => {
    expect(decodeOsc52ClipboardWrite(`c;${encode("Claude selection 🚀\n")}`)).toBe("Claude selection 🚀\n");
    expect(decodeOsc52ClipboardWrite(`;${encode("default selection")}`)).toBe("default selection");
  });

  it("rejects reads, malformed base64, invalid UTF-8, empty writes and unknown selectors", () => {
    for (const payload of ["c;?", "c;", "c;%%%", "c;AB==", "c;/w==", `x;${encode("text")}`, "missing-separator"]) {
      expect(decodeOsc52ClipboardWrite(payload), payload).toBeUndefined();
    }
  });

  it("rejects a decoded payload above the 1 MiB bound before decoding it", () => {
    const encoded = "A".repeat(Math.ceil((MAX_OSC52_CLIPBOARD_BYTES + 1) / 3) * 4);
    expect(decodeOsc52ClipboardWrite(`c;${encoded}`)).toBeUndefined();
  });

  it("registers only a consumed OSC 52 writer and never answers a read request", async () => {
    let identifier = -1;
    let handler: ((data: string) => boolean) | undefined;
    const parser = {
      registerOscHandler: vi.fn((value: number, callback: (data: string) => boolean) => {
        identifier = value;
        handler = callback;
        return { dispose: vi.fn() };
      }),
    };
    const write = vi.fn();
    installOsc52ClipboardWrite(parser, write);
    expect(identifier).toBe(52);
    expect(handler?.("c;?")).toBe(true);
    expect(handler?.(`c;${encode("from tmux")}`)).toBe(true);
    await Promise.resolve();
    await Promise.resolve();
    expect(write).toHaveBeenCalledTimes(1);
    expect(write).toHaveBeenCalledWith("from tmux");
  });
});
