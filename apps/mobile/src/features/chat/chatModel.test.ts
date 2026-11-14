import { describe, expect, it } from "vitest";

import { ChatCommandKind, ChatItemKind, ChatToolStatus, ChatTurnState } from "../../protocol/gen/chat_pb";
import {
  applyChatEvent,
  applyOlderPage,
  compactTokens,
  newChatPane,
  replyToSpeak,
  sideQuestion,
  slashMatches,
  usageFraction,
  usageLabel,
  withPickedCommand,
  type ChatPane,
} from "./chatModel";
import {
  catalog,
  chatItems,
  deltaEvent,
  itemEvent,
  paneFrom,
  PANE,
  questionTurnEvents,
  SERVER,
  snapshotEvent,
  state,
  stateEvent,
  text,
  tool,
  truncateEvent,
  updatedEvent,
} from "./testing";

const opened = (revision = 10n, items = chatItems(), olderCursor = "") => paneFrom([snapshotEvent(revision, items, { olderCursor })]);

describe("following a chat (ChatEvent rules)", () => {
  it("a snapshot replaces what the pane holds and sets the revision", () => {
    const pane = opened();
    expect(pane.status).toBe("open");
    expect(pane.revision).toBe(10n);
    expect(pane.items.map((item) => item.id)).toEqual(["q1", "think", "a1", "read", "test"]);
    const again = applyChatEvent(pane, snapshotEvent(40n, [text("only", ChatItemKind.USER, "hi")])).pane;
    expect(again.items.map((item) => item.id)).toEqual(["only"]);
    expect(again.revision).toBe(40n);
    expect(again.snapshotSeq).toBe(pane.snapshotSeq + 1);
  });

  it("ignores events for a pane it holds no snapshot of", () => {
    const fresh = newChatPane(PANE, SERVER);
    const outcome = applyChatEvent(fresh, itemEvent(1n, text("x", ChatItemKind.USER, "x")));
    expect(outcome.reopen).toBe(false);
    expect(outcome.pane).toBe(fresh);
  });

  it("applies only the next revision, ignores old ones, and reopens on a gap", () => {
    const pane = opened();
    const next = applyChatEvent(pane, itemEvent(11n, text("n", ChatItemKind.NOTICE, "Interrupted.")));
    expect(next.pane.items.at(-1)?.id).toBe("n");
    expect(next.pane.revision).toBe(11n);

    const stale = applyChatEvent(next.pane, itemEvent(11n, text("dup", ChatItemKind.NOTICE, "dup")));
    expect(stale.pane).toBe(next.pane);
    expect(stale.reopen).toBe(false);

    const gap = applyChatEvent(next.pane, itemEvent(13n, text("late", ChatItemKind.NOTICE, "late")));
    expect(gap.reopen).toBe(true);
    expect(gap.pane.resyncing).toBe(true);
    expect(gap.pane.items.some((item) => item.id === "late")).toBe(false);
    // While resyncing nothing applies, and no second OPEN is asked for.
    const after = applyChatEvent(gap.pane, itemEvent(14n, text("later", ChatItemKind.NOTICE, "later")));
    expect(after.reopen).toBe(false);
    expect(after.pane.items.some((item) => item.id === "later")).toBe(false);
    // The fresh snapshot ends it.
    expect(applyChatEvent(after.pane, snapshotEvent(14n, chatItems())).pane.resyncing).toBe(false);
  });

  it("streams deltas into an item and replaces it in place when updated", () => {
    const pane = paneFrom([snapshotEvent(0n, chatItems()), ...questionTurnEvents(0n)]);
    const reply = pane.items.find((item) => item.id === "turn-1-reply");
    expect(reply?.text).toBe("The detector joins rows the terminal marks as soft-wrapped. Before I change it, two things to settle.");
    const test = pane.items.find((item) => item.id === "turn-1-test");
    expect(test?.tool?.status).toBe(ChatToolStatus.OK);
    // The finished tool kept its place: before the reply.
    expect(pane.items.findIndex((item) => item.id === "turn-1-test")).toBeLessThan(pane.items.findIndex((item) => item.id === "turn-1-reply"));
    expect(pane.state?.turn).toBe(ChatTurnState.WAITING);
    expect(pane.state?.pending?.questions).toHaveLength(2);
  });

  it("records an update or delta for an item it does not hold, for the paging rule", () => {
    const pane = opened();
    const updated = applyChatEvent(pane, updatedEvent(11n, text("older-one", ChatItemKind.ASSISTANT, "x"))).pane;
    expect(updated.ignoredUnheldAt).toBe(11n);
    expect(updated.items).toHaveLength(pane.items.length);
    const delta = applyChatEvent(updated, deltaEvent(12n, "older-two", "y")).pane;
    expect(delta.ignoredUnheldAt).toBe(12n);
  });

  it("truncates after a held item, drops everything for an empty id, and reopens for one it does not hold", () => {
    const pane = opened(10n, chatItems(), "cursor-1");
    expect(applyChatEvent(pane, truncateEvent(11n, "think")).pane.items.map((item) => item.id)).toEqual(["q1", "think"]);
    const all = applyChatEvent(pane, truncateEvent(11n, "")).pane;
    expect(all.items).toEqual([]);
    expect(all.olderCursor).toBe("");
    const unknown = applyChatEvent(pane, truncateEvent(11n, "not-loaded"));
    expect(unknown.reopen).toBe(true);
  });

  it("drops a sent message's placeholder once the host shows the item", () => {
    const pane: ChatPane = { ...opened(), outgoing: [{ id: "m1", text: "hello", acked: true }] };
    expect(applyChatEvent(pane, itemEvent(11n, text("m1", ChatItemKind.USER, "hello"))).pane.outgoing).toEqual([]);
    // A snapshot is the truth about every acknowledged send.
    expect(applyChatEvent(pane, snapshotEvent(20n, chatItems())).pane.outgoing).toEqual([]);
    const unacked: ChatPane = { ...pane, outgoing: [{ id: "m2", text: "still going", acked: false }] };
    expect(applyChatEvent(unacked, snapshotEvent(20n, chatItems())).pane.outgoing).toHaveLength(1);
  });
});

