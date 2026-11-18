import { describe, expect, it } from "vitest";

import { ChatTurnState } from "../../protocol/gen/chat_pb";
import {
  defaultBranchName,
  folderLabel,
  gitRows,
  inRepository,
  NO_GIT_DETAILS,
  NOT_A_REPOSITORY,
  turnEnded,
  type GitDetails,
  type GitRead,
  type GitSummary,
} from "./gitDetails";

const ready = <T,>(value: T): GitRead<T> => ({ status: "ready", value });
const summary = (baseRef: string, files: number, added: number, removed: number): GitRead<GitSummary> => ready({ baseRef, files: BigInt(files), added: BigInt(added), removed: BigInt(removed) });
const READING = { status: "reading" } as const;
const FAILED = { status: "failed" } as const;

function details(fields: Partial<GitDetails> = {}): GitDetails {
  return {
    repository: "git",
    root: "/home/operator/dev/muxflow",
    branch: ready("gpui/chat"),
    changes: summary("origin/main", 14, 412, 96),
    uncommitted: summary("HEAD", 1, 3, 1),
    ...fields,
  };
}

// The cases of `folder_label`'s test in apps/desktop-gpui/src/chat/details.rs,
// with home read off the path instead of voted on by the host's panes.
describe("the folder as Details names it", () => {
  const repo = "/home/operator/dev/muxflow";

  it("is the repository's folder and the path within it", () => {
    expect(folderLabel("/home/operator/dev/muxflow", repo)).toBe("muxflow");
    expect(folderLabel("/home/operator/dev/muxflow/apps/mobile", repo)).toBe("muxflow/apps/mobile");
    // A worktree goes by its own folder.
    expect(folderLabel("/home/operator/dev/muxflow-chat/apps", "/home/operator/dev/muxflow-chat/")).toBe("muxflow-chat/apps");
    expect(folderLabel("/srv/repo/src", "/srv/repo")).toBe("repo/src");
  });

  it("is relative to home outside a repository, for each shape of home", () => {
    expect(folderLabel("/home/operator")).toBe("~");
    expect(folderLabel("/home/operator/notes/drafts")).toBe("~/notes/drafts");
    expect(folderLabel("/Users/operator/notes")).toBe("~/notes");
    expect(folderLabel("/root")).toBe("~");
    expect(folderLabel("/root/notes")).toBe("~/notes");
    // A repository the folder is not in names nothing.
    expect(folderLabel("/home/operator/dev/muxflow-old", repo)).toBe("~/dev/muxflow-old");
    expect(folderLabel("/home/operator/dev", repo)).toBe("~/dev");
  });

  it("stays as it is outside both", () => {
    expect(folderLabel("/srv/app")).toBe("/srv/app");
    expect(folderLabel("/rootless/x")).toBe("/rootless/x");
    expect(folderLabel("/home")).toBe("/home");
    expect(folderLabel("/srv/app", "")).toBe("/srv/app");
    expect(folderLabel("/srv/app", "/")).toBe("/srv/app");
    expect(folderLabel("")).toBe("");
  });
});

describe("the default branch's name", () => {
  it("drops everything through the first slash", () => {
    expect(defaultBranchName("origin/main")).toBe("main");
    expect(defaultBranchName("origin/release/2.0")).toBe("release/2.0");
    expect(defaultBranchName("main")).toBe("main");
  });
});

