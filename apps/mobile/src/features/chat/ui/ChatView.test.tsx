// The phone chat mocks (docs/design/chat-mock.html, "Phone" and "Chat
// pieces"), each rendered from scripted chat events. With MUXFLOW_RENDER_DIR
// set, every state is also drawn as an HTML page at phone width.

import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { ChatConnection, ChatItemKind, ChatToolStatus, ChatTurnState, type ChatItem } from "../../../protocol/gen/chat_pb";
import { Operation } from "../../../protocol/gen/envelope_pb";
import { textOf, writeRender } from "../../../testing/renderTree";
import { allByLabel, byLabel, byText, isHost, longPress, press, render, rerender, type } from "../../../testing/render";
import { VoiceController } from "../../voice/VoiceController";
import { VoiceRecorderCoordinator } from "../../voice/recorderCoordinator";
import { FakeConnection, FakeFiles, FakePlayer, FakeRecorder, speechResponse } from "../../voice/testing";
import { voiceStore } from "../../voice/voiceStore";
import { newChatPane, slashMatches, type ChatPane } from "../chatModel";
import {
  chatItems,
  itemEvent,
  paneFrom,
  PANE,
  permissionRequest,
  questionTurnEvents,
  SERVER,
  snapshotEvent,
  state,
  stateEvent,
  text,
  tool,
} from "../testing";
import { ChatView, type ChatViewProps } from "./ChatView";
import { ChatReplyRow, ChatVoiceBar } from "./ChatVoiceParts";
import type { PendingAnswer } from "./PendingCard";

type Calls = { send: string[]; answers: PendingAnswer[]; stop: number; options: unknown[]; edits: string[]; voice: boolean[]; older: number; details: boolean[] };

/** ChatView with the screen's own state (draft, edit, details) and every action recorded. */
function Harness({ pane, calls, connected = true, voiceMode = false, voice, busy = false }: { pane: ChatPane | undefined; calls: Calls; connected?: boolean; voiceMode?: boolean; voice?: ChatViewProps["voice"]; busy?: boolean }) {
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
      onAnswer={(answer) => calls.answers.push(answer)}
      onBack={() => undefined}
      onCancelEdit={() => { setEditing(undefined); setDraft(""); }}
      onDetails={(open) => { calls.details.push(open); setDetailsOpen(open); }}
      onDraft={setDraft}
      onEdit={(item) => { calls.edits.push(item.id); setEditing(item); setDraft(item.text); }}
      onLoadOlder={() => { calls.older += 1; }}
      onPickCommand={(command) => setDraft(`/${command.name} `)}
      onRetry={() => undefined}
      onSend={() => { calls.send.push(draft); setDraft(""); }}
      onSetOptions={(options) => calls.options.push(options)}
      onStop={() => { calls.stop += 1; }}
      onVoiceMode={(on) => calls.voice.push(on)}
      pane={pane}
      title="Wrapped link bug"
      voice={voice}
      voiceMode={voiceMode}
    />
  );
}

const newCalls = (): Calls => ({ send: [], answers: [], stop: 0, options: [], edits: [], voice: [], older: 0, details: [] });

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
    byLabel(view, "Voice mode: read replies aloud");
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
    press(byText(view, "Next"));
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
    expect(shown).toContain("Working…");
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
    const bubble = view.root.findAll((node) => isHost(node, "Pressable") && node.props.accessibilityHint === "Hold to edit and resend");
    expect(bubble).toHaveLength(1);
    longPress(bubble[0]!);
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
    expect(view.root.findAll((node) => isHost(node, "Pressable") && node.props.accessibilityHint === "Hold to edit and resend")).toHaveLength(0);
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
    rerender(view, <Harness calls={calls} pane={paneFrom([snapshotEvent(5n, chatItems(), { olderCursor: "c1" })])} />);
    press(byText(view, "Load earlier messages"));
    expect(calls.older).toBe(1);
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

  it("Voice mode, inside the chat: the voice bar replaces the composer and the reply plays", async () => {
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
      const voice = {
        status: null,
        reply: <ChatReplyRow controller={controller} message={message} playbackRate={1.5} />,
        bar: <ChatVoiceBar controller={controller} hint="" onStop={() => undefined} phase="idle" working={false} />,
      };
      const view = render(<Harness calls={calls} pane={pane} voice={voice} voiceMode />);
      const shown = textOf(view.toJSON());
      expect(shown).toContain("Hold to talk. Swipe left to lock, right to cancel.");
      byLabel(view, "Reply read aloud");
      expect(shown).toContain("1.5×");
      expect(allByLabel(view, "Message")).toHaveLength(0);
      expect(byLabel(view, "Voice mode: read replies aloud").props.accessibilityState).toEqual({ checked: true });
      byLabel(view, /^Pause reply|^Play reply/);
      writeRender("voice", view.toJSON());
      press(byLabel(view, "Voice mode: read replies aloud"));
      expect(calls.voice).toEqual([false]);
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
