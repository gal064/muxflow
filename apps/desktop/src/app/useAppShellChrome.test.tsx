// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAppShellChrome } from "./useAppShellChrome";
import { NOTICE_DISMISS_MS, type StatusNotice } from "../features/shell/statusNotice";
import { defaultAppState, type ShellState } from "../features/shell/types";

/**
 * Drives the hook the way `App` does: one status string in, the visible notice
 * out, with the ability to push a new status and watch what the old notice does.
 */
async function shellChrome(initial: string, initialShell = defaultAppState.shell, initiallyInline = false) {
  let notice: StatusNotice | undefined;
  let downloadsVisible = false;
  let sequence = 0;
  let status = initial;
  let shell = initialShell;
  let inline = initiallyInline;
  function Harness({ status, sequence, shell, inline }: { status: string; sequence: number; shell: ShellState; inline: boolean }) {
    const chrome = useAppShellChrome(status, sequence, shell, inline);
    notice = chrome.notice;
    downloadsVisible = chrome.downloadsVisible;
    return null;
  }
  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<Harness sequence={sequence} status={status} shell={shell} inline={inline} />); });
  return {
    get notice() { return notice; },
    get downloadsVisible() { return downloadsVisible; },
    async setStatus(next: string, shownInline = false) {
      status = next;
      inline = shownInline;
      sequence += 1;
      await act(async () => renderer.update(<Harness sequence={sequence} status={status} shell={shell} inline={inline} />));
    },
    async setShell(next: ShellState) {
      shell = next;
      await act(async () => renderer.update(<Harness sequence={sequence} status={status} shell={shell} inline={inline} />));
    },
    async unmount() { await act(async () => renderer.unmount()); },
  };
}

describe("status notice lifecycle", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  it("uses the visible Downloads row and keeps notices when the panel is hidden", async () => {
    const files = { ...defaultAppState.shell, panelOpen: true, panelSurface: "files" as const };
    const chrome = await shellChrome("Download queued: /work/report", files, true);
    expect(chrome.downloadsVisible).toBe(true);
    expect(chrome.notice).toBeUndefined();
    await chrome.setStatus("Download complete: /Downloads/report", true);
    expect(chrome.notice).toBeUndefined();
    await chrome.setShell({ ...files, panelOpen: false });
    expect(chrome.notice).toBeUndefined();
    await chrome.setStatus("Download complete: /Downloads/another");
    expect(chrome.notice?.message).toBe("Download complete: /Downloads/another");
    await chrome.setShell(files);
    // A notice originally needed for a hidden result must not be cleared just
    // because another host's Downloads panel opens later.
    expect(chrome.notice?.message).toBe("Download complete: /Downloads/another");
    await chrome.setStatus("Your connection to devhost is unstable; Muxflow keeps losing the link and reconnecting.");
    expect(chrome.notice?.message).toContain("is unstable");
    await chrome.setStatus("Could not open the save panel: denied");
    expect(chrome.notice?.severity).toBe("problem");
    await chrome.unmount();
  });

  it("keeps download results visible when the window is too narrow for the panel", async () => {
    const original = window.innerWidth;
    window.innerWidth = 300;
    const chrome = await shellChrome("Download complete: /Downloads/report", {
      ...defaultAppState.shell, panelOpen: true, panelSurface: "files",
    });
    expect(chrome.downloadsVisible).toBe(false);
    expect(chrome.notice?.message).toBe("Download complete: /Downloads/report");
    await chrome.unmount();
    window.innerWidth = original;
  });

  it("does not let routine chatter take down the message a mutation just made", async () => {
    // Every tmux mutation is followed within milliseconds by its own progress
    // statuses, and both of these are classified routine. Clearing the notice
    // on them meant a bulk close's receipt — and any refusal the close
    // produced — was gone before it could be read.
    const chrome = await shellChrome("Closed 3 tabs.");
    expect(chrome.notice?.message).toBe("Closed 3 tabs.");
    await chrome.setStatus("Topology changed; reconciling…");
    expect(chrome.notice?.message).toBe("Closed 3 tabs.");
    await chrome.setStatus("Live");
    expect(chrome.notice?.message).toBe("Closed 3 tabs.");

    // Held through the chatter, but not held forever: an informational notice
    // keeps its own clock rather than the status's, or surviving the chatter
    // would mean never timing out.
    await act(async () => { vi.advanceTimersByTime(NOTICE_DISMISS_MS + 1); });
    expect(chrome.notice).toBeUndefined();
    await chrome.unmount();
  });

  it("keeps a refusal up through the chatter and until something else is said", async () => {
    const chrome = await shellChrome("Closed 1 of 2 tabs; 1 could not be closed.");
    expect(chrome.notice?.severity).toBe("problem");
    await chrome.setStatus("Live");
    await act(async () => { vi.advanceTimersByTime(NOTICE_DISMISS_MS * 3); });
    expect(chrome.notice?.message).toBe("Closed 1 of 2 tabs; 1 could not be closed.");
    // A real message still replaces it; this is a rule about chatter, not a
    // notice that outranks everything after it.
    await chrome.setStatus("Helper installed. Version 0.2.0.");
    expect(chrome.notice?.message).toBe("Helper installed. Version 0.2.0.");
    await chrome.unmount();
  });

  it("re-shows the same message when it is set again after being dismissed", async () => {
    // A workspace create refused twice for the same directory is two refusals.
    // Keyed on the string alone, the second one never re-fired the effect and
    // the user saw nothing.
    const chrome = await shellChrome("Live");
    await chrome.setStatus("tmux_action_rejected: workspace start directory /x does not exist or is not a directory");
    const first = chrome.notice;
    expect(first?.severity).toBe("problem");
    await act(async () => { chrome.notice; });
    await chrome.setStatus("tmux_action_rejected: workspace start directory /x does not exist or is not a directory");
    expect(chrome.notice?.id).not.toBe(first?.id);
    expect(chrome.notice?.message).toBe(first?.message);
    await chrome.unmount();
  });
});
