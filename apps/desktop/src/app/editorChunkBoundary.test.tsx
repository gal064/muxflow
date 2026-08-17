// @vitest-environment jsdom
// jsdom, because both surfaces mount an editor host and measure its layout.
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
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
 * What has been evaluated, in the order it happened.
 *
 * The point of the lazy boundary is that the editor bundle is fetched and
 * evaluated *after* the remote read has been issued. That ordering is not
 * observable from the rendered output, so the module standing in for Monaco
 * announces itself here and the data clients announce their reads into the
 * same list.
 *
 * The mirror-image claim — that content which never reaches an editor never
 * fetches the bundle at all — needs a module registry in which nothing has
 * imported it, so it lives in `editorChunkAbsence.test.tsx`: one claim per
 * registry, one registry per file.
 */
const probe = vi.hoisted(() => ({ evaluated: [] as string[] }));

function EditorStub(_props: { value: string; onChange(content: string): void }) { return null; }
function DiffEditorStub(_props: { original: string; modified: string }) { return null; }

vi.mock("@monaco-editor/react", () => {
  probe.evaluated.push("editor module");
  return {
    default: (props: { value: string; onChange(content: string): void }) => <EditorStub {...props} />,
    DiffEditor: (props: { original: string; modified: string }) => <DiffEditorStub {...props} />,
  };
});
// The real module registers Monaco's workers and defines the theme against the
// live token file; neither is what these tests are about.
vi.mock("../features/files/monaco", () => ({ ADE_MONACO_THEME: "ade-test-theme" }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const scope: FileWorkspaceScope = {
  clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1,
  terminalEpoch: 41, sessionId: "$1", paneId: "%1",
};
const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
const markdownTab: AppOwnedTab = {
  id: "tab-1", kind: "markdown", title: "note.md", resource: "/repo/note.md", viewMode: "split",
  hostProfileId: "local", serverIdentity: "s", sessionId: "$1", sessionName: "work", order: 0, preview: false,
  rootPath: "/repo", rootToken: "root",
};

function textFile(content: string): OpenFile {
  return {
    kind: "text",
    file: { path: "/repo/note.md", content, generation: "g1", sizeBytes: String(content.length), lineEnding: "lf", encoding: "utf-8" },
  };
}

/**
 * The real timer and clock, captured before any test installs a fake one.
 *
 * Resolving a lazily imported module is real work on the event loop, and a test
 * that has taken over `setTimeout` to drive autosave must still be able to wait
 * for it.
 */
const realSetTimeout = globalThis.setTimeout;
const realNow = globalThis.Date.now;

/**
 * Turns the loop, inside `act`, until `until` holds or the budget runs out.
 *
 * Each turn is its own `act` scope on purpose: React commits the state update
 * that renders the lazy element when the scope exits, so a single scope that
 * waited inside itself would be waiting for an import that had not been
 * started yet.
 */
async function turnLoop(until: () => boolean, budgetMs: number) {
  const started = realNow();
  do {
    await act(async () => {
      await Promise.resolve();
      await new Promise((resolve) => { realSetTimeout(resolve, 1); });
    });
    if (until()) return;
  } while (realNow() - started < budgetMs);
}

function fileClient(opened: OpenFile) {
  const writes: Array<{ content: string }> = [];
  const client = {
    openFile: vi.fn(async () => {
      probe.evaluated.push("openFile");
      return opened;
    }),
    writeText: vi.fn(async (_scope: FileWorkspaceScope, _root: ActiveRoot, request: { content: string; operationId: string }) => {
      writes.push({ content: request.content });
      return { path: "/repo/note.md", generation: "saved", operationId: request.operationId, sizeBytes: "5" };
    }),
    acquireDirectoryWatch: vi.fn(async (): Promise<DirectoryWatchLease> => ({
      fresh: true,
      snapshot: { rootToken: "root", directory: "/repo", revision: "1", entries: [], recoveredFromOverflow: false, complete: true },
      release: () => undefined,
    })),
    subscribe: vi.fn(async (_scope: FileWorkspaceScope, _next: (event: WorkspaceEvent) => void) => () => undefined),
    listDirectory: vi.fn(), resolveActiveRoot: vi.fn(),
    mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
  } as unknown as FileWorkspaceClient;
  return { client, writes };
}

async function mountMarkdown(content: string) {
  const host = fileClient(textFile(content));
  let renderer!: ReturnType<typeof create>;
  const render = (tab: AppOwnedTab) => <AppTabSurface
    canWrite
    client={host.client}
    activeRoot={root}
    onDirty={vi.fn()}
    onDownload={vi.fn()}
    onStatus={vi.fn()}
    onViewMode={vi.fn()}
    scope={scope}
    tab={tab}
  />;
  await act(async () => { renderer = create(render(markdownTab)); });
  await turnLoop(() => renderer.root.findAllByType(EditorStub).length > 0, 10_000);
  return {
    ...host,
    renderer,
    async update(next: Partial<AppOwnedTab>) {
      await act(async () => { renderer.update(render({ ...markdownTab, ...next })); });
      await turnLoop(() => false, 5);
    },
  };
}

describe("the editor chunk boundary", () => {
  it("issues the remote read before the editor module is evaluated", async () => {
    const surface = await mountMarkdown("hello");
    expect(
      probe.evaluated,
      "the editor bundle was evaluated before the file host had been asked for anything",
    ).toEqual(["openFile", "editor module"]);
    expect(surface.renderer.root.findAllByType(EditorStub)).toHaveLength(1);
    await act(async () => { surface.renderer.unmount(); });
  });

  it("keeps a dirty buffer and its pending save when the editor unmounts", async () => {
    // Autosave state lives above the lazy boundary. Held inside it, the editor
    // going away — a Markdown view switched to preview, a chunk transition —
    // would take an unsaved buffer with it, or flush it as a side effect of a
    // rendering decision.
    const surface = await mountMarkdown("hello");
    // Installed after the mount, so the editor chunk resolves on the real loop.
    vi.useFakeTimers();
    try {
      const editor = surface.renderer.root.findByType(EditorStub);
      await act(async () => { editor.props.onChange("hello there"); });
      expect(JSON.stringify(surface.renderer.toJSON())).toContain("Unsaved");

      await surface.update({ viewMode: "preview" });
      expect(surface.renderer.root.findAllByType(EditorStub), "the editor survived a preview-only view").toHaveLength(0);
      expect(surface.writes, "unmounting the editor flushed the buffer as a side effect").toEqual([]);
      expect(JSON.stringify(surface.renderer.toJSON()), "the dirty state went away with the editor").toContain("Unsaved");

      await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
      expect(surface.writes, "the pending save was lost with the editor").toEqual([{ content: "hello there" }]);

      // And the buffer is still the typed one when the editor comes back.
      await surface.update({ viewMode: "split" });
      expect(surface.renderer.root.findByType(EditorStub).props.value).toBe("hello there");
      await act(async () => { surface.renderer.unmount(); });
    } finally {
      vi.useRealTimers();
    }
  });

  it("publishes the Markdown preview on a delay while the buffer and save state stay immediate", async () => {
    const surface = await mountMarkdown("before");
    vi.useFakeTimers();
    try {
      const previewHtml = () => surface.renderer.root
        .findByProps({ className: "markdown-preview" }).props.dangerouslySetInnerHTML.__html as string;
      expect(previewHtml(), "the first preview waited for a timer").toContain("before");
      const editor = surface.renderer.root.findByType(EditorStub);
      await act(async () => { editor.props.onChange("after"); });
      expect(surface.renderer.root.findByType(EditorStub).props.value, "the editor waited for the preview").toBe("after");
      expect(JSON.stringify(surface.renderer.toJSON()), "the save state waited for the preview").toContain("Unsaved");
      expect(previewHtml(), "the preview was re-sanitized on the keystroke").toContain("before");

      await act(async () => { await vi.advanceTimersByTimeAsync(500); });
      expect(previewHtml()).toContain("after");
      await act(async () => { surface.renderer.unmount(); });
    } finally {
      vi.useRealTimers();
    }
  });

  it("issues the diff request before the diff editor module is evaluated", async () => {
    probe.evaluated.length = 0;
    const client = gitClient();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...gitProps(client)} />); });
    await turnLoop(() => renderer.root.findAllByType(DiffEditorStub).length > 0, 10_000);
    expect(probe.evaluated[0], "the diff editor rendered before the diff was asked for").toBe("diff");
    expect(renderer.root.findAllByType(DiffEditorStub)).toHaveLength(1);
    await act(async () => { renderer.unmount(); });
  });
});

