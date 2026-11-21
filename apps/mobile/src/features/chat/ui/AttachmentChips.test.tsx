// The files a message carries, as its bubble draws them
// (docs/design/mobile-chat-mock.html, S7, with the plan's departure: only the
// images this phone attached have a thumbnail).

import { create } from "@bufbuild/protobuf";
import { act, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, describe, expect, it } from "vitest";

import { ChatAttachmentSchema, type ChatAttachment } from "../../../protocol/gen/chat_pb";
import { HostError } from "../../../protocol/HostConnection";
import { allByLabel, byLabel, isHost, longPress, press, render } from "../../../testing/render";
import { ChatImagesContext, type ChatImages } from "../attach/chatImages";
import { clearLocalImages, localImageFor, rememberLocalImage } from "../attach/localImages";
import { AttachmentChips, type ShownAttachment } from "./AttachmentChips";
import { IMAGE_NOT_DRAWN } from "./ImageViewer";

const UPLOADS = "/home/dev/.cache/muxflow/uploads";

function attached(path: string, name: string, mediaType: string, sizeBytes = 0n): ChatAttachment {
  return create(ChatAttachmentSchema, { path, name, mediaType, sizeBytes });
}

interface Asked {
  path: string;
  answer: (uri: string) => void;
  refuse: (error: unknown) => void;
}

/** The host as the chips reach it: every read waits for the test's answer. */
function host(): { images: ChatImages; asked: Asked[]; released: string[] } {
  const asked: Asked[] = [];
  const released: string[] = [];
  return {
    asked,
    released,
    images: {
      load: (path) => new Promise((resolve, reject) => asked.push({ path, answer: (uri) => resolve({ uri }), refuse: reject })),
      release: (uri) => released.push(uri),
    },
  };
}

function chips(attachments: readonly ShownAttachment[], images: ChatImages = host().images, onLongPress?: () => void): ReactTestRenderer {
  return render(
    <ChatImagesContext.Provider value={images}>
      <AttachmentChips attachments={attachments} onLongPress={onLongPress} />
    </ChatImagesContext.Provider>,
  );
}

function texts(node: ReactTestInstance): string[] {
  return node.findAll((child) => isHost(child, "Text")).map((child) => child.children.join(""));
}

/** The viewer, when it is open. */
function viewer(view: ReactTestRenderer): ReactTestInstance | undefined {
  return view.root.findAll((node) => isHost(node, "Modal"))[0];
}

/** The picture the viewer shows. */
function shown(view: ReactTestRenderer): unknown {
  return viewer(view)?.findAll((node) => isHost(node, "Image"))[0]?.props.source;
}

async function settle(): Promise<void> {
  await act(async () => {
    for (let turn = 0; turn < 6; turn += 1) await Promise.resolve();
  });
}

beforeEach(clearLocalImages);

