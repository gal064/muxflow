// @vitest-environment jsdom
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ReactElement } from "react";
import type { PanelExtensionProps, PanelHost, PanelSessionContext } from "../shell/panelExtensions";
import { aiConversations, conversationKey } from "./conversationStore";
import { AiPanel } from "./AiPanel";
import { registerPaneScreenReader } from "../terminal/screenReaderRegistry";
import { terminalCacheKey } from "../terminal/TerminalStateCache";
import type { AiChatMessage, AiEvent } from "./types";

const api = vi.hoisted(() => ({
  chat: vi.fn(),
  cancel: vi.fn(),
  sendInput: vi.fn(),
  automationsList: vi.fn(),
  automationsRunNow: vi.fn(),
  portsDetect: vi.fn(),
}));
vi.mock("./api", () => ({
  aiChat: (settings: unknown, messages: unknown, tools: unknown, onEvent: (event: AiEvent) => void) =>
    api.chat(settings, messages, tools, onEvent),
}));
vi.mock("../automations/api", () => ({
  automationsApi: { list: api.automationsList, runNow: api.automationsRunNow },
}));
vi.mock("../ports/api", async (original) => ({
  ...await original<typeof import("../ports/api")>(),
  portsApi: { detect: api.portsDetect },
}));
vi.mock("../terminal/api", () => ({ sendInput: api.sendInput }));

const devbox: PanelHost = {
  profileId: "devbox", label: "Devbox", clientId: "client-1", connected: true,
  serverIdentity: "tmux:a", hasPane: () => false,
};

const windowA = { id: "@1", sessionId: "$1", index: 0, name: "editor", active: true, layout: "even-horizontal" };
const windowB = { id: "@2", sessionId: "$1", index: 1, name: "build", active: false, layout: "even-horizontal" };
const paneA = {
  id: "%1", sessionId: "$1", windowId: "@1", index: 0, active: true,
  width: 80, height: 24, left: 0, top: 0, currentPath: "/w", currentCommand: "node",
};
const paneB = {
  id: "%2", sessionId: "$1", windowId: "@2", index: 0, active: true,
  width: 80, height: 24, left: 0, top: 0, currentPath: "/w", currentCommand: "vim",
};

const session = (overrides: Partial<PanelSessionContext> = {}): PanelSessionContext => ({
  hostLabel: "Devbox",
  transport: "ssh",
  connected: true,
  snapshot: {
    sessions: [{ id: "$1", name: "work", windowCount: 2, attachedClients: 1 }],
    windows: [windowA, windowB],
    panes: [paneA, paneB],
  },
  activeSessionId: "$1",
  activeWindowId: "@1",
  activePaneId: "%1",
  agents: [],
  openTabs: [],
  portForwards: [],
  assistant: { baseUrl: "http://127.0.0.1:11434/v1", model: "qwen", apiKey: "" },
  sshConnection: { mode: "ssh", profileId: "devbox", target: "devbox" },
  gitStatus: {
    repository: { id: "r", worktreeRoot: "/w", initial: false, detachedHead: false, headName: "main" },
    generation: "1", sourceGeneration: "1", authoritative: true,
    entries: [{
      path: "cG", displayPath: "src/a.ts", indexKind: "modified", worktreeKind: "none",
      indexStatus: "M", worktreeStatus: " ", conflicted: false, untracked: false,
      ignored: false, submodule: false, symlink: false, binary: false,
    }],
  },
  ...overrides,
});

function props(overrides: Partial<PanelExtensionProps> = {}): PanelExtensionProps {
  return {
    hosts: [devbox],
    activeProfileId: "devbox",
    onOpenPane: vi.fn(),
    onMessage: vi.fn(),
    session: session(),
    ...overrides,
  };
}

/** A call whose rounds the test finishes by hand: each aiChat gets one. */
function streamedCall() {
  const rounds: { finish?: () => void; emit?: (event: AiEvent) => void }[] = [];
  api.chat.mockImplementation((_settings, _messages, _tools, onEvent: (event: AiEvent) => void) => {
    let resolve: (() => void) | undefined;
    const done = new Promise<void>((res) => { resolve = res; });
    rounds.push({ emit: (event) => onEvent(event), finish: () => resolve?.() });
    return { done, cancel: api.cancel };
  });
  return {
    /** Emits into the newest round's stream. */
    emit: (event: AiEvent) => rounds[rounds.length - 1]?.emit?.(event),
    finish: async () => {
      await act(async () => {
        rounds[rounds.length - 1]?.finish?.();
        await new Promise((resolve) => setTimeout(resolve, 0));
      });
    },
  };
}

