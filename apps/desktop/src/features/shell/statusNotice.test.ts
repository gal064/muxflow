import { describe, expect, it } from "vitest";
import { noticeDismissDelay, noticeForStatus, NOTICE_DISMISS_MS } from "./statusNotice";

describe("status notices", () => {
  it("shows anything it has not been told is routine", () => {
    // A denylist on purpose: an unclassified message must surface rather than
    // disappear, because a silently swallowed refusal is the defect this
    // module exists to prevent.
    expect(noticeForStatus("Something nobody thought about", 1)?.message).toBe("Something nobody thought about");
    expect(noticeForStatus("Helper installed. Version 0.2.0.", 1)?.severity).toBe("info");
  });

  it("stays quiet for the progress the rest of the shell already shows", () => {
    for (const routine of [
      "Live",
      "Discovering local tmux…",
      "Waiting for authoritative tmux state…",
      "Topology changed; reconciling…",
      "Connecting to remote-linux…",
      "Connection reconnecting…",
      "Opened README.md",
      "Viewing the last known workspace. Writes remain frozen.",
      "   ",
    ]) expect(noticeForStatus(routine, 1), routine).toBeUndefined();
  });

  it("classifies refusals as problems, and problems do not time out", () => {
    const refusal = noticeForStatus("This action is unavailable until the authoritative connection is live.", 1)!;
    expect(refusal.severity).toBe("problem");
    expect(noticeDismissDelay(refusal)).toBeUndefined();
    const info = noticeForStatus("Helper installed. Version 0.2.0.", 1)!;
    expect(noticeDismissDelay(info)).toBe(NOTICE_DISMISS_MS);
  });

  it("treats the messages the app actually emits on failure as problems", () => {
    for (const message of [
      "Agent Codex one has no exact pane match; navigation is unavailable.",
      "Agent destination Codex one is no longer available: pane closed.",
      "Could not save application tabs; changes will be retried: disk full",
      "A terminal grid of 812 columns exceeds the supported bound; the request was refused.",
      "Downloads require a live file host.",
      "No agent is waiting on you.",
    ]) expect(noticeForStatus(message, 1)?.severity, message).toBe("problem");
  });

  it("times out a bulk close's receipt but not its partial failure", () => {
    // The tab-strip closes no longer ask first, so this sentence is the whole
    // acknowledgement of a set that vanished. It has to appear — a clipped tab
    // can be closed unseen — and it has to go away on its own.
    const receipt = noticeForStatus("Closed 3 tabs.", 1)!;
    expect(receipt.severity).toBe("info");
    expect(noticeDismissDelay(receipt)).toBe(NOTICE_DISMISS_MS);
    expect(noticeForStatus("Closed 1 tab.", 2)?.severity).toBe("info");
    // Deliberately not suppressed by a `Closed ` routine prefix: the partial
    // outcome opens with the same two words, and a prefix rule would silence
    // the one message of the pair a person has to act on.
    const partial = noticeForStatus("Closed 1 of 2 tabs; 1 could not be closed.", 3)!;
    expect(partial.severity).toBe("problem");
    expect(noticeDismissDelay(partial)).toBeUndefined();
    // A terminal held back by the commit-time agent recheck is a survivor too,
    // and its sentence contains none of the other refusal words. Left as info
    // it timed out after six seconds, which is the same silence the counted
    // outcome exists to break.
    const heldBack = noticeForStatus("Closed 1 of 2 tabs; 1 still had an agent and was left open.", 4)!;
    expect(heldBack.severity).toBe("problem");
    expect(noticeDismissDelay(heldBack)).toBeUndefined();
  });

  it("carries an id so the same message twice re-shows the notice", () => {
    expect(noticeForStatus("same", 1)?.id).toBe(1);
    expect(noticeForStatus("same", 2)?.id).toBe(2);
  });
});

describe("consequential bookkeeping failures", () => {
  it("does not turn a disconnect's pane teardown into a permanent red alert", () => {
    // Measured on the packaged app: dropping the link made every mounted pane's
    // hide request fail, and the last one became a non-dismissing alert naming
    // an internal pane id. The strip and the host row already say what happened.
    expect(noticeForStatus("Could not mark %129 hidden: terminal client is no longer attached", 1)).toBeUndefined();
    expect(noticeForStatus("Could not mark %7 visible: connection closed", 2)).toBeUndefined();
    expect(noticeForStatus("Could not mark agent attention seen: request timed out", 3)).toBeUndefined();
    // Anything that is not that exact bookkeeping shape still shows: the module
    // is a denylist of known chatter, not an allowlist of known problems.
    expect(noticeForStatus("Could not mark the file read-only", 4)?.severity).toBe("problem");
    expect(noticeForStatus("Could not save application tabs; changes will be retried: nope", 5)?.severity).toBe("problem");
  });

  it("treats a host's coded rejection as a problem even without a word boundary", () => {
    const notice = noticeForStatus("tmux_action_rejected: workspace start directory /x does not exist or is not a directory", 1);
    expect(notice?.severity).toBe("problem");
  });
});
