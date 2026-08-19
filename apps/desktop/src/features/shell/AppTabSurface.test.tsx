// @vitest-environment jsdom
// jsdom, because the surface mounts an editor host and measures its layout.
import { act, create } from "react-test-renderer";
import { beforeAll, describe, expect, it, vi } from "vitest";
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
// browser clipboard contribution. The lazy boundary that keeps it out of the
// surface's own module lives in `AppTabSurface`; this file asserts the
// file-open data flow around it.
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
  /** False when the Explorer had already armed this directory's watch. */
  fresh?: boolean;
  /** Holds every open until released, so a late answer can be aimed at a dead surface. */
  holdOpens?: boolean;
  /** Holds the watch bootstrap, so the read can be made to land first. */
  holdWatch?: boolean;
}

function surfaceClient(fixture: Fixture) {
  const generations = [...(fixture.generations ?? ["g1"])];
  const opens: string[] = [];
  const writes: string[] = [];
  const signals: AbortSignal[] = [];
  const releases: number[] = [];
  const settleOpen: Array<() => void> = [];
  let releaseWatch: (() => void) | undefined;
  let listener: ((event: WorkspaceEvent) => void) | undefined;
  const client = {
    openFile: vi.fn(async (
      _scope: FileWorkspaceScope,
      _root: ActiveRoot,
      _path: string,
      signal?: AbortSignal,
    ) => {
      if (signal) signals.push(signal);
      const generation = generations.length > 1 ? generations.shift()! : generations[0];
      if (fixture.holdOpens) {
        await new Promise<void>((resolve) => { settleOpen.push(resolve); });
      }
      opens.push(generation);
      return opened(generation);
    }),
    writeText: vi.fn(async (_scope: FileWorkspaceScope, _root: ActiveRoot, request: { operationId: string }) => {
      writes.push(request.operationId);
      return { path: "/repo/note.txt", generation: "saved", operationId: request.operationId, sizeBytes: "5" };
    }),
    acquireDirectoryWatch: vi.fn(async (): Promise<DirectoryWatchLease> => {
      if (fixture.holdWatch) {
        await new Promise<void>((resolve) => { releaseWatch = resolve; });
      }
      return {
        fresh: fixture.fresh ?? true,
        snapshot: fixture.bootstrap,
        release: () => releases.push(releases.length + 1),
      };
    }),
    subscribe: vi.fn(async (_scope: FileWorkspaceScope, next: (event: WorkspaceEvent) => void) => {
      listener = next;
      return () => { listener = undefined; };
    }),
    listDirectory: vi.fn(), resolveActiveRoot: vi.fn(),
    mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
  } as unknown as FileWorkspaceClient;
  return {
    client, opens, writes, signals, releases,
    publish: (event: WorkspaceEvent) => listener?.(event),
    settleOpens: () => { settleOpen.splice(0).forEach((resolve) => resolve()); },
    settleWatch: () => { releaseWatch?.(); releaseWatch = undefined; },
  };
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
  // The editor is imported lazily, so in production the surface renders its
  // Suspense fallback while that chunk arrives. Resolving the module once up
  // front leaves these tests measuring the surface rather than however long the
  // runner takes to transform a file.
  beforeAll(async () => { await import("../files/FileEditor"); });

  it("waits before it says it is loading, and says it the same way the editor stage does", async () => {
    const surface = await mount({ bootstrap: listing([entry("/repo/note.txt", "g1")]), holdOpens: true });
    const pending = JSON.stringify(surface.renderer.toJSON());
    expect(pending).toContain("Loading…");
    // The delay is the point, and it is the class that carries it: the line is
    // in the DOM from the first frame and invisible until the read has lasted
    // long enough to be worth explaining, so a fast open shows the frame and
    // then the file with no text in between.
    expect(pending).toContain("loading-delayed");
    // Stage-specific wording would give away that two indicators handed off.
    expect(pending, "the read stage named itself instead of matching the editor stage").not.toContain("Loading file");

    await act(async () => { surface.settleOpens(); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(
      JSON.stringify(surface.renderer.toJSON()),
      "the indicator outlived the content it was standing in for",
    ).not.toContain("Loading…");
    await act(async () => { surface.renderer.unmount(); });
  });

  it("accepts the first read when the watch bootstrap agrees, rather than re-reading on principle", async () => {
    const surface = await mount({ bootstrap: listing([entry("/repo/note.txt", "g1")]) });
    expect(surface.opens, "the watch bootstrap triggered a second full open").toEqual(["g1"]);
    expect(surface.client.acquireDirectoryWatch).toHaveBeenCalledTimes(1);
    await act(async () => { surface.renderer.unmount(); });
  });

  it("never re-opens the file because a watch it merely joined disagrees", async () => {
    // The Explorer already had this folder open, so the bootstrap is the
    // listing from whenever *that* happened — arbitrarily older than the read
    // that just completed. Reconciling against it re-read the whole file
    // remotely on the strength of a row nobody claimed was current.
    const surface = await mount({
      bootstrap: listing([entry("/repo/note.txt", "stale")]),
      generations: ["g1", "g2"],
      fresh: false,
    });
    expect(surface.opens, "a joined watch's stale bootstrap forced a second open").toEqual(["g1"]);
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

  it("never restarts a read that has not landed yet, however often the directory is rescanned", async () => {
    // The large-file loop, in miniature (tests/phase15/large-file-open-bug.md).
    // A 5.2 MB file's read outlives the interval between directory snapshots,
    // so it is still in flight when the next one arrives. Nothing has been
    // shown yet, so `shownGeneration()` is undefined and every rescan looked
    // like a generation mismatch -- aborting the read that was making progress
    // and starting another that would be aborted in turn. 171 attempts, zero
    // successes, forever.
    const surface = await mount({
      bootstrap: listing([entry("/repo/note.txt", "g1")]),
      generations: ["g1", "g5"],
      holdOpens: true,
      holdWatch: true,
    });
    expect(surface.opens, "the read is deliberately still in flight").toHaveLength(0);
    expect(surface.signals).toHaveLength(1);

    for (const generation of ["g2", "g3", "g4"]) {
      await act(async () => {
        surface.publish({
          kind: "directorySnapshot",
          rootToken: "root",
          listing: listing([entry("/repo/note.txt", generation)]),
        });
        await Promise.resolve();
      });
    }

    expect(
      surface.signals[0]?.aborted,
      "a rescan aborted the in-flight read, which is the loop this file documents",
    ).toBe(false);
    expect(
      surface.signals,
      "every rescan started another read on top of the one already running",
    ).toHaveLength(1);

    // The deferred opinion is still honoured: once the read lands it is
    // compared against the newest listing, and re-read once because it moved.
    await act(async () => { surface.settleOpens(); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(surface.opens[0]).toBe("g1");
    await act(async () => { surface.renderer.unmount(); });
  });

  it("never restarts a read that has not landed yet, however often the file is said to have changed", async () => {
    // The same loop through the other branch, and the shape the live app
    // actually died in: the host echoed the kernel's access events for this
    // client's *own read* back as fileChanged, so every read attempt produced
    // the event that aborted the next one -- 355 of 356 read attempts in one
    // measured session were this. The host no longer echoes accesses, but
    // whatever produces a change-event stream, the surface must not let it
    // abort the very read that would answer it.
    const surface = await mount({
      bootstrap: listing([entry("/repo/note.txt", "g1")]),
      generations: ["g1", "g5"],
      holdOpens: true,
      holdWatch: true,
    });
    expect(surface.opens, "the read is deliberately still in flight").toHaveLength(0);
    expect(surface.signals).toHaveLength(1);

    for (const generation of ["g2", "g3", "g4"]) {
      await act(async () => {
        surface.publish({
          kind: "fileChanged",
          rootToken: "root",
          path: "/repo/note.txt",
          generation,
        });
        await Promise.resolve();
      });
    }

    expect(
      surface.signals[0]?.aborted,
      "a change event aborted the in-flight read instead of waiting for it",
    ).toBe(false);
    expect(
      surface.signals,
      "every change event started another read on top of the one already running",
    ).toHaveLength(1);

    // The parked notice is still honoured: the landed read is compared against
    // the newest claimed generation and re-read once because it differs.
    await act(async () => { surface.settleOpens(); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(surface.opens[0]).toBe("g1");
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
  /**
   * The plan's own required outcome for a remote open: "a cancelled/stale open
   * publishes no late buffer", and "one owning cancellation spans control and
   * bulk work".
   *
   * Nothing tested either. The surface holds one `AbortController` per load and
   * one serial; a closing tab has to abort the in-flight read *and* refuse its
   * answer if it arrives anyway, because aborting a request already on the wire
   * does not un-send the bytes.
   */
  it("aborts an in-flight open on unmount and publishes nothing when it answers late", async () => {
    const surface = await mount({
      bootstrap: listing([entry("/repo/note.txt", "g1")]),
      holdOpens: true,
    });
    expect(surface.opens, "the read should still be in flight").toEqual([]);
    expect(surface.signals, "the open was issued without a cancellation").toHaveLength(1);
    expect(surface.signals[0].aborted).toBe(false);

    await act(async () => { surface.renderer.unmount(); });
    expect(surface.signals[0].aborted, "closing the tab left the remote read running").toBe(true);

    // And the answer arrives anyway, as it does whenever the abort loses the
    // race with the wire.
    await act(async () => { surface.settleOpens(); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(surface.renderer.toJSON(), "a dead surface published a late buffer").toBeNull();
  });

  it("releases the parent watch lease when the tab closes", async () => {
    const surface = await mount({ bootstrap: listing([entry("/repo/note.txt", "g1")]) });
    expect(surface.releases, "the lease was released before the tab closed").toEqual([]);
    await act(async () => { surface.renderer.unmount(); });
    expect(
      surface.releases,
      "closing the tab left a directory watch armed on the host forever",
    ).toEqual([1]);
  });

  it("abandons a watch bootstrap still in flight when the tab closes", async () => {
    // The bootstrap is a full directory listing. A tab closed while it is in
    // flight must stop paying for it rather than receive it and throw it away.
    const surface = await mount({
      bootstrap: listing([entry("/repo/note.txt", "g1")]),
      holdWatch: true,
    });
    await act(async () => { surface.renderer.unmount(); });
    await act(async () => { surface.settleWatch(); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    // Released rather than retained: the lease resolved after the unmount, and
    // the surface hands it straight back.
    expect(surface.releases, "a lease acquired after unmount was never released").toEqual([1]);
  });

  /**
   * A parked bootstrap opinion belongs to the read it was waiting for.
   *
   * Without the serial, an opinion the bootstrap parked for read #1 was
   * consumed by whichever read happened to finish next — so a later, unrelated
   * load compared its own content against a generation that described a
   * different read entirely, and re-opened the file for it.
   */
  it("never lets a parked bootstrap opinion reconcile a later, unrelated read", async () => {
    // The bootstrap describes "g1" and lands first. Read #1 also answers "g1",
    // consuming the opinion; the external change that follows answers "g2" and
    // must not be re-read against the bootstrap's stale opinion.
    const surface = await mount({
      bootstrap: listing([entry("/repo/note.txt", "g1")]),
      generations: ["g1", "g2", "g3"],
    });
    expect(surface.opens).toEqual(["g1"]);
    await act(async () => {
      surface.publish({ kind: "fileChanged", rootToken: "root", path: "/repo/note.txt", generation: "g2" });
      await Promise.resolve();
    });
    await act(async () => { await Promise.resolve(); });
    expect(
      surface.opens,
      "a consumed bootstrap opinion re-opened a later read that had nothing to do with it",
    ).toEqual(["g1", "g2"]);
    await act(async () => { surface.renderer.unmount(); });
  });
});
