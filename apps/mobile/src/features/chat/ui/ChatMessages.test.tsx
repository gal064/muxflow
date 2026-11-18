// A message's actions on the phone (docs/design/mobile-chat-mock.html, S5):
// the sheet a hold opens, the row under the newest finished reply, fork, edit
// and resend, and the draft kept per chat. Each state is rendered from
// scripted chat events, with the screen's own hooks for the reply field and
// the fork. With MUXFLOW_RENDER_DIR set, the states are also drawn as HTML
// pages at phone width.

import * as Clipboard from "expo-clipboard";
import { useEffect } from "react";
import { act, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ChatItemKind, ChatToolKind, ChatTurnState, type ChatItem } from "../../../protocol/gen/chat_pb";
import { HostError } from "../../../protocol/HostConnection";
import { colors } from "../../../ui/tokens";
import { textOf, writeRender } from "../../../testing/renderTree";
import { allByLabel, byLabel, byText, isHost, longPress, press, render as renderOnce, rerender, type } from "../../../testing/render";
import { pressHardwareBack } from "../../../testing/stubs/reactNative";
import type { ChatPane } from "../chatModel";
import { forgetDrafts } from "../drafts";
import { messageTime, replyTime } from "../messageTime";
import { catalog, paneFrom, PANE, SERVER, snapshotEvent, state, text, tool } from "../testing";
import { useFork } from "../useFork";
import { useReplyDraft } from "../useReplyDraft";
import { ChatView } from "./ChatView";
import { COPIED_CLOSE_MS } from "./MessageSheet";
import { FOCUS_AFTER_SHEET_MS } from "./useMessageActions";

vi.mock("expo-clipboard", () => ({ setStringAsync: vi.fn(async () => true) }));

// The reply field says when it is focused: the stub's own field keeps no count.
const field = vi.hoisted(() => ({ focused: 0, blurred: 0, has: false }));
vi.mock("react-native", async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>();
  const { createElement, forwardRef, useImperativeHandle } = await import("react");
  const TextInput = forwardRef<unknown, Record<string, unknown>>(function TextInput(props, ref) {
    useImperativeHandle(ref, () => ({
      focus() {
        field.focused += 1;
        field.has = true;
      },
      blur() {
        field.blurred += 1;
        field.has = false;
      },
      isFocused: () => field.has,
    }));
    return createElement("TextInput", props);
  });
  return { ...actual, TextInput };
});

/** What the screen did outside itself, and how the host answers a fork. */
interface World {
  /** FORK requests, by the item each named. */
  forks: string[];
  /** Answers the newest FORK: the fork's pane, or a refusal. */
  answer: ((forked: string | HostError) => void) | undefined;
  /** Panes waited for, and the fork chats opened. */
  waited: string[];
  opened: string[];
  /** Toasts. */
  said: string[];
  /** Messages sent, with the message each one replaces when it is an edit. */
  sent: { text: string; editing: string | undefined }[];
  /** Times the chat was left by Back. */
  left: number;
}

const newWorld = (): World => ({ forks: [], answer: undefined, waited: [], opened: [], said: [], sent: [], left: 0 });

/** The chat screen without its connection: ChatView with the screen's hooks for the reply field and the fork. */
function Screen({ pane, world, paneId = PANE, server = SERVER, connected = true, focused, voiceReady = false }: { pane: ChatPane; world: World; paneId?: string; server?: string; connected?: boolean; focused?: boolean; voiceReady?: boolean }) {
  const { draft, setDraft, editing, setEditing, edit, cancelEdit, sentEdit } = useReplyDraft(server, paneId);
  // As the screen does: a message that left the conversation cannot be edited.
  useEffect(() => {
    if (editing && !pane.items.some((item) => item.id === editing.id)) setEditing(undefined);
  }, [editing, pane, setEditing]);
  const { forking, fork } = useFork({
    fork: (itemId) => {
      world.forks.push(itemId);
      return new Promise<string>((resolve, reject) => {
        world.answer = (forked) => (typeof forked === "string" ? resolve(forked) : reject(forked));
      });
    },
    wait: async (forked) => {
      world.waited.push(forked);
    },
    open: (forked) => world.opened.push(forked),
    say: (message) => world.said.push(message),
  });
  return (
    <ChatView
      busy={false}
      commands={[]}
      connected={connected}
      detailsOpen={false}
      draft={draft}
      editing={editing}
      folder="~/dev/muxflow"
      forking={forking}
      hostLabel="devbox"
      olderFailed={false}
      onAnswer={() => undefined}
      onBack={() => { world.left += 1; }}
      onCancelEdit={cancelEdit}
      onDetails={() => undefined}
      onDraft={setDraft}
      onEdit={edit}
      onFork={fork}
      onLoadOlder={() => undefined}
      onPickCommand={() => undefined}
      onRetry={() => undefined}
      onRetryOlder={() => undefined}
      // As the screen does once the host took the message.
      onSend={() => {
        world.sent.push({ text: draft, editing: editing?.id });
        if (editing) sentEdit();
        else setDraft("");
      }}
      onSetOptions={() => undefined}
      onStop={() => undefined}
      onStopTask={() => undefined}
      onVoiceMode={() => undefined}
      pane={pane}
      stopping={false}
      title="Wrapped links"
      focused={focused}
      voice={voiceReady ? { status: null, reply: null, bar: null, ready: true } : undefined}
      voiceMode={voiceReady}
    />
  );
}

