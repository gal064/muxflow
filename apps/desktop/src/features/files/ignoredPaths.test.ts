import { describe, expect, it } from "vitest";
import { ignoredPathsFromStatus } from "./ignoredPaths";
import type { GitStatusEntry, GitStatusSnapshot } from "../git/types";

const entry = (displayPath: string, ignored: boolean): GitStatusEntry => ({
  path: btoa(displayPath),
  displayPath,
  indexKind: "none",
  worktreeKind: ignored ? "ignored" : "modified",
  indexStatus: ".",
  worktreeStatus: ignored ? "!" : "M",
  conflicted: false,
  untracked: false,
  ignored,
  submodule: false,
  symlink: false,
  binary: false,
});

const status = (overrides: Partial<GitStatusSnapshot> = {}): GitStatusSnapshot => ({
  repository: { id: "repo", worktreeRoot: "/repo", initial: false, detachedHead: false },
  generation: "1",
  sourceGeneration: "1",
  entries: [entry("target", true), entry("dist", true), entry("src/main.rs", false)],
  authoritative: true,
  ...overrides,
});

describe("ignoredPathsFromStatus", () => {
  it("names the ignored entries as absolute paths under the worktree root", () => {
    expect(ignoredPathsFromStatus(status())).toEqual(new Set(["/repo/target", "/repo/dist"]));
  });

  it("does not double a separator when the worktree root has a trailing slash", () => {
    const snapshot = status();
    expect(ignoredPathsFromStatus({ ...snapshot, repository: { ...snapshot.repository, worktreeRoot: "/repo/" } }))
      .toEqual(new Set(["/repo/target", "/repo/dist"]));
  });

  it("hides nothing at all unless the status is a real answer", () => {
    // Each of these means "show everything". A non-authoritative or oversized
    // status reports zero entries, so reading it as truth would empty the tree
    // on the first repository too large to walk.
    expect(ignoredPathsFromStatus(undefined)).toBeUndefined();
    expect(ignoredPathsFromStatus(status({ authoritative: false, entries: [] }))).toBeUndefined();
    expect(ignoredPathsFromStatus(status({ oversized: true }))).toBeUndefined();
  });

  it("distinguishes an authoritative empty answer from no answer", () => {
    // A clean repository with nothing ignored is a set, not `undefined`.
    expect(ignoredPathsFromStatus(status({ entries: [entry("src/main.rs", false)] }))).toEqual(new Set());
  });

  it("never decodes the opaque path identity", () => {
    // `path` is base64 here purely to prove it is not what gets read: a
    // reconstruction from it would produce a name nothing on disk matches.
    const snapshot = status({ entries: [entry("build output", true)] });
    expect(ignoredPathsFromStatus(snapshot)).toEqual(new Set(["/repo/build output"]));
  });
});
