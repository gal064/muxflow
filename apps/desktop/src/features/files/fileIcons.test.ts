import { describe, expect, it } from "vitest";
import { fileIcon } from "./fileIcons";

describe("fileIcon", () => {
  it("gives a directory the folder shape and moves only its lid when expanded", () => {
    expect(fileIcon({ name: "src", kind: "directory" })).toEqual({ icon: "folder", color: "var(--term-4)" });
    expect(fileIcon({ name: "src", kind: "directory" }, true)).toEqual({ icon: "folderOpen", color: "var(--term-4)" });
  });

  it("maps the extension table to a shape and an ANSI tint", () => {
    const table: [string, string, string][] = [
      ["main.ts", "fileCode", "var(--term-4)"],
      ["App.tsx", "fileCode", "var(--term-4)"],
      ["main.js", "fileCode", "var(--term-3)"],
      ["lib.rs", "fileCode", "var(--term-1)"],
      ["setup.py", "fileCode", "var(--term-2)"],
      ["package.json", "fileData", "var(--term-3)"],
      ["Cargo.toml", "fileData", "var(--term-3)"],
      ["ci.yml", "fileData", "var(--term-3)"],
      ["README.md", "markdown", "var(--term-4)"],
      ["styles.css", "fileCode", "var(--term-5)"],
      ["index.html", "fileCode", "var(--term-1)"],
      ["build.sh", "fileShell", "var(--term-2)"],
      ["logo.svg", "fileImage", "var(--term-5)"],
      ["shot.png", "fileImage", "var(--term-5)"],
      ["Cargo.lock", "fileLock", "var(--chrome-faint)"],
      ["notes.txt", "fileText", "var(--chrome-dim)"],
    ];
    for (const [name, icon, color] of table) {
      expect(fileIcon({ name, kind: "file" }), name).toEqual({ icon, color });
    }
  });

  it("is case-insensitive about the extension", () => {
    expect(fileIcon({ name: "PHOTO.PNG", kind: "file" })).toEqual(fileIcon({ name: "photo.png", kind: "file" }));
  });

  it("reads a dotfile and a *.config.* name as configuration, not as its language", () => {
    for (const name of [".gitignore", ".env", ".env.local", ".eslintrc.js", "vite.config.ts", "vitest.config.mts"]) {
      expect(fileIcon({ name, kind: "file" }), name).toEqual({ icon: "fileConfig", color: "var(--chrome-dim)" });
    }
  });

  it("falls back to the plain page for an unknown or extensionless name", () => {
    for (const name of ["Makefile", "LICENSE", "archive.unknownext", "trailing."]) {
      expect(fileIcon({ name, kind: "file" }), name).toEqual({ icon: "file", color: "var(--chrome-dim)" });
    }
  });

  it("keeps a symlink's target shape and gives it the symlink tint", () => {
    expect(fileIcon({ name: "link.rs", kind: "symlink", targetKind: "file" }))
      .toEqual({ icon: "fileCode", color: "var(--term-6)" });
    expect(fileIcon({ name: "vendor", kind: "symlink", targetKind: "directory" }))
      .toEqual({ icon: "folder", color: "var(--term-6)" });
    expect(fileIcon({ name: "vendor", kind: "symlink", targetKind: "directory" }, true))
      .toEqual({ icon: "folderOpen", color: "var(--term-6)" });
  });
});
