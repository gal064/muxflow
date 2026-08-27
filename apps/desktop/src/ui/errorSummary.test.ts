import { describe, expect, it } from "vitest";
import { summarizeSurfaceError } from "./errorSummary";

describe("summarizeSurfaceError", () => {
  it("answers the two rejections M10-E059 recorded with something a person can act on", () => {
    // Both strings are verbatim from the Phase 10 manual pass.
    const slash = summarizeSurfaceError("file_mutation_rejected: mutation parent changed or is not a no-follow directory");
    expect(slash.summary).toContain("plain file name");
    expect(slash.detail).toBe("file_mutation_rejected: mutation parent changed or is not a no-follow directory");

    const long = summarizeSurfaceError("file_mutation_rejected: File name too long (os error 63)");
    expect(long.summary).toBe("That name is longer than this filesystem allows. Use a shorter one.");
    expect(long.detail).toContain("os error 63");
  });

  it("answers the refusal a mid-reconcile Git action produces", () => {
    // Seen on the packaged app as a full-width red banner over the diff,
    // enumerating three internal states and naming "mutation".
    const raw = "mutation_rejected: host connection is not writable (disconnected, reconciling, or read-only)";
    expect(summarizeSurfaceError(raw).summary).toBe("The host helper connection is read-only.");
    expect(summarizeSurfaceError(raw).detail).toBe(raw);
  });

  it("distinguishes a settling connection from an incompatible read-only helper", () => {
    expect(summarizeSurfaceError(
      "connection_unavailable: host connection is disconnected or reconciling",
    ).summary).toBe("The connection to the host is still reconnecting.");
    expect(summarizeSurfaceError(
      "connection_read_only: host helper connection is read-only",
    ).summary).toBe("The host helper connection is read-only.");
  });

  it("never loses the diagnostic it summarizes", () => {
    const raw = "git_rejected: fatal: pathspec 'x' did not match any files";
    expect(summarizeSurfaceError(raw).detail).toBe(raw);
  });

  it("falls back to the code family when the reason is unrecognized", () => {
    expect(summarizeSurfaceError("tmux_action_rejected: window is in an unexpected layout").summary)
      .toBe("tmux refused that action.");
    expect(summarizeSurfaceError("upload_preflight_rejected: quota policy 7").summary)
      .toBe("The host refused that upload.");
  });

  it("falls back to the message itself when nothing is recognized, and never to an empty line", () => {
    // The fallback chain is the point: an unmapped rejection is still shown.
    expect(summarizeSurfaceError("The moon is in the wrong phase.").summary).toBe("The moon is in the wrong phase.");
    expect(summarizeSurfaceError("The moon is in the wrong phase.").detail).toBeUndefined();
    expect(summarizeSurfaceError("   ").summary).toBe("Something went wrong.");
  });

  it("leaves a sentence the app wrote itself exactly as the app wrote it", () => {
    // The status channel carries both host diagnostics and the app's own
    // sentences. Answering "which pane failed to hide" with generic advice
    // about checking the connection would throw away the useful half.
    const own = "Could not mark %117 hidden: timed out";
    expect(summarizeSurfaceError(own).summary).toBe(own);
    expect(summarizeSurfaceError(own).detail).toBeUndefined();
    const agent = "Agent Codex has no exact pane match; navigation is unavailable.";
    expect(summarizeSurfaceError(agent).summary).toBe(agent);
    // Only a structured host code opts into the rewrite.
    expect(summarizeSurfaceError("terminal_visibility_rejected: timed out").summary)
      .toBe("The host did not answer in time. Check the connection and try again.");
  });

  it("strips the JavaScript wrappers a rethrow adds and keeps only the first sentence in front", () => {
    const result = summarizeSurfaceError("Error: Error: something odd happened. And then more detail nobody needs first.");
    expect(result.summary).toBe("something odd happened.");
    expect(result.detail).toContain("more detail nobody needs first");
  });

  it("bounds a single-sentence wall of text at a word boundary", () => {
    const wall = `x ${"word ".repeat(80)}end`;
    const result = summarizeSurfaceError(wall);
    expect(result.summary.length).toBeLessThanOrEqual(141);
    expect(result.summary.endsWith("…")).toBe(true);
    expect(result.summary).not.toContain("wor…");
    expect(result.detail).toBe(wall.trim());
  });

  it("answers a push refusal with what to do, not with the filesystem reading of its words", () => {
    // "Permission denied" is also an errno phrase, and the reason table would
    // otherwise send a person looking at the wrong machine's permissions.
    const auth = summarizeSurfaceError("git_auth_failed: git@github.com: Permission denied (publickey).");
    expect(auth.summary).toContain("run git push in a terminal once");
    expect(auth.detail).toContain("publickey");
    const rejected = summarizeSurfaceError("git_push_rejected: ! [rejected] master -> master (non-fast-forward)");
    expect(rejected.summary).toBe("The remote rejected the push (non-fast-forward?). Pull or rebase first.");
    expect(rejected.detail).toContain("non-fast-forward");
    // The state every unpublished branch is in, and the code does not carry one
    // of the suffixes that opts a rejection into being rewritten at all.
    const missing = summarizeSurfaceError("git_no_upstream: no upstream branch is configured for the current branch; run `git push -u` in a terminal once");
    expect(missing.summary).toBe("This branch has no upstream yet; run git push -u in a terminal once, then retry.");
    expect(missing.detail).toContain("git push -u");
  });

  it("keeps a multi-line diagnostic's first line as the summary and the whole thing as detail", () => {
    const result = summarizeSurfaceError("commit failed\npre-commit hook output\nline 2");
    expect(result.summary).toBe("commit failed");
    expect(result.detail).toBe("commit failed\npre-commit hook output\nline 2");
  });

  it("points a refused workspace start directory at Settings, not at a refresh", () => {
    const text = summarizeSurfaceError("tmux_action_rejected: workspace start directory /nope does not exist or is not a directory");
    expect(text.summary).toContain("Settings");
    expect(text.summary).not.toContain("Refresh");
  });
});
