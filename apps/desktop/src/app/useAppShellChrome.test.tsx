// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAppShellChrome } from "./useAppShellChrome";
import { NOTICE_DISMISS_MS, type StatusNotice } from "../features/shell/statusNotice";

/**
 * Drives the hook the way `App` does: one status string in, the visible notice
 * out, with the ability to push a new status and watch what the old notice does.
 */
async function shellChrome(initial: string) {
  let notice: StatusNotice | undefined;
  function Harness({ status }: { status: string }) {
    notice = useAppShellChrome(status).notice;
    return null;
  }
  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<Harness status={initial} />); });
  return {
    get notice() { return notice; },
    async setStatus(status: string) { await act(async () => renderer.update(<Harness status={status} />)); },
    async unmount() { await act(async () => renderer.unmount()); },
  };
}

describe("status notice lifecycle", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

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
});
