// Photos and files on the chat screen (docs/design/mobile-chat-mock.html, S7),
// from the plus button to the message the host shows back. The screen's files
// are the real ones (`useAttachments`) over stubbed pickers and an upload the
// test settles; the chat client is the real one over a scripted connection.

import { create } from "@bufbuild/protobuf";
import { useState } from "react";
import { act, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useStore } from "zustand";

import { ChatAction, ChatItemKind, type ChatItem } from "../../../protocol/gen/chat_pb";
import { Operation, ResponseSchema, type Request } from "../../../protocol/gen/envelope_pb";
import { HostError } from "../../../protocol/HostConnection";
import { allByLabel, byLabel, isHost, press, render, type } from "../../../testing/render";
import { documentPicker } from "../../../testing/stubs/expoDocumentPicker";
import { imagePicker } from "../../../testing/stubs/expoImagePicker";
import { FakeConnection } from "../../voice/testing";
import type { Uploader } from "../attach/attachmentQueue";
import { clearLocalImages } from "../attach/localImages";
import { pickFiles } from "../attach/pickers";
import { UploadError, type UploadedFile } from "../attach/upload";
import { sendWithFiles, useAttachments, type AttachDeps } from "../attach/useAttachments";
import { ChatClient, createChatStore } from "../ChatClient";
import { catalog, chatItems, itemEvent, PANE, SERVER, snapshotEvent, text } from "../testing";
import { ChatView } from "./ChatView";

// The message sheet's Copy reaches the clipboard, which is native.
vi.mock("expo-clipboard", () => ({ setStringAsync: vi.fn(async () => true) }));

const UPLOADS = "/home/dev/.cache/muxflow/uploads";

interface Started {
  name: string;
  signal: AbortSignal;
  progress: (sentBytes: number, totalBytes: number) => void;
  finish: () => void;
  fail: (error: unknown) => void;
}

interface Chat {
  view: ReactTestRenderer;
  client: ChatClient;
  uploads: Started[];
  said: string[];
  /** The SEND requests the host got. */
  sends: () => { text: string; files: string[]; id: string }[];
  refuseSend: (error: Error | undefined) => void;
}

function chat(options: { attachments?: boolean; attach?: boolean; items?: ChatItem[] } = {}): Chat {
  const connection = new FakeConnection();
  const store = createChatStore();
  const client = new ChatClient({ store, getConnection: () => connection, serverIdentity: () => SERVER });
  let refused: Error | undefined;
  connection.answer(Operation.CHAT, (request: Request) => {
    const asked = request.chat!;
    if (asked.action === ChatAction.OPEN) {
      const capabilities = { ...catalog().capabilities!, attachments: options.attachments ?? true };
      client.onEvent(snapshotEvent(10n, options.items ?? chatItems(), { catalog: catalog({ capabilities }) }));
    }
    if (asked.action === ChatAction.SEND && refused) throw refused;
    return create(ResponseSchema, { ok: true });
  });

  const uploads: Started[] = [];
  const said: string[] = [];
  const upload: Uploader = (file, signal, progress) =>
    new Promise<UploadedFile>((resolve, reject) => {
      signal.addEventListener("abort", () => reject(new UploadError("The upload was cancelled.", true)));
      uploads.push({ name: file.name, signal, progress, finish: () => resolve({ hostPath: `${UPLOADS}/${file.name}`, name: file.name, sizeBytes: file.size }), fail: reject });
    });
  const deps: AttachDeps = {
    upload,
    pick: pickFiles,
    toSource: (file) => ({ name: file.name ?? "file", size: file.size ?? 1000, localUri: file.uri, read: async () => new Uint8Array(0) }),
    say: (message) => said.push(message),
  };

  /** The chat screen's own part: the draft, and a send that takes the field's files with it. */
  function Screen() {
    const pane = useStore(store, (state) => state.panes[PANE]);
    const files = useAttachments(PANE, deps);
    const [draft, setDraft] = useState("");
    const send = (): void => {
      const body = draft.trim();
      setDraft("");
      sendWithFiles(files, (attached) => client.send(PANE, body, attached)).catch((error: unknown) => {
        setDraft((current) => current || body);
        said.push(error instanceof Error ? error.message : String(error));
      });
    };
    return (
      <ChatView
        attach={options.attach === false ? undefined : files}
        busy={false}
        commands={[]}
        connected
        detailsOpen={false}
        draft={draft}
        editing={undefined}
        folder="~/dev/muxflow"
        hostLabel="devbox"
        olderFailed={false}
        onAnswer={() => undefined}
        onBack={() => undefined}
        onCancelEdit={() => undefined}
        onDetails={() => undefined}
        onDraft={setDraft}
        onEdit={() => undefined}
        onLoadOlder={() => undefined}
        onPickCommand={() => undefined}
        onRetry={() => undefined}
        onRetryOlder={() => undefined}
        onSend={send}
        onSetOptions={() => undefined}
        onStop={() => undefined}
        onStopTask={() => undefined}
        onVoiceMode={() => undefined}
        pane={pane}
        stopping={false}
        title="Wrapped links"
        voiceMode={false}
      />
    );
  }

  client.acquire(PANE);
  const view = render(<Screen />);
  return {
    view,
    client,
    uploads,
    said,
    sends: () => connection.of(Operation.CHAT).map((request) => request.chat!).filter((asked) => asked.action === ChatAction.SEND)
      .map((asked) => ({ text: asked.text, files: asked.attachments.map((file) => `${file.path}|${file.name}`), id: asked.clientMessageId })),
    refuseSend: (error) => { refused = error; },
  };
}

