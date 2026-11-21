// Recorded sessions for the history tests, the rows of the phone mock "History". Test-only.

import { create } from "@bufbuild/protobuf";
import { HistoryEntrySchema, type HistoryEntry } from "../../protocol/gen/history_pb";

// Thursday 12 November 2026, 15:00 local.
export const NOW = new Date(2026, 10, 12, 15, 0, 0).getTime();
export const at = (days: number, hours = 15, minutes = 0) => BigInt(new Date(2026, 10, 12 + days, hours, minutes).getTime());

export function historyEntries(): HistoryEntry[] {
  const entry = (fields: Omit<Partial<HistoryEntry>, "$typeName" | "$unknown">) => create(HistoryEntrySchema, { adapterId: "claude", workspaceName: "muxflow", cwd: "/work/muxflow", ...fields });
  return [
    entry({ nativeSessionId: "s1", title: "Wrapped link bug", running: true, agentId: "agent-1", lastActiveAtUnixMillis: at(0, 14, 50) }),
    entry({ nativeSessionId: "s2", adapterId: "codex", title: "File links on mobile", lastActiveAtUnixMillis: at(-1) }),
    entry({ nativeSessionId: "s3", title: "Docs link checker", workspaceName: "site", lastActiveAtUnixMillis: at(-2), snoozedUntilUnixMillis: at(4, 9) }),
    entry({ nativeSessionId: "s4", title: "Release notes", workspaceName: "site", lastActiveAtUnixMillis: at(-3, 10) }),
    entry({ nativeSessionId: "s5", title: "OSC 8 hyperlink support", lastActiveAtUnixMillis: at(-31) }),
  ];
}

