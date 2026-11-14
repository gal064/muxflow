import { useRouter } from "expo-router";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useStore } from "zustand";

import type { HistoryEntry } from "../../../protocol/gen/history_pb";
import { toRouteParam } from "../../../navigation/routeParams";
import { toast } from "../../../session/connectionManager";
import { sessionStore } from "../../../store/sessionStore";
import { useSession } from "../../../ui/hooks";
import { refusalText } from "../../chat/refusals";
import { waitForPane } from "../../terminal/WorkspaceActionsSheet";
import { historyRows, type HistoryFilter } from "../historyModel";
import { historyClient, historyStore } from "../historyRuntime";
import { entryKey, HistoryView } from "./HistoryView";

const RESUMED_PANE_WAIT_MS = 3_000;

/** History: every session the host has seen; a tap opens a running one or resumes the rest. */
export function HistoryScreen() {
  const router = useRouter();
  const entries = useStore(historyStore, (s) => s.entries);
  const loaded = useStore(historyStore, (s) => s.loaded);
  const error = useStore(historyStore, (s) => s.error);
  const adapters = useSession((s) => s.adapters);
  const connected = useSession((s) => s.connection.state === "connected");
  const [filter, setFilter] = useState<HistoryFilter>("all");
  const [query, setQuery] = useState("");
  const [busyKey, setBusyKey] = useState<string | undefined>(undefined);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => (connected ? historyClient.watch() : undefined), [connected]);
  // Relative times ("14:05", "yesterday") move on while the screen stays open.
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(timer);
  }, []);

  const providerName = useCallback((adapterId: string) => adapters.find((adapter) => adapter.id === adapterId)?.displayName ?? adapterId, [adapters]);
  const rows = useMemo(() => historyRows(entries, filter, query, providerName), [entries, filter, providerName, query]);

  const openPane = useCallback((paneId: string, sessionId: string) => {
    router.push({ pathname: "/terminal/[paneId]", params: { paneId: toRouteParam(paneId), sessionId: toRouteParam(sessionId) } });
  }, [router]);

  const open = useCallback(async (entry: HistoryEntry) => {
    if (busyKey) return;
    if (entry.running) {
      const agent = sessionStore.getState().agents[entry.agentId];
      if (agent?.present && agent.route.paneId) {
        openPane(agent.route.paneId, agent.route.sessionId);
        return;
      }
    }
    setBusyKey(entryKey(entry));
    try {
      const at = await historyClient.resume({ adapterId: entry.adapterId, nativeSessionId: entry.nativeSessionId });
      if (!at.paneId) throw new Error("The host didn't say where the session opened.");
      await waitForPane(at.paneId, RESUMED_PANE_WAIT_MS);
      openPane(at.paneId, at.sessionId);
    } catch (failure) {
      toast(`Couldn't resume: ${refusalText(failure)}`);
    } finally {
      setBusyKey(undefined);
    }
  }, [busyKey, openPane]);

  const wake = useCallback(async (entry: HistoryEntry) => {
    if (busyKey) return;
    setBusyKey(entryKey(entry));
    try {
      await historyClient.wake({ adapterId: entry.adapterId, nativeSessionId: entry.nativeSessionId });
    } catch (failure) {
      toast(`Couldn't wake it: ${refusalText(failure)}`);
    } finally {
      setBusyKey(undefined);
    }
  }, [busyKey]);

  return (
    <HistoryView
      busyKey={busyKey}
      error={connected ? error : "Not connected. History comes back when the phone reconnects."}
      filter={filter}
      loading={connected && !loaded}
      now={now}
      onFilter={setFilter}
      onOpen={(entry) => void open(entry)}
      onQuery={setQuery}
      onWake={(entry) => void wake(entry)}
      query={query}
      rows={connected ? rows : []}
      total={connected ? entries.length : 0}
    />
  );
}
