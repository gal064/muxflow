// The chips on the reply field and the sheet that fills them
// (docs/design/mobile-chat-mock.html, S7).

import { Platform } from "react-native";
import { act, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";

import { allByLabel, byLabel, isHost, press, render } from "../../../testing/render";
import { colors } from "../../../ui/tokens";
import type { AttachmentEntry } from "../attach/attachmentQueue";
import type { AttachChoice } from "../attach/pickers";
import { AttachSheet } from "./AttachSheet";
import { AttachStrip } from "./AttachStrip";

function entry(fields: Partial<AttachmentEntry> & Pick<AttachmentEntry, "id" | "name">): AttachmentEntry {
  return { sizeBytes: 18_432, mediaType: "", status: "ready", sentBytes: 18_432, ...fields };
}

function flat(style: unknown): Record<string, unknown> {
  if (!style) return {};
  if (Array.isArray(style)) return Object.assign({}, ...style.map(flat));
  return style as Record<string, unknown>;
}

/** The words drawn under `node`, one entry a text element. */
function texts(node: ReactTestInstance): string[] {
  return node.findAll((child) => isHost(child, "Text")).map((child) => child.children.join(""));
}

/** The chip's box: what its labelled part and its cross sit in. */
function chipOf(view: ReactTestRenderer, label: string | RegExp): ReactTestInstance {
  let node = byLabel(view, label).parent;
  while (node && typeof node.type !== "string") node = node.parent;
  if (!node) throw new Error(`nothing holds ${String(label)}`);
  return node;
}

/** The upload bar of a chip, by how wide it is drawn; undefined when it has none. */
function barOf(chip: ReactTestInstance): unknown {
  const bars = chip.findAll((node) => isHost(node, "View") && flat(node.props.style).backgroundColor === colors.accent);
  return bars.length === 0 ? undefined : flat(bars[0]!.props.style).width;
}

const strip = (entries: AttachmentEntry[], removed: string[] = []): ReactTestRenderer =>
  render(<AttachStrip entries={entries} onRemove={(id) => removed.push(id)} />);

describe("the chips on the reply field", () => {
  it("draws nothing with nothing attached", () => {
    expect(strip([]).toJSON()).toBeNull();
  });

  it("an image is its thumbnail from the phone's own file; any other file its mark, name and size", () => {
    const view = strip([
      entry({ id: "a", name: "shot.png", mediaType: "image/png", localUri: "file:///cache/shot.png", sizeBytes: 2048, sentBytes: 2048 }),
      entry({ id: "b", name: "layout.log" }),
    ]);
    const thumbnail = byLabel(view, "shot.png, 2 KB");
    expect(isHost(thumbnail, "Image")).toBe(true);
    expect(thumbnail.props.source).toEqual({ uri: "file:///cache/shot.png" });
    const file = byLabel(view, "layout.log, 18 KB");
    expect(texts(file)).toEqual(["layout.log", "18 KB"]);
    expect(file.findAll((node) => isHost(node, "Svg"))).toHaveLength(1);
    // Neither is moving: no bar.
    expect(barOf(chipOf(view, "shot.png, 2 KB"))).toBeUndefined();
    expect(barOf(chipOf(view, "layout.log, 18 KB"))).toBeUndefined();
  });

  it("an upload shows how far it is, and a file that waits says so", () => {
    const view = strip([
      entry({ id: "a", name: "shot.png", mediaType: "image/png", localUri: "file:///cache/shot.png", status: "uploading", sizeBytes: 1000, sentBytes: 600 }),
      entry({ id: "b", name: "layout.log", status: "uploading", sizeBytes: 1000, sentBytes: 250 }),
      entry({ id: "c", name: "notes.md", status: "queued", sentBytes: 0 }),
    ]);
    expect(barOf(chipOf(view, "shot.png, Uploading 60%"))).toBe("60%");
    expect(barOf(chipOf(view, "layout.log, Uploading 25%"))).toBe("25%");
    expect(texts(byLabel(view, "layout.log, Uploading 25%"))).toEqual(["layout.log", "Uploading 25%"]);
    expect(barOf(chipOf(view, "notes.md, Waiting to upload"))).toBe("0%");
  });

  it("a file that could not be sent says why on its chip, an image too", () => {
    const view = strip([
      entry({ id: "a", name: "huge.png", mediaType: "image/png", localUri: "file:///cache/huge.png", status: "failed", sentBytes: 0, error: "huge.png is over 25 MB, the most an image can be." }),
      entry({ id: "b", name: "layout.log", status: "failed", sentBytes: 4096, error: "The connection to the host was lost." }),
    ]);
    const image = byLabel(view, "huge.png, huge.png is over 25 MB, the most an image can be.");
    // The reason needs the room: no thumbnail.
    expect(isHost(image, "Image")).toBe(false);
    expect(texts(image)).toEqual(["huge.png", "huge.png is over 25 MB, the most an image can be."]);
    const reason = byLabel(view, "layout.log, The connection to the host was lost.").findAll((node) => isHost(node, "Text"))[1]!;
    expect(flat(reason.props.style).color).toBe(colors.dangerInk);
    expect(flat(chipOf(view, /^layout\.log/).props.style).borderColor).toBe(colors.danger);
    expect(barOf(chipOf(view, /^layout\.log/))).toBeUndefined();
  });

  it("every chip has a cross that takes its file away, 44 dp to the touch", () => {
    const removed: string[] = [];
    const view = strip([
      entry({ id: "a", name: "shot.png", mediaType: "image/png", localUri: "file:///cache/shot.png", status: "uploading", sentBytes: 10 }),
      entry({ id: "b", name: "layout.log", status: "failed", error: "The upload failed." }),
      entry({ id: "c", name: "notes.md" }),
    ], removed);
    expect(allByLabel(view, /^Remove /)).toHaveLength(3);
    press(byLabel(view, "Remove shot.png"));
    press(byLabel(view, "Remove layout.log"));
    expect(removed).toEqual(["a", "b"]);
    const cross = byLabel(view, "Remove notes.md");
    const slop = cross.props.hitSlop as { left: number };
    expect((flat(cross.props.style).width as number) + slop.left).toBeGreaterThanOrEqual(44);
    expect(flat(chipOf(view, "notes.md, 18 KB").props.style).minHeight).toBeGreaterThanOrEqual(44);
    // Drawn, not a glyph.
    expect(cross.findAll((node) => isHost(node, "Svg"))).toHaveLength(1);
    expect(cross.findAll((node) => isHost(node, "Text"))).toHaveLength(0);
  });
});

describe("the attach sheet", () => {
  afterEach(() => {
    (Platform as { OS: string }).OS = "android";
    vi.useRealTimers();
  });

  function sheet(chosen: AttachChoice[], dismissed: { count: number }, visible = true): ReactTestRenderer {
    return render(<AttachSheet onChoose={(choice) => chosen.push(choice)} onDismiss={() => { dismissed.count += 1; }} visible={visible} />);
  }

  it("offers the photo library, the camera and a file, under its title", () => {
    const view = sheet([], { count: 0 });
    expect(texts(view.root)).toEqual(["Attach", "Photo library", "Take a photo", "File"]);
    for (const label of ["Photo library", "Take a photo", "File"]) {
      const row = byLabel(view, label);
      expect(row.findAll((node) => isHost(node, "Svg"))).toHaveLength(1);
      expect(flat(row.props.style).minHeight).toBeGreaterThanOrEqual(44);
    }
  });

  it("draws nothing while closed", () => {
    expect(sheet([], { count: 0 }, false).toJSON()).toBeNull();
  });

  it("closes and passes the choice on", () => {
    const chosen: AttachChoice[] = [];
    const dismissed = { count: 0 };
    const view = sheet(chosen, dismissed);
    press(byLabel(view, "Photo library"));
    press(byLabel(view, "Take a photo"));
    press(byLabel(view, "File"));
    expect(chosen).toEqual(["library", "camera", "file"]);
    expect(dismissed.count).toBe(3);
  });

  it("on an iPhone, opens the picker only once the sheet has closed", () => {
    vi.useFakeTimers();
    (Platform as { OS: string }).OS = "ios";
    const chosen: AttachChoice[] = [];
    const view = sheet(chosen, { count: 0 });
    press(byLabel(view, "Take a photo"));
    expect(chosen).toEqual([]);
    act(() => (view.root.find((node) => isHost(node, "Modal")).props.onDismiss as () => void)());
    expect(chosen).toEqual(["camera"]);
    // Said once, even when the wait runs out afterwards.
    act(() => {
      vi.advanceTimersByTime(5_000);
    });
    expect(chosen).toEqual(["camera"]);

    // A sheet that never says it closed does not swallow the choice.
    press(byLabel(view, "File"));
    act(() => {
      vi.advanceTimersByTime(5_000);
    });
    expect(chosen).toEqual(["camera", "file"]);
  });
});
