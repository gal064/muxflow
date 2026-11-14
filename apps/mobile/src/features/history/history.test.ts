import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";

import { Operation, ResponseSchema } from "../../protocol/gen/envelope_pb";
import { HistoryAction, HistoryEntrySchema, HistoryEventSchema } from "../../protocol/gen/history_pb";
import { FakeConnection } from "../voice/testing";
import { createHistoryStore, HistoryClient } from "./HistoryClient";
import { fuzzyMatch, historyMeta, historyRows, snoozeBlocked, snoozeChoices, wakeLabel, whenLabel } from "./historyModel";

import { at, historyEntries, NOW } from "./testing";

const provider = (adapterId: string) => (adapterId === "codex" ? "Codex" : "Claude Code");

describe("history rows", () => {
  it("lists every session in the host's order, or the snoozed ones", () => {
    const entries = historyEntries();
    expect(historyRows(entries, "all", "", provider).map((entry) => entry.nativeSessionId)).toEqual(["s1", "s2", "s3", "s4", "s5"]);
    expect(historyRows(entries, "snoozed", "", provider).map((entry) => entry.nativeSessionId)).toEqual(["s3"]);
  });

  it("searches titles first, then workspace and provider, fuzzily", () => {
    const entries = historyEntries();
    expect(historyRows(entries, "all", "link", provider).map((entry) => entry.nativeSessionId)).toEqual(["s1", "s2", "s3", "s5"]);
    expect(historyRows(entries, "all", "site", provider).map((entry) => entry.nativeSessionId)).toEqual(["s3", "s4"]);
    expect(historyRows(entries, "all", "codex", provider).map((entry) => entry.nativeSessionId)).toEqual(["s2"]);
    expect(fuzzyMatch("Wrapped link bug", "wlb")).toBe(true);
    expect(fuzzyMatch("Wrapped link bug", "bw")).toBe(false);
  });

  it("says running, the wake time, or when it last ran, as the mock does", () => {
    const [running, yesterday, snoozed, recent, old] = historyEntries();
    expect(historyMeta(running!, NOW)).toBe("running");
    expect(historyMeta(yesterday!, NOW)).toBe("yesterday");
    expect(historyMeta(snoozed!, NOW)).toBe("Mon 09:00");
    expect(historyMeta(recent!, NOW)).toBe("Mon");
    expect(historyMeta(old!, NOW)).toBe("12 Oct");
    expect(historyMeta(create(HistoryEntrySchema, { woke: true, snoozedUntilUnixMillis: at(-1) }), NOW)).toBe("woke");
    expect(whenLabel(Number(at(0, 9, 5)), NOW)).toBe("09:05");
    expect(wakeLabel(Number(at(1, 9)), NOW)).toBe("tomorrow 09:00");
    expect(wakeLabel(Number(at(10, 9)), NOW)).toBe("22 Nov 09:00");
  });
});

describe("snooze", () => {
  it("offers the desktop's default intervals", () => {
    const choices = snoozeChoices(NOW);
    expect(choices.map((choice) => choice.label)).toEqual(["In 1 hour", "In 3 hours", "Tomorrow, 09:00", "Monday, 09:00"]);
    expect(choices[0]!.untilMs).toBe(NOW + 3_600_000);
    expect(choices[2]!.untilMs).toBe(Number(at(1, 9)));
    expect(choices[3]!.untilMs).toBe(Number(at(4, 9)));
    // On a Monday, Monday means next week.
    const monday = new Date(2026, 10, 16, 8, 0).getTime();
    expect(new Date(snoozeChoices(monday)[3]!.untilMs).getDate()).toBe(23);
    // On a Sunday, tomorrow at 09:00 is Monday at 09:00: it is offered once.
    const sunday = new Date(2026, 10, 15, 20, 0).getTime();
    expect(snoozeChoices(sunday).map((choice) => choice.label)).toEqual(["In 1 hour", "In 3 hours", "Tomorrow, 09:00"]);
  });

  it("is refused while working or blocked, and for a session the host has not identified", () => {
    const agent = { lifecycle: "idle", nativeSessionId: "s1", present: true };
    expect(snoozeBlocked(agent)).toBeUndefined();
    expect(snoozeBlocked({ ...agent, lifecycle: "working" })).toMatch(/working/);
    expect(snoozeBlocked({ ...agent, lifecycle: "blocked" })).toMatch(/waiting for you/);
    expect(snoozeBlocked({ ...agent, nativeSessionId: "" })).toMatch(/identified/);
    expect(snoozeBlocked({ ...agent, present: false })).toMatch(/ended/);
  });
});

