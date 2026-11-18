import { describe, expect, it } from "vitest";

import { ChatItemKind, ChatTaskKind, ChatToolKind, ChatToolStatus, ChatTurnState, type ChatItem } from "../../protocol/gen/chat_pb";
import {
  backgroundLabel,
  elapsed,
  figures,
  isDrawn,
  liveSteps,
  startedAt,
  stepsCounts,
  stepsLabel,
  stepsLine,
  stepsSoFar,
  subagentLine,
  subagentName,
  taskLine,
  tokens,
  toolRowText,
} from "./steps";
import { FILE, subagent, text, tool } from "./testing";

const WORKING = ChatTurnState.WORKING;
const user = (id: string) => text(id, ChatItemKind.USER, "Do it.");
const says = (id: string) => text(id, ChatItemKind.ASSISTANT, "Done.");
const thought = (id: string, streaming = false, body = "Boxed output is hard-wrapped.") => text(id, ChatItemKind.REASONING, body, { streaming });
const run = (id: string, status = ChatToolStatus.OK, detail = "cargo test") => tool(id, { title: "run", detail, status });
const read = (id: string) => tool(id, { kind: ChatToolKind.READ, title: "read", detail: "src/view.rs", path: "src/view.rs" });
const edit = (id: string, path: string, added: number, removed: number, status = ChatToolStatus.OK) => tool(id, { kind: ChatToolKind.EDIT, title: "edit", detail: path, path, added, removed, status });
const of = (kind: ChatToolKind, detail: string, status = ChatToolStatus.OK) => tool(detail, { kind, title: "", detail, status });
const toolOf = (item: ChatItem) => item.tool!;

describe("which steps are drawn", () => {
  it("leaves out thinking with no text, a tool item without its tool, and thinking that streams while the turn is in progress", () => {
    expect(isDrawn(thought("t"), WORKING)).toBe(true);
    expect(isDrawn(thought("t", false, " \n"), ChatTurnState.IDLE)).toBe(false);
    expect(isDrawn(thought("t", true), WORKING)).toBe(false);
    expect(isDrawn(thought("t", true), ChatTurnState.WAITING)).toBe(false);
    expect(isDrawn(thought("t", true), ChatTurnState.IDLE)).toBe(true);
    expect(isDrawn({ ...run("bare"), tool: undefined }, WORKING)).toBe(false);
    // A subagent that runs keeps its row on the phone.
    expect(isDrawn(subagent("g", { status: ChatToolStatus.RUNNING }), WORKING)).toBe(true);
    expect(isDrawn(text("odd", ChatItemKind.UNSPECIFIED, "?"), WORKING)).toBe(false);
  });
});

describe("the steps so far", () => {
  it("counts the drawn steps since the newest message, reply text or notice", () => {
    expect(stepsSoFar([], WORKING)).toBe(0);
    expect(stepsSoFar([user("u")], WORKING)).toBe(0);
    expect(stepsSoFar([user("u"), run("a"), thought("r"), run("b", ChatToolStatus.RUNNING)], WORKING)).toBe(3);
    expect(stepsSoFar([user("u"), run("a"), thought("blank", true, " ")], WORKING)).toBe(1);
    expect(stepsSoFar([user("u"), run("a"), says("a1"), run("b")], WORKING)).toBe(1);
    expect(stepsSoFar([run("a"), text("n", ChatItemKind.NOTICE, "Context compacted")], WORKING)).toBe(0);
    // Thinking that still streams is not a step yet; a subagent that runs is one.
    expect(stepsSoFar([user("u"), run("a"), thought("r", true)], WORKING)).toBe(1);
    expect(stepsSoFar([user("u"), run("a"), subagent("g", { status: ChatToolStatus.RUNNING })], WORKING)).toBe(2);
  });

  it("is worded as the desktop words it", () => {
    expect(stepsLabel(1, true)).toBe("1 step so far");
    expect(stepsLabel(7, true)).toBe("7 steps so far");
    expect(stepsLabel(1, false)).toBe("1 step");
    expect(stepsLabel(12, false)).toBe("12 steps");
  });
});