/** Lets the snapshot, a picker's answer, an upload and a send run. */
async function settle(): Promise<void> {
  await act(async () => {
    for (let turn = 0; turn < 12; turn += 1) await Promise.resolve();
  });
}

function texts(node: ReactTestInstance): string[] {
  return node.findAll((child) => isHost(child, "Text")).map((child) => child.children.join(""));
}

/** The chips on the reply field, by their cross. */
const onField = (view: ReactTestRenderer): string[] => allByLabel(view, /^Remove /).map((node) => (node.props.accessibilityLabel as string).slice("Remove ".length));

/** Your messages in the conversation, oldest first: each bubble's files and words. */
function bubbles(view: ReactTestRenderer): string[][] {
  const list = view.root.find((node) => isHost(node, "FlatList"));
  return list.findAll((node) => isHost(node, "Pressable") && typeof node.props.delayLongPress === "number" && node.props.onAccessibilityAction !== undefined)
    .map((bubble) => [
      ...bubble.findAll((node) => typeof node.type === "string" && /^(Image|File) /.test(String(node.props.accessibilityLabel ?? ""))).map((node) => node.props.accessibilityLabel as string),
      ...texts(bubble).filter((line) => !/^\d+(\.\d)? (B|KB|MB)$/.test(line)),
    ]);
}

async function attach(view: ReactTestRenderer, choice: "Photo library" | "Take a photo" | "File"): Promise<void> {
  press(byLabel(view, "Attach"));
  press(byLabel(view, choice));
  await settle();
}

beforeEach(() => {
  imagePicker.reset();
  documentPicker.reset();
  clearLocalImages();
});

describe("the plus button", () => {
  it("is offered when the agent takes files with a message", async () => {
    const { view } = chat();
    await settle();
    expect(allByLabel(view, "Attach")).toHaveLength(1);
  });

  it("is not offered to an agent, or by a host, without the attachments capability", async () => {
    const { view } = chat({ attachments: false });
    await settle();
    expect(allByLabel(view, "Attach")).toHaveLength(0);
    byLabel(view, "Message");
  });

  it("is not offered by a screen that has no files to attach", async () => {
    const { view } = chat({ attach: false });
    await settle();
    expect(allByLabel(view, "Attach")).toHaveLength(0);
  });

  it("opens the attach sheet, which closes once a choice is made or it is dismissed", async () => {
    const { view } = chat();
    await settle();
    expect(allByLabel(view, "Photo library")).toHaveLength(0);
    press(byLabel(view, "Attach"));
    expect(["Photo library", "Take a photo", "File"].map((label) => allByLabel(view, label).length)).toEqual([1, 1, 1]);
    press(byLabel(view, "File"));
    await settle();
    expect(allByLabel(view, "Photo library")).toHaveLength(0);
    expect(documentPicker.opened).toHaveLength(1);

    press(byLabel(view, "Attach"));
    const sheet = view.root.findAll((node) => isHost(node, "Modal")).find((modal) => texts(modal).includes("Attach"))!;
    act(() => (sheet.props.onRequestClose as () => void)());
    expect(allByLabel(view, "Photo library")).toHaveLength(0);
    expect(documentPicker.opened).toHaveLength(1);
  });
});