async function render(component: ReactElement): Promise<ReactTestRenderer> {
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(component); });
  mounted.push(renderer);
  return renderer;
}

async function ask(renderer: ReactTestRenderer, question: string) {
  const box = renderer.root.findByProps({ "aria-label": "Ask about this session" });
  await act(async () => box.props.onChange({ target: { value: question } }));
  const button = renderer.root.findAll((node) => node.type === "button" && node.props.children === "Ask").pop()!;
  // The click gathers the async context first; one macrotask settles the
  // whole chain of awaited host reads before the call is checked.
  await act(async () => {
    button.props.onClick();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function text(renderer: ReactTestRenderer): string {
  return JSON.stringify(renderer.toJSON());
}

/** Every rendered tree, so `afterEach` can unmount it: the store publishes
 * to mounted panels, and a post-test write lands outside `act`. */
const mounted: ReactTestRenderer[] = [];

beforeEach(() => {
  vi.clearAllMocks();
  aiConversations.clearAll();
  api.automationsList.mockResolvedValue({
    automations: [{
      definition: {
        id: "nightly", name: "Nightly", enabled: true, schedule: "0 7 * * *",
        cwd: "/w", agent: "claude", command: "", prompt: "", keepTabs: 3, maxRuntimeMinutes: 60,
      },
      definitionError: "", nextRun: "", recentRuns: [], commandLine: "claude",
    }],
    fileError: "", generation: "1", stateDir: "/s",
  });
  api.automationsRunNow.mockResolvedValue({
    automations: [{
      definition: {
        id: "nightly", name: "Nightly", enabled: true, schedule: "0 7 * * *",
        cwd: "/w", agent: "claude", command: "", prompt: "", keepTabs: 3, maxRuntimeMinutes: 60,
      },
      definitionError: "", nextRun: "", commandLine: "claude",
      recentRuns: [{
        number: 13, scheduledFor: "", startedAt: 0, finishedAt: 0, status: "launched",
        sessionId: "$2", windowId: "@9", paneId: "%9", serverIdentity: "tmux:a", lastMessage: "", error: "",
      }],
    }],
    fileError: "", generation: "2", stateDir: "/s",
  });
  api.portsDetect.mockResolvedValue([{ port: 8080, process: "python" }]);
  api.sendInput.mockResolvedValue(undefined);
});

afterEach(async () => {
  // Unmount first: the store publishes to mounted panels, and clearing it
  // under a live one would land a state update outside `act`.
  for (const renderer of mounted.splice(0)) renderer.unmount();
  aiConversations.clearAll();
});

describe("the AI panel", () => {
  it("says so when no endpoint is configured, and refuses to ask", async () => {
    const renderer = await render(<AiPanel {...props({
      session: session({ assistant: { baseUrl: "", model: "", apiKey: "" } }),
    })} />);
    expect(text(renderer)).toContain("No model is configured");
    expect(text(renderer)).toContain("Settings");
    const box = renderer.root.findByProps({ "aria-label": "Ask about this session" });
    expect(box.props.disabled).toBe(true);
  });

  it("asks with a snapshot of every tab, and fills the answer as it streams", async () => {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props()} />);
    expect(text(renderer)).toContain("Nothing asked yet");
    await ask(renderer, "what is going on?");

    expect(api.chat).toHaveBeenCalledTimes(1);
    const messages = api.chat.mock.calls[0][1] as AiChatMessage[];
    expect(messages[0]).toMatchObject({ role: "system" });
    expect(messages[0].content).toContain("## Session tab");
    expect(messages[0].content).toContain("## Git tab");
    expect(messages[0].content).toContain("M src/a.ts");
    expect(messages[0].content).toContain("## Ports tab");
    expect(messages[0].content).toContain("8080 (python)");
    expect(messages[0].content).toContain("Nightly");
    expect(messages[1]).toEqual({ role: "user", content: "what is going on?" });

    await act(async () => call.emit({ type: "delta", text: "Two things" }));
    expect(text(renderer)).toContain("Two things");
    expect(text(renderer)).toContain("Stop");
    await act(async () => call.emit({ type: "delta", text: " are running." }));
    expect(text(renderer)).toContain("Two things are running.");
    await call.finish();
    expect(text(renderer)).not.toContain("Stop");
  });

  it("leaves a turned-off tab out of the snapshot", async () => {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props()} />);
    // The source boxes are the panel's five checkboxes, in tab order.
    const checkboxes = renderer.root.findAll((node) => node.type === "input" && node.props.type === "checkbox");
    expect(checkboxes).toHaveLength(5);
    await act(async () => checkboxes[2].props.onChange({ target: { checked: false } })); // Git
    await ask(renderer, "anything?");
    const messages = api.chat.mock.calls[0][1] as AiChatMessage[];
    expect(messages[0].content).toContain("## Session tab");
    expect(messages[0].content).not.toContain("## Git tab");
    await call.finish();
    // Back on for the tests after this one: the checkbox memory is shared.
    await act(async () => checkboxes[2].props.onChange({ target: { checked: true } }));
  });

  it("shows the server's refusal and lets the next question try again", async () => {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props()} />);
    await ask(renderer, "hello?");
    await act(async () => call.emit({ type: "error", message: "the model server said 401: bad key" }));
    expect(text(renderer)).toContain("bad key");
    await call.finish();
    // The failure does not wedge the panel: a new question is askable.
    const box = renderer.root.findByProps({ "aria-label": "Ask about this session" });
    expect(box.props.disabled).toBe(false);
  });

  it("keeps the partial answer when the user stops, and survives a tab switch mid-answer", async () => {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props()} />);
    await ask(renderer, "long one?");
    await act(async () => call.emit({ type: "delta", text: "half an answer" }));
    // Stop is the panel's own button, and it must work after a real unmount
    // and remount — the means to cancel lives in the store, not here.
    await act(async () => renderer.update(<div />));
    await act(async () => renderer.update(<AiPanel {...props()} />));
    const stop = renderer.root.findByProps({ children: "Stop", type: "button", className: "primary" });
    await act(async () => stop.props.onClick());
    expect(api.cancel).toHaveBeenCalledTimes(1);
    expect(text(renderer)).toContain("half an answer");
    await call.finish();
  });

  it("asks the host for automations and listening ports only when those tabs are on", async () => {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props({
      session: session({ sshConnection: undefined }),
    })} />);
    await ask(renderer, "q");
    expect(api.automationsList).toHaveBeenCalledWith("client-1");
    // Port detection needs an SSH connection; without one it is skipped.
    expect(api.portsDetect).not.toHaveBeenCalled();
    await call.finish();
  });
});

