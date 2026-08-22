// @vitest-environment jsdom
// jsdom, because opening a row action's dialog goes through `useModalDialog`,
// which manages real focus.
import { renderToStaticMarkup } from "react-dom/server";
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { rowCommandRegistry } from "../../commands/rowCommands";
import { enablePerfProbe, perfHighWaterSnapshot, resetPerfProbe } from "../../perf/probe";
import { ExplorerTree } from "./ExplorerTree";
import type { ActiveRoot, DirectoryListing } from "./types";
import { INTERNAL_PATH_DRAG_TYPE } from "../terminal/internalPathDrag";

const root: ActiveRoot = { token: "root-1", paneId: "%1", cwd: "/r", path: "/r", gitWorktree: true, revision: "1" };
const listing: DirectoryListing = {
  rootToken: "root-1", directory: "/r", revision: "2", recoveredFromOverflow: false, complete: true,
  entries: [
    { path: "/r/.env", name: ".env", kind: "file", sizeBytes: "10", modifiedMillis: "1", generation: "1", executable: false, expandable: false },
    { path: "/r/ignored.log", name: "ignored.log", kind: "file", sizeBytes: "20", modifiedMillis: "1", generation: "1", executable: false, expandable: false },
    { path: "/r/.git", name: ".git", kind: "directory", sizeBytes: "0", modifiedMillis: "1", generation: "1", executable: false, expandable: false },
    { path: "/r/node_modules", name: "node_modules", kind: "directory", sizeBytes: "0", modifiedMillis: "1", generation: "1", executable: false, expandable: false },
    { path: "/r/link", name: "link", kind: "symlink", sizeBytes: "0", modifiedMillis: "1", generation: "1", executable: false, expandable: false, targetKind: "directory", symlinkTarget: "/outside" },
  ],
};

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("ExplorerTree", () => {
  it("writes private same-host payloads for both file and folder rows", async () => {
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<ExplorerTree root={root} scopeIdentity="scope" hostProfileId="local" serverIdentity="server-a"
      listings={new Map([["/r", listing]])} expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />); });
    const values = new Map<string, string>();
    const dataTransfer = { effectAllowed: "all", setData: (type: string, value: string) => values.set(type, value) };
    const rows = renderer.root.findAllByProps({ className: "file-row" });
    for (const row of [rows[0], rows[2]]) row.props.onDragStart({ dataTransfer });
    expect(JSON.parse(values.get(INTERNAL_PATH_DRAG_TYPE)!)).toMatchObject({ version: 1, hostProfileId: "local", serverIdentity: "server-a", path: "/r/.git" });
    expect(JSON.parse(values.get(INTERNAL_PATH_DRAG_TYPE)!).gestureId).toEqual(expect.any(String));
    expect(rows[0].props.draggable).toBe(true);
    await act(async () => { renderer.unmount(); });
  });

  it("disables path drag when rows are not bound to a live host identity", async () => {
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<ExplorerTree root={root} scopeIdentity="stale"
      listings={new Map([["/r", listing]])} expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />); });
    expect(renderer.root.findAllByProps({ className: "file-row" })[0].props.draggable).toBe(false);
    await act(async () => { renderer.unmount(); });
  });

  it("exposes the deterministic 4,096-row Phase 14 DOM high-water hook", async () => {
    enablePerfProbe(async () => undefined);
    const wide: DirectoryListing = {
      ...listing,
      entries: Array.from({ length: 4_096 }, (_, index) => ({
        path: `/r/wide/file-${index}`, name: `file-${index}`, kind: "file" as const,
        sizeBytes: "1", modifiedMillis: "1", generation: "1", executable: false, expandable: false,
      })),
    };
    let renderer!: ReturnType<typeof create>;
    try {
      const startedAt = performance.now();
      await act(async () => {
        renderer = create(<ExplorerTree root={root} scopeIdentity="phase14" listings={new Map([["/r", wide]])}
          expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
          onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()}
          onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
      });
      // Recorded, deliberately not asserted: a jsdom mount is not a browser
      // paint, so this informs the windowing decision rather than gating it.
      // The 150 ms budget belongs to the instrumented runtime lane.
      const mountMillis = Math.round(performance.now() - startedAt);
      const highWater = perfHighWaterSnapshot();
      const renderedRows = renderer.root.findAllByProps({ className: "file-row" }).length;
      const logicalRows = highWater["explorer.logicalRows"];
      expect(logicalRows).toBe(wide.entries.length);
      expect(renderedRows).toBeGreaterThan(0);
      expect(highWater["explorer.renderedRows"]).toBe(renderedRows);
      // The invariant the row budget actually rests on: what the tree mounts is
      // a function of the viewport, not of the directory. `rendered <= logical`
      // would hold with no windowing at all, which is what this lane exists to
      // detect the absence of — so the same tree is measured four times larger
      // and must mount exactly as much.
      let larger!: ReturnType<typeof create>;
      const quadrupled = {
        ...wide,
        entries: Array.from({ length: 16_384 }, (_, index) => ({
          path: `/r/wide/file-${index}`, name: `file-${index}`, kind: "file" as const,
          sizeBytes: "1", modifiedMillis: "1", generation: "1", executable: false, expandable: false,
        })),
      };
      await act(async () => {
        larger = create(<ExplorerTree root={root} scopeIdentity="phase14" listings={new Map([["/r", quadrupled]])}
          expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
          onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()}
          onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
      });
      const largerRows = larger.root.findAllByProps({ className: "file-row" }).length;
      await act(async () => { larger.unmount(); });
      expect(largerRows, "the mounted band grew with the directory").toBe(renderedRows);
      console.log(`PHASE14_METRIC ${JSON.stringify({ lane: "explorerWide", entries: wide.entries.length, logicalRowsHighWater: logicalRows, renderedRowsHighWater: highWater["explorer.renderedRows"], rowParity: logicalRows === wide.entries.length, mountedRowsAtFourTimesTheEntries: largerRows, jsdomMountMillis: mountMillis })}`);
    } finally {
      await act(async () => { renderer?.unmount(); });
      resetPerfProbe();
    }
  });

  it("keeps roving focus and sibling counts working while windowed", async () => {
    const wide: DirectoryListing = {
      ...listing,
      entries: Array.from({ length: 4_096 }, (_, index) => ({
        path: `/r/wide/file-${index}`, name: `file-${index}`, kind: "file" as const,
        sizeBytes: "1", modifiedMillis: "1", generation: "1", executable: false, expandable: false,
      })),
    };
    let renderer!: ReturnType<typeof create>;
    try {
      await act(async () => {
        renderer = create(<ExplorerTree root={root} scopeIdentity="windowed" listings={new Map([["/r", wide]])}
          expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
          onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()}
          onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
      });
      const mounted = () => renderer.root.findAllByProps({ className: "file-row" });
      expect(mounted().length, "a windowed tree must not mount the whole directory").toBeLessThan(4_096);

      // Every mounted row reports its position among its own siblings, which is
      // the only way an assistive technology can know where it is once the DOM
      // no longer contains the whole level.
      const first = mounted()[0];
      expect(first.props["aria-setsize"]).toBe(4_096);
      expect(first.props["aria-posinset"]).toBe(1);
      // Exactly one row is in the tab order, wherever the window sits.
      expect(mounted().filter((row) => row.props.tabIndex === 0)).toHaveLength(1);

      // Arrow-down past the mounted band keeps focus roving: the window follows
      // and the row the tree asks to focus is really in the document.
      const step = (index: number) => act(async () => {
        renderer.root.findByProps({ "data-tree-index": index }).props.onKeyDown({
          key: "ArrowDown", target: 1, currentTarget: 1, preventDefault: vi.fn(),
        });
      });
      const band = mounted().length;
      const target = band + 5;
      for (let index = 0; index < target; index += 1) await step(index);
      // The window followed the cursor: the row the tree now considers focused
      // is mounted, is the only one in the tab order, and still knows where it
      // sits among its siblings.
      const arrived = renderer.root.findAllByProps({ "data-tree-index": target });
      expect(arrived, "arrowing past the mounted band dropped the focused row").not.toHaveLength(0);
      expect(arrived[0].props.tabIndex).toBe(0);
      expect(arrived[0].props["aria-posinset"]).toBe(target + 1);
      expect(mounted().filter((row) => row.props.tabIndex === 0)).toHaveLength(1);
      expect(mounted().length, "the band must stay bounded while scrolling").toBeLessThan(4_096);
    } finally {
      await act(async () => { renderer?.unmount(); });
    }
  });

  it("scrolls a row into view when the keyboard reaches one the viewport is not over", async () => {
    // The tree assigns `scrollTop` on the element it holds a ref to, so nothing
    // about it is observable without one. Under `react-test-renderer` refs are
    // null unless a node mock supplies them, which is why the windowed-focus
    // test above cannot see this at all.
    const wide: DirectoryListing = {
      ...listing,
      entries: Array.from({ length: 4_096 }, (_, index) => ({
        path: `/r/wide/file-${index}`, name: `file-${index}`, kind: "file" as const,
        sizeBytes: "1", modifiedMillis: "1", generation: "1", executable: false, expandable: false,
      })),
    };
    // Enough of an element for the tree to measure a row, move the viewport,
    // and hand focus to a row — which is all it asks its ref for.
    const tree = {
      scrollTop: 0, clientHeight: 400,
      querySelector: () => ({ offsetHeight: 20, focus: () => undefined }),
    };
    let renderer!: ReturnType<typeof create>;
    try {
      await act(async () => {
        renderer = create(<ExplorerTree root={root} scopeIdentity="scroll" listings={new Map([["/r", wide]])}
          expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
          onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()}
          onRefresh={vi.fn()} onLoadMore={vi.fn()} />, {
          createNodeMock: (element) => (element.props as { role?: string }).role === "tree" ? tree : null,
        });
      });
      expect(tree.scrollTop, "a row already in view must not move the viewport").toBe(0);

      // 400px of viewport over 20px rows is twenty visible rows, so row twenty
      // is the first one that is not.
      const step = (index: number) => act(async () => {
        renderer.root.findByProps({ "data-tree-index": index }).props.onKeyDown({
          key: "ArrowDown", target: 1, currentTarget: 1, preventDefault: vi.fn(),
        });
      });
      for (let index = 0; index < 20; index += 1) await step(index);
      expect(tree.scrollTop, "the keyboard reached a row the viewport was not over").toBe(20);
      // And it scrolls the minimum: one more row is one more row of offset.
      await step(20);
      expect(tree.scrollTop).toBe(40);
    } finally {
      await act(async () => { renderer?.unmount(); });
    }
  });

  it("reaches the ends of a windowed directory with Home and End", async () => {
    // Before windowing every row was in the DOM and the browser's own
    // find-as-you-type could reach row 4,000. With ~46 rows mounted it cannot,
    // so without these keys the only way to the end of a large directory is
    // 4,000 ArrowDown presses — each a state commit, a frame, and a scroll
    // assignment.
    const wide: DirectoryListing = {
      ...listing,
      entries: Array.from({ length: 4_096 }, (_, index) => ({
        path: `/r/wide/file-${index}`, name: `file-${index}`, kind: "file" as const,
        sizeBytes: "1", modifiedMillis: "1", generation: "1", executable: false, expandable: false,
      })),
    };
    let renderer!: ReturnType<typeof create>;
    try {
      await act(async () => {
        renderer = create(<ExplorerTree root={root} scopeIdentity="ends" listings={new Map([["/r", wide]])}
          expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
          onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()}
          onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
      });
      const press = (index: number, key: string) => act(async () => {
        renderer.root.findByProps({ "data-tree-index": index }).props.onKeyDown({
          key, target: 1, currentTarget: 1, preventDefault: vi.fn(),
        });
      });
      const roving = () => renderer.root.findAllByProps({ className: "file-row" })
        .filter((row) => row.props.tabIndex === 0)
        .map((row) => row.props["data-tree-index"]);

      await press(0, "End");
      expect(roving(), "End did not reach the last row").toEqual([4_095]);
      expect(renderer.root.findAllByProps({ "data-tree-index": 4_095 })).not.toHaveLength(0);

      await press(4_095, "Home");
      expect(roving(), "Home did not return to the first row").toEqual([0]);
    } finally {
      await act(async () => { renderer?.unmount(); });
    }
  });

  it("keeps the keyboard on the row the user chose when the directory changes underneath it", async () => {
    // Precise external changes are the branch's central mechanism: an agent
    // creating one file patches a single row in place rather than re-listing.
    // With focus held as a position, every such insert above the cursor moved
    // it to a different file, and deleting the focused row dropped DOM focus
    // to the document body — the "no scroll/focus loss" outcome, inverted by
    // the very change that made patching cheap.
    const rows = (names: string[]): DirectoryListing => ({
      ...listing,
      entries: names.map((name) => ({
        path: `/r/${name}`, name, kind: "file" as const, sizeBytes: "1",
        modifiedMillis: "1", generation: "1", executable: false, expandable: false,
      })),
    });
    const view = (entries: DirectoryListing) => <ExplorerTree root={root} scopeIdentity="focus" listings={new Map([["/r", entries]])}
      expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()}
      onRefresh={vi.fn()} onLoadMore={vi.fn()} />;
    // The tree hands DOM focus to a row through its ref, so without a node mock
    // the ref is null and the restore cannot be observed at all — the roving
    // `tabIndex` would look right while the keyboard sat on `<body>`.
    const focused: string[] = [];
    const treeNode = {
      scrollTop: 0, clientHeight: 400,
      contains: () => false,
      querySelector: (selector: string) => ({
        offsetHeight: 20,
        focus: () => focused.push(selector),
      }),
    };
    let renderer!: ReturnType<typeof create>;
    try {
      await act(async () => {
        renderer = create(view(rows(["b.txt", "c.txt"])), {
          createNodeMock: (element) => (element.props as { role?: string }).role === "tree" ? treeNode : null,
        });
      });
      const roving = () => renderer.root.findAllByProps({ className: "file-row" })
        .filter((row) => row.props.tabIndex === 0)
        .map((row) => row.props["data-tree-index"]);
      // Stand on "c.txt", the second row.
      await act(async () => {
        renderer.root.findByProps({ "data-tree-index": 1 }).props.onFocus();
      });
      expect(roving()).toEqual([1]);

      // An agent creates a file that sorts above it.
      await act(async () => { renderer.update(view(rows(["a.txt", "b.txt", "c.txt"]))); });
      expect(roving(), "an insert above the cursor moved it to another file").toEqual([2]);
      expect(renderer.root.findByProps({ "data-tree-index": 2 }).props["aria-selected"]).toBe(true);

      // And then deletes the row the user is standing on. Removing a focused
      // element sends focus to `<body>`, so the tree has to hand it back — and
      // it has to know it owned the keyboard *before* the removal, which is
      // not a question the document can answer afterwards.
      focused.length = 0;
      await act(async () => { renderer.update(view(rows(["a.txt", "b.txt"]))); });
      expect(roving(), "the tree lost its keyboard cursor entirely").toHaveLength(1);
      expect(roving()[0]).toBe(1);
      expect(focused, "the roving index moved but DOM focus was left on the body")
        .toEqual(['[data-tree-index="1"]']);
    } finally {
      await act(async () => { renderer?.unmount(); });
    }
  });

  it("reports each row's position among its own siblings, not in the flattened walk", async () => {
    // The tree role's setsize/posinset are per level. Using the flattened index
    // makes a nested row announce a position in the whole walk, and makes
    // setsize change on every insertion anywhere — which also invalidates every
    // memoized row the extraction exists to keep still.
    const nested: DirectoryListing = {
      ...listing,
      entries: [
        { path: "/r/lib", name: "lib", kind: "directory", sizeBytes: "0", modifiedMillis: "1", generation: "1", executable: false, expandable: true },
        { path: "/r/a.txt", name: "a.txt", kind: "file", sizeBytes: "1", modifiedMillis: "1", generation: "1", executable: false, expandable: false },
      ],
    };
    const child: DirectoryListing = {
      rootToken: "root-1", directory: "/r/lib", revision: "2", recoveredFromOverflow: false, complete: true,
      entries: ["one", "two", "three"].map((name) => ({
        path: `/r/lib/${name}`, name, kind: "file" as const, sizeBytes: "1",
        modifiedMillis: "1", generation: "1", executable: false, expandable: false,
      })),
    };
    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(<ExplorerTree root={root} scopeIdentity="nested" listings={new Map([["/r", nested], ["/r/lib", child]])}
        expanded={new Set(["/r", "/r/lib"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
        onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()}
        onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
    });
    const rows = renderer.root.findAllByProps({ className: "file-row" });
    const reported = rows.map((row) => ({
      level: row.props["aria-level"],
      position: row.props["aria-posinset"],
      size: row.props["aria-setsize"],
    }));
    // Two top-level rows, three inside `lib` — five rows in the walk, but each
    // level counts only itself.
    expect(reported).toEqual([
      { level: 1, position: 1, size: 2 },
      { level: 2, position: 1, size: 3 },
      { level: 2, position: 2, size: 3 },
      { level: 2, position: 3, size: 3 },
      { level: 1, position: 2, size: 2 },
    ]);
    await act(async () => { renderer.unmount(); });
  });

  it("keeps roving focus, selection, and the row context menu on the row the keyboard reached", async () => {
    // Every one of these handlers now crosses a memo boundary and a forwarding
    // ref, so the interactions the plan names by name are asserted here rather
    // than assumed to have survived the extraction.
    const onToggle = vi.fn();
    const withDirectory: DirectoryListing = {
      ...listing,
      entries: [...listing.entries, { path: "/r/src", name: "src", kind: "directory", sizeBytes: "0", modifiedMillis: "1", generation: "1", executable: false, expandable: true }],
    };
    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(<ExplorerTree root={root} scopeIdentity="keys" listings={new Map([["/r", withDirectory]])}
        expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
        onToggle={onToggle} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()}
        onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
    });
    const row = (index: number) => renderer.root.findByProps({ "data-tree-index": index });
    const selected = () => renderer.root.findAllByProps({ className: "file-row" })
      .filter((candidate) => candidate.props["aria-selected"] === true)
      .map((candidate) => candidate.props["data-tree-index"]);
    const tabbable = () => renderer.root.findAllByProps({ className: "file-row" })
      .filter((candidate) => candidate.props.tabIndex === 0)
      .map((candidate) => candidate.props["data-tree-index"]);

    expect(selected()).toEqual([0]);
    expect(tabbable()).toEqual([0]);

    // Roving focus: Down moves selection and the tab stop together.
    await act(async () => { row(0).props.onKeyDown({ key: "ArrowDown", target: 1, currentTarget: 1, preventDefault: vi.fn() }); });
    expect(selected()).toEqual([1]);
    expect(tabbable()).toEqual([1]);
    await act(async () => { row(1).props.onKeyDown({ key: "ArrowUp", target: 1, currentTarget: 1, preventDefault: vi.fn() }); });
    expect(selected()).toEqual([0]);

    // ArrowRight on a collapsed directory expands it rather than moving.
    const directoryIndex = withDirectory.entries.length - 1;
    await act(async () => { row(directoryIndex).props.onPointerDown(); });
    expect(selected()).toEqual([directoryIndex]);
    await act(async () => {
      row(directoryIndex).props.onKeyDown({ key: "ArrowRight", target: 1, currentTarget: 1, preventDefault: vi.fn() });
    });
    expect(onToggle).toHaveBeenCalledWith("/r/src");

    // The row context menu opens for the row it was raised on, and moves the
    // tree's focus cursor there.
    await act(async () => { row(1).props.onContextMenu({ preventDefault: vi.fn(), clientX: 4, clientY: 5 }); });
    expect(selected()).toEqual([1]);
    const menu = JSON.stringify(renderer.toJSON());
    expect(menu).toContain("Actions for ignored.log");
    expect(menu).toContain("Rename…");
    await act(async () => { renderer.unmount(); });
  });

  it("shows dotfiles/ignored entries while protected and symlink directories stay collapsed", () => {
    const html = renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map([["/r", listing]])} expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false} error={undefined}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
    expect(html).toContain(".env");
    expect(html).toContain("ignored.log");
    expect(html).toContain(".git");
    expect(html).toContain("node_modules");
    expect(html).toContain("link");
    expect(html).not.toContain('aria-expanded="false"');
    expect(html).toContain('role="tree"');
    expect(html).toContain('role="treeitem"');
    expect(html).toContain('aria-level="1"');
    expect(html).toContain('tabindex="0"');
    // The tree carries no resting controls at all now: creating, renaming,
    // downloading and refreshing are right-click items, not header buttons.
    expect(html).not.toContain('aria-label="New file"');
    expect(html).not.toContain('aria-label="Refresh Explorer"');
    expect(html).not.toContain("•••");
  });

  it("hides what git calls ignored, and stops descending into it", () => {
    // Git reports `target/` once and never its contents, so what has to hold
    // is that dropping the directory drops its whole subtree — an expanded
    // `/r/target` whose listing is already cached must contribute no rows.
    const withTarget: DirectoryListing = {
      ...listing,
      entries: [
        ...listing.entries,
        { path: "/r/target", name: "target", kind: "directory", sizeBytes: "0", modifiedMillis: "1", generation: "1", executable: false, expandable: true },
        { path: "/r/src", name: "src", kind: "directory", sizeBytes: "0", modifiedMillis: "1", generation: "1", executable: false, expandable: true },
      ],
    };
    const targetListing: DirectoryListing = {
      rootToken: "root-1", directory: "/r/target", revision: "2", recoveredFromOverflow: false, complete: true,
      entries: [{ path: "/r/target/debug", name: "debug", kind: "directory", sizeBytes: "0", modifiedMillis: "1", generation: "1", executable: false, expandable: true }],
    };
    const listings = new Map([["/r", withTarget], ["/r/target", targetListing]]);
    const expanded = new Set(["/r", "/r/target"]);
    const shown = (ignoredPaths?: ReadonlySet<string>) => renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={listings} expanded={expanded} loading={new Set()} requestedReads={0} transfers={[]} disabled={false} error={undefined} ignoredPaths={ignoredPaths}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />);

    const filtered = shown(new Set(["/r/target", "/r/ignored.log"]));
    expect(filtered).not.toContain(">target<");
    expect(filtered, "an already-cached child of a dropped directory still produced a row").not.toContain(">debug<");
    expect(filtered).not.toContain("ignored.log");
    expect(filtered).toContain(">src<");
    expect(filtered).toContain(".env");

    // A path that merely shares a prefix is a different file, not a child.
    expect(shown(new Set(["/r/tar"]))).toContain(">target<");

    // Degradation: no authoritative status means no set at all, and the tree
    // shows everything rather than guessing.
    const everything = shown(undefined);
    expect(everything).toContain(">target<");
    expect(everything).toContain("ignored.log");
    // An empty set is an authoritative "nothing is ignored", not a hidden tree.
    expect(shown(new Set())).toBe(everything);
  });

  it("offers the show-ignored escape hatch only when something is actually hidden", async () => {
    const listings = new Map([["/r", listing]]);
    const shown = async (ignoredPaths?: ReadonlySet<string>) => {
      let renderer!: ReturnType<typeof create>;
      await act(async () => { renderer = create(<ExplorerTree root={root} scopeIdentity="scope" listings={listings} expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false} error={undefined} ignoredPaths={ignoredPaths}
        onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />); });
      // The Explorer header is the surface the toggle lives on.
      const header = renderer.root.findAllByProps({ className: "explorer-root" })[0];
      await act(async () => { header.props.onContextMenu({ preventDefault: vi.fn(), clientX: 1, clientY: 1 }); });
      return renderer;
    };

    const degraded = await shown(undefined);
    expect(JSON.stringify(degraded.toJSON())).not.toContain("ignored files");
    await act(async () => { degraded.unmount(); });

    const filtering = await shown(new Set(["/r/ignored.log"]));
    expect(JSON.stringify(filtering.toJSON())).not.toContain("ignored.log");
    expect(JSON.stringify(filtering.toJSON())).toContain("Show ignored files");
    const toggle = filtering.root.findByProps({ "data-menu-item": "ignored" });
    await act(async () => { toggle.props.onClick(); });
    expect(JSON.stringify(filtering.toJSON())).toContain("ignored.log");
    await act(async () => { filtering.unmount(); });
  });

  it("asks for a preview on a single click and a permanent tab on every deliberate open", async () => {
    const onOpen = vi.fn();
    const onToggle = vi.fn();
    const withDirectory: DirectoryListing = {
      ...listing,
      entries: [...listing.entries, { path: "/r/src", name: "src", kind: "directory", sizeBytes: "0", modifiedMillis: "1", generation: "1", executable: false, expandable: true }],
    };
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map([["/r", withDirectory]])} expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false} error={undefined}
      onToggle={onToggle} onOpen={onOpen} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />); });
    const row = renderer.root.findAllByProps({ className: "file-main" })[0];

    await act(async () => { row.props.onClick(); });
    expect(onOpen).toHaveBeenLastCalledWith(expect.objectContaining({ path: "/r/.env" }), { preview: true });
    // The click of a double-click has already fired; the second click pins the
    // tab that first one created.
    await act(async () => { row.props.onDoubleClick(); });
    expect(onOpen).toHaveBeenLastCalledWith(expect.objectContaining({ path: "/r/.env" }), { preview: false });

    const treeRow = renderer.root.findAllByProps({ "data-tree-index": 0 })[0];
    await act(async () => { treeRow.props.onKeyDown({ key: "Enter", target: 1, currentTarget: 1, preventDefault: vi.fn() }); });
    expect(onOpen).toHaveBeenLastCalledWith(expect.objectContaining({ path: "/r/.env" }), { preview: false });

    await act(async () => { treeRow.props.onPointerDown(); });
    await act(async () => { rowCommandRegistry.run("files.open"); });
    expect(onOpen).toHaveBeenLastCalledWith(expect.objectContaining({ path: "/r/.env" }), { preview: false });

    // An expandable directory toggles and never opens, single or double.
    const directory = renderer.root.findAllByProps({ className: "file-main" }).at(-1)!;
    onOpen.mockClear();
    await act(async () => { directory.props.onClick(); directory.props.onDoubleClick(); });
    expect(onOpen).not.toHaveBeenCalled();
    expect(onToggle).toHaveBeenCalledWith("/r/src");
    await act(async () => { renderer.unmount(); });
  });

  it("never changes the tree's height to say a directory is being re-read", () => {
    // The reported flicker: the "Loading…" row lives inside the scrolling box,
    // so showing it while rows are already up grew the content by a row and
    // shrank it again on every filesystem event — the row blinking on a short
    // listing, and macOS revealing and re-hiding the overlay scrollbars on a
    // long one. Whatever else a refresh does, it must not move that content.
    const shown = (props: { loading: Set<string>; requestedReads: number; listings: Map<string, DirectoryListing> }) =>
      renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" expanded={new Set(["/r"])} transfers={[]} disabled={false} error={undefined}
        onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} {...props} />);
    const listings = new Map([["/r", listing]]);
    const resting = shown({ loading: new Set(), requestedReads: 0, listings });
    const refreshing = shown({ loading: new Set(["/r"]), requestedReads: 0, listings });
    expect(resting).not.toContain("Loading…");
    expect(refreshing, "a re-read put a row back inside the scrolling tree").not.toContain("Loading…");
    // The tree's own markup is untouched by the refresh; only the busy flag moves.
    expect(refreshing.replace(' aria-busy="true"', "")).toBe(resting);
    expect(refreshing).toContain('aria-busy="true"');

    // A directory that has never answered is the one case that still says so,
    // because then there is nothing for the wait to be in front of.
    const first = shown({ loading: new Set(["/r"]), requestedReads: 0, listings: new Map() });
    expect(first).toContain("Loading…");
    // And an empty directory does not swap between two lines on every re-read.
    const empty = new Map([["/r", { ...listing, entries: [] }]]);
    expect(shown({ loading: new Set(["/r"]), requestedReads: 0, listings: empty })).toContain("This directory is empty.");
    expect(shown({ loading: new Set(), requestedReads: 0, listings: empty })).toContain("This directory is empty.");

    // A refresh the user asked for is the one wait that is visible, and it is
    // drawn in the header — outside the scrolling box, so it cannot flicker it.
    const requested = shown({ loading: new Set(["/r"]), requestedReads: 1, listings });
    expect(requested).toContain("Refreshing…");
    expect(requested.slice(0, requested.indexOf('role="tree"'))).toContain("Refreshing…");
  });

  it("exposes accessible cancellation for queued/running downloads but never terminal cancellation", () => {
    const html = renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map()} expanded={new Set()} loading={new Set()} requestedReads={0} disabled={false}
      transfers={[
        { id: "queued", scopeKey: "scope", path: "/r/queued", kind: "file", state: "queued", completedBytes: "0", totalBytes: "5368709120", filesCompleted: "0" },
        { id: "running", scopeKey: "scope", path: "/r/running", kind: "file", state: "running", completedBytes: "2", totalBytes: "10", filesCompleted: "0" },
        { id: "cancelled", scopeKey: "scope", path: "/r/cancelled", kind: "file", state: "cancelled", outcome: "notPublished", completedBytes: "2", filesCompleted: "0", cleanupStatus: "removed" },
      ]}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
    expect(html).toContain("Downloads");
    expect(html).toContain("Download /r/queued: Queued");
    expect(html).toContain("Download /r/running: Transferring");
    expect(html).toContain("5368709120");
    expect(html).toContain("Cancel download /r/queued");
    expect(html).toContain("Cancel download /r/running");
    expect(html).not.toContain("Cancel download /r/cancelled");
  });

  it("routes the accessible running-download button to its exact transfer id", async () => {
    const cancel = vi.fn(async () => undefined);
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map()} expanded={new Set()} loading={new Set()} requestedReads={0} disabled={false}
      transfers={[{ id: "running-id", scopeKey: "scope", path: "/r/running", kind: "file", state: "running", completedBytes: "2", totalBytes: "10", filesCompleted: "0" }]}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={cancel} onRefresh={vi.fn()} onLoadMore={vi.fn()} />); });
    await act(async () => { renderer.root.findByProps({ "aria-label": "Cancel download /r/running" }).props.onClick(); });
    expect(cancel).toHaveBeenCalledWith("running-id");
    await act(async () => { renderer.unmount(); });
  });

  it("reaches every row action from the command registry, on the row the tree has focus on", async () => {
    // The palette's half of "a removed button becomes a palette command, a
    // context-menu item and a shortcut". Nothing here goes through the menu:
    // this is the route ⌘K takes.
    const onDownload = vi.fn(async () => undefined);
    const onRefresh = vi.fn();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map([["/r", listing]])} expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false} error={undefined}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={onDownload} onCancelTransfer={vi.fn()} onRefresh={onRefresh} onLoadMore={vi.fn()} />); });

    expect(rowCommandRegistry.available()).toEqual([
      "files.open", "files.download", "files.rename", "files.move", "files.duplicate", "files.delete",
      "files.newFile", "files.newFolder", "files.refresh",
    ]);
    // Clicking a row is pointing at it. macOS WebKit does not focus a button on
    // click, so a tree that only listened for focus went on offering actions for
    // whichever row the keyboard last visited — measured on the packaged app,
    // where clicking README.md left `.git` as the palette's subject.
    const gitDirectory = renderer.root.findAllByProps({ "data-tree-index": 2 })[0];
    await act(async () => { gitDirectory.props.onPointerDown(); });
    expect(rowCommandRegistry.available()).not.toContain("files.open");
    const dotEnv = renderer.root.findAllByProps({ "data-tree-index": 0 })[0];
    await act(async () => { dotEnv.props.onPointerDown(); });
    expect(rowCommandRegistry.available()).toContain("files.open");
    // Row 0 is `.env`; the tree's own focus cursor is what "selected" means.
    await act(async () => { rowCommandRegistry.run("files.download"); });
    expect(onDownload).toHaveBeenCalledWith({ path: "/r/.env", kind: "file" });
    await act(async () => { rowCommandRegistry.run("files.refresh"); });
    expect(onRefresh).toHaveBeenCalled();
    await act(async () => { rowCommandRegistry.run("files.rename"); });
    expect(JSON.stringify(renderer.toJSON())).toContain("Rename");

    await act(async () => { renderer.unmount(); });
    // A closed panel offers nothing; the palette must not hold a row nobody
    // can see.
    expect(rowCommandRegistry.available()).toEqual([]);
    expect(rowCommandRegistry.run("files.rename")).toEqual({ ran: false });
  });

  it("withholds the mutating row actions while the host is read-only", async () => {
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map([["/r", listing]])} expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled error={undefined}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />); });
    expect(rowCommandRegistry.available()).toEqual(["files.open", "files.download", "files.refresh"]);
    await act(async () => { renderer.unmount(); });
  });

  it("summarizes a host rejection and keeps the diagnostic behind a disclosure", () => {
    const html = renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map()} expanded={new Set()} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
      error="file_mutation_rejected: File name too long (os error 63)"
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
    expect(html).toContain("That name is longer than this filesystem allows.");
    expect(html).toContain("<details");
    expect(html).toContain("os error 63");
  });

  it("makes commit authoritative and renders unknown/cleanup outcomes as terminal alerts", () => {
    const html = renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map()} expanded={new Set()} loading={new Set()} requestedReads={0} disabled={false}
      transfers={[
        { id: "verifying", scopeKey: "scope", path: "/r/commit", kind: "file", state: "verifying", completedBytes: "12", totalBytes: "12", filesCompleted: "0" },
        { id: "unknown", scopeKey: "scope", path: "/r/unknown", kind: "file", state: "failed", outcome: "unknown", failureKind: "outcomeUnknown", completedBytes: "12", filesCompleted: "0", cleanupStatus: "failed", cleanupError: "permission denied" },
        { id: "timeout", scopeKey: "scope", path: "/r/timeout", kind: "file", state: "failed", outcome: "unknown", failureKind: "timeout", completedBytes: "12", filesCompleted: "0" },
        { id: "stale", scopeKey: "scope", path: "/r/stale", kind: "file", state: "failed", outcome: "unknown", failureKind: "staleScope", completedBytes: "12", filesCompleted: "0" },
      ]}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
    expect(html).toContain("Verifying and committing");
    expect(html).toContain("awaiting the authoritative backend outcome");
    expect(html).toContain("outcome is unknown");
    expect(html).toContain("Download timed out before an authoritative result arrived.");
    expect(html).toContain("Download stopped because the connection scope changed.");
    expect(html).toContain("Partial cleanup failed: permission denied");
    expect(html).not.toContain("Cancel download /r/commit");
  });
});