// Noon, so that a few minutes earlier is today wherever the tests run.
const NOON = new Date(2026, 0, 15, 12, 0).getTime();
const minutesAgo = (minutes: number) => ({ atUnixMillis: BigInt(NOON - minutes * 60_000) });
const user = (id: string, body: string, ago: number) => text(id, ChatItemKind.USER, body, minutesAgo(ago));
const says = (id: string, body: string, ago: number, overrides: Parameters<typeof text>[3] = {}) => text(id, ChatItemKind.ASSISTANT, body, { ...minutesAgo(ago), ...overrides });

/** The mock's conversation: two turns, the first with a step between its two texts. */
function conversation(): ChatItem[] {
  return [
    user("u1", "Why does it break?", 30),
    says("a1", "Reading the detector.", 29),
    tool("r1", { kind: ChatToolKind.READ, title: "read", detail: "links.rs", path: "links.rs" }),
    says("a2", "Going with: **Boxed output only**, Unit tests. The fix is in `links.rs`.", 20),
    user("u2", "Run the full suite", 10),
    says("a3", "Not pushed. The remote branch is unchanged.", 5),
  ];
}

const idle = (items: ChatItem[], fields: Parameters<typeof snapshotEvent>[2] = {}) => paneFrom([snapshotEvent(5n, items, fields)]);
const working = (items: ChatItem[]) => idle(items, { state: state({ turn: ChatTurnState.WORKING }) });
const capable = (capabilities: { rewind?: boolean; fork?: boolean }) =>
  catalog({ capabilities: { rewind: true, sendMidTurn: true, model: true, effort: true, speed: true, permission: true, fork: true, questions: true, ...capabilities } });

function flat(style: unknown): Record<string, unknown> {
  if (!style) return {};
  if (Array.isArray(style)) return Object.assign({}, ...style.map(flat));
  return typeof style === "object" ? (style as Record<string, unknown>) : {};
}

function within(node: ReactTestInstance, container: ReactTestInstance): boolean {
  for (let at: ReactTestInstance | null = node; at; at = at.parent) if (at === container) return true;
  return false;
}

/** The pressable a hold on the message reading `body` reaches. */
function message(view: ReactTestRenderer, body: string): ReactTestInstance {
  const holds = view.root.findAll((node) => isHost(node, "Pressable") && typeof node.props.onLongPress === "function"
    && node.findAll((child) => isHost(child, "Text") && readText(child).includes(body)).length > 0);
  if (holds.length === 0) throw new Error(`no message reads "${body}"`);
  return holds.at(-1)!;
}

/** A text element's own words, its styled runs joined. */
function readText(node: ReactTestInstance): string {
  return node.children.map((child) => (typeof child === "string" ? child : readText(child))).join("");
}

/** The open sheet, or undefined. */
function sheetOf(view: ReactTestRenderer): ReactTestInstance | undefined {
  return view.root.findAll((node) => isHost(node, "Modal"))[0];
}

/** The titles of the open sheet's rows, top to bottom. */
function sheetRows(view: ReactTestRenderer): string[] {
  const sheet = sheetOf(view);
  if (!sheet) return [];
  return sheet.findAll((node) => isHost(node, "Pressable") && node.props.accessibilityRole === "button")
    .map((row) => readText(row.findAll((node) => isHost(node, "Text"))[0]!));
}

/** The text of the open sheet that can be selected. */
function selectableText(view: ReactTestRenderer): ReactTestInstance {
  return sheetOf(view)!.find((node) => isHost(node, "Text") && node.props.selectable === true);
}

/** The words of the open sheet. */
function sheetText(view: ReactTestRenderer): string {
  const sheet = sheetOf(view);
  return sheet ? sheet.findAll((node) => isHost(node, "Text")).map(readText).join(" | ") : "";
}

/** The list's cells, as the screen draws them: oldest first. */
function cells(view: ReactTestRenderer): ReactTestInstance[] {
  const list = view.root.find((node) => isHost(node, "FlatList"));
  return list.children
    .filter((child): child is ReactTestInstance => typeof child !== "string")
    .map((row) => row.findAll((node) => isHost(node, "View"))[0]!);
}