describe("elapsed time", () => {
  it("reads in seconds, then minutes, then hours", () => {
    expect(elapsed(400)).toBe("0s");
    expect(elapsed(12_000)).toBe("12s");
    expect(elapsed(59_999)).toBe("59s");
    expect(elapsed(64_000)).toBe("1m 04s");
    expect(elapsed(125_000)).toBe("2m 05s");
    expect(elapsed(3_720_000)).toBe("1h 02m");
    expect(elapsed(3_900_000)).toBe("1h 05m");
    expect(elapsed(-5)).toBe("0s");
  });

  it("starts from a time only when the host gave one", () => {
    expect(startedAt(0n)).toBeUndefined();
    expect(startedAt(1_767_225_600_000n)).toBe(1_767_225_600_000);
  });
});

// The desktop's own cases (apps/desktop-gpui/src/chat/timeline.rs, tests).
describe("a group's line", () => {
  it("counts by kind in the desktop's words", () => {
    const steps = [read("r1"), read("r2"), run("c1"), of(ChatToolKind.SEARCH, "fn render_row"), edit("e1", "src/chat/view.rs", 84, 9), edit("e2", "src/chat/pane.rs", 36, 5), edit("e3", "src/chat/view.rs", 0, 0)];
    expect(stepsCounts(steps)).toEqual({ counts: "read 2 · ran 1 · searched 1 · edited 2 files", failed: 0, added: 120, removed: 14 });
    // One of each: the singular.
    expect(stepsCounts([read("r1"), edit("e1", "a.rs", 1, 0)]).counts).toBe("read 1 · edited 1 file");
    // Edits that name no file are counted as edits.
    expect(stepsCounts([edit("e1", "", 1, 0), edit("e2", "", 2, 0)]).counts).toBe("edited 2");
    expect(stepsCounts([of(ChatToolKind.WEB, "lucide.dev"), of(ChatToolKind.MCP, "github.get_pull_request"), subagent("g"), of(ChatToolKind.OTHER, "Update the task list"), of(ChatToolKind.UNSPECIFIED, "?")]).counts)
      .toBe("fetched 1 · called 1 · 1 subagent · 2 other");
    expect(stepsCounts([read("r1"), read("r2"), of(ChatToolKind.SEARCH, "upload"), subagent("g")]).counts).toBe("read 2 · searched 1 · 1 subagent");
    expect(stepsCounts([subagent("g1"), subagent("g2")]).counts).toBe("2 subagents");
  });

  it("says thinking first and does not time it", () => {
    expect(stepsCounts([thought("t1"), read("r")]).counts).toBe("thought · read 1");
    expect(stepsCounts([thought("t1"), read("r"), thought("t2")]).counts).toBe("thought 2 times · read 1");
  });

  it("counts a failed step, and not the lines of a failed edit", () => {
    const steps = [read("r"), run("c", ChatToolStatus.FAILED), edit("e", "src/view.rs", 10, 2, ChatToolStatus.FAILED), edit("f", "src/pane.rs", 3, 1)];
    expect(stepsCounts(steps)).toEqual({ counts: "read 1 · ran 1 · edited 2 files", failed: 2, added: 3, removed: 1 });
  });

  it("reads as title, counts, failures and lines; while live, the count so far and no kinds", () => {
    const steps = [read("r"), run("c1"), edit("e", FILE, 3, 1), run("c2", ChatToolStatus.FAILED)];
    expect(stepsLine(steps, false)).toEqual({ title: "4 steps", counts: "read 1 · ran 2 · edited 1 file", failed: "1 failed", added: 3, removed: 1, label: "4 steps, 1 failed, read 1 · ran 2 · edited 1 file" });
    expect(stepsLine(steps, true)).toEqual({ title: "4 steps so far", counts: "", failed: "1 failed", added: 3, removed: 1, label: "4 steps so far, 1 failed" });
    expect(stepsLine([read("a"), read("b"), read("c")], false)).toMatchObject({ title: "3 steps", counts: "read 3", failed: "", label: "3 steps, read 3" });
  });

  it("shows, before a live group is opened, the subagents that run and the newest other step", () => {
    const running = (id: string) => subagent(id, { status: ChatToolStatus.RUNNING });
    const ids = (steps: ChatItem[]) => liveSteps(steps).map((item) => item.id);
    expect(ids([read("r1"), read("r2"), run("c", ChatToolStatus.RUNNING)])).toEqual(["c"]);
    expect(ids([read("r1"), running("g1"), read("r2"), running("g2")])).toEqual(["g1", "r2", "g2"]);
    // A subagent that ended is a step like any other.
    expect(ids([read("r1"), running("g1"), subagent("g2")])).toEqual(["g1", "g2"]);
    expect(ids([running("g1"), running("g2"), running("g3")])).toEqual(["g1", "g2", "g3"]);
    expect(ids([])).toEqual([]);
  });
});

