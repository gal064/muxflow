// The reply field's Send button (docs/design/mobile-chat-mock.html, S9):
// plain grey with nothing typed, the accent with something to send, dimmed
// when it cannot be sent for another reason.

import type { ReactTestInstance, ReactTestRenderer } from "react-test-renderer";
import { describe, expect, it } from "vitest";

import { byLabel, isHost, press, render } from "../../../testing/render";
import { colors } from "../../../ui/tokens";
import { Composer, type ComposerProps } from "./Composer";

function composer(fields: Partial<ComposerProps> = {}, sent: string[] = []): ReactTestRenderer {
  return render(
    <Composer
      canSendMidTurn
      commands={[]}
      disabled={false}
      draft=""
      editing={false}
      onCancelEdit={() => undefined}
      onDraft={() => undefined}
      onPickCommand={() => undefined}
      onSend={() => sent.push(fields.draft ?? "")}
      onStop={() => undefined}
      sending={false}
      sideQuestion={false}
      waiting={false}
      working={false}
      {...fields}
    />,
  );
}

function flat(style: unknown): Record<string, unknown> {
  if (!style) return {};
  if (Array.isArray(style)) return Object.assign({}, ...style.map(flat));
  return style as Record<string, unknown>;
}

/** The colour Send's arrow is drawn in. */
function arrowColor(send: ReactTestInstance): unknown {
  return send.find((node) => isHost(node, "Path")).props.stroke;
}

describe("the reply field's Send", () => {
  it("is plain grey with nothing typed, not a faded accent", () => {
    for (const draft of ["", "   "]) {
      const send = byLabel(composer({ draft }), "Send");
      expect(send.props.disabled).toBe(true);
      expect(send.props.accessibilityState).toEqual({ disabled: true });
      const style = flat(send.props.style);
      expect(style.backgroundColor).toBe(colors.chromeRaised);
      expect(style.opacity).toBeUndefined();
      expect(arrowColor(send)).toBe(colors.chromeFaint);
    }
  });

  it("is the accent with something to send", () => {
    const sent: string[] = [];
    const send = byLabel(composer({ draft: "Run the tests." }, sent), "Send");
    expect(send.props.disabled).toBe(false);
    const style = flat(send.props.style);
    expect(style.backgroundColor).toBe(colors.accent);
    expect(style.opacity).toBeUndefined();
    expect(arrowColor(send)).toBe(colors.accentInk);
    press(send);
    expect(sent).toEqual(["Run the tests."]);
  });

  it("dims when a typed message cannot go: not connected, sending, or a request waits", () => {
    for (const fields of [{ disabled: true }, { sending: true }, { waiting: true, canSendMidTurn: false }]) {
      const send = byLabel(composer({ draft: "Run the tests.", ...fields }), "Send");
      expect(send.props.disabled).toBe(true);
      const style = flat(send.props.style);
      expect(style.backgroundColor).toBe(colors.accent);
      expect(style.opacity).toBe(0.4);
    }
  });

  it("draws its arrow, with no text glyph", () => {
    const send = byLabel(composer({ draft: "Run the tests." }), "Send");
    expect(send.findAll((node) => isHost(node, "Svg"))).toHaveLength(1);
    expect(send.findAll((node) => isHost(node, "Text"))).toHaveLength(0);
  });
});