/** Lets promises that are already settled run their continuations. */
async function settle(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

function advance(ms: number): void {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

// Every screen a test drew is taken down after it, as a screen that is left
// is: one still editing would go on answering the Back button.
const drawn: ReactTestRenderer[] = [];
function render(element: Parameters<typeof renderOnce>[0]): ReactTestRenderer {
  const view = renderOnce(element);
  drawn.push(view);
  return view;
}

const copied = () => vi.mocked(Clipboard.setStringAsync).mock.calls.map((call) => call[0]);

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(NOON);
  vi.mocked(Clipboard.setStringAsync).mockClear();
  field.focused = 0;
  field.blurred = 0;
  field.has = false;
  forgetDrafts();
});
afterEach(() => {
  act(() => {
    for (const view of drawn.splice(0)) view.unmount();
  });
  vi.useRealTimers();
});

describe("holding a message (mobile-chat-mock.html, S5)", () => {
  it("a reply: Copy message with its day and time, Select text, Fork from here", () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    expect(sheetOf(view)).toBeUndefined();
    longPress(message(view, "Going with:"));
    expect(sheetRows(view)).toEqual(["Copy message", "Select text", "Fork from here"]);
    const when = messageTime(BigInt(NOON - 20 * 60_000), NOON);
    expect(when.startsWith("Today, ")).toBe(true);
    // The first row carries the time.
    const first = sheetOf(view)!.findAll((node) => isHost(node, "Pressable") && node.props.accessibilityRole === "button")[0]!;
    expect(first.findAll((node) => isHost(node, "Text")).map(readText)).toEqual(["Copy message", when]);
    expect(sheetText(view)).toContain("A new chat with the conversation up to this message");
    writeRender("message-sheet-reply", view.toJSON());
  });

  it("your own message: Edit and resend as well, and the message is outlined while its sheet is open", () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    const bubble = message(view, "Run the full suite");
    expect(flat(bubble.props.style).borderColor).not.toBe(colors.accent);
    longPress(bubble);
    expect(sheetRows(view)).toEqual(["Copy message", "Select text", "Edit and resend", "Fork from here"]);
    expect(sheetText(view)).toContain("Removes everything after it");
    expect(sheetText(view)).toContain(messageTime(BigInt(NOON - 10 * 60_000), NOON));
    expect(flat(message(view, "Run the full suite").props.style)).toMatchObject({ borderColor: colors.accent, backgroundColor: colors.accentWash });
    writeRender("message-sheet-own", view.toJSON());
    // Closed by a tap outside: the outline goes with it.
    press(byLabel(view, "Close"));
    expect(sheetOf(view)).toBeUndefined();
    expect(flat(message(view, "Run the full suite").props.style).borderColor).not.toBe(colors.accent);
  });

  it("offers only what the agent can do", () => {
    const noFork = render(<Screen pane={idle(conversation(), { catalog: capable({ fork: false }) })} world={newWorld()} />);
    longPress(message(noFork, "Run the full suite"));
    expect(sheetRows(noFork)).toEqual(["Copy message", "Select text", "Edit and resend"]);

    const noRewind = render(<Screen pane={idle(conversation(), { catalog: capable({ rewind: false }) })} world={newWorld()} />);
    longPress(message(noRewind, "Run the full suite"));
    expect(sheetRows(noRewind)).toEqual(["Copy message", "Select text", "Fork from here"]);

    // A message the agent cannot rewind to.
    const fixed = conversation();
    fixed[0] = text("u1", ChatItemKind.USER, "Why does it break?", { ...minutesAgo(30), rewindable: false });
    const view = render(<Screen pane={idle(fixed)} world={newWorld()} />);
    longPress(message(view, "Why does it break?"));
    expect(sheetRows(view)).toEqual(["Copy message", "Select text", "Fork from here"]);
  });

  it("the turn in progress cannot be forked or edited around; an earlier one can be forked", () => {
    const view = render(<Screen pane={working(conversation())} world={newWorld()} />);
    longPress(message(view, "Not pushed."));
    expect(sheetRows(view)).toEqual(["Copy message", "Select text"]);
    press(byLabel(view, "Close"));
    longPress(message(view, "Reading the detector."));
    expect(sheetRows(view)).toEqual(["Copy message", "Select text", "Fork from here"]);
  });

  it("a chat that cannot be used still copies", () => {
    const view = render(<Screen connected={false} pane={idle(conversation())} world={newWorld()} />);
    longPress(message(view, "Run the full suite"));
    expect(sheetRows(view)).toEqual(["Copy message", "Select text"]);
  });

  it("steps, notices and a message still on its way have no sheet", () => {
    const pane = idle([...conversation(), text("n1", ChatItemKind.NOTICE, "Interrupted")]);
    const sending: ChatPane = { ...pane, outgoing: [{ id: "out-1", text: "One more thing", acked: false }] };
    const view = render(<Screen pane={sending} world={newWorld()} />);
    // Only the five messages take a hold.
    const holds = view.root.findAll((node) => isHost(node, "Pressable") && typeof node.props.onLongPress === "function");
    expect(holds).toHaveLength(5);
    expect(byLabel(view, "Your message, sending").props.disabled).toBe(true);
    expect(byLabel(view, "Your message, sending").props.onLongPress).toBeUndefined();
  });

  it("a screen reader opens the sheet, or the edit, from the message's own actions", () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    const bubble = message(view, "Run the full suite");
    expect(bubble.props.accessibilityActions).toEqual([{ name: "longpress", label: "Message actions" }, { name: "activate", label: "Edit and resend" }]);
    act(() => {
      (bubble.props.onAccessibilityAction as (event: unknown) => void)({ nativeEvent: { actionName: "longpress" } });
    });
    expect(sheetRows(view)).toContain("Edit and resend");
    press(byLabel(view, "Close"));
    act(() => {
      (message(view, "Run the full suite").props.onAccessibilityAction as (event: unknown) => void)({ nativeEvent: { actionName: "activate" } });
    });
    expect(byLabel(view, "Message").props.value).toBe("Run the full suite");
    // No sheet was in the way: the field is focused at once.
    expect(field.focused).toBe(1);
  });

  it("a message that left the conversation closes its sheet", () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    longPress(message(view, "Not pushed."));
    expect(sheetOf(view)).toBeDefined();
    rerender(view, <Screen pane={idle(conversation().slice(0, 4))} world={newWorld()} />);
    expect(sheetOf(view)).toBeUndefined();
  });
});

