// The chat's header (docs/design/mobile-chat-mock.html, S8) in each state it
// can be in, rendered from scripted chat events.

import type { ReactTestRenderer } from "react-test-renderer";
import { describe, expect, it } from "vitest";

import { ChatConnection, ChatTurnState } from "../../../protocol/gen/chat_pb";
import { allByLabel, byLabel, isHost, press, render } from "../../../testing/render";
import { textOf, writeRender } from "../../../testing/renderTree";
import { colors, stateColors } from "../../../ui/tokens";
import { newChatPane, type ChatPane } from "../chatModel";
import { chatItems, PANE, paneFrom, questionRequest, SERVER, snapshotEvent, state } from "../testing";
import { ChatHeader } from "./ChatHeader";

type Calls = { back: number; details: number };

function header(pane: ChatPane | undefined, fields: { connected?: boolean; calls?: Calls; title?: string } = {}): ReactTestRenderer {
  const calls = fields.calls ?? { back: 0, details: 0 };
  return render(
    <ChatHeader
      connected={fields.connected ?? true}
      onBack={() => { calls.back += 1; }}
      onDetails={() => { calls.details += 1; }}
      pane={pane}
      title={fields.title ?? "Wrapped links"}
    />,
  );
}

const paneWith = (overrides: Parameters<typeof state>[0] = {}): ChatPane => paneFrom([snapshotEvent(5n, chatItems(), { state: state(overrides) })]);

function flat(style: unknown): Record<string, unknown> {
  if (!style) return {};
  if (Array.isArray(style)) return Object.assign({}, ...style.map(flat));
  return style as Record<string, unknown>;
}

/** The state dot's colour, or undefined when none is drawn. */
function dotColor(view: ReactTestRenderer): unknown {
  const dots = view.root.findAll((node) => isHost(node, "View") && flat(node.props.style).width === 7 && flat(node.props.style).height === 7);
  return dots.length === 0 ? undefined : flat(dots[0]!.props.style).backgroundColor;
}

describe("the chat header", () => {
  it("Working: the title over the state, model and effort, with the working dot", () => {
    const view = header(paneWith({ turn: ChatTurnState.WORKING }));
    expect(textOf(view.toJSON())).toBe("Wrapped links Working · Opus 5.5 · High");
    expect(dotColor(view)).toBe(stateColors.working);
    writeRender("header-working", view.toJSON(), 80);
  });

  it("Waiting for you: the dot that asks for you", () => {
    const view = header(paneWith({ turn: ChatTurnState.WAITING, pending: questionRequest() }));
    expect(textOf(view.toJSON())).toBe("Wrapped links Waiting for you · Opus 5.5 · High");
    expect(dotColor(view)).toBe(colors.danger);
  });

  it("Idle and Ended: a faint dot", () => {
    const idle = header(paneWith());
    expect(textOf(idle.toJSON())).toBe("Wrapped links Idle · Opus 5.5 · High");
    expect(dotColor(idle)).toBe(colors.chromeFaint);
    for (const ended of [paneWith({ connection: ChatConnection.ENDED }), paneWith({ connection: ChatConnection.FAILED }), { ...paneWith({ turn: ChatTurnState.WORKING }), status: "gone" as const }]) {
      const view = header(ended);
      expect(textOf(view.toJSON())).toBe("Wrapped links Ended · Opus 5.5 · High");
      expect(dotColor(view)).toBe(colors.chromeFaint);
    }
  });

  it("Not connected: no state word and no dot; the model and effort stay", () => {
    const view = header(paneWith({ turn: ChatTurnState.WORKING }), { connected: false });
    expect(textOf(view.toJSON())).toBe("Wrapped links Opus 5.5 · High");
    expect(dotColor(view)).toBeUndefined();
  });

  it("Opening, or no chat yet: the title alone", () => {
    for (const pane of [undefined, newChatPane(PANE, SERVER)]) {
      const view = header(pane);
      expect(textOf(view.toJSON())).toBe("Wrapped links");
      expect(dotColor(view)).toBeUndefined();
    }
  });

  it("leaves out an effort the model does not have", () => {
    const view = header(paneWith({ options: { model: "sonnet-5-5", effort: "", permission: 1, speed: 1 } }));
    expect(textOf(view.toJSON())).toBe("Wrapped links Idle · Sonnet 5.5");
  });

  it("opens Details from the title and from its button, which are not labelled alike", () => {
    const calls: Calls = { back: 0, details: 0 };
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
    expect(byLabel(header(undefined), "Chat details").props.accessibilityValue).toEqual({ text: "Wrapped links" });
  });

  it("draws its buttons, with no text glyph; voice mode is the reply field's mic, not a button here", () => {
    const view = header(paneWith());
    for (const label of ["Back", "Details"]) {
      const button = byLabel(view, label);
      expect(button.findAll((node) => isHost(node, "Svg"))).toHaveLength(1);
      expect(button.findAll((node) => isHost(node, "Text"))).toHaveLength(0);
    }
    expect(allByLabel(view, /./)).toHaveLength(3);
  });
});
