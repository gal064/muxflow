// The reply field's mic (docs/design/mobile-voice-mock.html): a tap opens
// voice mode, a hold dictates one message. Let go to send, swipe left to lock,
// right to discard; a hold that cannot record says why and wakes nothing.

import type { ReactTestInstance, ReactTestRenderer } from "react-test-renderer";
import { act } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { byLabel, press, render, rerender } from "../../../testing/render";
import { textOf } from "../../../testing/renderTree";
import { dictation } from "../testing";
import { Composer, type ComposerProps } from "./Composer";
import { DICTATION_HOLD_MS, type ComposerDictation } from "./ComposerMic";

function composer(fields: Partial<ComposerDictation>, calls: string[], props: Partial<ComposerProps> = {}): ReactTestRenderer {
  return render(composerElement(fields, calls, props));
}

const at = (pageX: number, pageY = 600) => ({ nativeEvent: { pageX, pageY } });

function call(node: ReactTestInstance, handler: string, event?: unknown): void {
  act(() => {
    (node.props[handler] as ((event?: unknown) => void) | undefined)?.(event);
  });
}

function down(mic: ReactTestInstance): void {
  call(mic, "onPressIn", at(300));
}

/** The finger comes up at `pageX`: the press ends, then (a real release) is a press. */
function up(mic: ReactTestInstance, pageX = 300): void {
  call(mic, "onPressOut", at(pageX));
  call(mic, "onPress");
}

function hold(): void {
  act(() => {
    vi.advanceTimersByTime(DICTATION_HOLD_MS);
  });
}