describe("HistoryClient", () => {
  function setup() {
    const connection = new FakeConnection();
    const store = createHistoryStore();
    const woke: string[] = [];
    let generation = 1n;
    connection.answer(Operation.SESSION_HISTORY, (request) => {
      if (request.history?.action === HistoryAction.RESUME) return create(ResponseSchema, { ok: true, history: { sessionId: "$2", windowId: "@3", paneId: "%8" } });
      return create(ResponseSchema, { ok: true, history: { generation, entries: request.history?.action === HistoryAction.LIST ? historyEntries() : [] } });
    });
    const client = new HistoryClient({
      store,
      getConnection: () => connection,
      topology: () => ({ expectedServerIdentity: "server-a", expectedTopologyGeneration: 7n }),
      onWoke: (entries) => woke.push(...entries.map((entry) => entry.title)),
    });
    return { connection, store, client, woke, bump: () => { generation += 1n; } };
  }
  const lists = (connection: FakeConnection) => connection.of(Operation.SESSION_HISTORY).filter((request) => request.history?.action === HistoryAction.LIST).length;

  it("lists when watched and after a change, not while nobody looks", async () => {
    const { connection, store, client } = setup();
    client.onEvent(create(HistoryEventSchema, { generation: 2n }));
    expect(lists(connection)).toBe(0);
    const release = client.watch();
    await client.list();
    expect(store.getState().entries).toHaveLength(5);
    expect(store.getState().loaded).toBe(true);
    const before = lists(connection);
    client.onEvent(create(HistoryEventSchema, { generation: 1n }));
    expect(lists(connection)).toBe(before);
    client.onEvent(create(HistoryEventSchema, { generation: 2n }));
    expect(lists(connection)).toBe(before + 1);
    release();
  });

  it("names a woken session, and resumes and snoozes with the topology it saw", async () => {
    const { connection, client, woke } = setup();
    client.onEvent(create(HistoryEventSchema, { generation: 3n, woke: [create(HistoryEntrySchema, { title: "Docs link checker" })] }));
    expect(woke).toEqual(["Docs link checker"]);
    expect(await client.resume({ adapterId: "claude", nativeSessionId: "s4" })).toEqual({ sessionId: "$2", windowId: "@3", paneId: "%8" });
    await client.snooze({ adapterId: "claude", nativeSessionId: "s4" }, 1_800_000_000_000);
    await client.wake({ adapterId: "claude", nativeSessionId: "s3" });
    const [resume, snooze, wake] = connection.of(Operation.SESSION_HISTORY).map((request) => request.history!);
    expect(resume).toMatchObject({ action: HistoryAction.RESUME, nativeSessionId: "s4", expectedServerIdentity: "server-a", expectedTopologyGeneration: 7n });
    expect(snooze).toMatchObject({ action: HistoryAction.SNOOZE, snoozeUntilUnixMillis: 1_800_000_000_000n, expectedTopologyGeneration: 7n });
    expect(wake).toMatchObject({ action: HistoryAction.WAKE, nativeSessionId: "s3" });
  });

  it("keeps the refusal of a LIST for the screen", async () => {
    const { connection, store, client } = setup();
    connection.answer(Operation.SESSION_HISTORY, () => { throw new Error("History isn't available on this host yet."); });
    await client.list();
    expect(store.getState()).toMatchObject({ loaded: true, loading: false, error: "History isn't available on this host yet." });
  });
});
