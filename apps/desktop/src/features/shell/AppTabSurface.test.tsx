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
vi.mock("@monaco-editor/react", () => ({ default: () => null }));
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
  return { rootToken: "root", directory: "/repo", revision: "1", entries, overflowRecovery: false, complete: true, ...overrides };
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
  let listener: ((event: WorkspaceEvent) => void) | undefined;
  const client = {
    openFile: vi.fn(async () => {
      const generation = generations.length > 1 ? generations.shift()! : generations[0];
      opens.push(generation);
      return opened(generation);
    }),
    acquireDirectoryWatch: vi.fn(async (): Promise<DirectoryWatchLease> => ({
      snapshot: fixture.bootstrap,
      release: () => undefined,
    })),
    subscribe: vi.fn(async (_scope: FileWorkspaceScope, next: (event: WorkspaceEvent) => void) => {
      listener = next;
      return () => { listener = undefined; };
    }),
    writeText: vi.fn(), listDirectory: vi.fn(), resolveActiveRoot: vi.fn(),
    mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
  } as unknown as FileWorkspaceClient;
  return { client, opens, publish: (event: WorkspaceEvent) => listener?.(event) };
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
});