describe("the Git rows", () => {
  it("are the branch, the changes against the default branch, and the uncommitted work", () => {
    expect(gitRows(details())).toEqual({
      branch: "gpui/chat",
      changes: { title: "Changes vs main", count: { kind: "lines", added: "+412", removed: "−96", files: "14 files" }, label: "Changes vs main: +412 −96, 14 files" },
      uncommitted: { title: "Uncommitted", count: { kind: "lines", added: "+3", removed: "−1", files: "1 file" }, label: "Uncommitted: +3 −1, 1 file" },
    });
  });

  it("are not drawn outside a repository, or before the host has said there is one", () => {
    expect(gitRows(NO_GIT_DETAILS)).toEqual({});
    expect(gitRows(NOT_A_REPOSITORY)).toEqual({});
    expect(gitRows(details({ repository: "none" }))).toEqual({});
    expect(gitRows(details({ repository: "unknown" }))).toEqual({});
  });

  it("say Detached HEAD with no current branch", () => {
    expect(gitRows(details({ branch: ready("") })).branch).toBe("Detached HEAD");
  });

  it("show … while a read is on its way", () => {
    const rows = gitRows(details({ branch: READING, changes: READING, uncommitted: READING }));
    expect(rows.branch).toBe("…");
    expect(rows.changes).toEqual({ title: "Changes", count: { kind: "reading" }, label: "Changes: …" });
    expect(rows.uncommitted).toEqual({ title: "Uncommitted", count: { kind: "reading" }, label: "Uncommitted: …" });
  });

  it("leave out a row whose read failed", () => {
    expect(gitRows(details({ branch: FAILED, changes: FAILED, uncommitted: FAILED }))).toEqual({});
    const rows = gitRows(details({ changes: FAILED }));
    expect(rows.changes).toBeUndefined();
    expect(rows.branch).toBe("gpui/chat");
    expect(rows.uncommitted?.title).toBe("Uncommitted");
  });

  it("leave out Changes with no default branch, and with nothing on the branch", () => {
    expect(gitRows(details({ changes: summary("", 0, 0, 0) })).changes).toBeUndefined();
    expect(gitRows(details({ changes: summary("origin/main", 0, 0, 0) })).changes).toBeUndefined();
  });

  it("say none for a clean tree, and count a binary file that changes no line", () => {
    expect(gitRows(details({ uncommitted: summary("HEAD", 0, 0, 0) })).uncommitted).toEqual({ title: "Uncommitted", count: { kind: "none" }, label: "Uncommitted: none" });
    expect(gitRows(details({ uncommitted: summary("HEAD", 1, 0, 0) })).uncommitted?.count).toEqual({ kind: "lines", added: "+0", removed: "−0", files: "1 file" });
  });

  it("leave out Uncommitted unless its answer is against HEAD", () => {
    // A host that does not know `summary_base` answers with the default-branch summary.
    expect(gitRows(details({ uncommitted: summary("origin/main", 14, 412, 96) })).uncommitted).toBeUndefined();
    expect(gitRows(details({ uncommitted: summary("", 0, 0, 0) })).uncommitted).toBeUndefined();
  });
});

describe("what is kept between reads", () => {
  it("keeps what was read for the same root, and starts over for another", () => {
    const held = details();
    expect(inRepository(held, held.root)).toBe(held);
    expect(inRepository(held, "/srv/other")).toEqual({ ...NO_GIT_DETAILS, repository: "git", root: "/srv/other" });
    expect(inRepository(NOT_A_REPOSITORY, "/srv/repo")).toEqual({ ...NO_GIT_DETAILS, repository: "git", root: "/srv/repo" });
  });
});

describe("when a turn has ended", () => {
  it("is when it goes from working or waiting to idle", () => {
    expect(turnEnded(ChatTurnState.WORKING, ChatTurnState.IDLE)).toBe(true);
    expect(turnEnded(ChatTurnState.WAITING, ChatTurnState.IDLE)).toBe(true);
    expect(turnEnded(ChatTurnState.IDLE, ChatTurnState.IDLE)).toBe(false);
    expect(turnEnded(ChatTurnState.UNSPECIFIED, ChatTurnState.IDLE)).toBe(false);
    expect(turnEnded(ChatTurnState.WORKING, ChatTurnState.WAITING)).toBe(false);
    expect(turnEnded(ChatTurnState.IDLE, ChatTurnState.WORKING)).toBe(false);
  });
});
