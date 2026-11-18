// Steps, subagents and background tasks on the phone
// (docs/design/mobile-chat-mock.html, S2 and S4), each rendered from scripted
// chat events. With MUXFLOW_RENDER_DIR set, the states are also drawn as HTML
// pages at phone width.

import { act, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ChatItemKind, ChatTaskKind, ChatToolKind, ChatToolStatus, ChatTurnState, type ChatItem } from "../../../protocol/gen/chat_pb";
import { textOf, writeRender } from "../../../testing/renderTree";
import { allByLabel, byLabel, isHost, press, render, rerender } from "../../../testing/render";
import type { ChatPane } from "../chatModel";
import { backgroundTask, FILE, paneFrom, snapshotEvent, state, stepItems, subagent, text, tool } from "../testing";
import { ChatView } from "./ChatView";

// The replies' Copy buttons reach the clipboard, which is native.
vi.mock("expo-clipboard", () => ({ setStringAsync: vi.fn(async () => true) }));

type Calls = { stopTask: string[]; refuse?: boolean };

function chat(pane: ChatPane, calls: Calls = { stopTask: [] }, connected = true) {
  return (
    <ChatView
      busy={false}
      commands={[]}
      connected={connected}
      detailsOpen={false}
      draft=""
      editing={undefined}
      folder="~/dev/muxflow"
      hostLabel="devbox"
      olderFailed={false}
      onAnswer={() => undefined}
      onBack={() => undefined}
      onCancelEdit={() => undefined}
      onDetails={() => undefined}
      onDraft={() => undefined}
      onEdit={() => undefined}
      onLoadOlder={() => undefined}
      onPickCommand={() => undefined}
      onRetry={() => undefined}
      onRetryOlder={() => undefined}
      onSend={() => undefined}
      onSetOptions={() => undefined}
      onStop={() => undefined}
      onStopTask={(taskId) => {
        calls.stopTask.push(taskId);
        return calls.refuse ? Promise.reject(new Error("refused")) : Promise.resolve();
      }}
      onVoiceMode={() => undefined}
      pane={pane}
      stopping={false}
      title="Wrapped links"
      voiceMode={false}
    />
  );
}

const user = (id: string, body = "Find out why wrapped paths are missed.") => text(id, ChatItemKind.USER, body);
const says = (id: string, body = "A path that breaks right after a slash is dropped.") => text(id, ChatItemKind.ASSISTANT, body);
const idle = (items: ChatItem[], overrides: Parameters<typeof state>[0] = {}) => paneFrom([snapshotEvent(5n, items, { state: state(overrides) })]);
const working = (items: ChatItem[], overrides: Parameters<typeof state>[0] = {}) => idle(items, { turn: ChatTurnState.WORKING, ...overrides });

function listOf(view: ReactTestRenderer): ReactTestInstance {
  return view.root.find((node) => isHost(node, "FlatList"));
}

function within(node: ReactTestInstance, container: ReactTestInstance): boolean {
  for (let at: ReactTestInstance | null = node; at; at = at.parent) if (at === container) return true;
  return false;
}

/** The style a host element is drawn with, flattened. */
function styleOf(node: ReactTestInstance): Record<string, unknown> {
  const style = node.props.style as unknown;
  return Array.isArray(style) ? Object.assign({}, ...style.flat(4).filter(Boolean)) : ((style ?? {}) as Record<string, unknown>);
}

/** Whether a text element is a run inside another one. */
function insideText(node: ReactTestInstance): boolean {
  for (let at = node.parent; at; at = at.parent) if (isHost(at, "Text")) return true;
  return false;
}

/** What a text element reads, its runs joined. */
function said(node: ReactTestInstance): string {
  return node.children.map((child) => (typeof child === "string" ? child : said(child))).join("");
}