describe("older pages (ChatResponse.revision rules)", () => {
  const older = [text("o1", ChatItemKind.USER, "Earlier question"), text("o2", ChatItemKind.ASSISTANT, "Earlier answer.")];

  it("prepends a page read at or before the pane's revision", () => {
    const pane = opened(10n, chatItems(), "c1");
    const outcome = applyOlderPage(pane, { snapshotSeq: pane.snapshotSeq, cursor: "c1", revision: 10n, older, olderCursor: "c0" });
    expect(outcome.kind).toBe("applied");
    expect(outcome.pane.items.slice(0, 2).map((item) => item.id)).toEqual(["o1", "o2"]);
    expect(outcome.pane.olderCursor).toBe("c0");
  });

  it("drops a page asked for before the current snapshot or for a cursor the pane no longer has", () => {
    const pane = opened(10n, chatItems(), "c1");
    expect(applyOlderPage(pane, { snapshotSeq: pane.snapshotSeq - 1, cursor: "c1", revision: 10n, older, olderCursor: "" }).kind).toBe("dropped");
    expect(applyOlderPage(pane, { snapshotSeq: pane.snapshotSeq, cursor: "c9", revision: 10n, older, olderCursor: "" }).kind).toBe("dropped");
  });

  it("holds a page until the pane catches up, then applies it with that event", () => {
    const pane = opened(10n, chatItems(), "c1");
    const held = applyOlderPage(pane, { snapshotSeq: pane.snapshotSeq, cursor: "c1", revision: 12n, older, olderCursor: "" });
    expect(held.kind).toBe("held");
    const first = applyChatEvent(held.pane, stateEvent(11n, state()));
    expect(first.page).toBeUndefined();
    const second = applyChatEvent(first.pane, stateEvent(12n, state()));
    expect(second.page?.revision).toBe(12n);
    expect(applyOlderPage(second.pane, second.page!).kind).toBe("applied");
  });

  it("asks again when a change to an item it did not hold was ignored after the page was read", () => {
    const pane = applyChatEvent(opened(10n, chatItems(), "c1"), updatedEvent(11n, text("o2", ChatItemKind.ASSISTANT, "Changed."))).pane;
    expect(applyOlderPage(pane, { snapshotSeq: pane.snapshotSeq, cursor: "c1", revision: 10n, older, olderCursor: "" }).kind).toBe("refetch");
    expect(applyOlderPage(pane, { snapshotSeq: pane.snapshotSeq, cursor: "c1", revision: 11n, older, olderCursor: "" }).kind).toBe("applied");
  });
});

