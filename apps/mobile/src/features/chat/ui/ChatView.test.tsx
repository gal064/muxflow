// The phone chat mocks (docs/design/chat-mock.html, "Phone" and "Chat
// pieces"), each rendered from scripted chat events. With MUXFLOW_RENDER_DIR
// set, every state is also drawn as an HTML page at phone width.

import { useState } from "react";
import { FlatList, Keyboard } from "react-native";
import { flatListScrolls } from "../../../testing/stubs/reactNative";
import { act, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ChatConnection, ChatItemKind, ChatToolKind, ChatToolStatus, ChatTurnState, type ChatItem } from "../../../protocol/gen/chat_pb";
import { Operation } from "../../../protocol/gen/envelope_pb";
import { textOf, writeRender } from "../../../testing/renderTree";
import { allByLabel, byLabel, byText, isHost, longPress, press, render, rerender, type } from "../../../testing/render";
import { VoiceController } from "../../voice/VoiceController";
import { VoiceRecorderCoordinator } from "../../voice/recorderCoordinator";
import { FakeConnection, FakeFiles, FakePlayer, FakeRecorder, speechResponse } from "../../voice/testing";
import { voiceStore } from "../../voice/voiceStore";
import { newChatPane, slashMatches, withPickedCommand, type ChatPane } from "../chatModel";
import {
  chatItems,
  deltaEvent,
  dictation,
  itemEvent,
  paneFrom,
  PANE,
  permissionRequest,
  questionRequest,
  questionTurnEvents,
  SERVER,
  snapshotEvent,
  state,
  stateEvent,
  text,
  tool,
} from "../testing";
import { ChatView, type ChatViewProps } from "./ChatView";
import { ChatReplyRow, ChatVoicePane } from "./ChatVoiceParts";
import type { PendingAnswer } from "./PendingCard";

// The replies' Copy buttons reach the clipboard, which is native.
vi.mock("expo-clipboard", () => ({ setStringAsync: vi.fn(async () => true) }));

type Calls = { send: string[]; answers: PendingAnswer[]; stop: number; options: unknown[]; edits: string[]; voice: boolean[]; older: number; retryOlder: number; details: boolean[] };

/** ChatView with the screen's own state (draft, edit, details) and every action recorded. */
function Harness({ pane, calls, connected = true, voiceMode = false, voice, busy = false, stopping = false, olderFailed = false }: { pane: ChatPane | undefined; calls: Calls; connected?: boolean; voiceMode?: boolean; voice?: ChatViewProps["voice"]; busy?: boolean; stopping?: boolean; olderFailed?: boolean }) {
  const [draft, setDraft] = useState("");
  const [editing, setEditing] = useState<ChatItem | undefined>(undefined);
  const [detailsOpen, setDetailsOpen] = useState(false);
  return (
    <ChatView
      busy={busy}
      commands={slashMatches(pane?.catalog, draft)}
      connected={connected}
      detailsOpen={detailsOpen}
      draft={draft}
      editing={editing}
      folder="~/dev/muxflow"
      hostLabel="devbox"
      olderFailed={olderFailed}
      onAnswer={(answer) => calls.answers.push(answer)}
      onBack={() => undefined}
      onCancelEdit={() => { setEditing(undefined); setDraft(""); }}
      onDetails={(open) => { calls.details.push(open); setDetailsOpen(open); }}
      onDraft={setDraft}
      onEdit={(item) => { calls.edits.push(item.id); setEditing(item); setDraft(item.text); }}
      onLoadOlder={() => { calls.older += 1; }}
      onPickCommand={(command) => setDraft((current) => withPickedCommand(current, command))}
      onRetry={() => undefined}
      onRetryOlder={() => { calls.retryOlder += 1; }}
      onSend={() => { calls.send.push(draft); setDraft(""); }}
      onSetOptions={(options) => calls.options.push(options)}
      onStop={() => { calls.stop += 1; }}
      onStopTask={() => undefined}
      onVoiceMode={(on) => calls.voice.push(on)}
      pane={pane}
      stopping={stopping}
      title="Wrapped link bug"
      voice={voice}
      voiceMode={voiceMode}
    />
  );
}

const newCalls = (): Calls => ({ send: [], answers: [], stop: 0, options: [], edits: [], voice: [], older: 0, retryOlder: 0, details: [] });

/** The conversation's list. */
function listOf(view: ReactTestRenderer): ReactTestInstance {
  return view.root.find((node) => isHost(node, "FlatList"));
}

/** The list reports itself `offset` dp from the newest message, with no finger on it. */
function reportOffset(view: ReactTestRenderer, offset: number): void {
  act(() => {
    (listOf(view).props.onScroll as (event: unknown) => void)({ nativeEvent: { contentOffset: { x: 0, y: offset } } });
  });
}

/** The reader drags the list to `offset` dp from the newest message and lets go. */
function scrollTo(view: ReactTestRenderer, offset: number): void {
  act(() => {
    (listOf(view).props.onScrollBeginDrag as () => void)();
  });
  reportOffset(view, offset);
  act(() => {
    (listOf(view).props.onScrollEndDrag as (event: unknown) => void)({ nativeEvent: { contentOffset: { x: 0, y: offset }, velocity: { x: 0, y: 0 } } });
  });
}

