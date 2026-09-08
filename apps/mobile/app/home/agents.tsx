import { useFocusEffect, useRouter } from "expo-router";
import { useCallback, useEffect, useRef, useState } from "react";
import { Dimensions, Keyboard, type LayoutChangeEvent } from "react-native";
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

  const empty = (
    <EmptyState
      heading="No agents running"
      lines={[
        "Agents started from the desktop show up here. Codex and Claude Code are supported.",
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
        onDismiss={() => setActionsFor(null)}
        sessionId={actionsFor?.route.sessionId}
        title={actionsFor ? agentTitle({ adapters, windows }, actionsFor) : undefined}
        visible={actionsFor !== null}
      />
    </>
  );
}