/** The list cell `node` is drawn in: the one view that reports its height. */
function cellOf(node: ReactTestInstance): ReactTestInstance {
  for (let at: ReactTestInstance | null = node; at; at = at.parent) {
    if (isHost(at, "View") && typeof at.props.onLayout === "function") return at;
  }
  throw new Error("not in a cell that reports its height");
}

/** The cell is laid out `height` tall, and the list has taken what that made the conversation send it. */
function layOut(cell: ReactTestInstance, height: number): void {
  act(() => {
    (cell.props.onLayout as (event: unknown) => void)({ nativeEvent: { layout: { x: 0, y: 0, width: 336, height } } });
  });
  act(() => {
    vi.advanceTimersByTime(0);
  });
}

/** The unseen copy of a row's text that says whether one line holds it reports `lines` lines. */
function probe(row: ReactTestInstance, lines: number): void {
  const probes = row.findAll((node) => isHost(node, "Text") && typeof node.props.onTextLayout === "function");
  expect(probes).toHaveLength(1);
  act(() => {
    (probes[0]!.props.onTextLayout as (event: unknown) => void)({ nativeEvent: { lines: Array.from({ length: lines }, () => ({})) } });
  });
}

const settle = () => act(async () => {});

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-01-01T00:10:00Z"));
});
afterEach(() => {
  vi.useRealTimers();
});