/** The top of the conversation came within reach. */
function reachTop(view: ReactTestRenderer): void {
  act(() => {
    (listOf(view).props.onEndReached as (info: { distanceFromEnd: number }) => void)({ distanceFromEnd: 0 });
  });
}

function focus(node: ReactTestInstance): void {
  act(() => {
    (node.props.onFocus as (() => void) | undefined)?.();
  });
}

/** Whether `node` is drawn inside `container`. */
function within(node: ReactTestInstance, container: ReactTestInstance): boolean {
  for (let at: ReactTestInstance | null = node; at; at = at.parent) if (at === container) return true;
  return false;
}

/** Whether `node` sits inside something that is not displayed. */
function hidden(node: ReactTestInstance): boolean {
  for (let at: ReactTestInstance | null = node; at; at = at.parent) {
    const style = at.props.style as unknown;
    const flat = Array.isArray(style) ? Object.assign({}, ...style.filter(Boolean)) : style;
    if (flat && typeof flat === "object" && (flat as { display?: string }).display === "none") return true;
  }
  return false;
}

// The status line's clock is a timer: it runs only when a test moves the time.
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-01-01T00:10:00Z"));
});
afterEach(() => {
  vi.useRealTimers();
});

describe("phone chat mocks", () => {
  it("Chat: your message, the agent's reply, a tool row, and the composer", () => {
    const calls = newCalls();
    const view = render(<Harness calls={calls} pane={paneFrom([snapshotEvent(5n, chatItems())])} />);
    const shown = textOf(view.toJSON());
    expect(shown).toContain("Wrapped link bug");
    expect(shown).toContain("Find out why wrapped paths are missed.");
    expect(shown).toContain("The detector joins rows that the terminal marks as soft-wrapped.");
    expect(shown).toContain("run cargo test links ok");
    expect(shown).toContain("Reply");
    // Voice mode is the reply field's mic, not a switch in the header.
    expect(allByLabel(view, "Voice mode: read replies aloud")).toHaveLength(0);
    byLabel(view, "Details");
    byLabel(view, "Talk");
    expect(byLabel(view, "Send").props.disabled).toBe(true);
    expect(allByLabel(view, "Stop")).toHaveLength(0);
    // A tool row opens on its output.
    const row = byLabel(view, "run cargo test links, ok");
    press(row);
    expect(textOf(view.toJSON())).toContain("test result: ok. 14 passed; 0 failed");
    // Typing and sending.
    type(byLabel(view, "Message"), "Add the fixture and run the tests.");
    expect(byLabel(view, "Send").props.disabled).toBe(false);
    press(byLabel(view, "Send"));
    expect(calls.send).toEqual(["Add the fixture and run the tests."]);
    writeRender("chat", view.toJSON());
  });

  it("Permission request: the Allow / Deny card", () => {
    const calls = newCalls();
    const pane = paneFrom([
      snapshotEvent(5n, [text("a2", ChatItemKind.ASSISTANT, "I can add that case and a fixture for it.")]),
      stateEvent(6n, state({ turn: ChatTurnState.WAITING, pending: permissionRequest() })),
    ]);
    const view = render(<Harness calls={calls} pane={pane} />);
    const shown = textOf(view.toJSON());
    expect(shown).toContain("Edit a file?");
    expect(shown).toContain("links.rs");
    byLabel(view, "Edit a file?: links.rs, waiting for you");
    press(byText(view, "Allow"));
    press(byText(view, "Deny"));
    expect(calls.answers).toEqual([{ kind: "permission", allow: true }, { kind: "permission", allow: false }]);
    writeRender("permission", view.toJSON());
  });

  it("Question from the agent: one option per row, Other with a field, 1 of 2, then Submit", () => {
    const calls = newCalls();
    const pane = paneFrom([snapshotEvent(0n, chatItems()), ...questionTurnEvents(0n)]);
    const view = render(<Harness calls={calls} pane={pane} />);
    let shown = textOf(view.toJSON());
    expect(shown).toContain("How should the wrapped-path fix be scoped?");
    expect(shown).toContain("1 of 2");
    expect(shown).toContain("Boxed output only");
    expect(shown).toContain("Strip box borders before joining rows. Smallest change.");
    expect(shown).toContain("Other");
    writeRender("question", view.toJSON());
    expect(byText(view, "Next").props.disabled).toBe(true);
    press(byLabel(view, /^Other\./));
    type(byLabel(view, "Your answer"), "Only the boxed case, plus a fixture");
    writeRender("question-other", view.toJSON());
    // The keyboard is put away before the Other field goes, so its focus does not pass to the reply field and fold the card.
    const dismiss = vi.spyOn(Keyboard, "dismiss");
    press(byText(view, "Next"));
    expect(dismiss).toHaveBeenCalledTimes(1);
    dismiss.mockRestore();
    shown = textOf(view.toJSON());
    expect(shown).toContain("Which checks should run afterwards?");
    expect(shown).toContain("2 of 2");
    expect(shown).toContain("Choose any that apply.");
    expect(byLabel(view, /^Unit tests\./).props.accessibilityRole).toBe("checkbox");
    press(byLabel(view, /^Unit tests\./));
    press(byLabel(view, /^Clippy\./));
    press(byText(view, "Submit"));
    expect(calls.answers).toEqual([{
      kind: "questions",
      answers: [
        { questionId: "scope", optionIds: [], otherText: "Only the boxed case, plus a fixture" },
        { questionId: "checks", optionIds: ["tests", "clippy"], otherText: "" },
      ],
    }]);
    press(byText(view, "Skip"));
    expect(calls.answers.at(-1)).toEqual({ kind: "skip" });
  });

  it("Working, and file edits: Stop replaces Send, a running row, edit counts", () => {
    const calls = newCalls();
    const pane = paneFrom([
      snapshotEvent(5n, [
        text("q", ChatItemKind.USER, "Add the fixture."),
        tool("edit", { kind: 3, title: "edit", detail: "crates/terminal-interactions/src/links.rs", added: 3, removed: 1 }),
        tool("run", { title: "run", detail: "cargo test -p terminal-interactions links", status: ChatToolStatus.RUNNING }),
      ], { state: state({ turn: ChatTurnState.WORKING }) }),
    ]);
    const view = render(<Harness calls={calls} pane={pane} />);
    const shown = textOf(view.toJSON());
    expect(shown).toContain("+3 −1");
    expect(shown).toContain("running");
    expect(shown).toContain("Running cargo test -p terminal-interactions links…");
    expect(shown).toContain("Send a follow-up while it works");
    press(byLabel(view, "Stop"));
    expect(calls.stop).toBe(1);
    // Mid-turn: Send comes back once there is text, beside Stop.
    expect(allByLabel(view, "Send")).toHaveLength(0);
    type(byLabel(view, "Message"), "Also check the plain case.");
    byLabel(view, "Send");
    byLabel(view, "Stop");
    writeRender("working", view.toJSON());
  });

  it("Working with a provider that cannot take a message mid-turn: Stop only", () => {
    const pane = paneFrom([snapshotEvent(5n, chatItems(), { state: state({ turn: ChatTurnState.WORKING }) })]);
    const noMidTurn: ChatPane = { ...pane, catalog: { ...pane.catalog!, capabilities: { ...pane.catalog!.capabilities!, sendMidTurn: false } } };
    const view = render(<Harness calls={newCalls()} pane={noMidTurn} />);
    type(byLabel(view, "Message"), "more");
    expect(allByLabel(view, "Send")).toHaveLength(0);
    expect(textOf(view.toJSON())).not.toContain("Send a follow-up");
    // A side question goes to a fork while the main turn works.
    type(byLabel(view, "Message"), "/btw is the fixture name right?");
    expect(byLabel(view, "Send").props.disabled).toBe(false);
  });

  it("A request card waits: Send is held until it is answered when the provider takes no message mid-turn", () => {
    const pane = paneFrom([
      snapshotEvent(5n, chatItems()),
      stateEvent(6n, state({ turn: ChatTurnState.WAITING, pending: permissionRequest() })),
    ]);
    const noMidTurn: ChatPane = { ...pane, catalog: { ...pane.catalog!, capabilities: { ...pane.catalog!.capabilities!, sendMidTurn: false } } };
    const view = render(<Harness calls={newCalls()} pane={noMidTurn} />);
    expect(textOf(view.toJSON())).toContain("Answer the card above");
    type(byLabel(view, "Message"), "go ahead");
    expect(byLabel(view, "Send").props.disabled).toBe(true);
    // A side question goes to a fork, so the card does not hold it.
    type(byLabel(view, "Message"), "/btw is the fixture name right?");
    expect(byLabel(view, "Send").props.disabled).toBe(false);
    // A provider that takes messages mid-turn takes one now.
    const open = render(<Harness calls={newCalls()} pane={pane} />);
    type(byLabel(open, "Message"), "go ahead");
    expect(byLabel(open, "Send").props.disabled).toBe(false);
  });

  it("Slash commands and skills: the list, grouped, and a pick fills the composer", () => {
    const view = render(<Harness calls={newCalls()} pane={paneFrom([snapshotEvent(5n, chatItems())])} />);
    type(byLabel(view, "Message"), "/co");
    const shown = textOf(view.toJSON());
    expect(shown).toContain("Commands");
    expect(shown).toContain("/compact");
    expect(shown).toContain("Skills");
    expect(shown).toContain("/code-review");
    expect(shown).toContain("[level] [target]");
    writeRender("slash", view.toJSON());
    press(byLabel(view, "/compact, Summarise the conversation to free context"));
    expect(byLabel(view, "Message").props.value).toBe("/compact ");
    expect(allByLabel(view, /^\/code-review/)).toHaveLength(0);
  });

  it("Edit an earlier message: hold it, the banner says what sending does, Cancel restores", () => {
    const calls = newCalls();
    const view = render(<Harness calls={calls} pane={paneFrom([snapshotEvent(5n, chatItems())])} />);
    const bubble = view.root.findAll((node) => isHost(node, "Pressable") && node.props.accessibilityHint === "Hold for message actions");
    expect(bubble).toHaveLength(1);
    // A hold opens the message's sheet, where Edit and resend is.
    longPress(bubble[0]!);
    press(byText(view, "Edit and resend"));
    expect(calls.edits).toEqual(["q1"]);
    expect(textOf(view.toJSON())).toContain("Editing an earlier message. Sending removes everything after it. Files on disk stay as they are.");
    expect(byLabel(view, "Message").props.value).toBe("Find out why wrapped paths are missed.");
    writeRender("edit", view.toJSON());
    press(byText(view, "Cancel"));
    expect(textOf(view.toJSON())).not.toContain("Editing an earlier message");
  });

  it("A provider without rewind offers no edit", () => {
    const pane = paneFrom([snapshotEvent(5n, chatItems())]);
    const noRewind: ChatPane = { ...pane, catalog: { ...pane.catalog!, capabilities: { ...pane.catalog!.capabilities!, rewind: false } } };
    const view = render(<Harness calls={newCalls()} pane={noRewind} />);
    longPress(view.root.find((node) => isHost(node, "Pressable") && node.props.accessibilityHint === "Hold for message actions"));
    byText(view, "Copy message");
    expect(textOf(view.toJSON())).not.toContain("Edit and resend");
  });

  it("Details: host, folder, model, effort, permissions, speed and context", () => {
    const calls = newCalls();
    const view = render(<Harness calls={calls} pane={paneFrom([snapshotEvent(5n, chatItems())])} />);
    press(byLabel(view, "Details"));
    const shown = textOf(view.toJSON());
    for (const words of ["devbox", "~/dev/muxflow", "Opus 5.5", "Fable 5.2", "Sonnet 5.5", "Low", "Xhigh", "Max", "Auto", "Full", "Standard", "Fast", "209k / 1M", "Changes apply from the next message."]) {
      expect(shown).toContain(words);
    }
    expect(byLabel(view, "Model: Opus 5.5").props.accessibilityState).toMatchObject({ checked: true });
    expect(byLabel(view, "Effort: High").props.accessibilityState).toMatchObject({ checked: true });
    writeRender("details", view.toJSON());
    press(byLabel(view, "Effort: Max"));
    press(byLabel(view, "Permissions: Full"));
    press(byLabel(view, "Speed: Fast"));
    press(byLabel(view, "Model: Fable 5.2"));
    expect(calls.options).toEqual([{ effort: "max" }, { permission: 2 }, { speed: 2 }, { model: "fable-5-2" }]);
  });

  it("Details: an unlisted model allows only the permission mode; a model without fast hides Speed", () => {
    const pane = paneFrom([snapshotEvent(5n, chatItems(), { state: state({ options: { model: "fable-5-2", effort: "high", permission: 1, speed: 1 } }) })]);
    const view = render(<Harness calls={newCalls()} pane={pane} />);
    press(byLabel(view, "Details"));
    expect(allByLabel(view, /^Speed:/)).toHaveLength(0);
    const unlisted = paneFrom([snapshotEvent(5n, chatItems(), { state: state({ options: { model: "custom-model", effort: "", permission: 1, speed: 1 } }) })]);
    rerender(view, <Harness calls={newCalls()} pane={unlisted} />);
    expect(textOf(view.toJSON())).toContain("custom-model is set by the agent and isn't one it lists.");
    expect(allByLabel(view, /^Effort:/)).toHaveLength(0);
    byLabel(view, "Permissions: Auto");
  });

  it("Banners: opening, not connected, refused with Retry, pane closed, agent exited or failed, earlier messages", () => {
    const calls = newCalls();
    const view = render(<Harness calls={calls} pane={newChatPane(PANE, SERVER)} />);
    expect(textOf(view.toJSON())).toContain("Opening the chat…");
    rerender(view, <Harness calls={calls} connected={false} pane={paneFrom([snapshotEvent(5n, chatItems())])} />);
    expect(textOf(view.toJSON())).toContain("Not connected.");
    expect(byLabel(view, "Message").props.editable).toBe(false);
    rerender(view, <Harness calls={calls} pane={{ ...newChatPane(PANE, SERVER), status: "error", error: "This pane has no chat." }} />);
    expect(textOf(view.toJSON())).toContain("This pane has no chat. Retry");
    rerender(view, <Harness calls={calls} pane={{ ...paneFrom([snapshotEvent(5n, chatItems())]), status: "gone" }} />);
    expect(textOf(view.toJSON())).toContain("This chat's pane has closed.");
    rerender(view, <Harness calls={calls} pane={paneFrom([snapshotEvent(5n, chatItems(), { state: state({ connection: ChatConnection.ENDED }) })])} />);
    expect(textOf(view.toJSON())).toContain("The agent has exited.");
    rerender(view, <Harness calls={calls} pane={paneFrom([snapshotEvent(5n, chatItems(), { state: state({ connection: ChatConnection.FAILED, failure: "claude exited with status 1" }) })])} />);
    expect(textOf(view.toJSON())).toContain("claude exited with status 1");
    writeRender("failed", view.toJSON());
    rerender(view, <Harness calls={calls} pane={paneFrom([snapshotEvent(5n, chatItems(), { state: state({ historyComplete: false }) })])} />);
    expect(textOf(view.toJSON())).toContain("Earlier messages are not available.");
  });

  it("A message being sent shows at once, dimmed, until the host shows it back", () => {
    const pane = paneFrom([snapshotEvent(5n, chatItems())]);
    const view = render(<Harness calls={newCalls()} pane={{ ...pane, outgoing: [{ id: "m1", text: "On its way", acked: false }] }} />);
    byLabel(view, "Your message, sending");
    expect(textOf(view.toJSON())).toContain("On its way");
    const echoed = paneFrom([snapshotEvent(5n, chatItems()), itemEvent(6n, text("m1", ChatItemKind.USER, "On its way"))]);
    rerender(view, <Harness calls={newCalls()} pane={echoed} />);
    expect(allByLabel(view, "Your message, sending")).toHaveLength(0);
  });

  it("Voice mode, inside the chat: the talk pane replaces the composer and the reply plays", async () => {
    vi.useFakeTimers();
    try {
      const connection = new FakeConnection();
      connection.answer(Operation.VOICE_SPEAK, () => speechResponse(new TextEncoder().encode("mp3"), "Added a boxed-output fixture. All fourteen link tests pass."));
      const sent: string[] = [];
      const player = new FakePlayer();
      const controller = new VoiceController({
        serverIdentity: SERVER,
        sessionKey: "chat-voice-test",
        paneId: PANE,
        sessionId: "$1",
        store: voiceStore,
        getConnection: () => connection.asHostConnection(),
        recorder: new FakeRecorder(),
        recorderCoordinator: new VoiceRecorderCoordinator(),
        player,
        files: new FakeFiles(),
        appInForeground: () => true,
        submit: async (spoken) => { sent.push(spoken); },
        registerWithHost: false,
        tailHoldMs: 0,
      });
      controller.focus();
      await vi.advanceTimersByTimeAsync(0);
      expect(connection.of(Operation.VOICE_SESSION)).toHaveLength(0);
      const reply = "Added a boxed-output fixture. All fourteen link tests pass.";
      await controller.speakReply(reply, 1_767_225_660_000);
      const message = voiceStore.getState().sessions["chat-voice-test"]!.messages.at(-1)!;
      expect(message.fileUri).toBeDefined();
      // Auto-played: replies are read aloud in voice mode.
      expect(player.calls).toContain("play");

      const pane = paneFrom([snapshotEvent(5n, [text("q", ChatItemKind.USER, "Add the fixture and run the tests."), text("a", ChatItemKind.ASSISTANT, reply)])]);
      const calls = newCalls();
      const stops: string[] = [];
      const voice = {
        status: null,
        reply: <ChatReplyRow controller={controller} message={message} />,
        pane: <ChatVoicePane controller={controller} hint="" onKeyboard={() => stops.push("keyboard")} onStop={() => stops.push("stop")} phase="idle" stopping={false} working />,
        ready: true,
        dictation: dictation(),
      };
      const view = render(<Harness calls={calls} pane={pane} voice={voice} voiceMode />);
      const shown = textOf(view.toJSON());
      // The Voice screen's hold surface, with its Speed and Autoplay above it.
      expect(shown).toContain("Hold anywhere here to talk");
      byLabel(view, /^Playback speed /);
      expect(byLabel(view, "Autoplay replies").props.accessibilityRole).toBe("switch");
      // The reply's player is the conversation's last row, without a speed of its own.
      const last = listOf(view).children.at(-1);
      const row = byLabel(view, "Reply read aloud");
      expect(typeof last === "object" && within(row, last)).toBe(true);
      expect(row.findAll((node) => typeof node.props.accessibilityLabel === "string" && node.props.accessibilityLabel.startsWith("Playback speed"))).toHaveLength(0);
      expect(allByLabel(view, "Message")).toHaveLength(0);
      byLabel(view, /^Pause reply|^Play reply/);
      writeRender("voice", view.toJSON());
      press(byLabel(view, "Stop the agent"));
      press(byLabel(view, "Keyboard"));
      expect(stops).toEqual(["stop", "keyboard"]);
      controller.dispose();
    } finally {
      vi.useRealTimers();
    }
  });

  it("The composer's mic turns voice mode on", () => {
    const calls = newCalls();
    const view = render(<Harness calls={calls} pane={paneFrom([snapshotEvent(5n, chatItems())])} />);
    press(byLabel(view, "Talk"));
    expect(calls.voice).toEqual([true]);
  });
});