describe("the conversation belongs to the tab", () => {
  it("switches threads with the strip's tab, and each starts empty", async () => {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props()} />);
    await ask(renderer, "in editor?");
    await act(async () => call.emit({ type: "delta", text: "node is running" }));
    await call.finish();

    const elsewhere = props({ session: session({ activeWindowId: "@2", activePaneId: "%2" }) });
    await act(async () => renderer.update(<AiPanel {...elsewhere} />));
    expect(text(renderer)).toContain("Nothing asked yet");
    expect(text(renderer)).not.toContain("node is running");

    await act(async () => renderer.update(<AiPanel {...props()} />));
    expect(text(renderer)).toContain("in editor?");
    expect(text(renderer)).toContain("node is running");
  });

  it("discards the thread when its window closes", async () => {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props()} />);
    await ask(renderer, "in editor?");
    await act(async () => call.emit({ type: "delta", text: "yes" }));
    await call.finish();
    expect(aiConversations.read(conversationKey("devbox", "@1")).turns).toHaveLength(2);

    // The host says @1 is gone: the prune effect must drop its thread.
    const withoutA = props({
      session: session({
        snapshot: {
          sessions: [{ id: "$1", name: "work", windowCount: 1, attachedClients: 1 }],
          windows: [windowB],
          panes: [paneB],
        },
        activeWindowId: "@2",
        activePaneId: "%2",
      }),
    });
    await act(async () => renderer.update(<AiPanel {...withoutA} />));
    expect(aiConversations.read(conversationKey("devbox", "@1")).turns).toHaveLength(0);
  });

  it("waits for a tab: with no window on screen there is nothing to talk to", async () => {
    const renderer = await render(<AiPanel {...props({
      session: session({ activeWindowId: undefined, activePaneId: undefined }),
    })} />);
    expect(text(renderer)).toContain("Open a tab");
    const box = renderer.root.findByProps({ "aria-label": "Ask about this session" });
    expect(box.props.disabled).toBe(true);
  });
});