describe("a tool's row", () => {
  const row = (fields: Parameters<typeof tool>[1]) => toolRowText(toolOf(tool("t", { title: "", ...fields })));

  it("says the provider's verb, or one by kind", () => {
    expect(row({ title: "todo", kind: ChatToolKind.OTHER }).verb).toBe("todo");
    const verbs = [ChatToolKind.COMMAND, ChatToolKind.READ, ChatToolKind.EDIT, ChatToolKind.SEARCH, ChatToolKind.WEB, ChatToolKind.MCP, ChatToolKind.OTHER, ChatToolKind.UNSPECIFIED].map((kind) => row({ kind }).verb);
    expect(verbs).toEqual(["run", "read", "edit", "search", "web", "mcp", "tool", "tool"]);
  });

  it("names a file by its name and keeps the whole path for when it opens", () => {
    expect(row({ kind: ChatToolKind.READ, detail: FILE, path: FILE })).toMatchObject({ subject: "links.rs", full: FILE });
    expect(row({ kind: ChatToolKind.EDIT, detail: "links.rs", path: "links.rs" })).toMatchObject({ subject: "links.rs", full: "links.rs" });
    // Anything else is what it acted on, by its first line.
    expect(row({ detail: "cargo test -p terminal-interactions links" })).toMatchObject({ subject: "cargo test -p terminal-interactions links", full: "cargo test -p terminal-interactions links" });
    expect(row({ detail: "cat <<EOF\none\nEOF\n" })).toMatchObject({ subject: "cat <<EOF", full: "cat <<EOF\none\nEOF" });
    expect(row({ kind: ChatToolKind.SEARCH, detail: "", path: "src" })).toMatchObject({ subject: "src", full: "src" });
  });

  it("says how it went: line counts for an edit, else the result, else running, failed or done", () => {
    expect(row({ kind: ChatToolKind.EDIT, path: FILE, added: 3, removed: 1 })).toMatchObject({ outcome: "+3 −1", tone: "ok", lines: { added: 3, removed: 1 } });
    expect(row({ kind: ChatToolKind.EDIT, path: FILE, added: 3, removed: 1, status: ChatToolStatus.RUNNING })).toMatchObject({ lines: { added: 3, removed: 1 } });
    expect(row({ kind: ChatToolKind.EDIT, path: FILE, added: 3, removed: 1, status: ChatToolStatus.FAILED })).toEqual({ verb: "edit", subject: "links.rs", full: FILE, outcome: "failed", tone: "failed" });
    expect(row({ kind: ChatToolKind.EDIT, path: FILE })).toMatchObject({ outcome: "done", tone: "ok" });
    expect(row({ resultSummary: "14 passed" })).toMatchObject({ outcome: "14 passed", tone: "ok" });
    expect(row({})).toMatchObject({ outcome: "done", tone: "ok" });
    expect(row({ status: ChatToolStatus.RUNNING })).toMatchObject({ outcome: "running", tone: "working" });
    expect(row({ status: ChatToolStatus.RUNNING, resultSummary: "12 of 40" })).toMatchObject({ outcome: "12 of 40", tone: "working" });
    expect(row({ status: ChatToolStatus.FAILED })).toMatchObject({ outcome: "failed", tone: "failed" });
    expect(row({ status: ChatToolStatus.FAILED, resultSummary: "exit 101" })).toMatchObject({ outcome: "exit 101", tone: "failed" });
  });

  it("names a subagent by its type and the first line of what it was asked", () => {
    const explore = toolOf(subagent("g", { detail: "Find where uploads are staged\nand list each caller", status: ChatToolStatus.RUNNING }));
    expect(toolRowText(explore)).toEqual({ verb: "Explore", subject: "Find where uploads are staged", full: "Find where uploads are staged\nand list each caller", outcome: "running", tone: "working" });
    expect(toolRowText(toolOf(subagent("g", { status: ChatToolStatus.RUNNING, background: true })))).toMatchObject({ outcome: "in background", tone: "working" });
    expect(toolRowText(toolOf(subagent("g", { resultSummary: "3 files", background: true })))).toMatchObject({ outcome: "3 files", tone: "ok" });
    expect(toolRowText(toolOf(subagent("g", { status: ChatToolStatus.FAILED })))).toMatchObject({ outcome: "failed", tone: "failed" });
    expect(toolRowText(toolOf(subagent("g", { agentType: "" })))).toMatchObject({ verb: "Agent", outcome: "done" });
  });
});

