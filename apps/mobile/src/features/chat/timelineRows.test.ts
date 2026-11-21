import { describe, expect, it } from "vitest";

import { ChatItemKind, ChatToolKind, ChatToolStatus, ChatTurnState, type ChatItem } from "../../protocol/gen/chat_pb";
import { applyChatEvent, newChatPane } from "./chatModel";
import { chatItems, deltaEvent, itemEvent, paneFrom, PANE, SERVER, snapshotEvent, state, subagent, text, tool, updatedEvent } from "./testing";
import { latestMark, olderState, timelineRows, type TimelineRow } from "./timelineRows";

const IDLE = ChatTurnState.IDLE;
const WORKING = ChatTurnState.WORKING;

describe("the conversation's rows", () => {
  it("are one per item, newest first, with the messages still on their way at the end", () => {
    const rows = timelineRows(chatItems(), [{ id: "m1", text: "first", acked: true }, { id: "m2", text: "second", acked: false }], IDLE);
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
    expect(timelineRows(items, [], IDLE).map((row) => row.key)).toEqual(["a", "q"]);
  });

  it("keep a message's row when the host shows it back: the key is the id it will carry", () => {
    const pane = paneFrom([snapshotEvent(5n, chatItems())]);
    const sending = timelineRows(pane.items, [{ id: "m1", text: "On its way", acked: false }], IDLE);
    const echoed = applyChatEvent(pane, itemEvent(6n, text("m1", ChatItemKind.USER, "On its way"))).pane;
    const shown = timelineRows(echoed.items, echoed.outgoing, IDLE);
    expect(sending[0]).toMatchObject({ kind: "outgoing", key: "m1" });
    expect(shown[0]).toMatchObject({ kind: "item", key: "m1" });
    expect(shown.map((row) => row.key)).toEqual(sending.map((row) => row.key));
    // Held as an item and still listed as outgoing, it is drawn once.
    expect(timelineRows(echoed.items, [{ id: "m1", text: "On its way", acked: true }], IDLE).filter((row) => row.key === "m1")).toHaveLength(1);
  });

  it("keep the same row for an item that did not change", () => {
    const pane = paneFrom([snapshotEvent(5n, [...chatItems(), text("a9", ChatItemKind.ASSISTANT, "On", { streaming: true })])]);
    const before = timelineRows(pane.items, pane.outgoing, IDLE);
    const grown = applyChatEvent(pane, deltaEvent(6n, "a9", " it.")).pane;
    const after = timelineRows(grown.items, grown.outgoing, IDLE);
    expect(after[0]).not.toBe(before[0]);
    expect(after.slice(1)).toEqual(before.slice(1));
    after.slice(1).forEach((row, index) => expect(row).toBe(before[index + 1]));
  });
});