describe("the assistant acts through approved tools", () => {
  const sendToPaneCall = { id: "c1", name: "send_to_pane", arguments: JSON.stringify({ text: "git status", press_enter: true }) };

  async function askAndRequestTool() {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props()} />);
    await ask(renderer, "check the status for me");
    await act(async () => call.emit({ type: "toolCall", ...sendToPaneCall }));
    await call.finish();
    return { call, renderer };
  }

  it("parks a typed command on an approval card showing the exact text", async () => {
    const { renderer } = await askAndRequestTool();
    expect(text(renderer)).toContain("Type this into the terminal?");
    expect(text(renderer)).toContain("git status");
    expect(text(renderer)).toContain("press Enter");
    // Nothing has been typed and no second round has started.
    expect(api.sendInput).not.toHaveBeenCalled();
    expect(api.chat).toHaveBeenCalledTimes(1);
  });

  it("types only after Allow, tells the model what happened, and shows the round two answer", async () => {
    const { call, renderer } = await askAndRequestTool();
    const allow = renderer.root.findByProps({ children: "Allow", type: "button" });
    await act(async () => {
      allow.props.onClick();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(api.sendInput).toHaveBeenCalledWith("client-1", "%1", "git status\r");

    // Round two carries the OpenAI echo: the assistant's call and its answer.
    const messages = api.chat.mock.calls[1][1] as AiChatMessage[];
    const echo = messages.find((message) => message.role === "assistant" && message.toolCalls?.length);
    expect(echo?.toolCalls?.[0]).toMatchObject({ id: "c1", name: "send_to_pane" });
    const answer = messages.find((message) => message.role === "tool");
    expect(answer).toMatchObject({ toolCallId: "c1", content: "Typed and submitted in pane #0." });

    await act(async () => call.emit({ type: "delta", text: "two files changed" }));
    await call.finish();
    expect(text(renderer)).toContain("two files changed");
    expect(text(renderer)).toContain("done");
    expect(text(renderer)).not.toContain("Allow");
  });

  it("types nothing after Deny, and says so to the model", async () => {
    const { call, renderer } = await askAndRequestTool();
    const deny = renderer.root.findByProps({ children: "Deny", type: "button" });
    await act(async () => {
      deny.props.onClick();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(api.sendInput).not.toHaveBeenCalled();
    const messages = api.chat.mock.calls[1][1] as AiChatMessage[];
    expect(messages.find((message) => message.role === "tool")).toMatchObject({
      toolCallId: "c1", content: "The user denied this.",
    });
    expect(text(renderer)).toContain("denied");
    await act(async () => call.emit({ type: "delta", text: "not typing then" }));
    await call.finish();
    expect(text(renderer)).toContain("not typing then");
  });

  it("runs read-only tools without asking, and starts automations only with approval", async () => {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props()} />);
    await ask(renderer, "what is in each pane?");
    await act(async () => call.emit({ type: "toolCall", id: "r1", name: "list_panes", arguments: "{}" }));
    await call.finish();
    // No approval card for a read; the model already has the pane list.
    expect(text(renderer)).not.toContain("Allow");
    const messages = api.chat.mock.calls[1][1] as AiChatMessage[];
    expect(messages.find((message) => message.role === "tool")?.content).toContain("#0 (active): /w — running node");

    await act(async () => call.emit({
      type: "toolCall", id: "a1", name: "run_automation", arguments: JSON.stringify({ automation_id: "nightly" }),
    }));
    await call.finish();
    expect(text(renderer)).toContain("Run this automation now?");
    expect(api.automationsRunNow).not.toHaveBeenCalled();
    const allow = renderer.root.findByProps({ children: "Allow", type: "button" });
    await act(async () => {
      allow.props.onClick();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(api.automationsRunNow).toHaveBeenCalledWith("client-1", "nightly");
    const second = api.chat.mock.calls[2][1] as AiChatMessage[];
    expect(second.filter((message) => message.role === "tool").at(-1)?.content).toContain("run #13");
    await act(async () => call.emit({ type: "delta", text: "kicked it off" }));
    await call.finish();
  });

  it("reads a pane's screen without asking, and hands the model what it says", async () => {
    const call = streamedCall();
    const unregister = registerPaneScreenReader(
      terminalCacheKey("devbox", "%1"),
      (rows) => `screen, ${rows} rows: build FAILED on line 12`,
    );
    try {
      const renderer = await render(<AiPanel {...props()} />);
      await ask(renderer, "why did the build fail?");
      await act(async () => call.emit({ type: "toolCall", id: "p1", name: "read_pane", arguments: "{}" }));
      await call.finish();
      // A read is not an approval tool, and the screen's words reached the model.
      expect(text(renderer)).not.toContain("Allow");
      const messages = api.chat.mock.calls[1][1] as AiChatMessage[];
      const answer = messages.find((message) => message.role === "tool");
      expect(answer?.content).toContain("Pane #0:");
      expect(answer?.content).toContain("build FAILED on line 12");
      await act(async () => call.emit({ type: "delta", text: "line 12 is the cause" }));
      await call.finish();
      expect(text(renderer)).toContain("line 12 is the cause");
      // The action chip: summary text, and the done state's own class.
      expect(text(renderer)).toContain("read the active pane's screen");
      expect(renderer.root.findAll((node) => node.props.className === "ai-action ai-action-done")).toHaveLength(1);
    } finally {
      unregister();
    }
  });

  it("says so when the model asks to read a pane nothing is showing", async () => {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props()} />);
    await ask(renderer, "read the other pane?");
    await act(async () => call.emit({
      type: "toolCall", id: "p2", name: "read_pane", arguments: JSON.stringify({ pane: 7 }),
    }));
    await call.finish();
    const messages = api.chat.mock.calls[1][1] as AiChatMessage[];
    expect(messages.find((message) => message.role === "tool")?.content).toContain("no such pane");
    await act(async () => call.emit({ type: "delta", text: "there is no other pane" }));
    await call.finish();
  });
});

