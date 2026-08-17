// @vitest-environment jsdom
// jsdom, because the surfaces mount hosts and read their layout.
import { act, create } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppTabSurface } from "../features/shell/AppTabSurface";
import { GitDiffSurface } from "../features/git/GitDiffSurface";
import { GitRepositoryStore } from "../features/git/repositoryStore";
import type {
  ActiveRoot,
  DirectoryWatchLease,
  FileWorkspaceClient,
  FileWorkspaceScope,
  OpenFile,
  WorkspaceEvent,
} from "../features/files/types";
import type { AppOwnedTab } from "../features/shell/types";
import type { GitDiff, GitStatusSnapshot, GitWorkspaceClient } from "../features/git/types";

/**
 * Whether anything in this file has caused the editor bundle to be evaluated.
 *
 * A module is evaluated once per registry, and a registry is per test file, so
 * every case here has to be one that must not reach the editor: the first case
 * that did would make the rest of the file prove nothing. That is the whole
 * reason this is a separate file from `editorChunkBoundary.test.tsx`.
 */
const probe = vi.hoisted(() => ({ editorEvaluated: false }));

vi.mock("@monaco-editor/react", () => {
  probe.editorEvaluated = true;
  return { default: () => null, DiffEditor: () => null };
});
vi.mock("../features/files/monaco", () => ({ ADE_MONACO_THEME: "ade-test-theme" }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const scope: FileWorkspaceScope = {
  clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1,
  terminalEpoch: 41, sessionId: "$1", paneId: "%1",
};
const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
const fileTab: AppOwnedTab = {
  id: "tab-1", kind: "file", title: "note.txt", resource: "/repo/note.txt",
  hostProfileId: "local", serverIdentity: "s", sessionId: "$1", sessionName: "work", order: 0, preview: false,
  rootPath: "/repo", rootToken: "root",
};

/**
 * Long enough that an editor this surface had decided to fetch would have
 * arrived. Nothing else these surfaces do takes anywhere near it.
 */
async function settle(turns = 24) {
  for (let turn = 0; turn < turns; turn += 1) {
    await act(async () => {
      await Promise.resolve();
      await new Promise((resolve) => { setTimeout(resolve, 1); });
    });
  }
}

function fileClient(opened?: OpenFile) {
  return {
    openFile: vi.fn(async () => opened),
    writeText: vi.fn(),
    acquireDirectoryWatch: vi.fn(async (): Promise<DirectoryWatchLease> => ({
      fresh: true,
      snapshot: { rootToken: "root", directory: "/repo", revision: "1", entries: [], recoveredFromOverflow: false, complete: true },
      release: () => undefined,
    })),
    subscribe: vi.fn(async (_scope: FileWorkspaceScope, _next: (event: WorkspaceEvent) => void) => () => undefined),
    listDirectory: vi.fn(), resolveActiveRoot: vi.fn(),
    mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
  } as unknown as FileWorkspaceClient;
}

async function mountFile(options: { opened?: OpenFile; tab?: Partial<AppOwnedTab>; connected?: boolean } = {}) {
  let renderer!: ReturnType<typeof create>;
  await act(async () => {
    renderer = create(<AppTabSurface
      canWrite
      client={fileClient(options.opened)}
      activeRoot={root}
      onDirty={vi.fn()}
      onDownload={vi.fn()}
      onStatus={vi.fn()}
      onViewMode={vi.fn()}
      scope={options.connected === false ? undefined : scope}
      tab={{ ...fileTab, ...options.tab }}
    />);
  });
  await settle();
  return renderer;
}

describe("content that never reaches an editor", () => {
  let mounted: ReturnType<typeof create> | undefined;
  afterEach(async () => {
    await act(async () => { mounted?.unmount(); });
    mounted = undefined;
    expect(probe.editorEvaluated, "the editor bundle was fetched for content that cannot use it").toBe(false);
  });

  it("shows a binary file without fetching the editor", async () => {
    mounted = await mountFile({
      opened: {
        kind: "binary",
        file: { path: "/repo/note.txt", generation: "g1", sizeBytes: "2048", mime: "application/octet-stream", previewKind: "binary" },
      },
    });
    expect(JSON.stringify(mounted.toJSON())).toContain("binary-surface");
  });

  it("refuses a file past the editor limit without fetching the editor", async () => {
    mounted = await mountFile({
      opened: {
        kind: "text",
        file: { path: "/repo/note.txt", content: "x", generation: "g1", sizeBytes: String(11 * 1024 * 1024), lineEnding: "lf", encoding: "utf-8" },
      },
    });
    expect(JSON.stringify(mounted.toJSON())).toContain("larger than the 10 MiB editor limit");
  });

  it("renders a Markdown preview-only tab without fetching the editor", async () => {
    mounted = await mountFile({
      opened: {
        kind: "text",
        file: { path: "/repo/note.md", content: "# heading", generation: "g1", sizeBytes: "9", lineEnding: "lf", encoding: "utf-8" },
      },
      tab: { kind: "markdown", viewMode: "preview", resource: "/repo/note.md" },
    });
    const rendered = JSON.stringify(mounted.toJSON());
    expect(rendered).toContain("markdown-preview");
    expect(rendered, "the preview-only path still rendered an editor host").not.toContain("monaco-host");
  });

  it("explains a disconnected tab without fetching the editor", async () => {
    mounted = await mountFile({ connected: false });
    expect(JSON.stringify(mounted.toJSON())).toContain("Reconnect and select a terminal pane");
  });

  it("shows a binary diff without fetching the editor", async () => {
    const client = gitClient({ binary: true });
    await act(async () => {
      mounted = create(<GitDiffSurface
        activeRoot={root}
        canWrite
        onMessage={vi.fn()}
        repositories={new GitRepositoryStore(client)}
        scope={scope}
        tab={gitTab}
      />);
    });
    await settle();
    expect(JSON.stringify(mounted!.toJSON())).toContain("Binary changes cannot be displayed");
  });

  it("shows an oversized diff without fetching the editor", async () => {
    const client = gitClient({ tooLarge: true });
    await act(async () => {
      mounted = create(<GitDiffSurface
        activeRoot={root}
        canWrite
        onMessage={vi.fn()}
        repositories={new GitRepositoryStore(client)}
        scope={scope}
        tab={gitTab}
      />);
    });
    await settle();
    expect(JSON.stringify(mounted!.toJSON())).toContain("too large for the editor");
  });
});

const gitTab: AppOwnedTab = {
  id: "diff-1", kind: "gitDiff", title: "a", resource: "a",
  hostProfileId: "local", serverIdentity: "s", sessionId: "$1", sessionName: "work", order: 0,
  rootPath: "/repo", rootToken: "root",
  gitRepositoryId: "repo", gitPath: "YQ==", gitTarget: "unstaged",
};

const gitStatus: GitStatusSnapshot = {
  repository: { id: "repo", worktreeRoot: "/repo", initial: false, detachedHead: false, headName: "main" },
  generation: "7", sourceGeneration: "status-source", authoritative: true,
  entries: [{
    path: "YQ==", displayPath: "a", indexKind: "none", worktreeKind: "modified", indexStatus: ".", worktreeStatus: "M",
    conflicted: false, untracked: false, ignored: false, submodule: false, symlink: false, binary: false,
  }],
};

function gitClient(shape: { binary?: boolean; tooLarge?: boolean }): GitWorkspaceClient {
  const diff: GitDiff = {
    repository: gitStatus.repository, target: "unstaged", path: "YQ==", displayPath: "a",
    oldContent: new TextEncoder().encode("old\n"), newContent: new TextEncoder().encode("new\n"),
    sourceGeneration: "diff-source", binary: shape.binary ?? false, tooLarge: shape.tooLarge ?? false,
    oldMissing: false, newMissing: false, hunkCount: 1,
  };
  return {
    status: vi.fn(async () => gitStatus),
    watch: vi.fn(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: gitStatus, release: vi.fn() })),
    diff: vi.fn(async () => ({ diff, status: gitStatus })),
    prepareDiscard: vi.fn(),
    mutate: vi.fn(),
    commit: vi.fn(),
    subscribe: vi.fn(() => () => undefined),
  };
}
