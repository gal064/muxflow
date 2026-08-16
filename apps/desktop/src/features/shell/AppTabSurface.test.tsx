// @vitest-environment jsdom
// jsdom, because the surface mounts an editor host and measures its layout.
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { AppTabSurface } from "./AppTabSurface";
import type { AppOwnedTab } from "./types";
import type {
  ActiveRoot,
  DirectoryListing,
  DirectoryWatchLease,
  FileEntry,
  FileWorkspaceClient,
  FileWorkspaceScope,
  OpenFile,
  WorkspaceEvent,
} from "../files/types";

// Monaco itself is not under test here, and loading it in jsdom pulls in the
// browser clipboard contribution. P6 owns making this boundary lazy in
// production; this file only asserts the file-open data flow around it.
function EditorStub(_props: { onChange?(value: string): void }) { return null; }
vi.mock("@monaco-editor/react", () => ({ default: (props: { onChange?(value: string): void }) => <EditorStub {...props} /> }));
vi.mock("../files/monaco", () => ({ ADE_MONACO_THEME: "ade-test-theme" }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const scope: FileWorkspaceScope = {
  clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1,
  terminalEpoch: 41, sessionId: "$1", paneId: "%1",
};
const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
const tab: AppOwnedTab = {
  id: "tab-1", kind: "file", title: "note.txt", resource: "/repo/note.txt",
  hostProfileId: "local", serverIdentity: "s", sessionId: "$1", sessionName: "work", order: 0, preview: false,
  rootPath: "/repo", rootToken: "root",
};

function entry(path: string, generation: string): FileEntry {
  return {
    path, name: path.split("/").at(-1) ?? path, kind: "file", sizeBytes: "5",
    modifiedMillis: "1", generation, executable: false, expandable: false,
  };
}

function listing(entries: FileEntry[], overrides: Partial<DirectoryListing> = {}): DirectoryListing {
  return { rootToken: "root", directory: "/repo", revision: "1", entries, recoveredFromOverflow: false, complete: true, ...overrides };
}

function opened(generation: string): OpenFile {
  return {
    kind: "text",
    file: { path: "/repo/note.txt", content: "hello", generation, sizeBytes: "5", lineEnding: "none", encoding: "utf-8" },
  };
}

interface Fixture {
  bootstrap: DirectoryListing;
  generations?: string[];
}

function surfaceClient(fixture: Fixture) {
  const generations = [...(fixture.generations ?? ["g1"])];
  const opens: string[] = [];
  const writes: string[] = [];
  let listener: ((event: WorkspaceEvent) => void) | undefined;
  const client = {
    openFile: vi.fn(async () => {
      const generation = generations.length > 1 ? generations.shift()! : generations[0];
      opens.push(generation);
      return opened(generation);
    }),
    writeText: vi.fn(async (_scope: FileWorkspaceScope, _root: ActiveRoot, request: { operationId: string }) => {
      writes.push(request.operationId);
      return { path: "/repo/note.txt", generation: "saved", operationId: request.operationId, sizeBytes: "5" };
    }),
    acquireDirectoryWatch: vi.fn(async (): Promise<DirectoryWatchLease> => ({
      snapshot: fixture.bootstrap,
      release: () => undefined,
    })),
    subscribe: vi.fn(async (_scope: FileWorkspaceScope, next: (event: WorkspaceEvent) => void) => {
      listener = next;
      return () => { listener = undefined; };
    }),
    listDirectory: vi.fn(), resolveActiveRoot: vi.fn(),
    mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
  } as unknown as FileWorkspaceClient;
  return { client, opens, writes, publish: (event: WorkspaceEvent) => listener?.(event) };
}

async function mount(fixture: Fixture) {
  const surface = surfaceClient(fixture);
  let renderer!: ReturnType<typeof create>;
  await act(async () => {
    renderer = create(<AppTabSurface
      canWrite
      client={surface.client}
      activeRoot={root}
      onDirty={vi.fn()}
      onDownload={vi.fn()}
      onStatus={vi.fn()}
      onViewMode={vi.fn()}
      scope={scope}
      tab={tab}
    />);
  });
  await act(async () => { await Promise.resolve(); });
  await act(async () => { await Promise.resolve(); });
  return { ...surface, renderer };
}

describe("AppTabSurface", () => {
  it("accepts the first read when the watch bootstrap agrees, rather than re-reading on principle", async () => {
    const surface = await mount({ bootstrap: listing([entry("/repo/note.txt", "g1")]) });
    expect(surface.opens, "the watch bootstrap triggered a second full open").toEqual(["g1"]);
    expect(surface.client.acquireDirectoryWatch).toHaveBeenCalledTimes(1);
    await act(async () => { surface.renderer.unmount(); });
  });

  it("reloads exactly once when the bootstrap proves the file moved under the read", async () => {
    const surface = await mount({
      bootstrap: listing([entry("/repo/note.txt", "g2")]),
      generations: ["g1", "g2"],
    });
    expect(surface.opens).toEqual(["g1", "g2"]);
    // And it does not keep chasing: the reconciliation is once per bootstrap.
    await act(async () => { await Promise.resolve(); });
    expect(surface.opens).toEqual(["g1", "g2"]);
    await act(async () => { surface.renderer.unmount(); });
  });

  it("ignores an authoritative rescan that says nothing about this file", async () => {
    const surface = await mount({ bootstrap: listing([entry("/repo/note.txt", "g1")]) });
    await act(async () => {
      surface.publish({
        kind: "directorySnapshot",
        rootToken: "root",
        listing: listing([entry("/repo/note.txt", "g1"), entry("/repo/other.txt", "z9")]),
      });
      await Promise.resolve();
    });
    expect(surface.opens, "a rescan about a neighbouring file cost a remote read").toEqual(["g1"]);
    await act(async () => { surface.renderer.unmount(); });
  });

  it("reloads when an authoritative rescan carries a newer generation for this file", async () => {
    const surface = await mount({
      bootstrap: listing([entry("/repo/note.txt", "g1")]),
      generations: ["g1", "g5"],
    });
    await act(async () => {
      surface.publish({
        kind: "directorySnapshot",
        rootToken: "root",
        listing: listing([entry("/repo/note.txt", "g5")]),
      });
      await Promise.resolve();
    });
    expect(surface.opens).toEqual(["g1", "g5"]);
    await act(async () => { surface.renderer.unmount(); });
  });

  it("keeps a tab whose file a listing simply does not mention", async () => {
    // Absence from a listing is not authority to declare a deletion: a page
    // boundary, a name the host never reports, or a path spelled differently
    // would all retire a perfectly live editor.
    const surface = await mount({ bootstrap: listing([entry("/repo/other.txt", "z9")]) });
    expect(surface.opens).toEqual(["g1"]);
    expect(JSON.stringify(surface.renderer.toJSON())).not.toContain("deleted externally");
    await act(async () => {
      surface.publish({
        kind: "directorySnapshot", rootToken: "root",
        listing: listing([entry("/repo/other.txt", "z9")]),
      });
      await Promise.resolve();
    });
    expect(surface.opens).toEqual(["g1"]);
    expect(JSON.stringify(surface.renderer.toJSON())).not.toContain("deleted externally");
    // An explicit delete event is the one thing that may retire it.
    await act(async () => {
      surface.publish({ kind: "fileDeleted", rootToken: "root", path: "/repo/note.txt" });
      await Promise.resolve();
    });
    expect(JSON.stringify(surface.renderer.toJSON())).toContain("deleted externally");
    await act(async () => { surface.renderer.unmount(); });
  });

  it("never reconciles a symlink against an entry that describes the link", async () => {
    // The link's identity does not move when its target is rewritten, so
    // comparing it against the bytes would re-open every symlinked file twice.
    const link = { ...entry("/repo/note.txt", "link-identity"), kind: "symlink" as const, symlinkTarget: "target.txt" };
    const surface = await mount({ bootstrap: listing([link]), generations: ["g1", "g2"] });
    expect(surface.opens).toEqual(["g1"]);
    await act(async () => {
      surface.publish({ kind: "directorySnapshot", rootToken: "root", listing: listing([link]) });
      await Promise.resolve();
    });
    expect(surface.opens).toEqual(["g1"]);
    await act(async () => { surface.renderer.unmount(); });
  });

  it("ignores the echo of a precise event describing the content it already has", async () => {
    const surface = await mount({ bootstrap: listing([entry("/repo/note.txt", "g1")]) });
    await act(async () => {
      surface.publish({ kind: "fileChanged", rootToken: "root", path: "/repo/note.txt", generation: "g1" });
      await Promise.resolve();
    });
    expect(surface.opens).toEqual(["g1"]);
    await act(async () => {
      surface.publish({ kind: "fileChanged", rootToken: "root", path: "/repo/note.txt", generation: "g7" });
      await Promise.resolve();
    });
    expect(surface.opens).toHaveLength(2);
    await act(async () => { surface.renderer.unmount(); });
  });

  it("costs no remote read for the echo of a save it performed itself", async () => {
    vi.useFakeTimers();
    const surface = await mount({ bootstrap: listing([entry("/repo/note.txt", "g1")]) });
    expect(surface.opens).toEqual(["g1"]);
    // Type, then let autosave publish. The controller now owns generation
    // "saved"; anything holding a private copy of "g1" would treat the echo of
    // its own write as an external change and re-open the file remotely.
    const editor = surface.renderer.root.findByType(EditorStub);
    await act(async () => { editor.props.onChange("hello there"); });
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(surface.writes).toHaveLength(1);
    await act(async () => {
      surface.publish({
        kind: "fileChanged", rootToken: "root", path: "/repo/note.txt",
        generation: "saved", operationId: surface.writes[0],
      });
      await Promise.resolve();
    });
    expect(surface.opens, "the echo of our own save cost a remote re-open").toEqual(["g1"]);
    await act(async () => {
      surface.publish({
        kind: "directorySnapshot", rootToken: "root",
        listing: listing([entry("/repo/note.txt", "saved")]),
      });
      await Promise.resolve();
    });
    expect(surface.opens).toEqual(["g1"]);
    await act(async () => { surface.renderer.unmount(); });
    vi.useRealTimers();
  });
});
