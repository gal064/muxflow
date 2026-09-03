import { describe, expect, it } from "vitest";
import { FileKind } from "../../protocol/gen/envelope_pb";
import { compareEntries, entryAction, formatSize, isMarkdownName, visibleEntries } from "./entries";
import { directory, metadata } from "./testing";

describe("hidden names (§9.6 step 4)", () => {
  it("drops the six names the document lists and keeps other dotfiles", () => {
    const entries = visibleEntries([
      directory(".git"),
      directory("node_modules"),
      directory("target"),
      directory(".venv"),
      directory("__pycache__"),
      metadata(".DS_Store"),
      metadata(".gitignore"),
      metadata(".env.local"),
      directory("src"),
    ]);
    expect(entries.map((entry) => entry.name)).toEqual(["src", ".env.local", ".gitignore"]);
  });

  it("hides a name at any depth, not only at the root", () => {
    const entries = visibleEntries([metadata("node_modules", { path: "/w/packages/app/node_modules", kind: FileKind.DIRECTORY })]);
    expect(entries).toEqual([]);
  });
});

describe("ordering (§9.6 step 3)", () => {
  it("puts directories first, then Markdown, then other files, each case-insensitively", () => {
    const entries = visibleEntries([
      metadata("zeta.txt"),
      metadata("README.md"),
      directory("src"),
      metadata("Alpha.txt"),
      directory("Assets"),
      metadata("notes.markdown"),
    ]);
    expect(entries.map((entry) => entry.name)).toEqual([
      "Assets",
      "src",
      "notes.markdown",
      "README.md",
      "Alpha.txt",
      "zeta.txt",
    ]);
  });

  it("keeps a fixed order for names that fold together", () => {
    const left = { name: "a", kind: FileKind.FILE, markdown: false } as never;
    const right = { name: "A", kind: FileKind.FILE, markdown: false } as never;
    expect(Math.sign(compareEntries(left, right))).toBe(-Math.sign(compareEntries(right, left)));
  });
});

describe("markdown detection", () => {
  it("matches .md and .markdown, case-insensitively, and nothing else", () => {
    expect(isMarkdownName("PLAN.MD")).toBe(true);
    expect(isMarkdownName("notes.markdown")).toBe(true);
    expect(isMarkdownName("md")).toBe(false);
    expect(isMarkdownName("readme.mdx")).toBe(false);
  });
});

describe("what a tap does (§9.6 steps 4 and 5)", () => {
  it("opens directories and files, and leaves everything else inert", () => {
    expect(entryAction({ kind: FileKind.DIRECTORY, symlink: false, symlinkTargetKind: FileKind.UNSPECIFIED })).toBe("openDirectory");
    expect(entryAction({ kind: FileKind.FILE, symlink: false, symlinkTargetKind: FileKind.UNSPECIFIED })).toBe("openFile");
    expect(entryAction({ kind: FileKind.OTHER, symlink: false, symlinkTargetKind: FileKind.UNSPECIFIED })).toBe("none");
  });

  it("only opens a symlink whose target the host resolved to a file", () => {
    expect(entryAction({ kind: FileKind.SYMLINK, symlink: true, symlinkTargetKind: FileKind.FILE })).toBe("openFile");
    // What a real listing carries: enumeration never follows the link.
    expect(entryAction({ kind: FileKind.SYMLINK, symlink: true, symlinkTargetKind: FileKind.UNSPECIFIED })).toBe("none");
    expect(entryAction({ kind: FileKind.SYMLINK, symlink: true, symlinkTargetKind: FileKind.DIRECTORY })).toBe("none");
  });
});

describe("sizes (§9.6 step 3)", () => {
  it("spells the document's two examples", () => {
    expect(formatSize(12n * 1024n)).toBe("12 KB");
    // Truncated, not rounded, exactly as the desktop's `formatBytes` does.
    expect(formatSize(1_500_000n)).toBe("1.4 MB");
  });

  it("uses bytes below a kibibyte and one decimal below ten units", () => {
    expect(formatSize(0n)).toBe("0 B");
    expect(formatSize(1023n)).toBe("1023 B");
    expect(formatSize(1024n)).toBe("1.0 KB");
    expect(formatSize(1024n * 1024n * 1024n * 3n)).toBe("3.0 GB");
  });
});