describe("attaching and sending", () => {
  it("each of the sheet's three choices puts its files on the reply field", async () => {
    const { view, uploads } = chat();
    await settle();
    imagePicker.library = [{ uri: "file:///cache/a.png", fileName: "IMG_0001.PNG", fileSize: 2048 }, { uri: "file:///cache/b.jpg", fileName: "IMG_0002.JPG", fileSize: 4096 }];
    await attach(view, "Photo library");
    imagePicker.camera = [{ uri: "file:///cache/cam.jpg", fileSize: 900 }];
    await attach(view, "Take a photo");
    documentPicker.files = [{ uri: "file:///cache/layout.log", name: "layout.log", size: 18_432 }];
    await attach(view, "File");

    const names = onField(view);
    expect(names).toHaveLength(4);
    expect(names.slice(0, 2)).toEqual(["IMG_0001.PNG", "IMG_0002.JPG"]);
    expect(names[2]).toMatch(/^Photo .*\.jpg$/);
    expect(names[3]).toBe("layout.log");
    // One at a time, in the order they were added.
    expect(uploads.map((upload) => upload.name)).toEqual(["IMG_0001.PNG"]);
    byLabel(view, "IMG_0001.PNG, Uploading 0%");
    byLabel(view, "layout.log, Waiting to upload");
  });

  it("Send waits for the uploads, then sends each file by its host path and name", async () => {
    const { view, uploads, sends } = chat();
    await settle();
    imagePicker.library = [{ uri: "file:///cache/shot.png", fileName: "shot.png", fileSize: 2048 }];
    await attach(view, "Photo library");
    documentPicker.files = [{ uri: "file:///cache/layout.log", name: "layout.log", size: 18_432 }];
    await attach(view, "File");
    type(byLabel(view, "Message"), "Why is the header cut off here?");

    // One is still uploading, so Send waits for it.
    expect(byLabel(view, "Send").props.disabled).toBe(true);
    expect(byLabel(view, "Send").props.accessibilityHint).toBe("The attachments are still uploading.");
    act(() => uploads[0]!.progress(1024, 2048));
    byLabel(view, "shot.png, Uploading 50%");
    uploads[0]!.finish();
    await settle();
    expect(byLabel(view, "Send").props.disabled).toBe(true);
    uploads[1]!.finish();
    await settle();
    expect(byLabel(view, "Send").props.disabled).toBe(false);
    expect(byLabel(view, "Send").props.accessibilityHint).toBeUndefined();

    press(byLabel(view, "Send"));
    // On its way: the bubble has the files at once, the image from the phone's own file, and the field is empty.
    expect(bubbles(view).at(-1)).toEqual(["Image shot.png", "File layout.log", "layout.log", "Why is the header cut off here?"]);
    expect(byLabel(view, "Image shot.png").findAll((node) => isHost(node, "Image"))[0]!.props.source).toEqual({ uri: "file:///cache/shot.png" });
    expect(onField(view)).toEqual([]);
    await settle();
    expect(sends()).toEqual([{
      text: "Why is the header cut off here?",
      files: [`${UPLOADS}/shot.png|shot.png`, `${UPLOADS}/layout.log|layout.log`],
      id: sends()[0]!.id,
    }]);
    expect(onField(view)).toEqual([]);
    expect(byLabel(view, "Send").props.disabled).toBe(true);
  });

  it("sends files alone, and the message has no empty line under them", async () => {
    const { view, uploads, sends, client } = chat();
    await settle();
    imagePicker.library = [{ uri: "file:///cache/shot.png", fileName: "shot.png", fileSize: 2048 }];
    await attach(view, "Photo library");
    uploads[0]!.finish();
    await settle();
    press(byLabel(view, "Send"));
    await settle();
    expect(sends().map((send) => [send.text, send.files])).toEqual([["", [`${UPLOADS}/shot.png|shot.png`]]]);
    expect(bubbles(view).at(-1)).toEqual(["Image shot.png"]);

    // Shown back by the host: still this phone's thumbnail, with nothing fetched.
    act(() => client.onEvent(itemEvent(11n, text(sends()[0]!.id, ChatItemKind.USER, "", {
      attachments: [{ path: `${UPLOADS}/shot.png`, name: "shot.png", mediaType: "image/png", sizeBytes: 2048n }],
    }))));
    expect(bubbles(view).at(-1)).toEqual(["Image shot.png"]);
    expect(byLabel(view, "Image shot.png").findAll((node) => isHost(node, "Image"))[0]!.props.source).toEqual({ uri: "file:///cache/shot.png" });
  });

  it("a refused send puts the files, and the text, back on the reply field", async () => {
    const { view, uploads, sends, said, refuseSend } = chat();
    await settle();
    documentPicker.files = [{ uri: "file:///cache/layout.log", name: "layout.log", size: 18_432 }];
    await attach(view, "File");
    uploads[0]!.finish();
    await settle();
    type(byLabel(view, "Message"), "See the log.");
    refuseSend(new HostError("chat_turn_busy", "The agent is busy."));
    press(byLabel(view, "Send"));
    await settle();
    expect(said).toEqual(["The agent is busy."]);
    expect(onField(view)).toEqual(["layout.log"]);
    expect(byLabel(view, "Message").props.value).toBe("See the log.");
    expect(byLabel(view, "Send").props.disabled).toBe(false);

    // Sent again, it goes, with the same file and no second upload.
    refuseSend(undefined);
    press(byLabel(view, "Send"));
    await settle();
    expect(sends().at(-1)).toMatchObject({ text: "See the log.", files: [`${UPLOADS}/layout.log|layout.log`] });
    expect(uploads).toHaveLength(1);
    expect(onField(view)).toEqual([]);
  });

  it("a failed upload says why on its chip and holds Send until the chip is removed", async () => {
    const { view, uploads, sends } = chat();
    await settle();
    documentPicker.files = [{ uri: "file:///cache/a.txt", name: "a.txt", size: 100 }, { uri: "file:///cache/b.txt", name: "b.txt", size: 200 }];
    await attach(view, "File");
    uploads[0]!.fail(new UploadError("The connection to the host was lost."));
    await settle();
    byLabel(view, "a.txt, The connection to the host was lost.");
    uploads[1]!.finish();
    await settle();
    type(byLabel(view, "Message"), "Both, please.");
    expect(byLabel(view, "Send").props.disabled).toBe(true);
    expect(byLabel(view, "Send").props.accessibilityHint).toBe("An attachment failed to upload. Remove it to send.");

    press(byLabel(view, "Remove a.txt"));
    expect(byLabel(view, "Send").props.disabled).toBe(false);
    press(byLabel(view, "Send"));
    await settle();
    expect(sends().at(-1)!.files).toEqual([`${UPLOADS}/b.txt|b.txt`]);
  });

  it("removing a chip stops its upload", async () => {
    const { view, uploads } = chat();
    await settle();
    documentPicker.files = [{ uri: "file:///cache/a.txt", name: "a.txt", size: 100 }];
    await attach(view, "File");
    press(byLabel(view, "Remove a.txt"));
    expect(uploads[0]!.signal.aborted).toBe(true);
    await settle();
    expect(onField(view)).toEqual([]);
  });

  it("refuses the eleventh file in the desktop's words", async () => {
    const { view, said } = chat();
    await settle();
    documentPicker.files = Array.from({ length: 11 }, (_, index) => ({ uri: `file:///cache/${index}.txt`, name: `${index}.txt`, size: 10 }));
    await attach(view, "File");
    expect(onField(view)).toHaveLength(10);
    expect(said).toEqual(["A message takes at most 10 attachments."]);
    // The field is full: the next choice opens no picker and says so again.
    await attach(view, "Take a photo");
    expect(imagePicker.launches).toEqual([]);
    expect(said).toEqual(["A message takes at most 10 attachments.", "A message takes at most 10 attachments."]);
  });
});

