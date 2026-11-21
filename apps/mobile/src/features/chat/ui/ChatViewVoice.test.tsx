// Voice mode before voice can be used on the host
// (docs/design/mobile-chat-mock.html, S9): the reply field stays, under the
// setup card, and the talk pane (mobile-voice-mock.html) takes its place only
// once voice is ready.

import { View } from "react-native";
import type { ReactTestRenderer } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";

import { allByLabel, byLabel, press, render } from "../../../testing/render";
import { chatItems, dictation, paneFrom, snapshotEvent } from "../testing";
import { ChatView, type ChatViewProps } from "./ChatView";

// The conversation's Copy buttons reach the clipboard, which is native.
vi.mock("expo-clipboard", () => ({ setStringAsync: vi.fn(async () => true) }));

/** Stand-ins for the parts `useChatVoice` makes, each findable by its label. */
function parts(ready: boolean): NonNullable<ChatViewProps["voice"]> {
  return {
    status: <View accessibilityLabel="Voice setup card" />,
    reply: null,
    pane: <View accessibilityLabel="Talk pane" />,
    ready,
    dictation: dictation(),
  };
}

function chat(fields: { voiceMode: boolean; voice?: ChatViewProps["voice"]; calls?: boolean[] }): ReactTestRenderer {
  return render(
    <ChatView
      busy={false}
      commands={[]}
      connected
      detailsOpen={false}
      draft=""
      editing={undefined}
      folder="/home/operator/dev/muxflow"
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
      onStopTask={() => undefined}
      onVoiceMode={(on) => fields.calls?.push(on)}
      pane={paneFrom([snapshotEvent(5n, chatItems())])}
      stopping={false}
      title="Wrapped links"
      voice={fields.voice}
      voiceMode={fields.voiceMode}
    />,
  );
}

/** The labelled parts at the foot of the screen, top to bottom. */
function foot(view: ReactTestRenderer): string[] {
  return allByLabel(view, /^(Voice setup card|Talk pane|Message)$/).map((node) => node.props.accessibilityLabel as string);
}

describe("voice mode and the reply field", () => {
  it("voice not set up on the host: the reply field stays, under the setup card, and its mic leaves voice mode", () => {
    const calls: boolean[] = [];
    const view = chat({ voiceMode: true, voice: parts(false), calls });
    expect(foot(view)).toEqual(["Voice setup card", "Message"]);
    expect(byLabel(view, "Message").props.editable).toBe(true);
    byLabel(view, "Send");
    const mic = byLabel(view, "Talk");
    expect(mic.props.accessibilityState).toMatchObject({ selected: true });
    press(mic);
    expect(calls).toEqual([false]);
  });

  it("not connected in voice mode, with no voice yet: the mic still leaves it", () => {
    const calls: boolean[] = [];
    press(byLabel(chat({ voiceMode: true, voice: undefined, calls }), "Talk"));
    expect(calls).toEqual([false]);
  });

  it("voice ready: the talk pane takes the reply field's place", () => {
    const view = chat({ voiceMode: true, voice: parts(true) });
    // The card's place stays mounted: it holds the setup dialog and draws nothing once ready.
    expect(foot(view)).toEqual(["Voice setup card", "Talk pane"]);
  });

  it("voice mode on with nothing to show yet: the reply field", () => {
    expect(foot(chat({ voiceMode: true, voice: undefined }))).toEqual(["Message"]);
  });

  it("voice mode off: the reply field and its mic, whatever voice's state", () => {
    const calls: boolean[] = [];
    const view = chat({ voiceMode: false, voice: parts(true), calls });
    expect(foot(view)).toEqual(["Message"]);
    expect(calls).toEqual([]);
    // A tap on it is voice mode.
    press(byLabel(view, "Talk"));
    expect(calls).toEqual([true]);
  });
});