describe("the conversation as one anchored list (mobile-chat-mock.html, S1)", () => {
  it("is an inverted list, newest row first, that holds the reader's place", () => {
    const pane = paneFrom([snapshotEvent(5n, chatItems())]);
    const view = render(<Harness calls={newCalls()} pane={{ ...pane, outgoing: [{ id: "m1", text: "On its way", acked: false }] }} />);
    const list = listOf(view);
    expect(list.props.inverted).toBe(true);
    expect((view.root.findByType(FlatList).props.data as { key: string }[]).map((row) => row.key)).toEqual(["m1", "test", "read", "a1", "think", "q1"]);
    // At the end the newest row is the anchor, and a row that arrives is followed.
    expect(list.props.maintainVisibleContentPosition).toMatchObject({ minIndexForVisible: 0 });
    expect((list.props.maintainVisibleContentPosition as { autoscrollToTopThreshold: number }).autoscrollToTopThreshold).toBeGreaterThan(0);
    // Away from it the row after the newest is, so the newest row's text grows without moving what is read.
    scrollTo(view, 600);
    expect(listOf(view).props.maintainVisibleContentPosition).toMatchObject({ minIndexForVisible: 1 });
    // On screen the conversation still reads oldest to newest.
    const shown = textOf(view.toJSON());
    expect(shown.indexOf("Find out why wrapped paths are missed.")).toBeLessThan(shown.indexOf("run cargo test links ok"));
    expect(shown.indexOf("run cargo test links ok")).toBeLessThan(shown.indexOf("On its way"));
  });

  it("Latest: shows away from the end, gets its dot when something arrives, and goes back", () => {
    const events = [snapshotEvent(5n, chatItems(), { olderCursor: "c1" }), itemEvent(6n, text("a9", ChatItemKind.ASSISTANT, "On it.", { streaming: true }))];
    const calls = newCalls();
    const view = render(<Harness calls={calls} pane={paneFrom(events)} />);
    expect(allByLabel(view, "Jump to latest")).toHaveLength(0);
    // Within reach of the end is still the end.
    scrollTo(view, 60);
    expect(allByLabel(view, "Jump to latest")).toHaveLength(0);
    scrollTo(view, 400);
    expect(textOf(view.toJSON())).toContain("Latest");
    expect(byLabel(view, "Jump to latest").props.accessibilityHint).toBeUndefined();
    // Earlier messages loading above is not something new below.
    const older = { ...paneFrom(events), items: [text("q0", ChatItemKind.USER, "Earlier question"), ...paneFrom(events).items] };
    rerender(view, <Harness calls={calls} pane={older} />);
    expect(byLabel(view, "Jump to latest").props.accessibilityHint).toBeUndefined();
    // Text that grows at the end is.
    rerender(view, <Harness calls={calls} pane={paneFrom([...events, deltaEvent(7n, "a9", " Reading the detector.")])} />);
    expect(byLabel(view, "Jump to latest").props.accessibilityHint).toBe("New messages below");
    writeRender("latest", view.toJSON());
    // Going back clears it; the pill leaves once the list is at the end.
    press(byLabel(view, "Jump to latest"));
    expect(allByLabel(view, "Jump to latest")).toHaveLength(0);
    // A row that arrives while away brings the dot back.
    scrollTo(view, 400);
    expect(byLabel(view, "Jump to latest").props.accessibilityHint).toBeUndefined();
    rerender(view, <Harness calls={calls} pane={paneFrom([...events, deltaEvent(7n, "a9", " Reading the detector."), itemEvent(8n, tool("t9", { title: "run", detail: "cargo test" }))])} />);
    expect(byLabel(view, "Jump to latest").props.accessibilityHint).toBe("New messages below");
    // Scrolling back to the end by hand clears it too.
    scrollTo(view, 0);
    scrollTo(view, 400);
    expect(byLabel(view, "Jump to latest").props.accessibilityHint).toBeUndefined();
  });

  it("A row arriving at the end moves the list for a moment; that is not the reader leaving", () => {
    const view = render(<Harness calls={newCalls()} pane={paneFrom([snapshotEvent(5n, chatItems())])} />);
    // The list holds its place by the height of the row that arrived, then scrolls back.
    reportOffset(view, 300);
    expect(allByLabel(view, "Jump to latest")).toHaveLength(0);
    expect(listOf(view).props.maintainVisibleContentPosition).toMatchObject({ minIndexForVisible: 0 });
    reportOffset(view, 0);
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    expect(allByLabel(view, "Jump to latest")).toHaveLength(0);
    // Left there with no finger on it (a screen reader's scroll), it is away once that lasts.
    reportOffset(view, 300);
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    byLabel(view, "Jump to latest");
  });

  it("A reader let go a little short of the end is taken to it once; the glide stopping is not settled again", () => {
    const view = render(<Harness calls={newCalls()} pane={paneFrom([snapshotEvent(5n, chatItems())])} />);
    flatListScrolls.length = 0;
    scrollTo(view, 40);
    expect(flatListScrolls).toEqual([{ offset: 0, animated: true }]);
    // The glide stops short (the list reports no further move) and says it stopped, as a flick does.
    reportOffset(view, 3);
    act(() => {
      (listOf(view).props.onMomentumScrollEnd as () => void)();
      (listOf(view).props.onMomentumScrollEnd as () => void)();
    });
    expect(flatListScrolls).toHaveLength(1);
    // A flick that coasts to a stop near the end is still settled.
    act(() => {
      (listOf(view).props.onScrollBeginDrag as () => void)();
    });
    reportOffset(view, 30);
    act(() => {
      (listOf(view).props.onScrollEndDrag as (event: unknown) => void)({ nativeEvent: { contentOffset: { x: 0, y: 30 }, velocity: { x: 0, y: 2 } } });
    });
    expect(flatListScrolls).toHaveLength(1);
    act(() => {
      (listOf(view).props.onMomentumScrollEnd as () => void)();
    });
    expect(flatListScrolls).toHaveLength(2);
  });

  it("Earlier messages load by themselves when the top comes within reach, once per reach", () => {
    const calls = newCalls();
    const pane = paneFrom([snapshotEvent(5n, chatItems(), { olderCursor: "c1" })]);
    const view = render(<Harness calls={calls} pane={pane} />);
    expect(textOf(view.toJSON())).not.toContain("earlier messages");
    reachTop(view);
    expect(calls.older).toBe(1);
    // While the page is on its way the top says so, and reaching it again asks for nothing.
    rerender(view, <Harness calls={calls} pane={{ ...pane, loadingOlder: true }} />);
    expect(textOf(view.toJSON())).toContain("Loading earlier messages…");
    reachTop(view);
    expect(calls.older).toBe(1);
    // The page arrived and there is more: the next reach asks again.
    rerender(view, <Harness calls={calls} pane={{ ...pane, items: [text("q0", ChatItemKind.USER, "Earlier question"), ...pane.items], olderCursor: "c0" }} />);
    expect(textOf(view.toJSON())).not.toContain("Loading earlier messages…");
    reachTop(view);
    expect(calls.older).toBe(2);
    // Nothing older: nothing is asked for.
    rerender(view, <Harness calls={calls} pane={paneFrom([snapshotEvent(5n, chatItems())])} />);
    reachTop(view);
    expect(calls.older).toBe(2);
  });

  it("A load that failed is said at the top with Retry, and is not tried again by itself", () => {
    const calls = newCalls();
    const pane = paneFrom([snapshotEvent(5n, chatItems(), { olderCursor: "c1" })]);
    const view = render(<Harness calls={calls} olderFailed pane={pane} />);
    expect(textOf(view.toJSON())).toContain("Couldn't load earlier messages. Retry");
    reachTop(view);
    expect(calls.older).toBe(0);
    press(byText(view, "Retry"));
    expect(calls.retryOlder).toBe(1);
    writeRender("older-failed", view.toJSON());
  });
});