describe("Copy message and Select text", () => {
  it("Copy message copies the message as written, says Copied, and the sheet closes", async () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    longPress(message(view, "Going with:"));
    press(byText(view, "Copy message"));
    await settle();
    // The agent's own Markdown, not what is drawn from it.
    expect(copied()).toEqual(["Going with: **Boxed output only**, Unit tests. The fix is in `links.rs`."]);
    expect(sheetRows(view)[0]).toBe("Copied");
    advance(COPIED_CLOSE_MS - 1);
    expect(sheetOf(view)).toBeDefined();
    advance(1);
    expect(sheetOf(view)).toBeUndefined();
    // The next sheet starts as it always does.
    longPress(message(view, "Run the full suite"));
    expect(sheetRows(view)[0]).toBe("Copy message");
    advance(COPIED_CLOSE_MS * 2);
    expect(sheetOf(view)).toBeDefined();
  });

  it("a sheet closed by hand is not closed again by the copy made in it", async () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    longPress(message(view, "Going with:"));
    press(byText(view, "Copy message"));
    await settle();
    press(byLabel(view, "Close"));
    longPress(message(view, "Run the full suite"));
    advance(COPIED_CLOSE_MS * 2);
    expect(sheetRows(view)).toContain("Copy message");
  });

  it("Select text shows the message's plain text, selectable, and the next hold starts at the actions again", () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    longPress(message(view, "Going with:"));
    press(byText(view, "Select text"));
    expect(sheetRows(view)).toEqual([]);
    const sheet = sheetOf(view)!;
    expect(sheet.findAll((node) => isHost(node, "Text")).map(readText)).toContain("Select text");
    const plain = selectableText(view);
    expect(readText(plain)).toBe("Going with: Boxed output only, Unit tests. The fix is in links.rs.");
    // It scrolls inside the sheet when it is long.
    expect(within(plain, sheet.find((node) => isHost(node, "ScrollView")))).toBe(true);
    writeRender("message-select-text", view.toJSON());
    press(byLabel(view, "Close"));
    longPress(message(view, "Going with:"));
    expect(sheetRows(view)).toEqual(["Copy message", "Select text", "Fork from here"]);
  });

  it("your own message is selected as you typed it", () => {
    const items = [user("u1", "Use `**` for *bold*", 3), says("a1", "Done.", 2)];
    const view = render(<Screen pane={idle(items)} world={newWorld()} />);
    longPress(message(view, "Use `**`"));
    press(byText(view, "Select text"));
    expect(readText(selectableText(view))).toBe("Use `**` for *bold*");
  });

  it("the conversation's own text is not selectable in place, where a hold opens the sheet", () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    for (const body of ["Going with:", "Run the full suite"]) {
      const texts = message(view, body).findAll((node) => isHost(node, "Text"));
      expect(texts.length).toBeGreaterThan(0);
      for (const node of texts) expect(node.props.selectable === true).toBe(false);
    }
  });
});