describe("the reply field's mic", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("a tap opens voice mode and records nothing", () => {
    const calls: string[] = [];
    const mic = byLabel(composer({}, calls), "Talk");
    down(mic);
    up(mic);
    hold();
    expect(calls).toEqual(["pressIn", "release", "voiceMode"]);
  });

  it("a screen reader's activation, with no press, is a tap", () => {
    const calls: string[] = [];
    press(byLabel(composer({}, calls), "Talk"));
    expect(calls).toEqual(["voiceMode"]);
  });

  it("a hold records once past the tap, and letting go sends it", () => {
    const calls: string[] = [];
    const mic = byLabel(composer({}, calls), "Talk");
    down(mic);
    act(() => {
      vi.advanceTimersByTime(DICTATION_HOLD_MS - 1);
    });
    expect(calls).toEqual(["pressIn"]);
    hold();
    expect(calls).toEqual(["pressIn", "begin"]);
    up(mic);
    // Let go: the take is locked as the finger leaves, then the release sends it.
    expect(calls).toEqual(["pressIn", "begin", "lock", "release", "submit"]);
  });

  it("a thumb that slides off the mic mid-take locks it instead of leaving it recording", () => {
    const calls: string[] = [];
    const view = composer({}, calls);
    down(byLabel(view, "Talk"));
    hold();
    rerender(view, composerElement({ phase: "recording" }, calls));
    // Far up the screen: no swipe, and no release either (no onPress follows).
    call(byLabel(view, "Talk"), "onPressOut", at(300, 300));
    expect(calls).toEqual(["pressIn", "begin", "lock", "release"]);
    // Back onto the mic, the same finger is the same press: no new take, no voice mode.
    rerender(view, composerElement({ phase: "recordingLocked" }, calls));
    call(byLabel(view, "Talk"), "onPressIn", at(300));
    hold();
    up(byLabel(view, "Talk"));
    expect(calls).toEqual(["pressIn", "begin", "lock", "release", "release", "submit"]);
  });

  it("dictates a follow-up while the agent works, when it takes one, with nothing typed", () => {
    const calls: string[] = [];
    const mic = byLabel(composer({}, calls, { canSendMidTurn: true, working: true }), "Talk");
    down(mic);
    hold();
    expect(calls).toEqual(["pressIn", "begin"]);
  });

  it("swiping left locks the take, right discards it; neither sends on letting go", () => {
    for (const [dx, action] of [[-80, "lock"], [80, "cancel"]] as const) {
      const calls: string[] = [];
      const view = composer({}, calls);
      const mic = byLabel(view, "Talk");
      down(mic);
      hold();
      // The strip appears once the take is on.
      rerender(view, composerElement({ phase: "recording" }, calls));
      call(byLabel(view, "Talk"), "onTouchMove", at(300 + dx / 2));
      call(byLabel(view, "Talk"), "onTouchMove", at(300 + dx));
      up(byLabel(view, "Talk"), 300 + dx);
      expect(calls).toEqual(["pressIn", "begin", action, "release"]);
    }
  });

  it("reaching Discard by the screen's edge, 40 px from the mic, discards; short of it, letting go still sends", () => {
    for (const [dx, expected] of [[40, ["pressIn", "begin", "cancel", "release"]], [30, ["pressIn", "begin", "lock", "release", "submit"]]] as const) {
      const calls: string[] = [];
      const view = composer({}, calls);
      down(byLabel(view, "Talk"));
      hold();
      rerender(view, composerElement({ phase: "recording" }, calls));
      call(byLabel(view, "Talk"), "onTouchMove", at(300 + dx));
      up(byLabel(view, "Talk"), 300 + dx);
      expect(calls).toEqual(expected);
    }
  });

  it("a hold that cannot record says why, wakes no microphone and opens nothing", () => {
    const calls: string[] = [];
    const mic = byLabel(composer({ refusal: "Voice isn't set up on this host" }, calls), "Talk");
    down(mic);
    hold();
    up(mic);
    expect(calls).toEqual(["refused: Voice isn't set up on this host", "release"]);
  });

  it("waits for what Send waits for: an unanswered card", () => {
    const calls: string[] = [];
    const mic = byLabel(composer({}, calls, { canSendMidTurn: false, waiting: true }), "Talk");
    down(mic);
    hold();
    up(mic);
    expect(calls).toEqual(["refused: Answer the card above first", "release"]);
  });

  it("and a working agent that takes no message until it stops", () => {
    const calls: string[] = [];
    const mic = byLabel(composer({}, calls, { canSendMidTurn: false, working: true }), "Talk");
    down(mic);
    hold();
    expect(calls).toEqual(["refused: The agent takes a message once it stops"]);
  });

  it("recording: the strip shows the time and where to swipe", () => {
    const view = composer({ phase: "recording" }, []);
    expect(textOf(view.toJSON())).toContain("0:00 ‹ Lock · Discard ›");
    act(() => {
      vi.advanceTimersByTime(7_000);
    });
    expect(textOf(view.toJSON())).toContain("0:07");
  });

  it("locked: Discard and Send on the strip", () => {
    const calls: string[] = [];
    const view = composer({ phase: "recordingLocked" }, calls);
    expect(textOf(view.toJSON())).toContain("Locked");
    press(byLabel(view, "Discard the recording"));
    press(byLabel(view, "Send the recording"));
    expect(calls).toEqual(["cancel", "submit"]);
  });

  it("transcribing: said on the strip, and the mic takes no press", () => {
    const view = composer({ phase: "transcribing" }, []);
    expect(textOf(view.toJSON())).toContain("Transcribing…");
    expect(byLabel(view, "Talk").props.disabled).toBe(true);
  });

  it("the empty field says the mic can be held until a first dictation", () => {
    const placeholder = (view: ReactTestRenderer) => byLabel(view, "Message").props.placeholder as string;
    expect(placeholder(composer({ taught: false }, []))).toBe("Reply · hold the mic to dictate");
    expect(placeholder(composer({ taught: true }, []))).toBe("Reply");
    // A field already saying something else keeps it.
    expect(placeholder(composer({ taught: false }, [], { working: true }))).toBe("Send a follow-up while it works");
  });
});

function composerElement(fields: Partial<ComposerDictation>, calls: string[], props: Partial<ComposerProps> = {}) {
  return (
    <Composer
      canSendMidTurn
      commands={[]}
      disabled={false}
      draft=""
      editing={false}
      mic={{ on: false, onTap: () => calls.push("voiceMode"), dictation: dictation(fields, calls) }}
      onCancelEdit={() => undefined}
      onDraft={() => undefined}
      onPickCommand={() => undefined}
      onSend={() => undefined}
      onStop={() => undefined}
      sending={false}
      sideQuestion={false}
      waiting={false}
      working={false}
      {...props}
    />
  );
}
