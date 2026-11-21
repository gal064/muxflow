import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";

import { ChatAction, ChatItemKind, type ChatRequest } from "../../protocol/gen/chat_pb";
import { Operation, ResponseSchema, type Request } from "../../protocol/gen/envelope_pb";
import { HostError } from "../../protocol/HostConnection";
import { FakeConnection } from "../voice/testing";
import { ChatClient, createChatStore, OPEN_ATTEMPTS } from "./ChatClient";
import { chatItems, itemEvent, PANE, SERVER, snapshotEvent, text, truncateEvent } from "./testing";

function setup() {
  const connection = new FakeConnection();
  const store = createChatStore();
  const timers: (() => void)[] = [];
  let revision = 10n;
  let server = SERVER;
  const resendRefused: string[] = [];
  const client = new ChatClient({
    store,
    getConnection: () => connection,
    serverIdentity: () => server,
    onUnconfirmed: (_paneId, body, error) => resendRefused.push(error === undefined ? `unconfirmed: ${body}` : body),
    retryDelayMs: 1,
    setTimer: (_ms, run) => timers.push(run),
  });
  // The host's answer to OPEN: the snapshot as an event, then the response.
  let refuseOpen: HostError | undefined;
  const answers: Partial<Record<ChatAction, (chat: ChatRequest) => ReturnType<typeof create<typeof ResponseSchema>>>> = {};
  connection.answer(Operation.CHAT, (request: Request) => {
    const chat = request.chat!;
    if (chat.action === ChatAction.OPEN) {
      if (refuseOpen) throw refuseOpen;
      client.onEvent(snapshotEvent(revision, chatItems(), { olderCursor: "c1" }));
    }
    const custom = answers[chat.action];
    return custom ? custom(chat) : create(ResponseSchema, { ok: true });
  });
  const actions = () => connection.of(Operation.CHAT).map((request) => request.chat!.action);
  return {
    connection, store, client, timers, answers, actions,
    resendRefused,
    setRevision: (next: bigint) => { revision = next; },
    setServer: (next: string) => { server = next; },
    refuse: (error: HostError | undefined) => { refuseOpen = error; },
  };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("ChatClient", () => {
  it("opens on the first holder, quoting the pane and its server, and closes after the last", async () => {
    const { client, store, actions, connection } = setup();
    const releaseA = client.acquire(PANE);
    const releaseB = client.acquire(PANE);
    await flush();
    expect(actions()).toEqual([ChatAction.OPEN]);
    expect(connection.of(Operation.CHAT)[0]!.chat).toMatchObject({ paneId: PANE, expectedServerIdentity: SERVER });
    expect(store.getState().panes[PANE]?.status).toBe("open");
    releaseA();
    expect(actions()).toEqual([ChatAction.OPEN]);
    releaseB();
    expect(actions()).toEqual([ChatAction.OPEN, ChatAction.CLOSE]);
    expect(store.getState().panes[PANE]).toBeUndefined();
  });

  it("sends OPEN again on a lost change, once, and takes the fresh snapshot", async () => {
    const { client, store, actions, setRevision } = setup();
    client.acquire(PANE);
    await flush();
    setRevision(30n);
    client.onEvent(itemEvent(12n, text("late", ChatItemKind.NOTICE, "late")));
    client.onEvent(itemEvent(13n, text("later", ChatItemKind.NOTICE, "later")));
    await flush();
    expect(actions()).toEqual([ChatAction.OPEN, ChatAction.OPEN]);
    expect(store.getState().panes[PANE]?.revision).toBe(30n);
    expect(store.getState().panes[PANE]?.resyncing).toBe(false);
  });

  it("sends OPEN again when a truncate names an item it does not hold", async () => {
    const { client, actions } = setup();
    client.acquire(PANE);
    await flush();
    client.onEvent(truncateEvent(11n, "not-loaded"));
    await flush();
    expect(actions()).toEqual([ChatAction.OPEN, ChatAction.OPEN]);
  });

  it("opens every held chat again after a reconnect", async () => {
    const { client, actions, store } = setup();
    client.acquire(PANE);
    await flush();
    client.onConnected();
    expect(store.getState().panes[PANE]?.resyncing).toBe(false);
    await flush();
    expect(actions()).toEqual([ChatAction.OPEN, ChatAction.OPEN]);
  });

  it("does not reopen a held chat on a replaced tmux server: the pane id names another pane", async () => {
    const { client, actions, store, setServer } = setup();
    client.acquire(PANE);
    await flush();
    setServer("server-b");
    client.onConnected();
    await flush();
    expect(store.getState().panes[PANE]?.status).toBe("gone");
    expect(actions()).toEqual([ChatAction.OPEN]);
  });

  it("after the host is forgotten, reopens a held chat only on the server it was on", async () => {
    const same = setup();
    same.client.acquire(PANE);
    await flush();
    same.client.reset();
    same.client.onConnected();
    await flush();
    expect(same.actions()).toEqual([ChatAction.OPEN, ChatAction.OPEN]);
    expect(same.store.getState().panes[PANE]?.status).toBe("open");

    const other = setup();
    other.client.acquire(PANE);
    await flush();
    other.client.reset();
    other.setServer("server-b");
    other.client.onConnected();
    await flush();
    expect(other.actions()).toEqual([ChatAction.OPEN]);
    expect(other.store.getState().panes[PANE]?.status).toBe("gone");
  });

  it("opens again when the screen comes back before the first OPEN is answered", async () => {
    const { client, actions, connection, store } = setup();
    const answers: ((response: ReturnType<typeof create<typeof ResponseSchema>>) => void)[] = [];
    connection.answer(Operation.CHAT, (request: Request) => {
      if (request.chat!.action !== ChatAction.OPEN) return create(ResponseSchema, { ok: true });
      return new Promise((resolve) => answers.push(resolve)) as unknown as ReturnType<typeof create<typeof ResponseSchema>>;
    });
    const release = client.acquire(PANE);
    release();
    client.acquire(PANE);
    expect(actions()).toEqual([ChatAction.OPEN, ChatAction.CLOSE, ChatAction.OPEN]);
    // The first OPEN's late answer does not clear the second's.
    answers[0]!(create(ResponseSchema, { ok: true }));
    await flush();
    client.onEvent(snapshotEvent(10n, chatItems()));
    expect(store.getState().panes[PANE]?.status).toBe("open");
  });

  it("asks again for a snapshot wanted while an OPEN was still unanswered", async () => {
    const { client, actions, connection } = setup();
    const answers: (() => void)[] = [];
    connection.answer(Operation.CHAT, (request: Request) => {
      if (request.chat!.action !== ChatAction.OPEN) return create(ResponseSchema, { ok: true });
      return new Promise((resolve) => answers.push(() => resolve(create(ResponseSchema, { ok: true })))) as never;
    });
    client.acquire(PANE);
    // The snapshot comes ahead of the answer, then a gap before the answer lands.
    client.onEvent(snapshotEvent(10n, chatItems()));
    client.onEvent(itemEvent(12n, text("late", ChatItemKind.NOTICE, "x")));
    expect(actions()).toEqual([ChatAction.OPEN]);
    answers[0]!();
    await flush();
    expect(actions()).toEqual([ChatAction.OPEN, ChatAction.OPEN]);
  });

  it("asks for a fresh snapshot when a SEND goes unanswered on a live connection", async () => {
    const { client, actions, answers } = setup();
    client.acquire(PANE);
    await flush();
    answers[ChatAction.SEND] = () => { throw new Error("RequestTimeoutError"); };
    await client.send(PANE, "slow");
    // The snapshot from that OPEN sends it once more, and only once.
    expect(actions()).toEqual([ChatAction.OPEN, ChatAction.SEND, ChatAction.OPEN, ChatAction.SEND]);
  });

  it("offers Retry when the host does not answer an OPEN, and ends the chat when its server changed", async () => {
    const { client, refuse, store } = setup();
    refuse(new Error("RequestTimeoutError") as HostError);
    client.acquire(PANE);
    await flush();
    expect(store.getState().panes[PANE]).toMatchObject({ status: "error", error: "The host didn't answer." });
    refuse(new HostError("chat_server_changed", "changed"));
    client.retry(PANE);
    await flush();
    expect(store.getState().panes[PANE]?.status).toBe("gone");
  });

  it("tries a refused chat_queue_full OPEN again, then says it cannot open", async () => {
    const { client, actions, timers, refuse, store } = setup();
    refuse(new HostError("chat_queue_full", "busy"));
    client.acquire(PANE);
    for (let attempt = 1; attempt < OPEN_ATTEMPTS; attempt += 1) {
      await flush();
      expect(timers).toHaveLength(attempt);
      timers[attempt - 1]!();
    }
    await flush();
    expect(actions()).toHaveLength(OPEN_ATTEMPTS);
    expect(store.getState().panes[PANE]?.status).toBe("error");
    refuse(undefined);
    client.retry(PANE);
    await flush();
    expect(store.getState().panes[PANE]?.status).toBe("open");
  });

  it("shows a refusal to open, such as a pane without a chat", async () => {
    const { client, refuse, store } = setup();
    refuse(new HostError("chat_unavailable", "This pane has no chat."));
    client.acquire(PANE);
    await flush();
    expect(store.getState().panes[PANE]).toMatchObject({ status: "error", error: "This pane has no chat." });
  });

  it("ends a held chat whose pane left the topology, and tells the host", async () => {
    const { client, actions, store } = setup();
    client.acquire(PANE);
    await flush();
    client.reconcilePanes(SERVER, new Set(["%1"]));
    expect(store.getState().panes[PANE]?.status).toBe("gone");
    expect(actions()).toEqual([ChatAction.OPEN, ChatAction.CLOSE]);
    // Gone stays gone: later events and reconnects leave it.
    client.onConnected();
    client.onEvent(itemEvent(11n, text("x", ChatItemKind.NOTICE, "x")));
    expect(actions()).toEqual([ChatAction.OPEN, ChatAction.CLOSE]);
    client.reconcilePanes(SERVER, new Set());
  });

  it("shows a sent message at once, keeps it on a duplicate refusal, and removes it on any other", async () => {
    const { client, store, answers, connection } = setup();
    client.acquire(PANE);
    await flush();
    const sending = client.send(PANE, "hello");
    expect(store.getState().panes[PANE]?.outgoing.map((message) => message.text)).toEqual(["hello"]);
    await sending;
    const sent = connection.of(Operation.CHAT).at(-1)!.chat!;
    expect(sent).toMatchObject({ action: ChatAction.SEND, text: "hello" });
    expect(sent.clientMessageId).not.toBe("");
    expect(store.getState().panes[PANE]?.outgoing[0]?.acked).toBe(true);
    client.onEvent(itemEvent(11n, text(sent.clientMessageId, ChatItemKind.USER, "hello")));
    expect(store.getState().panes[PANE]?.outgoing).toEqual([]);

    answers[ChatAction.SEND] = () => { throw new HostError("chat_message_duplicate", "dup"); };
    await client.send(PANE, "again");
    expect(store.getState().panes[PANE]?.outgoing).toHaveLength(1);

    answers[ChatAction.SEND] = () => { throw new HostError("chat_turn_busy", "busy"); };
    await expect(client.send(PANE, "refused")).rejects.toThrow("busy");
    expect(store.getState().panes[PANE]?.outgoing.map((message) => message.text)).toEqual(["again"]);
  });

  it("keeps a message whose SEND answer was lost and sends it again, with the same id, after the next snapshot", async () => {
    const { client, store, answers, connection, resendRefused } = setup();
    const release = client.acquire(PANE);
    await flush();
    answers[ChatAction.SEND] = () => { throw new Error("connection closed"); };
    await client.send(PANE, "lost");
    const first = connection.of(Operation.CHAT).at(-1)!.chat!;
    expect(store.getState().panes[PANE]?.outgoing).toMatchObject([{ text: "lost", unsure: true, acked: false }]);

    // The snapshot lacks it: it goes again under the same id and is acked.
    delete answers[ChatAction.SEND];
    client.onEvent(snapshotEvent(20n, chatItems()));
    await flush();
    const again = connection.of(Operation.CHAT).at(-1)!.chat!;
    expect(again).toMatchObject({ action: ChatAction.SEND, text: "lost", clientMessageId: first.clientMessageId });
    expect(store.getState().panes[PANE]?.outgoing).toMatchObject([{ text: "lost", acked: true, unsure: false }]);

    // The snapshot has it: nothing goes again.
    answers[ChatAction.SEND] = () => { throw new Error("connection closed"); };
    await client.send(PANE, "arrived");
    const arrived = connection.of(Operation.CHAT).at(-1)!.chat!;
    const sends = connection.of(Operation.CHAT).length;
    client.onEvent(snapshotEvent(30n, [...chatItems(), text(arrived.clientMessageId, ChatItemKind.USER, "arrived")]));
    await flush();
    expect(connection.of(Operation.CHAT)).toHaveLength(sends);
    expect(store.getState().panes[PANE]?.outgoing).toEqual([]);

    // Refused when sent again: it leaves the screen and the user is told.
    await client.send(PANE, "refused later");
    answers[ChatAction.SEND] = () => { throw new HostError("chat_turn_busy", "busy"); };
    client.onEvent(snapshotEvent(40n, chatItems()));
    await flush();
    expect(store.getState().panes[PANE]?.outgoing).toEqual([]);
    expect(resendRefused).toEqual(["refused later"]);

    // A duplicate when sent again: the host has it from before the snapshot's window.
    answers[ChatAction.SEND] = () => { throw new Error("connection closed"); };
    await client.send(PANE, "older than the window");
    answers[ChatAction.SEND] = () => { throw new HostError("chat_message_duplicate", "dup"); };
    client.onEvent(snapshotEvent(50n, chatItems()));
    await flush();
    expect(store.getState().panes[PANE]?.outgoing).toEqual([]);

    // Leaving the chat with one still unconfirmed tells the user.
    answers[ChatAction.SEND] = () => { throw new Error("connection closed"); };
    await client.send(PANE, "unconfirmed");
    release();
    expect(resendRefused.at(-1)).toBe("unconfirmed: unconfirmed");
  });

  it("follows the new pane behind a reused id after the tmux server is replaced", async () => {
    const { client, actions, store, setServer } = setup();
    client.acquire(PANE);
    await flush();
    setServer("server-b");
    client.onConnected();
    expect(store.getState().panes[PANE]?.status).toBe("gone");
    client.acquire(PANE);
    await flush();
    expect(actions()).toEqual([ChatAction.OPEN, ChatAction.OPEN]);
    expect(store.getState().panes[PANE]).toMatchObject({ status: "open", serverIdentity: "server-b" });
  });

  it("rewinds then sends the edited text", async () => {
    const { client, actions, connection } = setup();
    client.acquire(PANE);
    await flush();
    await client.rewindAndSend(PANE, "q1", "Only boxed output.");
    expect(actions().slice(-2)).toEqual([ChatAction.REWIND, ChatAction.SEND]);
    expect(connection.of(Operation.CHAT).at(-2)!.chat!.itemId).toBe("q1");
  });

  it("answers, interrupts, sets options and forks with the pane named", async () => {
    const { client, answers, connection } = setup();
    client.acquire(PANE);
    await flush();
    await client.answer(PANE, "p1", { allow: true });
    await client.interrupt(PANE);
    await client.setOptions(PANE, { effort: "max" });
    answers[ChatAction.SIDE_FORK] = () => create(ResponseSchema, { ok: true, chat: { forkedPaneId: "%9" } });
    expect(await client.sideFork(PANE, "which file?")).toBe("%9");
    const sent = connection.of(Operation.CHAT).slice(1).map((request) => request.chat!);
    expect(sent.map((chat) => chat.action)).toEqual([ChatAction.ANSWER, ChatAction.INTERRUPT, ChatAction.SET_OPTIONS, ChatAction.SIDE_FORK]);
    expect(sent[0]).toMatchObject({ requestId: "p1", answer: { allow: true } });
    expect(sent[2]!.options?.effort).toBe("max");
    expect(sent[3]!.text).toBe("which file?");
  });

  it("stops a background task by its id, and passes a refusal on", async () => {
    const { client, answers, connection } = setup();
    client.acquire(PANE);
    await flush();
    await client.stopTask(PANE, "task-dev");
    const sent = connection.of(Operation.CHAT).at(-1)!.chat!;
    expect(sent).toMatchObject({ action: ChatAction.STOP_TASK, paneId: PANE, expectedServerIdentity: SERVER, taskId: "task-dev" });
    answers[ChatAction.STOP_TASK] = () => {
      throw new HostError("chat_task_not_stoppable", "This task can't be stopped.");
    };
    await expect(client.stopTask(PANE, "task-dev")).rejects.toMatchObject({ code: "chat_task_not_stoppable" });
  });

  it("loads an older page and drops one that answers a request from before a new snapshot", async () => {
    const { client, store, answers } = setup();
    client.acquire(PANE);
    await flush();
    answers[ChatAction.LOAD_OLDER] = (chat) => create(ResponseSchema, {
      ok: true,
      chat: { older: [text(`before-${chat.olderCursor}`, ChatItemKind.USER, "Earlier")], olderCursor: "", revision: 10n },
    });
    await client.loadOlder(PANE);
    expect(store.getState().panes[PANE]?.items[0]?.id).toBe("before-c1");
    expect(store.getState().panes[PANE]?.olderCursor).toBe("");

    // A page in flight while a fresh snapshot lands is dropped.
    client.onEvent(snapshotEvent(20n, chatItems(), { olderCursor: "c1" }));
    let release: () => void = () => undefined;
    answers[ChatAction.LOAD_OLDER] = () => {
      const response = create(ResponseSchema, { ok: true, chat: { older: [text("stale", ChatItemKind.USER, "x")], olderCursor: "", revision: 20n } });
      return new Promise((resolve) => { release = () => resolve(response); }) as never;
    };
    const loading = client.loadOlder(PANE);
    client.onEvent(snapshotEvent(21n, chatItems(), { olderCursor: "c1" }));
    release();
    await loading;
    expect(store.getState().panes[PANE]?.items.some((item) => item.id === "stale")).toBe(false);
  });

  it("drops an older page that answers the screen's earlier hold of the pane", async () => {
    const { client, store, answers } = setup();
    const release = client.acquire(PANE);
    await flush();
    let answer: () => void = () => undefined;
    answers[ChatAction.LOAD_OLDER] = () => {
      const response = create(ResponseSchema, { ok: true, chat: { older: [text("stale", ChatItemKind.USER, "x")], olderCursor: "", revision: 10n } });
      return new Promise((resolve) => { answer = () => resolve(response); }) as never;
    };
    const loading = client.loadOlder(PANE);
    release();
    client.acquire(PANE);
    await flush();
    answer();
    await loading;
    expect(store.getState().panes[PANE]?.items.some((item) => item.id === "stale")).toBe(false);
    expect(store.getState().panes[PANE]?.olderCursor).toBe("c1");
  });

  it("asks for a fresh snapshot when the cursor is no longer valid", async () => {
    const { client, answers, actions } = setup();
    client.acquire(PANE);
    await flush();
    answers[ChatAction.LOAD_OLDER] = () => { throw new HostError("chat_cursor_invalid", "old cursor"); };
    await client.loadOlder(PANE);
    await flush();
    expect(actions()).toEqual([ChatAction.OPEN, ChatAction.LOAD_OLDER, ChatAction.OPEN]);
  });

  it("does nothing while disconnected, and opens when the connection comes back", async () => {
    const { client, connection, actions, store } = setup();
    connection.state = "reconnecting";
    client.acquire(PANE);
    await flush();
    expect(actions()).toEqual([]);
    await expect(client.send(PANE, "x")).rejects.toThrow("Not connected.");
    expect(store.getState().panes[PANE]?.outgoing).toEqual([]);
    connection.state = "connected";
    client.onConnected();
    await flush();
    expect(actions()).toEqual([ChatAction.OPEN]);
  });
});