describe("a message's files", () => {
  it("draws nothing for a message without any", () => {
    expect(chips([]).toJSON()).toBeNull();
  });

  it("an image this phone attached is its thumbnail, and opens at once from the phone's own file", () => {
    rememberLocalImage(`${UPLOADS}/shot.png`, "file:///cache/shot.png", 2048);
    const reads = host();
    const view = chips([attached(`${UPLOADS}/shot.png`, "shot.png", "image/png", 2048n)], reads.images);
    const thumbnail = byLabel(view, "Image shot.png");
    expect(thumbnail.findAll((node) => isHost(node, "Image"))[0]!.props.source).toEqual({ uri: "file:///cache/shot.png" });
    expect(texts(thumbnail)).toEqual([]);
    expect(viewer(view)).toBeUndefined();

    press(thumbnail);
    expect(shown(view)).toEqual({ uri: "file:///cache/shot.png" });
    expect(texts(viewer(view)!)).toEqual(["shot.png"]);
    // Nothing was read from the host, and nothing is given back.
    expect(reads.asked).toEqual([]);
    press(byLabel(view, "Close"));
    expect(viewer(view)).toBeUndefined();
    expect(reads.released).toEqual([]);
  });

  it("does not take another file the host keeps under the same name for this phone's image", () => {
    rememberLocalImage(`${UPLOADS}/shot.png`, "file:///cache/shot.png", 2048);
    const view = chips([attached(`${UPLOADS}/shot.png`, "shot.png", "image/png", 999_999n)]);
    expect(byLabel(view, "Image shot.png").findAll((node) => isHost(node, "Image"))).toEqual([]);
  });

  it("a message on its way shows its image from the file it was sent with", () => {
    const view = chips([{ path: `${UPLOADS}/shot.png`, name: "shot.png", mediaType: "image/png", sizeBytes: 2048, localUri: "file:///cache/picked.png" }]);
    expect(byLabel(view, "Image shot.png").findAll((node) => isHost(node, "Image"))[0]!.props.source).toEqual({ uri: "file:///cache/picked.png" });
  });

  it("an image attached anywhere else is a named chip: tapping it reads it from the host and shows it", async () => {
    const reads = host();
    const view = chips([attached(`${UPLOADS}/desktop.png`, "desktop.png", "image/png", 880_640n)], reads.images);
    const chip = byLabel(view, "Image desktop.png");
    expect(texts(chip)).toEqual(["desktop.png", "860 KB"]);
    expect(chip.findAll((node) => isHost(node, "Image"))).toEqual([]);
    // Nothing is fetched unasked.
    expect(reads.asked).toEqual([]);

    press(chip);
    expect(reads.asked.map((ask) => ask.path)).toEqual([`${UPLOADS}/desktop.png`]);
    expect(texts(viewer(view)!)).toEqual(["desktop.png", "Loading the image…"]);
    expect(viewer(view)!.findAll((node) => isHost(node, "ActivityIndicator"))).toHaveLength(1);

    reads.asked[0]!.answer("file:///cache/chat-images/1.png");
    await settle();
    expect(shown(view)).toEqual({ uri: "file:///cache/chat-images/1.png" });
    expect(texts(viewer(view)!)).toEqual(["desktop.png"]);

    // Closing gives the file back; the system's Back closes it as the button does.
    act(() => (viewer(view)!.props.onRequestClose as () => void)());
    expect(viewer(view)).toBeUndefined();
    expect(reads.released).toEqual(["file:///cache/chat-images/1.png"]);
  });

  it("says why when the image cannot be read, in the host's own words", async () => {
    const reads = host();
    const view = chips([attached(`${UPLOADS}/gone.png`, "gone.png", "image/png")], reads.images);
    press(byLabel(view, "Image gone.png"));
    reads.asked[0]!.refuse(new HostError("file_not_found", "That file is no longer on the host."));
    await settle();
    expect(texts(viewer(view)!)).toEqual(["gone.png", "That file is no longer on the host."]);
    expect(shown(view)).toBeUndefined();
    press(byLabel(view, "Close"));
    expect(viewer(view)).toBeUndefined();

    // Asked again, it is read again.
    press(byLabel(view, "Image gone.png"));
    reads.asked[1]!.refuse(new Error("That attachment is not an image."));
    await settle();
    expect(texts(viewer(view)!)).toEqual(["gone.png", "That attachment is not an image."]);
    press(byLabel(view, "Close"));

    // A refusal with no words of its own still says something.
    press(byLabel(view, "Image gone.png"));
    reads.asked[2]!.refuse(new HostError("internal", ""));
    await settle();
    expect(texts(viewer(view)!)).toEqual(["gone.png", "The image could not be loaded."]);
  });

  it("says so when the phone has the file and cannot draw it", async () => {
    const reads = host();
    const view = chips([attached(`${UPLOADS}/logo.svg`, "logo.svg", "image/svg+xml")], reads.images);
    press(byLabel(view, "Image logo.svg"));
    reads.asked[0]!.answer("file:///cache/chat-images/2.svg");
    await settle();
    act(() => (viewer(view)!.findAll((node) => isHost(node, "Image"))[0]!.props.onError as () => void)());
    expect(texts(viewer(view)!)).toEqual(["logo.svg", IMAGE_NOT_DRAWN]);
  });

  it("does not show an image that arrives after the viewer was closed, and gives its file back", async () => {
    const reads = host();
    const view = chips([attached(`${UPLOADS}/slow.png`, "slow.png", "image/png")], reads.images);
    press(byLabel(view, "Image slow.png"));
    press(byLabel(view, "Close"));
    reads.asked[0]!.answer("file:///cache/chat-images/3.png");
    await settle();
    expect(viewer(view)).toBeUndefined();
    expect(reads.released).toEqual(["file:///cache/chat-images/3.png"]);
  });

  it("gives the file back when the message leaves the screen with the viewer open", async () => {
    const reads = host();
    const view = chips([attached(`${UPLOADS}/desktop.png`, "desktop.png", "image/png")], reads.images);
    press(byLabel(view, "Image desktop.png"));
    reads.asked[0]!.answer("file:///cache/chat-images/4.png");
    await settle();
    act(() => view.unmount());
    expect(reads.released).toEqual(["file:///cache/chat-images/4.png"]);
  });

  it("an image the agent's record holds inline is a named chip that does nothing", () => {
    const reads = host();
    const view = chips([attached("", "pasted.png", "image/png"), attached("", "", "image/jpeg")], reads.images);
    const chip = byLabel(view, "Image pasted.png");
    expect(isHost(chip, "View")).toBe(true);
    expect(chip.props.onPress).toBeUndefined();
    expect(texts(chip)).toEqual(["pasted.png"]);
    // One without a name is still said to be an image.
    expect(texts(byLabel(view, "Image Image"))).toEqual(["Image"]);
    expect(reads.asked).toEqual([]);
  });

  it("any other file is a named chip with its size, and does nothing", () => {
    const view = chips([attached(`${UPLOADS}/layout.log`, "layout.log", "", 18_432n)]);
    const chip = byLabel(view, "File layout.log");
    expect(isHost(chip, "View")).toBe(true);
    expect(texts(chip)).toEqual(["layout.log", "18 KB"]);
    expect(chip.findAll((node) => isHost(node, "Svg"))).toHaveLength(1);
  });

  it("goes back to a named chip when the phone's own file is gone", () => {
    rememberLocalImage(`${UPLOADS}/shot.png`, "file:///cache/evicted.png", 2048);
    const reads = host();
    const view = chips([attached(`${UPLOADS}/shot.png`, "shot.png", "image/png", 2048n)], reads.images);
    act(() => (byLabel(view, "Image shot.png").findAll((node) => isHost(node, "Image"))[0]!.props.onError as () => void)());
    const chip = byLabel(view, "Image shot.png");
    expect(texts(chip)).toEqual(["shot.png", "2 KB"]);
    expect(localImageFor(`${UPLOADS}/shot.png`, 2048)).toBeUndefined();
    // And is read from the host like any other.
    press(chip);
    expect(reads.asked.map((ask) => ask.path)).toEqual([`${UPLOADS}/shot.png`]);
  });

  it("passes a hold on the chips that take touches to the message", () => {
    rememberLocalImage(`${UPLOADS}/mine.png`, "file:///cache/mine.png", 2048);
    let held = 0;
    const view = chips([
      attached(`${UPLOADS}/mine.png`, "mine.png", "image/png"),
      attached(`${UPLOADS}/theirs.png`, "theirs.png", "image/png"),
      attached(`${UPLOADS}/layout.log`, "layout.log", ""),
    ], host().images, () => { held += 1; });
    longPress(byLabel(view, "Image mine.png"));
    longPress(byLabel(view, "Image theirs.png"));
    expect(held).toBe(2);
    expect(allByLabel(view, /^(Image|File) /)).toHaveLength(3);
  });
});