// The desktop's own cases (apps/desktop-gpui/src/chat/timeline.rs, tests), and where the phone departs from them.
describe("runs of steps", () => {
  const user = (id: string) => text(id, ChatItemKind.USER, "Do it.");
  const says = (id: string) => text(id, ChatItemKind.ASSISTANT, "Done.");
  const thought = (id: string, streaming = false, body = "Look at the view first.") => text(id, ChatItemKind.REASONING, body, { streaming });
  const read = (id: string, status = ChatToolStatus.OK) => tool(id, { kind: ChatToolKind.READ, title: "read", detail: "src/view.rs", path: "src/view.rs", status });
  /** The rows oldest first, a group as `key[its steps]` with `*` while it is live. */
  const laid = (items: ChatItem[], turn = IDLE) =>
    timelineRows(items, [], turn).reverse().map((row: TimelineRow) => (row.kind === "steps" ? `${row.key}[${row.items.map((item) => item.id).join(" ")}]${row.live ? "*" : ""}` : row.key));

  it("stay rows when one or two, and fold into one row from three", () => {
    expect(laid([user("u"), read("r1"), says("a")])).toEqual(["u", "r1", "a"]);
    expect(laid([user("u"), read("r1"), read("r2"), says("a")])).toEqual(["u", "r1", "r2", "a"]);
    expect(laid([user("u"), read("r1"), read("r2"), read("r3"), says("a")])).toEqual(["u", "r1[r1 r2 r3]", "a"]);
  });

  it("are split by the agent's text, and by a notice", () => {
    expect(laid([user("u"), says("a1"), read("r1"), read("r2"), says("a2"), read("r3"), read("r4"), read("r5"), says("a3")]))
      .toEqual(["u", "a1", "r1", "r2", "a2", "r3[r3 r4 r5]", "a3"]);
    const items = [user("u"), thought("t"), tool("c", { detail: "cargo test" }), read("e"), tool("m", { kind: ChatToolKind.MCP, detail: "github.get_pull_request" }), text("n", ChatItemKind.NOTICE, "Interrupted"), read("r")];
    expect(laid(items)).toEqual(["u", "t[t c e m]", "n", "r"]);
  });

  it("are live only at the end of a turn in progress", () => {
    const items = [user("u"), read("r1"), read("r2"), read("r3", ChatToolStatus.RUNNING)];
    expect(laid(items, WORKING)).toEqual(["u", "r1[r1 r2 r3]*"]);
    expect(laid(items, ChatTurnState.WAITING)).toEqual(["u", "r1[r1 r2 r3]*"]);
    expect(laid(items, IDLE)).toEqual(["u", "r1[r1 r2 r3]"]);
    // Followed by text, the run is over, though the turn goes on.
    expect(laid([...items, says("a")], WORKING)).toEqual(["u", "r1[r1 r2 r3]", "a"]);
  });

  it("leave out thinking that streams while the turn works, until it ends", () => {
    expect(laid([user("u"), read("r1"), read("r2"), thought("t", true)], WORKING)).toEqual(["u", "r1", "r2"]);
    expect(laid([user("u"), read("r1"), read("r2"), thought("t")], WORKING)).toEqual(["u", "r1[r1 r2 t]*"]);
    // A turn that ended mid-thought shows what there is of it.
    expect(laid([user("u"), read("r1"), read("r2"), thought("t", true)], IDLE)).toEqual(["u", "r1[r1 r2 t]"]);
    // Thinking with no text has nothing to show at all.
    expect(laid([user("u"), thought("s", false, "")])).toEqual(["u"]);
  });

  it("are known by their first item, drawn or not, so the row keeps its key as the run grows", () => {
    const first = thought("t", true);
    expect(laid([user("u"), first, read("r1"), read("r2"), read("r3")], WORKING)).toEqual(["u", "t[r1 r2 r3]*"]);
    expect(laid([user("u"), { ...first, streaming: false }, read("r1"), read("r2"), read("r3"), read("r4")], WORKING)).toEqual(["u", "t[t r1 r2 r3 r4]*"]);
  });

  it("keep a subagent that still runs: the phone draws it in its run, where the desktop moves it out", () => {
    const items = [user("u"), read("r1"), subagent("g", { status: ChatToolStatus.RUNNING }), read("r2")];
    expect(laid(items, WORKING)).toEqual(["u", "r1[r1 g r2]*"]);
    expect(laid(items.slice(0, 3), WORKING)).toEqual(["u", "r1", "g"]);
  });

  it("keep the same row while none of their steps changes", () => {
    const steps = [read("r1"), read("r2"), read("r3", ChatToolStatus.RUNNING)];
    const pane = paneFrom([snapshotEvent(5n, [user("u"), ...steps, says("a"), text("a9", ChatItemKind.ASSISTANT, "On", { streaming: true })])]);
    const before = timelineRows(pane.items, pane.outgoing, WORKING);
    const grown = applyChatEvent(pane, deltaEvent(6n, "a9", " it.")).pane;
    const after = timelineRows(grown.items, grown.outgoing, WORKING);
    expect(after[2]).toMatchObject({ kind: "steps", key: "r1" });
    expect(after[2]).toBe(before[2]);
    // A step that changed makes it a new row with the same key; so does the turn ending under a live one.
    const finished = applyChatEvent(grown, updatedEvent(7n, read("r3"))).pane;
    const changed = timelineRows(finished.items, finished.outgoing, WORKING);
    expect(changed[2]).not.toBe(after[2]);
    expect(changed[2]).toMatchObject({ kind: "steps", key: "r1", live: false });
    const live = timelineRows(steps, [], WORKING);
    expect(timelineRows(steps, [], WORKING)[0]).toBe(live[0]);
    expect(timelineRows(steps, [], IDLE)[0]).not.toBe(live[0]);
  });
});

describe("what the end of the conversation looks like", () => {
  const base = [...chatItems(), text("a9", ChatItemKind.ASSISTANT, "On", { streaming: true })];
  const mark = (items = base, outgoing: Parameters<typeof timelineRows>[1] = []) => latestMark(timelineRows(items, outgoing, IDLE));

  it("changes when a row arrives, the newest text grows, or the newest tool finishes", () => {
    expect(mark([])).toBe("");
    expect(mark([...base, text("n", ChatItemKind.NOTICE, "Interrupted.")])).not.toBe(mark());
    expect(mark([...base.slice(0, -1), { ...base.at(-1)!, text: "On it." }])).not.toBe(mark());
    expect(mark(base, [{ id: "m1", text: "more", acked: false }])).not.toBe(mark());
    const running = [...base, tool("t", { status: ChatToolStatus.RUNNING })];
    expect(mark([...base, tool("t", { status: ChatToolStatus.OK })])).not.toBe(mark(running));
  });

  it("changes when a group at the end takes a step, or its newest step finishes", () => {
    const steps = [tool("t1", {}), tool("t2", {}), tool("t3", { status: ChatToolStatus.RUNNING })];
    const group = mark([...base, ...steps]);
    expect(mark([...base, ...steps, tool("t4", {})])).not.toBe(group);
    expect(mark([...base, ...steps.slice(0, 2), tool("t3", { status: ChatToolStatus.OK })])).not.toBe(group);
    expect(mark([text("q0", ChatItemKind.USER, "Earlier question"), ...base, ...steps])).toBe(group);
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
