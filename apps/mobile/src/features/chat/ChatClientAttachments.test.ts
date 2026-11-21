// A message with files (docs/design/mobile-chat-plan.md §3 "Attachments"):
// SEND names each by its host path and its name, and the message on its way
// keeps them, with where they are on the phone, for its bubble to draw.

import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";

import { ChatAction, ChatItemKind } from "../../protocol/gen/chat_pb";
import { Operation, ResponseSchema, type Request } from "../../protocol/gen/envelope_pb";
import { HostError } from "../../protocol/HostConnection";
import { FakeConnection } from "../voice/testing";
import { ChatClient, createChatStore } from "./ChatClient";
import type { OutgoingAttachment } from "./chatModel";
import { chatItems, itemEvent, PANE, SERVER, snapshotEvent, text } from "./testing";

const UPLOADS = "/home/dev/.cache/muxflow/uploads";
const SHOT: OutgoingAttachment = { path: `${UPLOADS}/shot.png`, name: "shot.png", mediaType: "image/png", sizeBytes: 2048, localUri: "file:///cache/shot.png" };
const LOG: OutgoingAttachment = { path: `${UPLOADS}/layout.log`, name: "layout.log", mediaType: "", sizeBytes: 18_432 };

function setup() {
  const connection = new FakeConnection();
  const store = createChatStore();
  const unconfirmed: string[] = [];
  const client = new ChatClient({
    store,
    getConnection: () => connection,
    serverIdentity: () => SERVER,
    onUnconfirmed: (_paneId, body, error) => unconfirmed.push(error === undefined ? `unconfirmed: ${body}` : `refused: ${body}`),
  });
  let refuseSend: Error | undefined;
  connection.answer(Operation.CHAT, (request: Request) => {
    const chat = request.chat!;
    if (chat.action === ChatAction.OPEN) client.onEvent(snapshotEvent(10n, chatItems()));
    if (chat.action === ChatAction.SEND && refuseSend) throw refuseSend;
    return create(ResponseSchema, { ok: true });
  });
  const sends = () => connection.of(Operation.CHAT).map((request) => request.chat!).filter((chat) => chat.action === ChatAction.SEND);
  const outgoing = () => store.getState().panes[PANE]?.outgoing ?? [];
  return { client, connection, sends, outgoing, unconfirmed, refuse: (error: Error | undefined) => { refuseSend = error; } };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const wire = (attachments: readonly { path: string; name: string; mediaType: string; sizeBytes: bigint }[]) =>
  attachments.map(({ path, name, mediaType, sizeBytes }) => ({ path, name, mediaType, sizeBytes }));

describe("ChatClient, a message with files", () => {
  it("names each file by its host path and name, and nothing else", async () => {
    const { client, sends } = setup();
    client.acquire(PANE);
    await flush();
    await client.send(PANE, "Why is the header cut off here?", [SHOT, LOG]);
    const sent = sends().at(-1)!;
    expect(sent.text).toBe("Why is the header cut off here?");
    // The host works out type and size itself, and where the file is on the phone is the phone's business.
    expect(wire(sent.attachments)).toEqual([
      { path: `${UPLOADS}/shot.png`, name: "shot.png", mediaType: "", sizeBytes: 0n },
      { path: `${UPLOADS}/layout.log`, name: "layout.log", mediaType: "", sizeBytes: 0n },
    ]);
  });

  it("sends files with no text at all", async () => {
    const { client, sends } = setup();
    client.acquire(PANE);
    await flush();
    await client.send(PANE, "", [SHOT]);
    expect(sends().at(-1)).toMatchObject({ text: "" });
    expect(sends().at(-1)!.attachments.map((file) => file.path)).toEqual([`${UPLOADS}/shot.png`]);
  });

  it("shows the message at once with its files, where they are on the phone, until the host shows it back", async () => {
    const { client, sends, outgoing } = setup();
    client.acquire(PANE);
    await flush();
    const sending = client.send(PANE, "And this?", [SHOT, LOG]);
    expect(outgoing()).toMatchObject([{ text: "And this?", attachments: [SHOT, LOG], acked: false }]);
    await sending;
    expect(outgoing()).toMatchObject([{ attachments: [SHOT, LOG], acked: true }]);
    client.onEvent(itemEvent(11n, text(sends().at(-1)!.clientMessageId, ChatItemKind.USER, "And this?")));
    expect(outgoing()).toEqual([]);
  });

  it("keeps a message without files as it was: no attachments on it or on the wire", async () => {
    const { client, sends, outgoing } = setup();
    client.acquire(PANE);
    await flush();
    await client.send(PANE, "plain");
    expect(outgoing()[0]).toEqual({ id: outgoing()[0]!.id, text: "plain", acked: true, unsure: false });
    expect(sends().at(-1)!.attachments).toEqual([]);
  });

  it("a refusal throws and takes the message off screen, files and all", async () => {
    const { client, outgoing, refuse } = setup();
    client.acquire(PANE);
    await flush();
    refuse(new HostError("chat_attachment_invalid", "That attachment is no longer on the host."));
    await expect(client.send(PANE, "", [SHOT])).rejects.toThrow("That attachment is no longer on the host.");
    expect(outgoing()).toEqual([]);
  });

  it("sends a message whose answer was lost again with the same files", async () => {
    const { client, sends, outgoing, refuse } = setup();
    client.acquire(PANE);
    await flush();
    refuse(new Error("connection closed"));
    await client.send(PANE, "", [SHOT, LOG]);
    expect(outgoing()).toMatchObject([{ unsure: true, attachments: [SHOT, LOG] }]);
    const first = sends().at(-1)!;

    refuse(undefined);
    client.onEvent(snapshotEvent(20n, chatItems()));
    await flush();
    const again = sends().at(-1)!;
    expect(again).not.toBe(first);
    expect(again.clientMessageId).toBe(first.clientMessageId);
    expect(again.attachments.map((file) => `${file.path}|${file.name}`)).toEqual([`${UPLOADS}/shot.png|shot.png`, `${UPLOADS}/layout.log|layout.log`]);
    expect(outgoing()).toMatchObject([{ acked: true, unsure: false, attachments: [SHOT, LOG] }]);
  });

  it("an edited message goes with its files after the rewind", async () => {
    const { client, connection } = setup();
    client.acquire(PANE);
    await flush();
    await client.rewindAndSend(PANE, "q1", "Only boxed output.", [SHOT]);
    const [rewind, send] = connection.of(Operation.CHAT).slice(-2).map((request) => request.chat!);
    expect(rewind).toMatchObject({ action: ChatAction.REWIND, itemId: "q1" });
    expect(rewind!.attachments).toEqual([]);
    expect(send).toMatchObject({ action: ChatAction.SEND, text: "Only boxed output." });
    expect(send!.attachments.map((file) => file.path)).toEqual([`${UPLOADS}/shot.png`]);
  });
});
