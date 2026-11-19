// The reply field with files on it (docs/design/mobile-chat-mock.html, S7):
// the plus button, the chips above the field, and a Send that waits for them.

import { View } from "react-native";
import type { ReactTestInstance, ReactTestRenderer } from "react-test-renderer";
import { describe, expect, it } from "vitest";

import { allByLabel, byLabel, isHost, press, render } from "../../../testing/render";
import { colors } from "../../../ui/tokens";
import type { FilesState } from "../attach/attachState";
import { Composer, type ComposerProps } from "./Composer";

interface Calls {
  sent: number;
  opened: number;
}

function composer(files: FilesState | undefined, fields: Partial<ComposerProps> = {}, calls: Calls = { sent: 0, opened: 0 }): ReactTestRenderer {
  return render(
    <Composer
      attach={files === undefined ? undefined : { files, onOpen: () => { calls.opened += 1; }, strip: files === "none" ? null : <View accessibilityLabel="The chips" /> }}
      canSendMidTurn
      commands={[]}
      disabled={false}
      draft=""
      editing={false}
      onCancelEdit={() => undefined}
      onDraft={() => undefined}
      onPickCommand={() => undefined}
      onSend={() => { calls.sent += 1; }}
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

const arrowColor = (send: ReactTestInstance): unknown => send.find((node) => isHost(node, "Path")).props.stroke;

/** The labelled parts of the reply field, as they are drawn, first to last. */
function order(view: ReactTestRenderer): string[] {
  return allByLabel(view, /^(The chips|Attach|Message|Talk|Send|Stop)$/).map((node) => node.props.accessibilityLabel as string);
}

describe("the plus button", () => {
  it("is not there when nothing can be attached", () => {
    const view = composer(undefined, { mic: { on: false, onTap: () => undefined, dictation: undefined } });
    expect(order(view)).toEqual(["Message", "Talk", "Send"]);
  });

  it("sits left of the field, drawn, and opens the attach sheet", () => {
    const calls = { sent: 0, opened: 0 };
    const view = composer("none", { mic: { on: false, onTap: () => undefined, dictation: undefined } }, calls);
    expect(order(view)).toEqual(["Attach", "Message", "Talk", "Send"]);
    const plus = byLabel(view, "Attach");
    expect(plus.findAll((node) => isHost(node, "Svg"))).toHaveLength(1);
    expect(plus.findAll((node) => isHost(node, "Text"))).toHaveLength(0);
    // 34 dp drawn, 44 and over to the touch.
    expect((flat(plus.props.style).width as number) + 2 * (plus.props.hitSlop as number)).toBeGreaterThanOrEqual(44);
    press(plus);
    expect(calls.opened).toBe(1);
  });

  it("is off with the field, when nothing can be sent", () => {
    expect(byLabel(composer("none", { disabled: true }), "Attach").props.disabled).toBe(true);
  });

  it("stays while an earlier message is edited: the edit goes with files as any message does", () => {
    expect(order(composer("none", { editing: true, draft: "Only boxed output." }))).toEqual(["Attach", "Message", "Send"]);
  });
});

describe("the chips", () => {
  it("are drawn in the box, above the field", () => {
    expect(order(composer("ready"))).toEqual(["The chips", "Attach", "Message", "Send"]);
  });
});

describe("Send with files on the field", () => {
  it("goes with files alone, nothing typed", () => {
    const calls = { sent: 0, opened: 0 };
    const send = byLabel(composer("ready", {}, calls), "Send");
    expect(send.props.disabled).toBe(false);
    expect(flat(send.props.style).backgroundColor).toBe(colors.accent);
    expect(flat(send.props.style).opacity).toBeUndefined();
    expect(arrowColor(send)).toBe(colors.accentInk);
    expect(send.props.accessibilityHint).toBeUndefined();
    press(send);
    expect(calls.sent).toBe(1);
  });

  it("waits while one is still uploading, plain grey, typed text or not, and says why", () => {
    for (const draft of ["", "And on the tablet?"]) {
      const send = byLabel(composer("uploading", { draft }), "Send");
      expect(send.props.disabled).toBe(true);
      expect(send.props.accessibilityState).toEqual({ disabled: true });
      expect(flat(send.props.style).backgroundColor).toBe(colors.chromeRaised);
      expect(flat(send.props.style).opacity).toBeUndefined();
      expect(arrowColor(send)).toBe(colors.chromeFaint);
      expect(send.props.accessibilityHint).toBe("The attachments are still uploading.");
    }
  });

  it("is off, dimmed, after one failed, until it is removed", () => {
    const send = byLabel(composer("failed", { draft: "And on the tablet?" }), "Send");
    expect(send.props.disabled).toBe(true);
    expect(flat(send.props.style).backgroundColor).toBe(colors.accent);
    expect(flat(send.props.style).opacity).toBe(0.4);
    expect(send.props.accessibilityHint).toBe("An attachment failed to upload. Remove it to send.");
    // Removed: the text goes.
    expect(byLabel(composer("none", { draft: "And on the tablet?" }), "Send").props.disabled).toBe(false);
  });

  it("is plain grey with no files and nothing typed, as without attaching", () => {
    const send = byLabel(composer("none"), "Send");
    expect(send.props.disabled).toBe(true);
    expect(flat(send.props.style).backgroundColor).toBe(colors.chromeRaised);
  });

  it("does not hold a side question: that goes alone, and the files wait", () => {
    for (const files of ["uploading", "failed"] as const) {
      const send = byLabel(composer(files, { draft: "/btw why?", sideQuestion: true }), "Send");
      expect(send.props.disabled).toBe(false);
      expect(send.props.accessibilityHint).toBeUndefined();
    }
    // With nothing asked there is no side question to send, files or not.
    expect(byLabel(composer("ready", { draft: "", sideQuestion: true }), "Send").props.disabled).toBe(true);
  });

  it("shows beside Stop while the agent works, for files alone, when it takes messages mid-turn", () => {
    expect(order(composer("ready", { working: true }))).toEqual(["The chips", "Attach", "Message", "Stop", "Send"]);
    expect(order(composer("ready", { working: true, canSendMidTurn: false }))).toEqual(["The chips", "Attach", "Message", "Stop"]);
    expect(order(composer("none", { working: true }))).toEqual(["Attach", "Message", "Stop"]);
  });
});
