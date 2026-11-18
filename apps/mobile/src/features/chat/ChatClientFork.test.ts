// Fork from here as the client sends it (docs/design/mobile-chat-plan.md §3,
// "Fork and stopping a task").

import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";

import { ChatAction } from "../../protocol/gen/chat_pb";
import { Operation, ResponseSchema, type Request } from "../../protocol/gen/envelope_pb";
import { HostError } from "../../protocol/HostConnection";
import { chatFork } from "../../protocol/requests";
import { FakeConnection } from "../voice/testing";
import { ChatClient, createChatStore } from "./ChatClient";
import { chatItems, PANE, SERVER, snapshotEvent } from "./testing";
import { refusalText } from "./refusals";

type Answer = () => ReturnType<typeof create<typeof ResponseSchema>>;

function setup() {
  const connection = new FakeConnection();
  const client = new ChatClient({ store: createChatStore(), getConnection: () => connection, serverIdentity: () => SERVER });
  let fork: Answer = () => create(ResponseSchema, { ok: true, chat: { forkedPaneId: "%9" } });
  connection.answer(Operation.CHAT, (request: Request) => {
    const chat = request.chat!;
    if (chat.action === ChatAction.OPEN) client.onEvent(snapshotEvent(10n, chatItems()));
    return chat.action === ChatAction.FORK ? fork() : create(ResponseSchema, { ok: true });
  });
  const forks = () => connection.of(Operation.CHAT).map((request) => request.chat!).filter((chat) => chat.action === ChatAction.FORK);
  return { client, connection, forks, answerFork: (answer: Answer) => { fork = answer; } };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("ChatClient.fork", () => {
  it("the request names the pane, its server and the item", () => {
    const chat = chatFork({ paneId: PANE, expectedServerIdentity: SERVER }, "a1").chat!;
    expect(chat).toMatchObject({ action: ChatAction.FORK, paneId: PANE, expectedServerIdentity: SERVER, itemId: "a1" });
    // Nothing else rides along: no text, no request, no task.
    expect(chat).toMatchObject({ text: "", requestId: "", taskId: "", clientMessageId: "" });
  });

  it("sends the item and answers the pane the fork opened in", async () => {
    const { client, forks } = setup();
    client.acquire(PANE);
    await flush();
    expect(await client.fork(PANE, "a1")).toBe("%9");
    expect(forks()).toHaveLength(1);
    expect(forks()[0]).toMatchObject({ paneId: PANE, expectedServerIdentity: SERVER, itemId: "a1" });
  });

  it("answers no pane when the host names none", async () => {
    const { client, answerFork } = setup();
    client.acquire(PANE);
    await flush();
    answerFork(() => create(ResponseSchema, { ok: true }));
    expect(await client.fork(PANE, "a1")).toBe("");
  });

  it("passes a refusal on, with the host's own words", async () => {
    const { client, answerFork } = setup();
    client.acquire(PANE);
    await flush();
    answerFork(() => {
      throw new HostError("chat_fork_unavailable", "That reply cannot be forked.");
    });
    const refused = await client.fork(PANE, "a1").catch((error: unknown) => error);
    expect(refused).toMatchObject({ code: "chat_fork_unavailable" });
    expect(refusalText(refused)).toBe("That reply cannot be forked.");
    // The newest turn is still in progress.
    answerFork(() => {
      throw new HostError("chat_turn_busy", "The agent is working.");
    });
    expect(refusalText(await client.fork(PANE, "a1").catch((error: unknown) => error))).toBe("The agent is busy. Stop it first, or wait for the turn to end.");
  });

  it("a previous host, which does not know the action, is a refusal like any other", async () => {
    const { client, answerFork } = setup();
    client.acquire(PANE);
    await flush();
    answerFork(() => {
      throw new HostError("invalid_request", "Unknown chat action.");
    });
    expect(refusalText(await client.fork(PANE, "a1").catch((error: unknown) => error))).toBe("Unknown chat action.");
  });

  it("refuses without asking the host for a chat that is not open, or with no connection", async () => {
    const { client, connection, forks } = setup();
    await expect(client.fork(PANE, "a1")).rejects.toThrow("This chat is not open.");
    client.acquire(PANE);
    await flush();
    connection.state = "reconnecting";
    await expect(client.fork(PANE, "a1")).rejects.toThrow("Not connected.");
    expect(forks()).toHaveLength(0);
  });
});