describe("the newest finished reply", () => {
  it("has Copy, Fork and its time under it, and no earlier reply has", () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    const copy = byLabel(view, "Copy message");
    const fork = byLabel(view, "Fork from here");
    const newest = message(view, "Not pushed.");
    expect(within(copy, newest)).toBe(true);
    expect(within(fork, newest)).toBe(true);
    // The clock alone: it was written today.
    const when = replyTime(BigInt(NOON - 5 * 60_000), NOON);
    expect(when).not.toContain("Today");
    expect(newest.findAll((node) => isHost(node, "Text")).map(readText).at(-1)).toBe(when);
    // It is part of the reply's own row: the conversation has no row more for it.
    expect(cells(view)).toHaveLength(6);
    writeRender("reply-actions", view.toJSON());
  });

  it("is not there while the turn works, and comes when it ends, in the same row", () => {
    const world = newWorld();
    const items = conversation();
    const view = render(<Screen pane={working(items)} world={world} />);
    expect(allByLabel(view, "Copy message")).toHaveLength(0);
    expect(allByLabel(view, "Fork from here")).toHaveLength(0);
    const rowsWhileWorking = cells(view).length;
    rerender(view, <Screen pane={idle(items)} world={world} />);
    expect(within(byLabel(view, "Copy message"), message(view, "Not pushed."))).toBe(true);
    expect(cells(view)).toHaveLength(rowsWhileWorking);
  });

  it("is not under a reply still being written, nor while the agent waits on you", () => {
    const items = conversation();
    items[5] = says("a3", "Not pushed. The remo", 5, { streaming: true });
    expect(allByLabel(render(<Screen pane={idle(items)} world={newWorld()} />), "Copy message")).toHaveLength(0);
    const waiting = idle(conversation(), { state: state({ turn: ChatTurnState.WAITING }) });
    expect(allByLabel(render(<Screen pane={waiting} world={newWorld()} />), "Copy message")).toHaveLength(0);
  });

  it("goes when you send the next message, and is never under an older reply", () => {
    const world = newWorld();
    const pane = idle(conversation());
    const view = render(<Screen pane={pane} world={world} />);
    byLabel(view, "Copy message");
    rerender(view, <Screen pane={{ ...pane, outgoing: [{ id: "out-1", text: "And push it", acked: false }] }} world={world} />);
    expect(allByLabel(view, "Copy message")).toHaveLength(0);
    // Shown back by the host, with no reply yet.
    rerender(view, <Screen pane={idle([...conversation(), user("out-1", "And push it", 0)])} world={world} />);
    expect(allByLabel(view, "Copy message")).toHaveLength(0);
  });

  it("stays under the reply's last text when steps or a notice came after it", () => {
    const items = [...conversation(), tool("r9", { kind: ChatToolKind.READ, title: "read", detail: "open.rs" }), text("n1", ChatItemKind.NOTICE, "Interrupted")];
    const view = render(<Screen pane={idle(items)} world={newWorld()} />);
    expect(within(byLabel(view, "Copy message"), message(view, "Not pushed."))).toBe(true);
  });

  it("has no Fork where the agent cannot fork, or the chat cannot be used", () => {
    const noFork = render(<Screen pane={idle(conversation(), { catalog: capable({ fork: false }) })} world={newWorld()} />);
    byLabel(noFork, "Copy message");
    expect(allByLabel(noFork, "Fork from here")).toHaveLength(0);
    const offline = render(<Screen connected={false} pane={idle(conversation())} world={newWorld()} />);
    byLabel(offline, "Copy message");
    expect(allByLabel(offline, "Fork from here")).toHaveLength(0);
  });

  it("Copy copies the reply and shows that it did", async () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    press(byLabel(view, "Copy message"));
    await settle();
    expect(copied()).toEqual(["Not pushed. The remote branch is unchanged."]);
    byLabel(view, "Copied");
    advance(1500);
    byLabel(view, "Copy message");
  });
});

