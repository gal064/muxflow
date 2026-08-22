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
} from "./terminalInputPolicy";

const key = (overrides: Partial<KeyboardEvent> = {}) => ({
  altKey: false,
  ctrlKey: false,
  isComposing: false,
  key: "Enter",
  keyCode: 13,
  metaKey: false,
  shiftKey: false,
  ...overrides,
}) as KeyboardEvent;

describe("terminal input translation", () => {
  it("turns Shift-Enter into Control-J only for Codex", () => {
    const shiftedEnter = key({ key: "Enter", shiftKey: true });
    expect(translateTerminalKey(shiftedEnter, { alternateScreen: false, currentCommand: "codex", platform: "mac" }))
      .toBe("\n");
    expect(translateTerminalKey(shiftedEnter, { alternateScreen: false, currentCommand: "node", platform: "mac" }))
      .toBeUndefined();
    expect(translateTerminalKey(shiftedEnter, { alternateScreen: false, currentCommand: "zsh", platform: "mac" }))
      .toBeUndefined();
    expect(translateTerminalKey(shiftedEnter, { alternateScreen: true, currentCommand: "vim", platform: "mac" }))
      .toBeUndefined();
    expect(translateTerminalKey(key(), { alternateScreen: false, currentCommand: "codex", platform: "mac" }))
      .toBeUndefined();
  });

  it("maps bare macOS Command-Arrows in normal-screen terminal contexts", () => {
    const left = key({ key: "ArrowLeft", keyCode: 37, metaKey: true });
    const right = key({ key: "ArrowRight", keyCode: 39, metaKey: true });
    expect(translateTerminalKey(left, { alternateScreen: false, currentCommand: "/bin/zsh", platform: "mac" })).toBe("\u0001");
    expect(translateTerminalKey(right, { alternateScreen: false, currentCommand: "fish", platform: "mac" })).toBe("\u0005");
    expect(translateTerminalKey(left, { alternateScreen: false, currentCommand: "codex", platform: "mac" })).toBe("\u0001");
    expect(translateTerminalKey(right, { alternateScreen: false, currentCommand: "shell-wrapper", platform: "mac" })).toBe("\u0005");
    expect(translateTerminalKey(left, { alternateScreen: false, currentCommand: "zsh", platform: "linux" })).toBeUndefined();
    expect(translateTerminalKey(left, { alternateScreen: true, currentCommand: "zsh", platform: "mac" })).toBeUndefined();
    expect(translateTerminalKey(key({ key: "ArrowLeft", metaKey: true, altKey: true }), {
      alternateScreen: false, currentCommand: "zsh", platform: "mac",
    })).toBeUndefined();
  });

  it("never translates IME/composition events", () => {
    const context = { alternateScreen: false, currentCommand: "zsh", platform: "mac" as const };
    expect(translateTerminalKey(key({ shiftKey: true, isComposing: true }), context)).toBeUndefined();
    expect(translateTerminalKey(key({ key: "ArrowLeft", metaKey: true, keyCode: 229 }), context)).toBeUndefined();
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
