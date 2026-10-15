import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DockDropZones, dockFractionAt, dockSideAt } from "./WorkspaceDock";
import { finishInternalPathDrag, writeInternalPathDrag } from "../features/terminal/internalPathDrag";
import { registerNativeDragDropTarget } from "../features/terminal/nativeDragDropCoordinator";

const nativeDrag = vi.hoisted(() => ({ handler: undefined as ((event: { payload: Record<string, unknown> }) => void) | undefined }));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: vi.fn(async (handler: (event: { payload: Record<string, unknown> }) => void) => {
      nativeDrag.handler = handler;
      return vi.fn();
    }),
  }),
}));

const rect = { left: 0, top: 0, width: 1000, height: 600 };
const scope = { hostProfileId: "local", serverIdentity: "server-a" };
const renderers: ReactTestRenderer[] = [];

function transfer() {
  const values = new Map<string, string>();
  return {
    effectAllowed: "all" as DataTransfer["effectAllowed"],
    dropEffect: "none" as DataTransfer["dropEffect"],
    get types() { return [...values.keys()]; },
    getData: (type: string) => values.get(type) ?? "",
    setData: (type: string, value: string) => { values.set(type, value); },
  };
}

async function mount() {
  const onDockPath = vi.fn();
  const onError = vi.fn();
  const content = { current: { getBoundingClientRect: () => rect } as HTMLElement };
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(<DockDropZones content={content} fraction={0.4} onDockPath={onDockPath} onError={onError} scope={scope} />);
    await Promise.resolve();
  });
  renderers.push(renderer);
  return { renderer, onDockPath, onError };
}

afterEach(() => {
  for (const renderer of renderers.splice(0)) act(() => renderer.unmount());
  finishInternalPathDrag(false);
});

describe("dock geometry", () => {
  it("keeps a dragged divider inside the fraction bounds and the pixel floors", () => {
    expect(dockFractionAt(rect, 700, "right")).toBeCloseTo(0.3);
    expect(dockFractionAt(rect, 300, "left")).toBeCloseTo(0.3);
    // The terminal keeps 480px: a right dock stops at 52% of a 1000px workspace.
    expect(dockFractionAt(rect, 0, "right")).toBeCloseTo(0.52);
    // The document keeps 280px.
    expect(dockFractionAt(rect, 990, "right")).toBeCloseTo(0.28);
    // Too narrow for both floors: only the fraction bounds hold.
    expect(dockFractionAt({ left: 0, width: 600 }, 0, "right")).toBeCloseTo(0.7);
  });

  it("gives a dragged tab wide edge zones and a dragged path a strip", () => {
    expect(dockSideAt(rect, { x: 200, y: 10 }, "tab")).toBe("left");
    expect(dockSideAt(rect, { x: 800, y: 10 }, "tab")).toBe("right");
    expect(dockSideAt(rect, { x: 500, y: 10 }, "tab")).toBeUndefined();
    expect(dockSideAt(rect, { x: 100, y: 10 }, "path")).toBe("left");
    expect(dockSideAt(rect, { x: 200, y: 10 }, "path")).toBeUndefined();
    expect(dockSideAt(rect, { x: 950, y: 10 }, "path")).toBe("right");
    expect(dockSideAt(rect, { x: 10, y: 700 }, "path")).toBeUndefined();
  });
});

describe("DockDropZones", () => {
  it("draws nothing, and so blocks nothing, until a drag that can dock starts", async () => {
    const { renderer } = await mount();
    expect(renderer.toJSON()).toBeNull();
    await act(async () => writeInternalPathDrag(transfer(), { ...scope, path: "/repo/notes.md" }));
    expect(renderer.root.findAll((node) => node.props["data-drop-dock"] !== undefined)).toHaveLength(2);
    await act(async () => finishInternalPathDrag(false));
    expect(renderer.toJSON()).toBeNull();
  });

  it("docks a path dropped on an edge zone in the DOM lane", async () => {
    const { renderer, onDockPath } = await mount();
    const data = transfer();
    await act(async () => writeInternalPathDrag(data, { ...scope, path: "/repo/notes.md" }));
    const right = renderer.root.find((node) => node.props["data-drop-dock"] === "right");
    await act(async () => right.props.onDragOver({ preventDefault: vi.fn(), dataTransfer: data }));
    expect(renderer.root.findAll((node) => node.props.className === "dock-drop-hint right")).toHaveLength(1);
    await act(async () => right.props.onDrop({ preventDefault: vi.fn(), dataTransfer: data }));
    expect(onDockPath).toHaveBeenCalledWith("/repo/notes.md", "right");
  });

  it("wins a native drop at the edge over the pane under it, and leaves the middle to the pane", async () => {
    Object.assign(globalThis, { window: { __TAURI_INTERNALS__: {} } });
    const pane = { owns: () => true, setDragging: vi.fn(), drop: vi.fn(), fail: vi.fn() };
    const release = registerNativeDragDropTarget(pane);
    const { onDockPath } = await mount();
    const native = (type: string, x: number) => nativeDrag.handler?.({ payload: { type, paths: [], position: { x, y: 20 } } });

    writeInternalPathDrag(transfer(), { ...scope, path: "/repo/notes.md" });
    finishInternalPathDrag(true);
    await act(async () => { native("enter", 20); native("over", 20); });
    // dragend lands before macOS's native drop: the zones are gone, the claim is not.
    writeInternalPathDrag(transfer(), { ...scope, path: "/repo/notes.md" });
    await act(async () => { native("enter", 20); finishInternalPathDrag(true); native("drop", 20); });
    expect(onDockPath).toHaveBeenCalledWith("/repo/notes.md", "left");
    expect(pane.drop).not.toHaveBeenCalled();

    writeInternalPathDrag(transfer(), { ...scope, path: "/repo/notes.md" });
    await act(async () => { native("enter", 500); finishInternalPathDrag(true); native("drop", 500); });
    expect(pane.drop).toHaveBeenCalledWith([]);
    expect(onDockPath).toHaveBeenCalledTimes(1);

    // A file from the desktop is never ours: it uploads into the pane.
    await act(async () => {
      nativeDrag.handler?.({ payload: { type: "enter", paths: ["/tmp/a.png"], position: { x: 20, y: 20 } } });
      nativeDrag.handler?.({ payload: { type: "drop", paths: ["/tmp/a.png"], position: { x: 20, y: 20 } } });
    });
    expect(pane.drop).toHaveBeenLastCalledWith(["/tmp/a.png"]);
    release();
  });
});
