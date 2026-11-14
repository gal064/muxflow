import { useFocusEffect, useRouter } from "expo-router";
import { useCallback, useEffect, useRef, useState } from "react";
import { Dimensions, Keyboard, Platform, type LayoutChangeEvent } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { agentTitle, noAdapterWired } from "../../src/features/agents/agentViews";
import { markSeenIfNeeded } from "../../src/features/agents/markSeen";
import { refreshAgents } from "../../src/features/agents/refresh";
import { AgentList } from "../../src/features/agents/ui/AgentList";
import { toRouteParam } from "../../src/navigation/routeParams";
import { toast } from "../../src/session/connectionManager";
import { log } from "../../src/session/log";
import type { Agent } from "../../src/store/sessionStore";
import { EmptyState } from "../../src/ui/components/EmptyState";
import { useSession } from "../../src/ui/hooks";
import { WorkspaceActionsSheet } from "../../src/features/terminal/WorkspaceActionsSheet";
import { snoozeBlocked, wakeLabel } from "../../src/features/history/historyModel";
import { historyClient } from "../../src/features/history/historyRuntime";
import { SnoozeSheet } from "../../src/features/history/ui/SnoozeSheet";
import { refusalText } from "../../src/features/chat/refusals";

/** Agents tab — design.md §9.3.1. The list itself lives in `AgentList`. */
export default function AgentsScreen() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const insetBottom = useRef(insets.bottom);
  insetBottom.current = insets.bottom;
  const lastLayout = useRef("");
  const pendingLayout = useRef("");
  const layoutTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const connected = useSession((s) => s.connection.state === "connected");
  const adapters = useSession((s) => s.adapters);
  const windows = useSession((s) => s.windows);
  const [refreshing, setRefreshing] = useState(false);
  const [actionsFor, setActionsFor] = useState<Agent | null>(null);
  const [snoozeFor, setSnoozeFor] = useState<Agent | null>(null);
  // iOS cannot present a modal while another is still sliding away: the
  // Snooze sheet waits for the actions sheet to finish closing there.
  const snoozeAfterActions = useRef<Agent | null>(null);
  const openSnooze = useCallback((agent: Agent) => {
    if (Platform.OS === "ios") snoozeAfterActions.current = agent;
    else setSnoozeFor(agent);
  }, []);
  const actionsClosed = useCallback(() => {
    const agent = snoozeAfterActions.current;
    snoozeAfterActions.current = null;
    if (agent) setSnoozeFor(agent);
  }, []);
  const heldWindow = actionsFor?.present ? windows[actionsFor.route.windowId] : undefined;
  const screenSummary = useCallback(() => {
    const window = Dimensions.get("window");
    const keyboard = Keyboard.metrics();
    return `window=${Math.round(window.width)}x${Math.round(window.height)} safeBottom=${Math.round(insetBottom.current)} keyboard=${Keyboard.isVisible() ? `visible:${keyboard ? Math.round(keyboard.height) : "unknown"}` : `hidden:${keyboard ? Math.round(keyboard.height) : "none"}`}`;
  }, []);
  useFocusEffect(useCallback(() => {
    log(`[muxflow] ui.screen name=agents event=focus ${screenSummary()}`);
    return () => log(`[muxflow] ui.screen name=agents event=blur ${screenSummary()}`);
  }, [screenSummary]));
  useEffect(() => () => {
    if (layoutTimer.current !== undefined) clearTimeout(layoutTimer.current);
  }, []);
  const onLayout = useCallback((event: LayoutChangeEvent) => {
    const width = Math.round(event.nativeEvent.layout.width);
    const height = Math.round(event.nativeEvent.layout.height);
    pendingLayout.current = `${width}x${height}`;
    if (layoutTimer.current !== undefined) clearTimeout(layoutTimer.current);
    layoutTimer.current = setTimeout(() => {
      layoutTimer.current = undefined;
      if (pendingLayout.current === lastLayout.current) return;
      lastLayout.current = pendingLayout.current;
      log(`[muxflow] ui.screen name=agents event=layout view=${lastLayout.current} ${screenSummary()}`);
    }, 150);
  }, [screenSummary]);
  const onRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      await refreshAgents();
    } finally {
      setRefreshing(false);
    }
  }, []);
  const open = useCallback((agent: Agent) => {
    // A retained (gone) agent may have no pane; "/terminal/" would be an unmatched route.
    if (!agent.route.paneId) {
      toast("This agent has no terminal.");
      return;
    }
    markSeenIfNeeded(agent);
    router.push({ pathname: "/terminal/[paneId]", params: { paneId: toRouteParam(agent.route.paneId), sessionId: toRouteParam(agent.route.sessionId) } });
  }, [router]);
  const talk = useCallback((agent: Agent) => {
    if (!agent.route.paneId) {
      toast("This agent has no terminal.");
      return;
    }
    markSeenIfNeeded(agent);
    router.push({ pathname: "/voice/[paneId]", params: { paneId: toRouteParam(agent.route.paneId), sessionId: toRouteParam(agent.route.sessionId), agentId: toRouteParam(agent.id) } });
  }, [router]);

  const snooze = useCallback(async (agent: Agent, untilMs: number) => {
    setSnoozeFor(null);
    try {
      await historyClient.snooze({ adapterId: agent.adapterId, nativeSessionId: agent.nativeSessionId }, untilMs);
      toast(`Snoozed until ${wakeLabel(untilMs, Date.now())}.`);
    } catch (error) {
      toast(`Couldn't snooze: ${refusalText(error)}`);
    }
  }, []);

  const empty = (
    <EmptyState
      heading="No agents running"
      lines={[
        "Agents started from the desktop show up here. Codex, Claude Code and Pi are supported.",
        ...(noAdapterWired(adapters)
          ? ["Agent status hooks are not set up on this host. Set them up from the desktop: Settings → Connection → Set up agent status…"]
          : []),
      ]}
    />
  );

  return (
    <>
      <AgentList empty={connected ? empty : null} onLayout={onLayout} onLongPress={setActionsFor} onOpen={open} onRefresh={onRefresh} onTalk={talk} refreshing={refreshing} />
      <WorkspaceActionsSheet
        onClosed={actionsClosed}
        onDismiss={() => setActionsFor(null)}
        sessionId={actionsFor?.route.sessionId}
        title={actionsFor ? agentTitle({ adapters, windows }, actionsFor) : undefined}
        visible={actionsFor !== null}
        // An agent's pin is its tab's, as on the desktop; a gone agent's tab may be gone too.
        pin={heldWindow ? { label: heldWindow.pinned ? "Unpin agent" : "Pin agent", target: { kind: "window", sessionId: heldWindow.sessionId, windowId: heldWindow.id }, pinned: !heldWindow.pinned } : undefined}
        snooze={actionsFor ? { blocked: snoozeBlocked(actionsFor), onPress: () => openSnooze(actionsFor) } : undefined}
        // A gone agent has no pane left to close.
        close={actionsFor?.present && actionsFor.route.paneId
          ? { label: "Close agent", target: { kind: "pane", sessionId: actionsFor.route.sessionId, windowId: actionsFor.route.windowId, paneId: actionsFor.route.paneId } }
          : undefined}
      />
      <SnoozeSheet
        now={Date.now()}
        onDismiss={() => setSnoozeFor(null)}
        onPick={(untilMs) => { if (snoozeFor) void snooze(snoozeFor, untilMs); }}
        title={snoozeFor ? agentTitle({ adapters, windows }, snoozeFor) : undefined}
        visible={snoozeFor !== null}
      />
    </>
  );
}
