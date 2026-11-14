// The phone mock "History", and Snooze from the agent's long-press menu
// (docs/design/chat-mock.html), rendered from scripted history entries.

import { create } from "@bufbuild/protobuf";
import { useState } from "react";
import { describe, expect, it } from "vitest";

import { HistoryEntrySchema, type HistoryEntry } from "../../../protocol/gen/history_pb";
import { textOf, writeRender } from "../../../testing/renderTree";
import { allByLabel, byLabel, byText, press, render, type } from "../../../testing/render";
import { historyRows, type HistoryFilter } from "../historyModel";
import { at, historyEntries, NOW } from "../testing";
import { HistoryView } from "./HistoryView";
import { SnoozeSheet } from "./SnoozeSheet";

const provider = (adapterId: string) => (adapterId === "codex" ? "Codex" : "Claude Code");

function Harness({ entries, opened, woken, error = "", loading = false }: { entries: HistoryEntry[]; opened: string[]; woken: string[]; error?: string; loading?: boolean }) {
  const [filter, setFilter] = useState<HistoryFilter>("all");
  const [query, setQuery] = useState("");
  return (
    <HistoryView
      busyKey={undefined}
      error={error}
      filter={filter}
      loading={loading}
      now={NOW}
      onFilter={setFilter}
      onOpen={(entry) => opened.push(entry.nativeSessionId)}
      onQuery={setQuery}
      onWake={(entry) => woken.push(entry.nativeSessionId)}
      query={query}
      rows={historyRows(entries, filter, query, provider)}
      total={entries.length}
    />
  );
}

describe("phone history mock", () => {
  it("History: search, All or Snoozed, one row per session with its time", () => {
    const opened: string[] = [];
    const woken: string[] = [];
    const view = render(<Harness entries={historyEntries()} opened={opened} woken={woken} />);
    const shown = textOf(view.toJSON());
    expect(shown).toContain("Search sessions");
    expect(shown).toContain("All Snoozed");
    expect(shown).toContain("Wrapped link bug running");
    expect(shown).toContain("File links on mobile yesterday");
    expect(shown).toContain("Docs link checker Mon 09:00 Wake now");
    expect(shown).toContain("Release notes Mon");
    expect(shown).toContain("OSC 8 hyperlink support 12 Oct");
    writeRender("history", view.toJSON());

    press(byLabel(view, /^Wrapped link bug,/));
    expect(opened).toEqual(["s1"]);
    press(byLabel(view, "Wake Docs link checker now"));
    expect(woken).toEqual(["s3"]);

    type(byLabel(view, "Search sessions"), "site");
    expect(textOf(view.toJSON())).not.toContain("Wrapped link bug");
    expect(allByLabel(view, /^(Docs link checker|Release notes),/)).toHaveLength(2);

    type(byLabel(view, "Search sessions"), "");
    press(byText(view, "Snoozed"));
    expect(allByLabel(view, /^[^,]+, (muxflow|site), /)).toHaveLength(1);
    writeRender("history-snoozed", view.toJSON());
  });

  it("marks a woken session, and says when nothing matches or history is unavailable", () => {
    const woke = create(HistoryEntrySchema, { adapterId: "claude", nativeSessionId: "w1", title: "Docs link checker", woke: true, snoozedUntilUnixMillis: at(-1), workspaceName: "site" });
    const view = render(<Harness entries={[woke, ...historyEntries()]} opened={[]} woken={[]} />);
    expect(textOf(view.toJSON())).toContain("Docs link checker woke");
    expect(allByLabel(view, "Wake Docs link checker now")).toHaveLength(1);
    type(byLabel(view, "Search sessions"), "zzz");
    expect(textOf(view.toJSON())).toContain("No matching sessions");

    const unavailable = render(<Harness entries={[]} error="History isn't available on this host yet." opened={[]} woken={[]} />);
    expect(textOf(unavailable.toJSON())).toContain("History isn't available on this host yet.");
    const empty = render(<Harness entries={[]} opened={[]} woken={[]} />);
    expect(textOf(empty.toJSON())).toContain("No sessions yet");
  });

  it("Snooze: the desktop's intervals, each a tap", () => {
    const picked: number[] = [];
    const view = render(<SnoozeSheet now={NOW} onDismiss={() => undefined} onPick={(untilMs) => picked.push(untilMs)} title="Docs link checker" visible />);
    const shown = textOf(view.toJSON());
    expect(shown).toContain("Snooze Docs link checker");
    for (const label of ["In 1 hour", "In 3 hours", "Tomorrow, 09:00", "Monday, 09:00"]) expect(shown).toContain(label);
    writeRender("snooze", view.toJSON());
    press(byText(view, "Tomorrow, 09:00"));
    expect(picked).toEqual([Number(at(1, 9))]);
  });
});
