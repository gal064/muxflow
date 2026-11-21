import { describe, expect, it } from "vitest";

import { ChatItemKind, ChatToolKind, ChatTurnState } from "../../protocol/gen/chat_pb";
import { forkTarget, isMessage, messageActions, newestReplyEnd } from "./messageRules";
import { text, tool } from "./testing";

const user = (id: string, body = "Run the full suite") => text(id, ChatItemKind.USER, body);
const says = (id: string, body = "Not pushed. The remote branch is unchanged.") => text(id, ChatItemKind.ASSISTANT, body);
const read = (id: string) => tool(id, { kind: ChatToolKind.READ, title: "read", detail: "links.rs" });
const notice = (id: string) => text(id, ChatItemKind.NOTICE, "Interrupted");
const { IDLE, WORKING, WAITING } = ChatTurnState;

// Two turns: the first with two texts around a step and a notice after them.
const items = [user("u1"), says("a1"), read("r"), says("a2"), notice("n"), user("u2"), says("a3")];

describe("messages", () => {
  it("only your messages and the agent's text are messages", () => {
    expect(items.map(isMessage)).toEqual([true, true, false, true, false, true, true]);
    expect(isMessage(text("t", ChatItemKind.REASONING, "thinking"))).toBe(false);
  });
});

describe("what a fork names", () => {
  it("is the last text of the reply in the message's turn, whichever message of it was held", () => {
    for (const held of ["u1", "a1", "a2"]) expect(forkTarget(items, held, IDLE)).toBe("a2");
    for (const held of ["u2", "a3"]) expect(forkTarget(items, held, IDLE)).toBe("a3");
  });

  it("is nothing for the newest turn while it is in progress; an earlier turn can still be forked", () => {
    for (const turn of [WORKING, WAITING]) {
      expect(forkTarget(items, "u2", turn)).toBeUndefined();
      expect(forkTarget(items, "a3", turn)).toBeUndefined();
      expect(forkTarget(items, "a1", turn)).toBe("a2");
    }
  });

  it("is nothing for a turn with no reply, a step, or an item that is not held", () => {
    expect(forkTarget([...items, user("u3")], "u3", IDLE)).toBeUndefined();
    expect(forkTarget([...items, user("u3"), read("r3")], "u3", IDLE)).toBeUndefined();
    expect(forkTarget(items, "r", IDLE)).toBeUndefined();
    expect(forkTarget(items, "n", IDLE)).toBeUndefined();
    expect(forkTarget(items, "gone", IDLE)).toBeUndefined();
  });

  it("takes a turn whose start is not loaded from the first item held", () => {
    const tail = [says("a0"), read("r0"), says("a1"), user("u2"), says("a2")];
    expect(forkTarget(tail, "a0", IDLE)).toBe("a1");
  });
});

describe("the reply with Copy and Fork under it", () => {
  it("is the last text of the newest reply once the turn has ended", () => {
    expect(newestReplyEnd(items, IDLE, false)?.id).toBe("a3");
    // A step or a notice after the last text does not move it.
    expect(newestReplyEnd([user("u"), says("a1"), says("a2"), read("r"), notice("n")], IDLE, false)?.id).toBe("a2");
    // A chat that is not followed yet says no turn at all.
    expect(newestReplyEnd(items, ChatTurnState.UNSPECIFIED, false)?.id).toBe("a3");
  });

  it("is nothing while a turn is in progress or a message is on its way", () => {
    expect(newestReplyEnd(items, WORKING, false)).toBeUndefined();
    expect(newestReplyEnd(items, WAITING, false)).toBeUndefined();
    expect(newestReplyEnd(items, IDLE, true)).toBeUndefined();
  });

  it("is never an older reply: your newest message with no reply has none", () => {
    expect(newestReplyEnd([...items, user("u3")], IDLE, false)).toBeUndefined();
    expect(newestReplyEnd([...items, user("u3"), read("r3")], IDLE, false)).toBeUndefined();
    expect(newestReplyEnd([], IDLE, false)).toBeUndefined();
  });

  it("is nothing for a text still being written, or one with nothing in it", () => {
    expect(newestReplyEnd([user("u"), text("a", ChatItemKind.ASSISTANT, "Half a", { streaming: true })], IDLE, false)).toBeUndefined();
    expect(newestReplyEnd([user("u"), says("a", "")], IDLE, false)).toBeUndefined();
  });
});

describe("a held message's sheet", () => {
  const all = { edit: true, fork: true };

  it("a reply: copy, select, fork", () => {
    expect(messageActions(says("a"), all)).toEqual(["copy", "select", "fork"]);
  });

  it("your own message: edit and resend as well, between them", () => {
    expect(messageActions(user("u"), all)).toEqual(["copy", "select", "edit", "fork"]);
  });

  it("leaves out what does not apply", () => {
    expect(messageActions(user("u"), { edit: false, fork: false })).toEqual(["copy", "select"]);
    expect(messageActions(user("u"), { edit: true, fork: false })).toEqual(["copy", "select", "edit"]);
    // A message the agent cannot rewind to.
    expect(messageActions(text("u", ChatItemKind.USER, "Earlier", { rewindable: false }), all)).toEqual(["copy", "select", "fork"]);
    // Attachments and no text: nothing to copy.
    expect(messageActions(user("u", ""), all)).toEqual(["edit", "fork"]);
  });

  it("has nothing for a step or a notice", () => {
    expect(messageActions(read("r"), all)).toEqual([]);
    expect(messageActions(notice("n"), all)).toEqual([]);
  });
});
