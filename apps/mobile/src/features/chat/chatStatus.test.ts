import { describe, expect, it } from "vitest";

import { ChatItemKind, ChatToolKind, ChatToolStatus, ChatTurnState, type ChatItem } from "../../protocol/gen/chat_pb";
import { applyChatEvent, newChatPane } from "./chatModel";
import { chatStatus, doing, followTurn, newTurnClock, running } from "./chatStatus";
import { paneFrom, PANE, permissionRequest, SERVER, snapshotEvent, state, stateEvent, text, tool } from "./testing";

const user = (id: string, at = 0) => text(id, ChatItemKind.USER, "Do it.", { atUnixMillis: BigInt(at) });
const says = (id: string) => text(id, ChatItemKind.ASSISTANT, "Done.");
const thought = (id: string, streaming = false, body = "Boxed output is hard-wrapped.") => text(id, ChatItemKind.REASONING, body, { streaming });
const run = (id: string, status = ChatToolStatus.OK, detail = "cargo test") => tool(id, { title: "run", detail, status });
const agent = (id: string, status = ChatToolStatus.RUNNING) => tool(id, { kind: ChatToolKind.SUBAGENT, title: "agent", detail: "Find the callers", status });
const toolOf = (item: ChatItem) => item.tool!;

// The desktop's own cases (apps/desktop-gpui/src/chat/timeline.rs, tests).
describe("what the agent is doing", () => {
  it("works when nothing says more", () => {
    expect(doing([])).toBe("Working…");
    expect(doing([user("u")])).toBe("Working…");
    expect(doing([user("u"), run("t"), thought("r")])).toBe("Working…");
  });

  it("thinks while its newest thinking streams", () => {
    expect(doing([user("u"), run("t"), thought("r", true)])).toBe("Thinking…");
  });

  it("names the tool that runs, newest first", () => {
    expect(doing([user("u"), run("a", ChatToolStatus.RUNNING, "cargo build"), run("b", ChatToolStatus.RUNNING)])).toBe("Running cargo test…");
    expect(doing([user("u"), thought("r", true), run("b", ChatToolStatus.RUNNING)])).toBe("Running cargo test…");
  });

  it("looks only at the steps since the newest text", () => {
    expect(doing([run("t", ChatToolStatus.RUNNING), says("a")])).toBe("Working…");
    expect(doing([thought("r", true), user("u")])).toBe("Working…");
  });

  it("waits on its subagents when nothing else runs, wherever they started", () => {
    expect(doing([agent("s1")])).toBe("Waiting on 1 subagent…");
    expect(doing([agent("s1"), says("a"), agent("s2")])).toBe("Waiting on 2 subagents…");
    expect(doing([agent("s1"), agent("s2", ChatToolStatus.OK)])).toBe("Waiting on 1 subagent…");
    // A tool of its own that runs is said first.
    expect(doing([agent("s1"), run("t", ChatToolStatus.RUNNING)])).toBe("Running cargo test…");
  });

  it("says a running tool by its kind and what it acts on", () => {
    const of = (fields: Parameters<typeof tool>[1]) => running(toolOf(tool("t", { status: ChatToolStatus.RUNNING, detail: "", ...fields })));
    expect(of({ kind: ChatToolKind.COMMAND, detail: "cargo test" })).toBe("Running cargo test…");
    expect(of({ kind: ChatToolKind.READ, path: "src/links.rs" })).toBe("Reading src/links.rs…");
    expect(of({ kind: ChatToolKind.EDIT, detail: "links.rs", path: "src/links.rs" })).toBe("Editing links.rs…");
    expect(of({ kind: ChatToolKind.SEARCH, detail: "hard_wrap" })).toBe("Searching hard_wrap…");
    expect(of({ kind: ChatToolKind.WEB, title: "search", detail: "unicode width" })).toBe("Searching unicode width…");
    expect(of({ kind: ChatToolKind.WEB, title: "fetch", detail: "docs.rs/unicode-width" })).toBe("Fetching docs.rs/unicode-width…");
    expect(of({ kind: ChatToolKind.MCP, detail: "resolve-library-id" })).toBe("Calling resolve-library-id…");
    expect(of({ kind: ChatToolKind.COMMAND })).toBe("Running…");
    // A tool the chat has no verb for is said by what it does.
    expect(of({ kind: ChatToolKind.UNSPECIFIED, detail: "Update the task list" })).toBe("Update the task list…");
    expect(of({ kind: ChatToolKind.UNSPECIFIED })).toBe("Working…");
  });
});