describe("the slash list", () => {
  it("lists while the draft is a slash and a name, prefix matches first, grouped", () => {
    const groups = slashMatches(catalog(), "/c");
    expect(groups.map((group) => group.title)).toEqual(["Commands", "Skills"]);
    expect(groups[0]!.commands.map((command) => command.name)).toEqual(["clear", "compact"]);
    expect(groups[1]!.commands.map((command) => command.name)).toEqual(["code-review", "diagnose-crash"]);
    expect(slashMatches(catalog(), "/compact now")).toEqual([]);
    expect(slashMatches(catalog(), "hello")).toEqual([]);
    expect(slashMatches(catalog(), "/").flatMap((group) => group.commands)).toHaveLength(6);
  });

  it("lists the skills for a dollar in the last word, anywhere in the message", () => {
    const groups = slashMatches(catalog(), "look at this with $c");
    expect(groups.map((group) => [group.title, group.sigil])).toEqual([["Skills", "$"]]);
    expect(groups[0]!.commands.map((command) => command.name)).toEqual(["code-review", "diagnose-crash"]);
    expect(slashMatches(catalog(), "costs $5 a month")).toEqual([]);
    expect(slashMatches(catalog(), "a$c")).toEqual([]);
    const skill = groups[0]!.commands[0]!;
    expect(withPickedCommand("look at this with $c", skill)).toBe("look at this with $code-review ");
    expect(withPickedCommand("/c", skill)).toBe("/code-review ");
    expect(withPickedCommand("/review this with $c", skill)).toBe("/review this with $code-review ");
    // No skill is marked: the agent's own commands stand in.
    const unmarked = catalog();
    unmarked.commands = unmarked.commands.filter((command) => command.kind !== ChatCommandKind.SKILL);
    expect(slashMatches(unmarked, "$c").flatMap((group) => group.commands.map((command) => command.name))).toEqual(["clear", "compact"]);
  });

  it("offers Muxflow's side questions only to a provider that can fork", () => {
    expect(slashMatches(catalog(), "/b")[0]?.title).toBe("Muxflow");
    const noFork = catalog({ capabilities: { fork: false } });
    expect(slashMatches(noFork, "/b")).toEqual([]);
    expect(sideQuestion(catalog(), "/btw which file holds the join?")).toBe("which file holds the join?");
    expect(sideQuestion(catalog(), "/side  why? ")).toBe("why?");
    expect(sideQuestion(catalog(), "/btw")).toBeUndefined();
    expect(sideQuestion(catalog(), "/compact now")).toBeUndefined();
    expect(sideQuestion(noFork, "/btw why?")).toBeUndefined();
  });
});

describe("voice mode", () => {
  it("reads the newest reply once, when a turn ends", () => {
    const working = paneFrom([snapshotEvent(0n, chatItems(), { state: state({ turn: ChatTurnState.WORKING }) })]);
    const idle = { ...working, state: state({ turn: ChatTurnState.IDLE }) };
    expect(replyToSpeak(ChatTurnState.WORKING, idle, undefined)?.id).toBe("a1");
    expect(replyToSpeak(ChatTurnState.WORKING, idle, "a1")).toBeUndefined();
    expect(replyToSpeak(ChatTurnState.IDLE, idle, undefined)).toBeUndefined();
    expect(replyToSpeak(ChatTurnState.WORKING, working, undefined)).toBeUndefined();
    // A user message after the last reply: nothing new was said.
    const asked = { ...idle, items: [...idle.items, text("q2", ChatItemKind.USER, "and?")] };
    expect(replyToSpeak(ChatTurnState.WORKING, asked, undefined)).toBeUndefined();
    // Tools after the reply do not hide it.
    const tooled = { ...idle, items: [...idle.items, tool("t9", { detail: "ls" })] };
    expect(replyToSpeak(ChatTurnState.WAITING, tooled, undefined)?.id).toBe("a1");
  });
});

describe("context usage", () => {
  it("reads as the mock does", () => {
    const pane = opened();
    expect(usageLabel(pane)).toBe("209k / 1M");
    expect(usageFraction(pane)).toBeCloseTo(0.209);
    expect(compactTokens(1_500_000n)).toBe("1.5M");
    expect(compactTokens(950n)).toBe("950");
    const unknownWindow = { ...pane, state: state({ usage: { usedTokens: 12_000n, windowTokens: 0n } }) };
    expect(usageLabel(unknownWindow)).toBe("12k");
    expect(usageFraction(unknownWindow)).toBeUndefined();
  });
});