describe("Fork from here", () => {
  it("under the newest reply: sends the reply once, waits for the fork's pane, and opens it", async () => {
    const world = newWorld();
    const view = render(<Screen pane={idle(conversation())} world={world} />);
    press(byLabel(view, "Fork from here"));
    expect(world.forks).toEqual(["a3"]);
    // On its way: it shows it, and cannot be pressed again.
    const button = byLabel(view, "Fork from here");
    expect(button.props.disabled).toBe(true);
    expect(button.props.accessibilityState).toEqual({ busy: true, disabled: true });
    press(button);
    // Nor from the sheet of another message.
    longPress(message(view, "Reading the detector."));
    const row = byText(view, "Fork from here");
    expect(row.props.disabled).toBe(true);
    press(row);
    press(byLabel(view, "Close"));
    expect(world.forks).toEqual(["a3"]);
    expect(world.opened).toEqual([]);

    world.answer!("%9");
    await settle();
    expect(world.waited).toEqual(["%9"]);
    expect(world.opened).toEqual(["%9"]);
    expect(world.said).toEqual([]);
    // It can be forked again.
    expect(byLabel(view, "Fork from here").props.disabled).toBe(false);
    press(byLabel(view, "Fork from here"));
    expect(world.forks).toEqual(["a3", "a3"]);
  });

  it("from a message's sheet: names the last text of that message's reply, and the sheet closes when the fork opens", async () => {
    const world = newWorld();
    const view = render(<Screen pane={idle(conversation())} world={world} />);
    // Your first message: its turn ends with the second text of the reply.
    longPress(message(view, "Why does it break?"));
    press(byText(view, "Fork from here"));
    expect(world.forks).toEqual(["a2"]);
    // The sheet stays, with the row showing the fork on its way.
    const row = byText(view, "Fork from here");
    expect(row.props.accessibilityState).toEqual({ busy: true, disabled: true });
    press(row);
    expect(world.forks).toEqual(["a2"]);
    // The newest reply's own Fork is not this one, but waits for it.
    expect(byLabel(view, "Fork from here").props.accessibilityState).toEqual({ busy: false, disabled: true });
    writeRender("message-sheet-forking", view.toJSON());

    world.answer!("%12");
    await settle();
    expect(world.opened).toEqual(["%12"]);
    expect(sheetOf(view)).toBeUndefined();
  });

  it("refused: says why in the host's words, opens nothing, and can be tried again", async () => {
    const world = newWorld();
    const view = render(<Screen pane={idle(conversation())} world={world} />);
    press(byLabel(view, "Fork from here"));
    world.answer!(new HostError("chat_fork_unavailable", "That reply cannot be forked."));
    await settle();
    expect(world.said).toEqual(["That reply cannot be forked."]);
    expect(world.opened).toEqual([]);
    expect(world.waited).toEqual([]);
    expect(byLabel(view, "Fork from here").props.disabled).toBe(false);

    // From the sheet: it stays open, and its row is itself again.
    longPress(message(view, "Run the full suite"));
    press(byText(view, "Fork from here"));
    world.answer!(new HostError("chat_turn_busy", "The agent is working."));
    await settle();
    expect(world.said).toEqual(["That reply cannot be forked.", "The agent is busy. Stop it first, or wait for the turn to end."]);
    expect(byText(view, "Fork from here").props.disabled).toBe(false);
    press(byText(view, "Fork from here"));
    expect(world.forks).toEqual(["a3", "a3", "a3"]);
  });

  it("a host that names no pane opens nothing", async () => {
    const world = newWorld();
    const view = render(<Screen pane={idle(conversation())} world={world} />);
    press(byLabel(view, "Fork from here"));
    world.answer!("");
    await settle();
    expect(world.opened).toEqual([]);
    expect(world.said).toEqual([]);
    expect(byLabel(view, "Fork from here").props.disabled).toBe(false);
  });

  it("answered after the chat was left: the reader is not taken to the fork", async () => {
    const world = newWorld();
    const view = render(<Screen pane={idle(conversation())} world={world} />);
    press(byLabel(view, "Fork from here"));
    act(() => view.unmount());
    world.answer!("%9");
    await settle();
    expect(world.opened).toEqual([]);
  });
});