describe("the round budget asks before it stops", () => {
  /** Spend `n` rounds, each ending in a read-only tool call. */
  async function spendRounds(call: ReturnType<typeof streamedCall>, n: number) {
    for (let round = 0; round < n; round++) {
      await act(async () => call.emit({
        type: "toolCall", id: `t${round}`, name: "list_panes", arguments: "{}",
      }));
      await call.finish();
    }
  }

  it("offers Continue when the budget is spent, and buys more rounds", async () => {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props()} />);
    await ask(renderer, "keep digging");
    await spendRounds(call, 6);
    // Six rounds spent: the seventh asks the user, not the model.
    expect(api.chat).toHaveBeenCalledTimes(6);
    expect(text(renderer)).toContain("This is taking a while");

    const cont = renderer.root.findByProps({ children: "Continue", type: "button" });
    await act(async () => {
      cont.props.onClick();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(api.chat).toHaveBeenCalledTimes(7);
    expect(text(renderer)).not.toContain("This is taking a while");
    await act(async () => call.emit({ type: "delta", text: "found it" }));
    await call.finish();
    expect(text(renderer)).toContain("found it");
  });

  it("ends the run from the card without another round", async () => {
    const call = streamedCall();
    const renderer = await render(<AiPanel {...props()} />);
    await ask(renderer, "keep digging");
    await spendRounds(call, 6);
    expect(text(renderer)).toContain("This is taking a while");
    // The card's Stop, not the footer's: the card renders before the footer.
    const stops = renderer.root.findAll((node) => node.type === "button" && node.props.children === "Stop");
    await act(async () => {
      stops[0].props.onClick();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(api.chat).toHaveBeenCalledTimes(6);
    expect(text(renderer)).not.toContain("This is taking a while");
    const box = renderer.root.findByProps({ "aria-label": "Ask about this session" });
    expect(box.props.disabled).toBe(false);
  });
});