const gitStatus: GitStatusSnapshot = {
  repository: { id: "repo", worktreeRoot: "/repo", initial: false, detachedHead: false, headName: "main" },
  generation: "7", sourceGeneration: "status-source", authoritative: true,
  entries: [{
    path: "YQ==", displayPath: "a", indexKind: "none", worktreeKind: "modified", indexStatus: ".", worktreeStatus: "M",
    conflicted: false, untracked: false, ignored: false, submodule: false, symlink: false, binary: false,
  }],
};

const gitDiff: GitDiff = {
  repository: gitStatus.repository, target: "unstaged", path: "YQ==", displayPath: "a",
  oldContent: new TextEncoder().encode("old\n"), newContent: new TextEncoder().encode("new\n"),
  sourceGeneration: "diff-source", binary: false, tooLarge: false, oldMissing: false, newMissing: false, hunkCount: 1,
};

function gitClient(): GitWorkspaceClient {
  return {
    status: vi.fn(async () => gitStatus),
    watch: vi.fn(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: gitStatus, release: vi.fn() })),
    diff: vi.fn(async () => {
      probe.evaluated.push("diff");
      return { diff: gitDiff, status: gitStatus };
    }),
    prepareDiscard: vi.fn(async () => "one-time"),
    mutate: vi.fn(),
    commit: vi.fn(),
    subscribe: vi.fn(() => () => undefined),
  };
}

function gitProps(client: GitWorkspaceClient) {
  const tab: AppOwnedTab = {
    id: "diff-1", kind: "gitDiff", title: "a", resource: "a",
    hostProfileId: "local", serverIdentity: "s", sessionId: "$1", sessionName: "work", order: 0,
    rootPath: "/repo", rootToken: "root",
    gitRepositoryId: "repo", gitPath: "YQ==", gitTarget: "unstaged" as const,
  };
  return {
    tab, scope, activeRoot: root, canWrite: true,
    repositories: new GitRepositoryStore(client),
    onMessage: vi.fn(),
  };
}
