// The chat's header (docs/design/mobile-chat-mock.html, S8) in each state it
// can be in, rendered from scripted chat events.

import type { ReactTestRenderer } from "react-test-renderer";
import { describe, expect, it } from "vitest";

import { ChatConnection, ChatTurnState } from "../../../protocol/gen/chat_pb";
import { allByLabel, byLabel, isHost, press, render } from "../../../testing/render";
import { textOf, writeRender } from "../../../testing/renderTree";
import { newChatPane, type ChatPane } from "../chatModel";
import { chatItems, PANE, paneFrom, questionRequest, SERVER, snapshotEvent, state } from "../testing";
import { AgentMark } from "../../agents/ui/AgentMark";
import { ChatHeader } from "./ChatHeader";

type Calls = { back: number; details: number; files: number };

function header(pane: ChatPane | undefined, fields: { connected?: boolean; calls?: Calls; title?: string; agent?: boolean; finished?: boolean } = {}): ReactTestRenderer {
  const calls = fields.calls ?? { back: 0, details: 0, files: 0 };
  return render(
    <ChatHeader
      connected={fields.connected ?? true}
      agent={fields.agent === false ? undefined : { adapterId: "claude-code", finished: fields.finished === true }}
      onBack={() => { calls.back += 1; }}
      onDetails={() => { calls.details += 1; }}
      onFiles={() => { calls.files += 1; }}
      pane={pane}
      title={fields.title ?? "Wrapped links"}
    />,
  );
}

const paneWith = (overrides: Parameters<typeof state>[0] = {}): ChatPane => paneFrom([snapshotEvent(5n, chatItems(), { state: state(overrides) })]);

/** The agent's mark: the state is drawn as the agent list draws it, not as a dot of the header's own. */
const marks = (view: ReactTestRenderer) => view.root.findAll((node) => isHost(node, "View") && node.props.importantForAccessibility === "no-hide-descendants");
/** The state the mark draws: its badge, as `AgentMark` takes it. */
const markState = (view: ReactTestRenderer) => view.root.findByType(AgentMark).props.state as string;

describe("the chat header", () => {
  it("Working: the agent's mark, then the title over the state, model and effort", () => {
    const view = header(paneWith({ turn: ChatTurnState.WORKING }));
    expect(textOf(view.toJSON())).toBe("Wrapped links Working · Opus 5.5 · High");
    expect(marks(view)).toHaveLength(1);
    expect(markState(view)).toBe("working");
    expect(marks(header(paneWith(), { agent: false }))).toHaveLength(0);
    writeRender("header-working", view.toJSON(), 80);
  });

  it("Waiting for you", () => {
    const view = header(paneWith({ turn: ChatTurnState.WAITING, pending: questionRequest() }));
    expect(textOf(view.toJSON())).toBe("Wrapped links Waiting for you · Opus 5.5 · High");
    expect(markState(view)).toBe("blocked");
  });

  it("Finished: an idle chat whose last turn is not seen yet says so, and its mark draws it as the agent list does", () => {
    const view = header(paneWith(), { finished: true });
    expect(textOf(view.toJSON())).toBe("Wrapped links Finished · Opus 5.5 · High");
    expect(markState(view)).toBe("done");
    // A finished turn that is working again is working.
    expect(markState(header(paneWith({ turn: ChatTurnState.WORKING }), { finished: true }))).toBe("working");
  });

  it("Idle and Ended", () => {
    const idle = header(paneWith());
    expect(textOf(idle.toJSON())).toBe("Wrapped links Idle · Opus 5.5 · High");
    expect(markState(idle)).toBe("idle");
    for (const ended of [paneWith({ connection: ChatConnection.ENDED }), paneWith({ connection: ChatConnection.FAILED }), { ...paneWith({ turn: ChatTurnState.WORKING }), status: "gone" as const }]) {
      const view = header(ended);
      expect(textOf(view.toJSON())).toBe("Wrapped links Ended · Opus 5.5 · High");
    }
  });

  it("Not connected: no state word; the model and effort stay", () => {
    const view = header(paneWith({ turn: ChatTurnState.WORKING }), { connected: false });
    expect(textOf(view.toJSON())).toBe("Wrapped links Opus 5.5 · High");
  });

  it("Opening, or no chat yet: the title alone", () => {
    for (const pane of [undefined, newChatPane(PANE, SERVER)]) {
      const view = header(pane);
      expect(textOf(view.toJSON())).toBe("Wrapped links");
    }
  });

  it("leaves out an effort the model does not have", () => {
    const view = header(paneWith({ options: { model: "sonnet-5-5", effort: "", permission: 1, speed: 1 } }));
    expect(textOf(view.toJSON())).toBe("Wrapped links Idle · Sonnet 5.5");
  });

  it("opens Details from the title and from its button, which are not labelled alike", () => {
    const calls: Calls = { back: 0, details: 0, files: 0 };
    const view = header(paneWith({ turn: ChatTurnState.WORKING }), { calls });
    const title = byLabel(view, "Chat details");
    expect(title.props.accessibilityRole).toBe("button");
    // The label stands in for the block's text, so a screen reader gets that as its value.
    expect(title.props.accessibilityValue).toEqual({ text: "Wrapped links, Working · Opus 5.5 · High" });
    press(title);
    expect(calls.details).toBe(1);
    const button = byLabel(view, "Details");
    expect(button.props.accessibilityRole).toBe("button");
    press(button);
    expect(calls.details).toBe(2);
    press(byLabel(view, "Back"));
    expect(calls.back).toBe(1);
    press(byLabel(view, "Files"));
    expect(calls.files).toBe(1);
    expect(byLabel(header(undefined), "Chat details").props.accessibilityValue).toEqual({ text: "Wrapped links" });
  });

  it("draws its buttons, with no text glyph; voice mode is the reply field's mic, not a button here", () => {
    const view = header(paneWith());
    for (const label of ["Back", "Files", "Details"]) {
      const button = byLabel(view, label);
      expect(button.findAll((node) => isHost(node, "Svg"))).toHaveLength(1);
      expect(button.findAll((node) => isHost(node, "Text"))).toHaveLength(0);
    }
    expect(allByLabel(view, /./)).toHaveLength(4);
  });
});