describe("Edit and resend", () => {
  /** Holds your second message and picks Edit and resend. */
  function startEdit(view: ReactTestRenderer): void {
    longPress(message(view, "Run the full suite"));
    press(byText(view, "Edit and resend"));
  }

  it("the field takes the message and the focus, the message is outlined, and what sending removes is dimmed", () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    expect(cells(view).filter((cell) => flat(cell.props.style).opacity === 0.45)).toHaveLength(0);
    startEdit(view);
    expect(sheetOf(view)).toBeUndefined();
    expect(byLabel(view, "Message").props.value).toBe("Run the full suite");
    expect(textOf(view.toJSON())).toContain("Editing an earlier message. Sending removes everything after it. Files on disk stay as they are.");
    // The keyboard comes once the sheet's window has gone.
    expect(field.focused).toBe(0);
    advance(FOCUS_AFTER_SHEET_MS);
    expect(field.focused).toBe(1);
    expect(field.blurred).toBe(0);

    // A field that kept the focus after its keyboard was put away lets go first, so the keyboard comes back.
    longPress(message(view, "Why does it break?"));
    press(byText(view, "Edit and resend"));
    advance(FOCUS_AFTER_SHEET_MS);
    expect(field.blurred).toBe(1);
    expect(field.focused).toBe(1);
    advance(100);
    expect(field.focused).toBe(2);
    longPress(message(view, "Run the full suite"));
    press(byText(view, "Edit and resend"));
    advance(FOCUS_AFTER_SHEET_MS + 100);

    expect(flat(message(view, "Run the full suite").props.style)).toMatchObject({ borderColor: colors.accent, backgroundColor: colors.accentWash });
    // Oldest first: five rows up to the message, and the reply after it dimmed.
    expect(cells(view).map((cell) => flat(cell.props.style).opacity === 0.45)).toEqual([false, false, false, false, false, true]);
    // The reply that would go keeps its actions, dimmed with it.
    expect(within(byLabel(view, "Copy message"), cells(view)[5]!)).toBe(true);
    writeRender("edit-mode", view.toJSON());
  });

  it("editing an earlier message dims every row after it", () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    longPress(message(view, "Why does it break?"));
    press(byText(view, "Edit and resend"));
    expect(cells(view).map((cell) => flat(cell.props.style).opacity === 0.45)).toEqual([false, true, true, true, true, true]);
  });

  it("Back cancels the edit first and puts the draft back; only the next Back leaves the chat", () => {
    const world = newWorld();
    const view = render(<Screen pane={idle(conversation())} world={world} />);
    // Nothing is being edited: Back is the screen's.
    expect(pressHardwareBack()).toBe(false);
    type(byLabel(view, "Message"), "half a thought");
    startEdit(view);
    type(byLabel(view, "Message"), "Run the unit tests only");

    let taken = false;
    act(() => {
      taken = pressHardwareBack();
    });
    expect(taken).toBe(true);
    expect(world.left).toBe(0);
    expect(textOf(view.toJSON())).not.toContain("Editing an earlier message");
    expect(byLabel(view, "Message").props.value).toBe("half a thought");
    expect(cells(view).filter((cell) => flat(cell.props.style).opacity === 0.45)).toHaveLength(0);
    expect(flat(message(view, "Run the full suite").props.style).borderColor).not.toBe(colors.accent);
    // The second press is not taken: the chat is left.
    expect(pressHardwareBack()).toBe(false);
  });

  it("under another screen, Back is that screen's: the edit stays", () => {
    const world = newWorld();
    const view = render(<Screen pane={idle(conversation())} world={world} />);
    startEdit(view);
    // A fork or a notification opened a screen over this one.
    act(() => view.update(<Screen focused={false} pane={idle(conversation())} world={world} />));
    expect(pressHardwareBack()).toBe(false);
    expect(textOf(view.toJSON())).toContain("Editing an earlier message");
    // In front again, Back cancels the edit first.
    act(() => view.update(<Screen focused pane={idle(conversation())} world={world} />));
    let taken = false;
    act(() => {
      taken = pressHardwareBack();
    });
    expect(taken).toBe(true);
    expect(textOf(view.toJSON())).not.toContain("Editing an earlier message");
  });

  it("no edit is offered while the voice bar has the reply field's place", () => {
    const view = render(<Screen pane={idle(conversation())} voiceReady world={newWorld()} />);
    longPress(message(view, "Run the full suite"));
    expect(sheetRows(view)).not.toContain("Edit and resend");
    expect(sheetRows(view)).toContain("Copy message");
  });

  it("the header's Back does the same", () => {
    const world = newWorld();
    const view = render(<Screen pane={idle(conversation())} world={world} />);
    type(byLabel(view, "Message"), "half a thought");
    startEdit(view);
    press(byLabel(view, "Back"));
    expect(world.left).toBe(0);
    expect(textOf(view.toJSON())).not.toContain("Editing an earlier message");
    expect(byLabel(view, "Message").props.value).toBe("half a thought");
    press(byLabel(view, "Back"));
    expect(world.left).toBe(1);
  });

  it("Cancel on the strip puts the draft back too, and an edit moved to another message keeps the first draft", () => {
    const view = render(<Screen pane={idle(conversation())} world={newWorld()} />);
    type(byLabel(view, "Message"), "half a thought");
    startEdit(view);
    longPress(message(view, "Why does it break?"));
    press(byText(view, "Edit and resend"));
    expect(byLabel(view, "Message").props.value).toBe("Why does it break?");
    press(byText(view, "Cancel"));
    expect(byLabel(view, "Message").props.value).toBe("half a thought");
  });

  it("sending replaces the message, and leaves the edit", () => {
    const world = newWorld();
    const view = render(<Screen pane={idle(conversation())} world={world} />);
    startEdit(view);
    type(byLabel(view, "Message"), "Run the unit tests only");
    press(byLabel(view, "Send"));
    expect(world.sent).toEqual([{ text: "Run the unit tests only", editing: "u2" }]);
    expect(byLabel(view, "Message").props.value).toBe("");
    expect(textOf(view.toJSON())).not.toContain("Editing an earlier message");
    expect(pressHardwareBack()).toBe(false);
  });

  it("sending an edit brings back the draft from before it", () => {
    const world = newWorld();
    const view = render(<Screen pane={idle(conversation())} world={world} />);
    type(byLabel(view, "Message"), "half a thought");
    startEdit(view);
    type(byLabel(view, "Message"), "Run the unit tests only");
    press(byLabel(view, "Send"));
    expect(world.sent).toEqual([{ text: "Run the unit tests only", editing: "u2" }]);
    expect(byLabel(view, "Message").props.value).toBe("half a thought");
  });
});