describe("a message's files in the conversation", () => {
  it("draws the files of a message sent from anywhere else in its bubble, above its text", async () => {
    const items = [
      ...chatItems(),
      text("with-files", ChatItemKind.USER, "Why is the header cut off here?", {
        attachments: [
          { path: `${UPLOADS}/desktop.png`, name: "desktop.png", mediaType: "image/png", sizeBytes: 880_640n },
          { path: `${UPLOADS}/layout.log`, name: "layout.log", mediaType: "", sizeBytes: 18_432n },
        ],
      }),
      text("files-only", ChatItemKind.USER, "", { attachments: [{ path: "", name: "pasted.png", mediaType: "image/png", sizeBytes: 0n }] }),
    ];
    const { view } = chat({ items });
    await settle();
    expect(bubbles(view)).toEqual([
      ["Find out why wrapped paths are missed."],
      ["Image desktop.png", "File layout.log", "desktop.png", "layout.log", "Why is the header cut off here?"],
      // Not the empty bubble it used to be: its file, and no empty line.
      ["Image pasted.png", "pasted.png"],
    ]);
    // Not this phone's image: a named chip, with nothing fetched for it.
    expect(byLabel(view, "Image desktop.png").findAll((node) => isHost(node, "Image"))).toEqual([]);
  });
});