describe("above the reply field (mobile-chat-mock.html, S3)", () => {
  const user = (id: string, body: string, secondsAgo: number) => text(id, ChatItemKind.USER, body, { atUnixMillis: BigInt(Date.now() - secondsAgo * 1000) });
  const workingOn = (items: ChatItem[], overrides: Parameters<typeof state>[0] = {}) =>
    paneFrom([snapshotEvent(5n, items, { state: state({ turn: ChatTurnState.WORKING, ...overrides }) })]);
  const statusOf = (view: ReactTestRenderer) => view.root.findAll((node) => isHost(node, "Text") && node.props.accessibilityLiveRegion === "polite");

  it("The status line is outside the conversation and says what the agent does, for how long, and the steps so far", () => {
    const calls = newCalls();
    const view = render(<Harness calls={calls} pane={workingOn([user("q", "Add the fixture.", 6)])} />);
    const label = statusOf(view);
    expect(label).toHaveLength(1);
    expect(label[0]!.children.join("")).toBe("Working…");
    expect(within(label[0]!, listOf(view))).toBe(false);
    expect(textOf(view.toJSON())).toContain("Working… 6s");
    // The time is not announced each second: only the label is a live region.
    expect(view.root.findAll((node) => typeof node.type === "string" && node.props.accessibilityLiveRegion !== undefined)).toHaveLength(1);

    const running = [
      user("q", "Add the fixture.", 6),
      tool("read", { kind: ChatToolKind.READ, title: "read", detail: "links.rs", path: "src/links.rs" }),
      tool("run", { title: "run", detail: "cargo test -p terminal-interactions links", status: ChatToolStatus.RUNNING }),
    ];
    rerender(view, <Harness calls={calls} pane={workingOn(running)} />);
    expect(textOf(view.toJSON())).toContain("Running cargo test -p terminal-interactions links… 6s 2 steps so far");
    // The label gives way to the time and the count on a narrow screen.
    expect(statusOf(view)[0]!.props.numberOfLines).toBe(1);

    const thinking = [...running.slice(0, 2), text("think", ChatItemKind.REASONING, "Boxed output is hard-wrapped", { streaming: true })];
    rerender(view, <Harness calls={calls} pane={workingOn(thinking)} />);
    // Thinking that still streams is what the line says, and not a step yet.
    expect(textOf(view.toJSON())).toContain("Thinking… 6s 1 step so far");
    writeRender("status", view.toJSON());

    const agents = [user("q", "Check every caller.", 6), tool("agent", { kind: ChatToolKind.SUBAGENT, title: "agent", detail: "Find the other callers", status: ChatToolStatus.RUNNING })];
    rerender(view, <Harness calls={calls} pane={workingOn(agents)} />);
    expect(textOf(view.toJSON())).toContain("Waiting on 1 subagent… 6s 1 step so far");

    rerender(view, <Harness calls={calls} pane={workingOn(running, { compacting: true })} />);
    expect(statusOf(view)[0]!.children.join("")).toBe("Compacting context…");

    // Stop pressed: said at once, alone, and Stop is off until the turn stops.
    rerender(view, <Harness calls={calls} pane={workingOn(running)} stopping />);
    expect(textOf(view.toJSON())).toContain("Stopping… Send a follow-up while it works");
    expect(byLabel(view, "Stop").props.disabled).toBe(true);
  });

  it("The time counts from when the turn was seen to start, once a second", () => {
    const calls = newCalls();
    const idle = paneFrom([snapshotEvent(5n, [user("q0", "Earlier question", 600)])]);
    const view = render(<Harness calls={calls} pane={idle} />);
    expect(textOf(view.toJSON())).not.toContain("Working…");
    const started = paneFrom([snapshotEvent(5n, [user("q0", "Earlier question", 600)]), stateEvent(6n, state({ turn: ChatTurnState.WORKING }))]);
    rerender(view, <Harness calls={calls} pane={started} />);
    expect(textOf(view.toJSON())).toContain("Working… 0s");
    act(() => {
      vi.advanceTimersByTime(12_000);
    });
    expect(textOf(view.toJSON())).toContain("Working… 12s");
    act(() => {
      vi.advanceTimersByTime(113_000);
    });
    expect(textOf(view.toJSON())).toContain("Working… 2m 05s");
    // Waiting on the user, the request card is the status; the turn's time carries on after it.
    rerender(view, <Harness calls={calls} pane={paneFrom([snapshotEvent(5n, [user("q0", "Earlier question", 600)]), stateEvent(6n, state({ turn: ChatTurnState.WAITING, pending: permissionRequest() }))])} />);
    expect(textOf(view.toJSON())).not.toContain("Working…");
    rerender(view, <Harness calls={calls} pane={started} />);
    expect(textOf(view.toJSON())).toContain("Working… 2m 05s");
    // The turn ended: no line, and no timer left running.
    rerender(view, <Harness calls={calls} pane={idle} />);
    expect(textOf(view.toJSON())).not.toContain("Working…");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("No status line while the chat cannot be followed", () => {
    const view = render(<Harness calls={newCalls()} connected={false} pane={workingOn([user("q", "Add the fixture.", 6)])} />);
    expect(textOf(view.toJSON())).not.toContain("Working…");
  });

  it("The request card is fixed above the reply field and folds to one line", () => {
    const dismiss = vi.spyOn(Keyboard, "dismiss");
    const calls = newCalls();
    const pane = paneFrom([snapshotEvent(0n, chatItems()), ...questionTurnEvents(0n)]);
    const view = render(<Harness calls={calls} pane={pane} />);
    const option = () => byLabel(view, /^Boxed output only\./);
    // Not a row of the conversation, so scrolling cannot hide it.
    expect(within(option(), listOf(view))).toBe(false);
    expect(hidden(option())).toBe(false);
    expect(textOf(view.toJSON())).not.toContain("The agent asked a question");
    press(option());

    // Its arrow folds it to one line that still says the agent waits.
    press(byLabel(view, "Collapse request"));
    expect(hidden(option())).toBe(true);
    let shown = textOf(view.toJSON());
    expect(shown).toContain("The agent asked a question Answer");
    expect(shown).not.toContain("Boxed output only");
    writeRender("request-folded", view.toJSON());

    // The line unfolds it and closes the keyboard; what was chosen is still chosen.
    press(byText(view, "Answer"));
    expect(dismiss).toHaveBeenCalledTimes(1);
    expect(hidden(option())).toBe(false);
    expect(option().props.accessibilityState).toEqual({ checked: true });
    expect(textOf(view.toJSON())).not.toContain("The agent asked a question");

    // Focusing the reply field folds it, for room to read and type.
    focus(byLabel(view, "Message"));
    expect(hidden(option())).toBe(true);
    expect(textOf(view.toJSON())).toContain("The agent asked a question Answer");
    press(byText(view, "Answer"));

    // Typing in the card's own field never folds it.
    press(byLabel(view, /^Other\./));
    focus(byLabel(view, "Your answer"));
    type(byLabel(view, "Your answer"), "Only the boxed case");
    expect(hidden(byLabel(view, "Your answer"))).toBe(false);
    expect(textOf(view.toJSON())).not.toContain("The agent asked a question");

    // A new request arrives unfolded, even after this one was folded.
    press(byLabel(view, "Collapse request"));
    const next = paneFrom([snapshotEvent(0n, chatItems()), ...questionTurnEvents(0n), stateEvent(10n, state({ turn: ChatTurnState.WAITING, pending: permissionRequest() }))]);
    rerender(view, <Harness calls={calls} pane={next} />);
    expect(hidden(byText(view, "Allow"))).toBe(false);
    expect(within(byText(view, "Allow"), listOf(view))).toBe(false);
    press(byLabel(view, "Collapse request"));
    expect(textOf(view.toJSON())).toContain("The agent is asking for permission Answer");
    // Folded or not, it is the same request: answering still works once unfolded.
    press(byText(view, "Answer"));
    press(byText(view, "Allow"));
    expect(calls.answers).toEqual([{ kind: "permission", allow: true }]);
    dismiss.mockRestore();
  });

  it("A request with the same id is the same card: an update to it does not unfold or reset it", () => {
    const calls = newCalls();
    const events = [snapshotEvent(0n, chatItems()), ...questionTurnEvents(0n)];
    const view = render(<Harness calls={calls} pane={paneFrom(events)} />);
    press(byLabel(view, /^Boxed output only\./));
    press(byLabel(view, "Collapse request"));
    rerender(view, <Harness calls={calls} pane={paneFrom([...events, stateEvent(10n, state({ turn: ChatTurnState.WAITING, pending: questionRequest() }))])} />);
    expect(textOf(view.toJSON())).toContain("The agent asked a question Answer");
    press(byText(view, "Answer"));
    expect(byLabel(view, /^Boxed output only\./).props.accessibilityState).toEqual({ checked: true });
  });
});