describe("a draft per chat", () => {
  it("is there again when the chat is opened next", () => {
    const pane = idle(conversation());
    const first = render(<Screen pane={pane} world={newWorld()} />);
    type(byLabel(first, "Message"), "half a thought");
    act(() => first.unmount());
    const again = render(<Screen pane={pane} world={newWorld()} />);
    expect(byLabel(again, "Message").props.value).toBe("half a thought");
  });

  it("belongs to its chat: another pane, or the same pane id on another server, has its own", () => {
    const pane = idle(conversation());
    const first = render(<Screen pane={pane} world={newWorld()} />);
    type(byLabel(first, "Message"), "for this chat");
    act(() => first.unmount());
    const other = render(<Screen pane={pane} paneId="%8" world={newWorld()} />);
    expect(byLabel(other, "Message").props.value).toBe("");
    type(byLabel(other, "Message"), "for the other");
    act(() => other.unmount());
    const elsewhere = render(<Screen pane={pane} server="server-b" world={newWorld()} />);
    expect(byLabel(elsewhere, "Message").props.value).toBe("");
    act(() => elsewhere.unmount());
    expect(byLabel(render(<Screen pane={pane} world={newWorld()} />), "Message").props.value).toBe("for this chat");
    expect(byLabel(render(<Screen pane={pane} paneId="%8" world={newWorld()} />), "Message").props.value).toBe("for the other");
  });

  it("is gone once it is sent, or the field is emptied", () => {
    const pane = idle(conversation());
    const first = render(<Screen pane={pane} world={newWorld()} />);
    type(byLabel(first, "Message"), "send me");
    press(byLabel(first, "Send"));
    act(() => first.unmount());
    const second = render(<Screen pane={pane} world={newWorld()} />);
    expect(byLabel(second, "Message").props.value).toBe("");
    type(byLabel(second, "Message"), "typed");
    type(byLabel(second, "Message"), "");
    act(() => second.unmount());
    expect(byLabel(render(<Screen pane={pane} world={newWorld()} />), "Message").props.value).toBe("");
  });

  it("an edit in progress is not a draft: leaving in the middle of one keeps the draft from before it", () => {
    const pane = idle(conversation());
    const first = render(<Screen pane={pane} world={newWorld()} />);
    type(byLabel(first, "Message"), "half a thought");
    longPress(message(first, "Run the full suite"));
    press(byText(first, "Edit and resend"));
    type(byLabel(first, "Message"), "Run the unit tests only");
    act(() => first.unmount());
    const again = render(<Screen pane={pane} world={newWorld()} />);
    expect(byLabel(again, "Message").props.value).toBe("half a thought");
    expect(textOf(again.toJSON())).not.toContain("Editing an earlier message");
  });

  it("the text of an edit whose message went stays in the field, and is a draft only once it is changed", () => {
    const world = newWorld();
    const pane = idle(conversation());
    const first = render(<Screen pane={pane} world={world} />);
    type(byLabel(first, "Message"), "half a thought");
    longPress(message(first, "Run the full suite"));
    press(byText(first, "Edit and resend"));
    type(byLabel(first, "Message"), "Run the unit tests only");
    // Rewound from somewhere else, past the message being edited.
    const rewound = idle(conversation().slice(0, 4));
    rerender(first, <Screen pane={rewound} world={world} />);
    expect(textOf(first.toJSON())).not.toContain("Editing an earlier message");
    expect(byLabel(first, "Message").props.value).toBe("Run the unit tests only");
    act(() => first.unmount());
    const again = render(<Screen pane={rewound} world={world} />);
    expect(byLabel(again, "Message").props.value).toBe("half a thought");

    // Changed after the edit ended, it is what was typed last.
    longPress(message(again, "Why does it break?"));
    press(byText(again, "Edit and resend"));
    rerender(again, <Screen pane={idle(conversation().slice(4))} world={world} />);
    type(byLabel(again, "Message"), "Why does it break on wrapped paths?");
    act(() => again.unmount());
    expect(byLabel(render(<Screen pane={rewound} world={world} />), "Message").props.value).toBe("Why does it break on wrapped paths?");
  });

  it("a chat opened before the phone has connected takes its draft once the server is known", () => {
    const pane = idle(conversation());
    const first = render(<Screen pane={pane} world={newWorld()} />);
    type(byLabel(first, "Message"), "kept from before");
    act(() => first.unmount());
    const world = newWorld();
    const early = render(<Screen pane={pane} server="" world={world} />);
    expect(byLabel(early, "Message").props.value).toBe("");
    rerender(early, <Screen pane={pane} world={world} />);
    expect(byLabel(early, "Message").props.value).toBe("kept from before");
    // What was typed in the meantime is not replaced.
    act(() => early.unmount());
    const typed = render(<Screen pane={pane} server="" world={world} />);
    type(byLabel(typed, "Message"), "typed while connecting");
    rerender(typed, <Screen pane={pane} world={world} />);
    expect(byLabel(typed, "Message").props.value).toBe("typed while connecting");
  });
});