describe("steps as one line (mobile-chat-mock.html, S2)", () => {
  it("A finished turn is one line saying what was done, and no rows until it is tapped", () => {
    const pane = idle([user("q"), ...stepItems(), says("a")]);
    const view = render(chat(pane));
    let shown = textOf(view.toJSON());
    expect(shown).toContain("4 steps · read 1 · ran 2 · edited 1 file · 1 failed +3 −1");
    expect(shown).not.toContain("cargo clippy");
    expect(allByLabel(view, /^(run|read|edit) /)).toHaveLength(0);
    const line = byLabel(view, "4 steps, 1 failed, read 1 · ran 2 · edited 1 file");
    expect(line.props.accessibilityRole).toBe("button");
    expect(line.props.accessibilityState).toEqual({ expanded: false });
    // One line: the counts give way before the title, the failure and the line counts do.
    const parts = line.findAll((node) => isHost(node, "Text")).filter((node) => !insideText(node));
    expect(parts.map((part) => [said(part), styleOf(part).flexShrink])).toEqual([
      ["4 steps", 0],
      ["· read 1 · ran 2 · edited 1 file", 1],
      ["· 1 failed", 0],
      ["+3 −1", 0],
    ]);
    expect(parts.every((part) => part.props.numberOfLines === 1)).toBe(true);
    writeRender("steps-folded", view.toJSON());

    press(line);
    shown = textOf(view.toJSON());
    expect(shown).toContain("read links.rs done run cargo test -p terminal-interactions links 14 passed edit links.rs +3 −1 run cargo clippy exit 101");
    expect(byLabel(view, "4 steps, 1 failed, read 1 · ran 2 · edited 1 file").props.accessibilityState).toEqual({ expanded: true });
    writeRender("steps-open", view.toJSON());

    // It stays open when the conversation goes on, and when it is drawn again.
    rerender(view, chat(idle([user("q"), ...stepItems(), says("a"), user("q2", "And the fixture?")])));
    expect(textOf(view.toJSON())).toContain("run cargo clippy exit 101");
    press(byLabel(view, "4 steps, 1 failed, read 1 · ran 2 · edited 1 file"));
    expect(textOf(view.toJSON())).not.toContain("cargo clippy");
  });

  it("Fewer than three steps stay plain rows, with no line over them", () => {
    const view = render(chat(idle([user("q"), ...stepItems().slice(0, 2), says("a")])));
    const shown = textOf(view.toJSON());
    expect(shown).toContain("read links.rs done run cargo test -p terminal-interactions links 14 passed");
    expect(shown).not.toContain("steps");
  });

  it("While the turn works the line counts the steps so far and shows the subagents that run and the newest step", () => {
    const steps = [
      tool("r1", { kind: ChatToolKind.READ, title: "read", detail: FILE, path: FILE }),
      subagent("g", { status: ChatToolStatus.RUNNING, activity: "reading wrap.rs" }),
      tool("s1", { kind: ChatToolKind.SEARCH, title: "search", detail: "hard_wrap", resultSummary: "6 matches" }),
      tool("c1", { title: "run", detail: "cargo test -p terminal-interactions links", status: ChatToolStatus.RUNNING }),
    ];
    const view = render(chat(working([user("q"), ...steps])));
    let shown = textOf(view.toJSON());
    // No kinds while it works: the status line says what runs. Both count the same steps.
    expect(shown).toContain("4 steps so far Explore Find the other callers running reading wrap.rs");
    expect(shown).toContain("run cargo test -p terminal-interactions links running");
    expect(shown).toContain("Running cargo test -p terminal-interactions links… 0s 4 steps so far");
    expect(shown).not.toContain("hard_wrap");
    expect(shown).not.toContain("links.rs done");
    expect(byLabel(view, "4 steps so far").props.accessibilityState).toEqual({ expanded: false });
    writeRender("steps-live", view.toJSON());

    // A step arrives: still the newest only.
    const more = [...steps.slice(0, 3), tool("c1", { title: "run", detail: "cargo test -p terminal-interactions links", resultSummary: "14 passed" }), tool("c2", { title: "run", detail: "cargo clippy", status: ChatToolStatus.RUNNING })];
    rerender(view, chat(working([user("q"), ...more])));
    shown = textOf(view.toJSON());
    expect(shown).toContain("5 steps so far Explore Find the other callers running reading wrap.rs · 0s run cargo clippy running");
    expect(shown).not.toContain("14 passed");

    // Opened, every step.
    press(byLabel(view, "5 steps so far"));
    shown = textOf(view.toJSON());
    expect(shown).toContain("read links.rs done");
    expect(shown).toContain("search hard_wrap 6 matches");
    expect(shown).toContain("14 passed");

    // The turn ends: the same line, still open, now says what was done.
    const done = more.map((item) => (item.tool?.status === ChatToolStatus.RUNNING ? { ...item, tool: { ...item.tool, status: ChatToolStatus.OK } } : item));
    rerender(view, chat(idle([user("q"), ...done, says("a")])));
    shown = textOf(view.toJSON());
    expect(shown).toContain("5 steps · read 1 · ran 2 · searched 1 · 1 subagent");
    expect(shown).toContain("search hard_wrap 6 matches");
    expect(byLabel(view, "5 steps, read 1 · ran 2 · searched 1 · 1 subagent").props.accessibilityState).toEqual({ expanded: true });
  });

  it("Thinking that streams is not a step until it ends", () => {
    const steps = stepItems().slice(0, 2);
    const view = render(chat(working([user("q"), ...steps, text("t", ChatItemKind.REASONING, "Boxed output is hard-wrapped", { streaming: true })])));
    expect(textOf(view.toJSON())).not.toContain("Boxed output");
    expect(textOf(view.toJSON())).toContain("Thinking… 0s 2 steps so far");
    rerender(view, chat(working([user("q"), ...steps, text("t", ChatItemKind.REASONING, "Boxed output is hard-wrapped by the agent.")])));
    expect(textOf(view.toJSON())).toContain("3 steps so far Boxed output is hard-wrapped by the agent.");
  });
});