describe("when the turn started", () => {
  const NOW = 1_800_000_000_000;
  const idle = () => paneFrom([snapshotEvent(5n, [user("u", NOW - 600_000)])]);
  const turnTo = (pane: ReturnType<typeof idle>, revision: bigint, turn: ChatTurnState, overrides: Parameters<typeof state>[0] = {}) =>
    applyChatEvent(pane, stateEvent(revision, state({ turn, ...overrides }))).pane;

  it("is when this phone saw it start", () => {
    let clock = followTurn(newTurnClock(), idle(), NOW);
    expect(clock.turnStarted).toBeUndefined();
    const working = turnTo(idle(), 6n, ChatTurnState.WORKING);
    clock = followTurn(clock, working, NOW + 1_000);
    expect(clock.turnStarted).toBe(NOW + 1_000);
    // Later changes keep it, and nothing new is made when nothing changed.
    expect(followTurn(clock, working, NOW + 9_000)).toBe(clock);
    // Waiting on the user is still the same turn.
    const waiting = turnTo(working, 7n, ChatTurnState.WAITING, { pending: permissionRequest() });
    expect(followTurn(clock, waiting, NOW + 20_000).turnStarted).toBe(NOW + 1_000);
    // It ends with the turn, and the next one starts afresh.
    const ended = turnTo(waiting, 8n, ChatTurnState.IDLE);
    clock = followTurn(clock, ended, NOW + 30_000);
    expect(clock.turnStarted).toBeUndefined();
    expect(followTurn(clock, turnTo(ended, 9n, ChatTurnState.WORKING), NOW + 40_000).turnStarted).toBe(NOW + 40_000);
  });

  it("after a snapshot is when the user's newest message was sent, never later than now", () => {
    const opened = (at: number) => paneFrom([snapshotEvent(5n, [user("old", NOW - 900_000), says("a"), user("u", at), run("t")], { state: state({ turn: ChatTurnState.WORKING }) })]);
    expect(followTurn(newTurnClock(), opened(NOW - 42_000), NOW).turnStarted).toBe(NOW - 42_000);
    // A host clock ahead of this one is not a time still to come.
    expect(followTurn(newTurnClock(), opened(NOW + 5_000), NOW).turnStarted).toBe(NOW);
    // The host gave no time: from now.
    expect(followTurn(newTurnClock(), opened(0), NOW).turnStarted).toBe(NOW);
  });

  it("is not moved back by a snapshot of a turn already being timed", () => {
    const working = turnTo(idle(), 6n, ChatTurnState.WORKING);
    const clock = followTurn(followTurn(newTurnClock(), idle(), NOW), working, NOW + 1_000);
    const again = applyChatEvent(working, snapshotEvent(20n, [user("u", NOW - 600_000)], { state: state({ turn: ChatTurnState.WORKING }) })).pane;
    expect(followTurn(clock, again, NOW + 5_000).turnStarted).toBe(NOW + 1_000);
    // A snapshot that shows a newer message is a newer turn.
    const newer = applyChatEvent(working, snapshotEvent(20n, [user("u2", NOW + 3_000)], { state: state({ turn: ChatTurnState.WORKING }) })).pane;
    expect(followTurn(clock, newer, NOW + 5_000).turnStarted).toBe(NOW + 3_000);
  });

  it("times compacting from when it was first seen", () => {
    let clock = followTurn(newTurnClock(), idle(), NOW);
    const compacting = turnTo(idle(), 6n, ChatTurnState.IDLE, { compacting: true });
    clock = followTurn(clock, compacting, NOW + 2_000);
    expect(clock).toMatchObject({ turnStarted: undefined, compactingSince: NOW + 2_000 });
    expect(followTurn(clock, compacting, NOW + 8_000).compactingSince).toBe(NOW + 2_000);
    expect(followTurn(clock, turnTo(compacting, 7n, ChatTurnState.IDLE), NOW + 9_000).compactingSince).toBeUndefined();
  });

  it("is nothing for a chat that is not open", () => {
    expect(followTurn(newTurnClock(), undefined, NOW)).toEqual(newTurnClock());
    expect(followTurn(newTurnClock(), newChatPane(PANE, SERVER), NOW)).toEqual(newTurnClock());
  });
});

