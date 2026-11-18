import { describe, expect, it } from "vitest";

import { ChatConnection, ChatTurnState } from "../../protocol/gen/chat_pb";
import { headerLine, headerState } from "./chatHeader";
import { newChatPane, type ChatPane } from "./chatModel";
import { chatItems, PANE, paneFrom, questionRequest, SERVER, snapshotEvent, state } from "./testing";

const paneWith = (overrides: Parameters<typeof state>[0] = {}): ChatPane => paneFrom([snapshotEvent(5n, chatItems(), { state: state(overrides) })]);

describe("the header's state word", () => {
  it("says Working, Waiting for you, Idle or Ended, each with its dot", () => {
    expect(headerState(paneWith({ turn: ChatTurnState.WORKING }), true)).toEqual({ word: "Working", tone: "working" });
    expect(headerState(paneWith({ turn: ChatTurnState.WAITING, pending: questionRequest() }), true)).toEqual({ word: "Waiting for you", tone: "waiting" });
    expect(headerState(paneWith(), true)).toEqual({ word: "Idle", tone: "quiet" });
    expect(headerState(paneWith({ connection: ChatConnection.ENDED }), true)).toEqual({ word: "Ended", tone: "quiet" });
    expect(headerState(paneWith({ connection: ChatConnection.FAILED, failure: "The agent crashed." }), true)).toEqual({ word: "Ended", tone: "quiet" });
  });

  it("says Ended for an agent that stopped mid-turn, and for a pane that has closed", () => {
    expect(headerState(paneWith({ turn: ChatTurnState.WORKING, connection: ChatConnection.FAILED }), true)?.word).toBe("Ended");
    expect(headerState({ ...paneWith({ turn: ChatTurnState.WORKING }), status: "gone" }, true)?.word).toBe("Ended");
  });

  it("says nothing while the banner does: not connected, opening, refused, or the agent still connecting", () => {
    expect(headerState(paneWith({ turn: ChatTurnState.WORKING }), false)).toBeUndefined();
    expect(headerState(undefined, true)).toBeUndefined();
    expect(headerState(newChatPane(PANE, SERVER), true)).toBeUndefined();
    expect(headerState({ ...newChatPane(PANE, SERVER), status: "error", error: "This chat can't be opened." }, true)).toBeUndefined();
    expect(headerState(paneWith({ connection: ChatConnection.CONNECTING }), true)).toBeUndefined();
    expect(headerState(paneWith({ turn: ChatTurnState.UNSPECIFIED }), true)).toBeUndefined();
  });
});

describe("the header's second line", () => {
  it("is the state, the model and the effort", () => {
    expect(headerLine(paneWith({ turn: ChatTurnState.WORKING }), true)).toEqual({ state: { word: "Working", tone: "working" }, text: "Working · Opus 5.5 · High" });
    expect(headerLine(paneWith(), true)?.text).toBe("Idle · Opus 5.5 · High");
  });

  it("leaves out what is not known", () => {
    const options = { model: "sonnet-5-5", effort: "", permission: 1, speed: 1 };
    expect(headerLine(paneWith({ options }), true)?.text).toBe("Idle · Sonnet 5.5");
    // A model the agent set that its catalog does not list goes by its id.
    expect(headerLine(paneWith({ options: { ...options, model: "custom-model", effort: "xhigh" } }), true)?.text).toBe("Idle · custom-model · Xhigh");
    expect(headerLine(paneWith({ options: { ...options, model: "" } }), true)?.text).toBe("Idle");
    // Not connected: no state word, but what the chat runs with is still known.
    expect(headerLine(paneWith({ turn: ChatTurnState.WORKING }), false)).toEqual({ text: "Opus 5.5 · High" });
  });

  it("is not drawn with nothing to say", () => {
    expect(headerLine(undefined, true)).toBeUndefined();
    expect(headerLine(newChatPane(PANE, SERVER), true)).toBeUndefined();
    expect(headerLine(newChatPane(PANE, SERVER), false)).toBeUndefined();
  });
});