describe("plain rows", () => {
  it("A row with nothing more to show is not a button, however long its command", () => {
    const command = "cargo test -p terminal-interactions links -- --nocapture";
    const view = render(chat(idle([user("q"), tool("c", { title: "run", detail: command, resultSummary: "denied" })])));
    const row = byLabel(view, `run ${command}, denied`);
    expect(row.props.accessibilityRole).toBe("text");
    expect(row.props.disabled).toBe(true);
    expect(row.props.accessibilityState).toBeUndefined();
    // The whole of it fits on its line: still nothing to open.
    probe(row, 1);
    expect(byLabel(view, `run ${command}, denied`).props.disabled).toBe(true);
    // The screen cut it short: it opens on the whole command, once.
    probe(row, 2);
    const cut = byLabel(view, `run ${command}, denied`);
    expect(cut.props.accessibilityRole).toBe("button");
    expect(cut.props.disabled).toBe(false);
    press(cut);
    const shown = textOf(view.toJSON());
    expect(shown.split(command)).toHaveLength(3);
    expect(byLabel(view, `run ${command}, denied`).props.accessibilityState).toEqual({ expanded: true });
  });

  it("A row opens on its output, on the path of a file it named by its name, and on the rest of a command", () => {
    const view = render(chat(idle([
      user("q"),
      tool("t", { title: "run", detail: "cargo test links", resultSummary: "ok", outputPreview: "test result: ok. 14 passed\n", outputTruncated: true }),
      says("a"),
      tool("r", { kind: ChatToolKind.READ, title: "read", detail: FILE, path: FILE }),
      says("b"),
      tool("h", { title: "run", detail: "cat <<EOF\nboxed\nEOF" }),
    ])));
    const output = byLabel(view, "run cargo test links, ok");
    expect(output.props.accessibilityRole).toBe("button");
    press(output);
    expect(textOf(view.toJSON())).toContain("test result: ok. 14 passed Output shortened.");
    // Its command was all there: it is not said again.
    expect(textOf(view.toJSON()).split("cargo test links")).toHaveLength(2);

    press(byLabel(view, "read links.rs, done"));
    expect(textOf(view.toJSON())).toContain(FILE);

    press(byLabel(view, "run cat <<EOF, done"));
    expect(view.root.findAll((node) => isHost(node, "Text") && node.children.join("") === "cat <<EOF\nboxed\nEOF")).toHaveLength(1);
  });

  it("Thinking is one dim line that opens on all of it", () => {
    const thought = "Boxed output is hard-wrapped by the agent.\n\nSo the soft-wrap join never sees it.";
    const view = render(chat(idle([user("q"), text("t", ChatItemKind.REASONING, thought), says("a"), text("t2", ChatItemKind.REASONING, "Short.")])));
    const row = byLabel(view, `Thinking: ${thought}`);
    const line = row.findAll((node) => isHost(node, "Text") && styleOf(node).opacity !== 0);
    expect(line).toHaveLength(1);
    expect(line[0]!.props.numberOfLines).toBe(1);
    expect(styleOf(line[0]!).fontStyle).toBe("italic");
    expect(line[0]!.children.join("")).toBe("Boxed output is hard-wrapped by the agent. So the soft-wrap join never sees it.");
    press(row);
    expect(view.root.findAll((node) => isHost(node, "Text") && node.children.join("") === thought)).toHaveLength(1);
    // One that fits on its line has nothing to open.
    expect(byLabel(view, "Thinking: Short.").props.disabled).toBe(true);
  });

  it("An assistant's text is drawn as Markdown, its code in a box that scrolls sideways", () => {
    const reply = "The join is in **links.rs**:\n\n```rust\nfn join(rows: &[Row]) -> String { rows.iter().map(Row::text).collect() }\n```\n";
    const view = render(chat(idle([user("q"), says("a", reply)])));
    const shown = textOf(view.toJSON());
    expect(shown).toContain("The join is in links.rs:");
    expect(shown).not.toContain("**");
    expect(shown).not.toContain("```");
    const copy = byLabel(view, "Copy code");
    expect(within(copy, listOf(view))).toBe(true);
    const scrollers = listOf(view).findAll((node) => isHost(node, "ScrollView") && node.props.horizontal === true);
    expect(scrollers).toHaveLength(1);
    expect(scrollers[0]!.props.scrollEnabled).not.toBe(false);
    // A reply still being written says so to its renderer: a fence not closed yet is not drawn as code text.
    rerender(view, chat(working([user("q"), text("a", ChatItemKind.ASSISTANT, "```rust\nfn join() {}\n``", { streaming: true })])));
    expect(textOf(view.toJSON())).not.toContain("``");
  });
});