describe("a subagent's line", () => {
  it("reads as the desktop draws it", () => {
    const explore = toolOf(subagent("g", { detail: "Find where uploads are staged", status: ChatToolStatus.RUNNING, activity: "Reading terminal_upload.rs", toolUses: 7, tokens: 24_100n }));
    expect(subagentName(explore)).toEqual(["Explore", "Find where uploads are staged"]);
    expect(subagentLine(explore, 41_000)).toBe("Reading terminal_upload.rs · 7 tools · 24.1k tokens · 41s");
    expect(subagentLine(explore, undefined)).toBe("Reading terminal_upload.rs · 7 tools · 24.1k tokens");
    expect(figures(explore)).toBe("7 tools · 24.1k tokens");
    expect(figures(toolOf(subagent("g", { toolUses: 1 })))).toBe("1 tool");
    // Before its first figures: only the time.
    const fresh = toolOf(subagent("f", { agentType: "Plan", status: ChatToolStatus.RUNNING }));
    expect(subagentLine(fresh, 3_000)).toBe("3s");
    expect(figures(fresh)).toBe("");
    // Codex: "Agent", the first line of the prompt, and its state, which is the whole line.
    const codex = toolOf(subagent("x", { agentType: "", detail: "Find where uploads are staged\nand list each caller", status: ChatToolStatus.RUNNING, activity: "running", toolUses: 3 }));
    expect(subagentName(codex)).toEqual(["Agent", "Find where uploads are staged"]);
    expect(subagentLine(codex, 48_000)).toBe("running");
    expect(subagentLine(toolOf(subagent("x", { agentType: "" })), 48_000)).toBe("running");
  });

  it("says tokens as 950, 19.7k, 61k, 182k, 1.3M", () => {
    expect(tokens(0)).toBe("0");
    expect(tokens(950)).toBe("950");
    expect(tokens(1_000)).toBe("1k");
    expect(tokens(19_714)).toBe("19.7k");
    expect(tokens(24_100)).toBe("24.1k");
    expect(tokens(61_000)).toBe("61k");
    expect(tokens(99_949)).toBe("99.9k");
    expect(tokens(99_950)).toBe("100k");
    expect(tokens(182_000)).toBe("182k");
    expect(tokens(999_499)).toBe("999k");
    expect(tokens(999_500)).toBe("1M");
    expect(tokens(1_250_000)).toBe("1.3M");
  });
});

describe("background tasks", () => {
  it("are counted on the chip in the desktop's words", () => {
    expect(backgroundLabel(1)).toBe("1 background task");
    expect(backgroundLabel(2)).toBe("2 background tasks");
  });

  it("say what each is and how long it has run, the time left out when the host gave no start", () => {
    expect(taskLine(ChatTaskKind.COMMAND, 192_000)).toBe("Command · 3m 12s");
    expect(taskLine(ChatTaskKind.SUBAGENT, 64_000)).toBe("Subagent · 1m 04s");
    expect(taskLine(ChatTaskKind.OTHER, 5_000)).toBe("Task · 5s");
    expect(taskLine(ChatTaskKind.UNSPECIFIED, undefined)).toBe("Task");
    expect(taskLine(ChatTaskKind.COMMAND, undefined)).toBe("Command");
  });
});
