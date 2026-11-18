import { describe, expect, it } from "vitest";

import { ChatItemKind, ChatToolStatus } from "../../protocol/gen/chat_pb";
import { applyChatEvent, newChatPane } from "./chatModel";
import { chatItems, deltaEvent, itemEvent, paneFrom, PANE, SERVER, snapshotEvent, state, text, tool } from "./testing";
import { latestMark, olderState, timelineRows } from "./timelineRows";

describe("the conversation's rows", () => {
  it("are one per item, newest first, with the messages still on their way at the end", () => {
    const rows = timelineRows(chatItems(), [{ id: "m1", text: "first", acked: true }, { id: "m2", text: "second", acked: false }]);
    expect(rows.map((row) => `${row.kind}:${row.key}`)).toEqual(["outgoing:m2", "outgoing:m1", "item:test", "item:read", "item:a1", "item:think", "item:q1"]);
  });

  it("leave out what has nothing to draw", () => {
    const items = [
      text("q", ChatItemKind.USER, "hi"),
      text("blank", ChatItemKind.REASONING, "  \n"),
      text("odd", ChatItemKind.UNSPECIFIED, "?"),
      { ...tool("bare", {}), tool: undefined },
      text("a", ChatItemKind.ASSISTANT, "", { streaming: true }),
    ];
    expect(timelineRows(items, []).map((row) => row.key)).toEqual(["a", "q"]);
  });

  it("keep a message's row when the host shows it back: the key is the id it will carry", () => {
    const pane = paneFrom([snapshotEvent(5n, chatItems())]);
    const sending = timelineRows(pane.items, [{ id: "m1", text: "On its way", acked: false }]);
    const echoed = applyChatEvent(pane, itemEvent(6n, text("m1", ChatItemKind.USER, "On its way"))).pane;
    const shown = timelineRows(echoed.items, echoed.outgoing);
    expect(sending[0]).toMatchObject({ kind: "outgoing", key: "m1" });
    expect(shown[0]).toMatchObject({ kind: "item", key: "m1" });
    expect(shown.map((row) => row.key)).toEqual(sending.map((row) => row.key));
    // Held as an item and still listed as outgoing, it is drawn once.
    expect(timelineRows(echoed.items, [{ id: "m1", text: "On its way", acked: true }]).filter((row) => row.key === "m1")).toHaveLength(1);
  });

  it("keep the same row for an item that did not change", () => {
    const pane = paneFrom([snapshotEvent(5n, [...chatItems(), text("a9", ChatItemKind.ASSISTANT, "On", { streaming: true })])]);
    const before = timelineRows(pane.items, pane.outgoing);
    const grown = applyChatEvent(pane, deltaEvent(6n, "a9", " it.")).pane;
    const after = timelineRows(grown.items, grown.outgoing);
    expect(after[0]).not.toBe(before[0]);
    expect(after.slice(1)).toEqual(before.slice(1));
    after.slice(1).forEach((row, index) => expect(row).toBe(before[index + 1]));
  });
});

describe("what the end of the conversation looks like", () => {
  const base = [...chatItems(), text("a9", ChatItemKind.ASSISTANT, "On", { streaming: true })];
  const mark = (items = base, outgoing: Parameters<typeof timelineRows>[1] = []) => latestMark(timelineRows(items, outgoing));

  it("changes when a row arrives, the newest text grows, or the newest tool finishes", () => {
    expect(mark([])).toBe("");
    expect(mark([...base, text("n", ChatItemKind.NOTICE, "Interrupted.")])).not.toBe(mark());
    expect(mark([...base.slice(0, -1), { ...base.at(-1)!, text: "On it." }])).not.toBe(mark());
    expect(mark(base, [{ id: "m1", text: "more", acked: false }])).not.toBe(mark());
    const running = [...base, tool("t", { status: ChatToolStatus.RUNNING })];
    expect(mark([...base, tool("t", { status: ChatToolStatus.OK })])).not.toBe(mark(running));
  });

  it("does not change when earlier messages are loaded", () => {
    expect(mark([text("q0", ChatItemKind.USER, "Earlier question"), ...base])).toBe(mark());
  });
});

describe("what the top of the conversation says", () => {
  const open = (olderCursor: string, overrides: Parameters<typeof state>[0] = {}) => paneFrom([snapshotEvent(5n, chatItems(), { olderCursor, state: state(overrides) })]);

  it("says nothing while there is nothing older, or the chat is not open yet", () => {
    expect(olderState(undefined, false)).toBe("none");
    expect(olderState(newChatPane(PANE, SERVER), false)).toBe("none");
    expect(olderState(open(""), false)).toBe("none");
  });

  it("loads more, says so while it does, and offers Retry after a failure", () => {
    expect(olderState(open("c1"), false)).toBe("more");
    expect(olderState({ ...open("c1"), loadingOlder: true }, false)).toBe("loading");
    expect(olderState({ ...open("c1"), loadingOlder: true }, true)).toBe("loading");
    expect(olderState(open("c1"), true)).toBe("failed");
  });

  it("says earlier messages are not available only when none can be loaded", () => {
    expect(olderState(open("", { historyComplete: false }), false)).toBe("unavailable");
    expect(olderState(open("c1", { historyComplete: false }), false)).toBe("more");
  });

  it("asks a closed chat for nothing", () => {
    expect(olderState({ ...open("c1"), status: "gone" }, false)).toBe("none");
    expect(olderState({ ...open("", { historyComplete: false }), status: "gone" }, false)).toBe("unavailable");
  });
});