describe("subagent rows (mobile-chat-mock.html, S4)", () => {
  const ago = (item: ChatItem, seconds: number): ChatItem => ({ ...item, atUnixMillis: BigInt(Date.now() - seconds * 1000) });

  it("A running subagent says who it is, what it does now, its figures and its time, once a second", () => {
    const running = ago(subagent("g", { status: ChatToolStatus.RUNNING, activity: "reading crates/terminal-interactions/src/wrap.rs", toolUses: 12, tokens: 48_000n }), 64);
    const view = render(chat(working([user("q"), running])));
    expect(textOf(view.toJSON())).toContain("Explore Find the other callers running reading crates/terminal-interactions/src/wrap.rs · 12 tools · 48k tokens · 1m 04s");
    byLabel(view, "Explore Find the other callers, running");
    act(() => {
      vi.advanceTimersByTime(2_000);
    });
    expect(textOf(view.toJSON())).toContain("12 tools · 48k tokens · 1m 06s");
    writeRender("subagent", view.toJSON());
  });

  it("A finished one keeps its figures; nothing says when it ended, so no time is shown", () => {
    const done = ago(subagent("g", { detail: "Check the fixtures", resultSummary: "3 files", toolUses: 9, tokens: 31_000n }), 400);
    const view = render(chat(idle([user("q"), done, says("a")])));
    expect(textOf(view.toJSON())).toContain("Explore Check the fixtures 3 files 9 tools · 31k tokens A path");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("One left in the background says so, and one that failed says that", () => {
    const view = render(chat(idle([
      user("q"),
      ago(subagent("g", { status: ChatToolStatus.RUNNING, background: true, activity: "searching join_rows", toolUses: 1, tokens: 2_300n }), 5),
      says("a"),
      subagent("f", { status: ChatToolStatus.FAILED }),
    ])));
    const shown = textOf(view.toJSON());
    expect(shown).toContain("Explore Find the other callers in background searching join_rows · 1 tool · 2.3k tokens · 5s");
    expect(shown).toContain("Explore Find the other callers failed");
    byLabel(view, "Explore Find the other callers, in background");
  });

  it("Codex names no type and reports only that it runs, which is said once", () => {
    const view = render(chat(working([user("q"), ago(subagent("x", { agentType: "", status: ChatToolStatus.RUNNING, detail: "Find where uploads are staged\nand list each caller" }), 48)])));
    const shown = textOf(view.toJSON());
    expect(shown).toContain("Agent Find where uploads are staged running Waiting on 1 subagent…");
    // What it was asked ran over one line: the row opens on all of it.
    press(byLabel(view, "Agent Find where uploads are staged, running"));
    expect(view.root.findAll((node) => isHost(node, "Text") && node.children.join("") === "Find where uploads are staged\nand list each caller")).toHaveLength(1);
  });
});

describe("background tasks (mobile-chat-mock.html, S4)", () => {
  const tasks = () => [
    backgroundTask("dev", "pnpm dev", { ranSeconds: 192 }),
    backgroundTask("agent", "Find the other callers", { kind: ChatTaskKind.SUBAGENT, ranSeconds: 64 }),
    backgroundTask("watch", "Watching the build", { kind: ChatTaskKind.OTHER, stoppable: false }),
  ];
  const chipOf = (view: ReactTestRenderer) => allByLabel(view, /background tasks?$/);
  /** Whether `node` sits in a row drawn dimmed. */
  const dimmed = (node: ReactTestInstance) => {
    for (let at: ReactTestInstance | null = node; at; at = at.parent) if (typeof at.type === "string" && styleOf(at).opacity === 0.6) return true;
    return false;
  };

  it("A chip above the reply field counts them, turn or no turn, and not while the chat cannot be used", () => {
    const calls: Calls = { stopTask: [] };
    const view = render(chat(idle([user("q"), says("a")]), calls));
    expect(chipOf(view)).toHaveLength(0);
    rerender(view, chat(idle([user("q"), says("a")], { background: tasks().slice(0, 1) }), calls));
    expect(textOf(view.toJSON())).toContain("1 background task Reply");
    expect(within(byLabel(view, "1 background task"), listOf(view))).toBe(false);
    // Under the status line while a turn works.
    rerender(view, chat(working([user("q")], { background: tasks() }), calls));
    expect(textOf(view.toJSON())).toContain("Working… 0s 3 background tasks Send a follow-up while it works");
    writeRender("background-chip", view.toJSON());
    rerender(view, chat(working([user("q")], { background: tasks() }), calls, false));
    expect(chipOf(view)).toHaveLength(0);
  });

  it("The chip opens a sheet: each task with what it is and how long it has run, and Stop where it can be stopped", () => {
    const calls: Calls = { stopTask: [] };
    const view = render(chat(idle([user("q"), says("a")], { background: tasks() }), calls));
    expect(textOf(view.toJSON())).not.toContain("Command ·");
    // Closed, nothing ticks.
    expect(vi.getTimerCount()).toBe(0);
    press(byLabel(view, "3 background tasks"));
    let shown = textOf(view.toJSON());
    expect(shown).toContain("Background tasks pnpm dev Command · 3m 12s Stop Find the other callers Subagent · 1m 04s Stop Watching the build Task");
    expect(shown).not.toContain("Task ·");
    expect(allByLabel(view, /^Stop /).map((node) => node.props.accessibilityLabel)).toEqual(["Stop pnpm dev", "Stop Find the other callers"]);
    writeRender("background-sheet", view.toJSON());
    act(() => {
      vi.advanceTimersByTime(3_000);
    });
    shown = textOf(view.toJSON());
    expect(shown).toContain("Command · 3m 15s");
    expect(shown).toContain("Subagent · 1m 07s");
  });

  it("Stop is sent once; the row dims and its button is off until the task leaves the list or 10 seconds pass", async () => {
    const calls: Calls = { stopTask: [] };
    const view = render(chat(idle([user("q"), says("a")], { background: tasks() }), calls));
    press(byLabel(view, "3 background tasks"));
    const stop = () => byLabel(view, "Stop pnpm dev");
    expect(dimmed(stop())).toBe(false);
    press(stop());
    await settle();
    expect(calls.stopTask).toEqual(["dev"]);
    expect(dimmed(stop())).toBe(true);
    expect(stop().props.disabled).toBe(true);
    expect(stop().props.accessibilityState).toEqual({ disabled: true });
    expect(dimmed(byLabel(view, "Stop Find the other callers"))).toBe(false);
    // A second press sends nothing.
    press(stop());
    await settle();
    expect(calls.stopTask).toEqual(["dev"]);
    // No word from the host: it can be pressed again after 10 seconds.
    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    expect(dimmed(stop())).toBe(false);
    expect(stop().props.disabled).toBe(false);
    press(stop());
    await settle();
    expect(calls.stopTask).toEqual(["dev", "dev"]);
    expect(dimmed(stop())).toBe(true);

    // The task leaves the list: its row goes, the others stay as they were.
    rerender(view, chat(idle([user("q"), says("a")], { background: tasks().slice(1) }), calls));
    expect(allByLabel(view, "Stop pnpm dev")).toHaveLength(0);
    expect(dimmed(byLabel(view, "Stop Find the other callers"))).toBe(false);
    // The last one ends: the sheet closes by itself, and the chip goes.
    rerender(view, chat(idle([user("q"), says("a")]), calls));
    expect(textOf(view.toJSON())).not.toContain("Background tasks");
    expect(chipOf(view)).toHaveLength(0);
    // A task that starts later does not reopen it.
    rerender(view, chat(idle([user("q"), says("a")], { background: tasks().slice(0, 1) }), calls));
    expect(textOf(view.toJSON())).not.toContain("Background tasks");
    byLabel(view, "1 background task");
  });

  it("A Stop the host refuses leaves the row as it was", async () => {
    const calls: Calls = { stopTask: [], refuse: true };
    const view = render(chat(idle([user("q"), says("a")], { background: tasks() }), calls));
    press(byLabel(view, "3 background tasks"));
    press(byLabel(view, "Stop pnpm dev"));
    await settle();
    expect(calls.stopTask).toEqual(["dev"]);
    expect(dimmed(byLabel(view, "Stop pnpm dev"))).toBe(false);
    expect(byLabel(view, "Stop pnpm dev").props.disabled).toBe(false);
  });
});

describe("opening a row where it is", () => {
  const latest = (view: ReactTestRenderer) => allByLabel(view, "Jump to latest");
  const follows = (view: ReactTestRenderer) => (listOf(view).props.maintainVisibleContentPosition as { minIndexForVisible: number }).minIndexForVisible === 0;
  const stepsLine = (view: ReactTestRenderer) => byLabel(view, "4 steps, 1 failed, read 1 · ran 2 · edited 1 file");

  it("The list scrolls by what the row grew, so the tapped line stays; past the end's reach the reader is away at once", () => {
    const view = render(chat(idle([user("q"), ...stepItems(), says("a")])));
    const cell = cellOf(stepsLine(view));
    layOut(cell, 42);
    expect(latest(view)).toHaveLength(0);
    press(stepsLine(view));
    // Nothing moves until the row has its new height.
    expect(latest(view)).toHaveLength(0);
    layOut(cell, 342);
    // The list is now 300 dp from its end: the reader put it there, so no wait, and nothing glides it back.
    expect(latest(view)).toHaveLength(1);
    expect(follows(view)).toBe(false);
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    expect(latest(view)).toHaveLength(1);

    // Closing it takes the same 300 dp back: at the end again.
    press(stepsLine(view));
    layOut(cell, 42);
    expect(latest(view)).toHaveLength(0);
    expect(follows(view)).toBe(true);
    expect(vi.getTimerCount()).toBeLessThanOrEqual(1);
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    expect(vi.getTimerCount()).toBe(0);
  });

  it("A row that opens a little keeps the reader at the end", () => {
    const view = render(chat(idle([user("q"), tool("t", { title: "run", detail: "cargo test links", resultSummary: "ok", outputPreview: "ok\n" }), says("a")])));
    const row = byLabel(view, "run cargo test links, ok");
    layOut(cellOf(row), 42);
    press(row);
    layOut(cellOf(row), 90);
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    expect(latest(view)).toHaveLength(0);
    expect(follows(view)).toBe(true);
  });

  it("A row that grows by itself moves nothing: only a tap is held", () => {
    const steps = stepItems();
    const view = render(chat(working([user("q"), ...steps])));
    const cell = cellOf(byLabel(view, "4 steps so far, 1 failed"));
    layOut(cell, 72);
    rerender(view, chat(working([user("q"), ...steps, subagent("g", { status: ChatToolStatus.RUNNING })])));
    layOut(cell, 372);
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    expect(latest(view)).toHaveLength(0);
    expect(follows(view)).toBe(true);
    // Nor does a height that comes long after a tap that changed nothing.
    press(byLabel(view, "5 steps so far, 1 failed"));
    act(() => {
      vi.advanceTimersByTime(2_000);
    });
    layOut(cell, 700);
    expect(latest(view)).toHaveLength(0);
  });
});