describe("the status line", () => {
  const clock = { snapshotSeq: 1, turnStarted: 1_000, compactingSince: 5_000 };
  const pane = (items: ChatItem[], overrides: Parameters<typeof state>[0] = {}) => paneFrom([snapshotEvent(5n, items, { state: state({ turn: ChatTurnState.WORKING, ...overrides }) })]);

  it("shows while the turn works, with the turn's time and the steps so far", () => {
    expect(chatStatus(pane([user("u")]), clock, false)).toEqual({ label: "Working…", since: 1_000, steps: "" });
    expect(chatStatus(pane([user("u"), run("a"), run("b", ChatToolStatus.RUNNING)]), clock, false)).toEqual({ label: "Running cargo test…", since: 1_000, steps: "2 steps so far" });
    // Thinking that still streams is what the label says, and not a step yet.
    expect(chatStatus(pane([user("u"), run("a"), thought("r", true)]), clock, false)).toEqual({ label: "Thinking…", since: 1_000, steps: "1 step so far" });
    expect(chatStatus(pane([user("u"), run("a"), thought("r")]), clock, false)).toEqual({ label: "Working…", since: 1_000, steps: "2 steps so far" });
    expect(chatStatus(pane([user("u"), agent("s")]), clock, false)).toEqual({ label: "Waiting on 1 subagent…", since: 1_000, steps: "1 step so far" });
  });

  it("does not show while idle, nor while the turn waits on the user", () => {
    expect(chatStatus(undefined, clock, false)).toBeUndefined();
    expect(chatStatus(pane([user("u")], { turn: ChatTurnState.IDLE }), clock, false)).toBeUndefined();
    expect(chatStatus(pane([user("u")], { turn: ChatTurnState.WAITING, pending: permissionRequest() }), clock, false)).toBeUndefined();
    // Stop pressed just as the turn ended says nothing.
    expect(chatStatus(pane([user("u")], { turn: ChatTurnState.IDLE }), clock, true)).toBeUndefined();
  });

  it("says compacting whatever else the turn does, timed from when compacting began", () => {
    expect(chatStatus(pane([user("u"), run("a", ChatToolStatus.RUNNING)], { compacting: true }), clock, false)).toEqual({ label: "Compacting context…", since: 5_000, steps: "1 step so far" });
    // Outside a turn too.
    expect(chatStatus(pane([user("u"), run("a")], { turn: ChatTurnState.IDLE, compacting: true }), clock, false)).toEqual({ label: "Compacting context…", since: 5_000, steps: "" });
  });

  it("says Stopping alone, with no time, above everything else", () => {
    expect(chatStatus(pane([user("u"), run("a", ChatToolStatus.RUNNING)]), clock, true)).toEqual({ label: "Stopping…", since: undefined, steps: "" });
    expect(chatStatus(pane([user("u")], { compacting: true }), clock, true)).toEqual({ label: "Stopping…", since: undefined, steps: "" });
  });
});
