import { useRouter } from "expo-router";
import { useCallback, useRef, useState } from "react";
import { ActivityIndicator, Pressable, StyleSheet, Text, View } from "react-native";
import { useStore } from "zustand";

import { toRouteParam } from "../../navigation/routeParams";
import { getConnection, toast } from "../../session/connectionManager";
import { log } from "../../session/log";
import { prefsStore } from "../../store/prefsStore";
import { sessionStore } from "../../store/sessionStore";
import { Hairline } from "../../ui/components/Button";
import { Sheet } from "../../ui/components/Sheet";
import { useSession } from "../../ui/hooks";
import { colors, typeScale } from "../../ui/tokens";
import { createAgentWindow, createTerminalWindow, isConnectionScopeCurrent, type CreatedAgentWindow, type CreatedWindow } from "./createWindow";

const NEW_PANE_TOPOLOGY_WAIT_MS = 3_000;

interface WorkspaceActionsSheetProps {
  sessionId: string | undefined;
  title: string | undefined;
  visible: boolean;
  onDismiss(): void;
}

/** The two hold shortcuts shared by workspace and agent rows. */
export function WorkspaceActionsSheet({ sessionId, title, visible, onDismiss }: WorkspaceActionsSheetProps) {
  const router = useRouter();
  const connected = useSession((state) => state.connection.state === "connected");
  const agentCommand = useStore(prefsStore, (state) => state.agentCommand);
  const [creating, setCreating] = useState<"terminal" | "agent" | null>(null);
  const creatingRef = useRef<"terminal" | "agent" | null>(null);

  const createWindow = useCallback(async (kind: "terminal" | "agent") => {
    const connection = getConnection();
    const hasCommand = kind === "agent" && agentCommand.trim().length > 0;
    log(`create.window ui.press kind=${kind} session=${sessionId ?? "none"} connection=${connection ? "present" : "missing"} alreadyCreating=${creatingRef.current ? "yes" : "no"} commandPresent=${hasCommand ? "yes" : "no"} prefsHydrated=${prefsStore.getState().hydrated ? "yes" : "no"} topology=${sessionStore.getState().topologyGeneration}`);
    if (!sessionId || !connection || creatingRef.current) {
      log(`create.window ui.ignored kind=${kind} reason=${!sessionId ? "missing-session" : !connection ? "missing-connection" : "already-creating"}`);
      return;
    }
    if (kind === "agent" && !hasCommand) {
      log("create.window ui.ignored kind=agent reason=missing-command");
      onDismiss();
      toast("Set an agent command first.");
      router.push("/settings");
      return;
    }
    onDismiss();
    creatingRef.current = kind;
    setCreating(kind);
    try {
      let created: CreatedWindow;
      let agentCreated: CreatedAgentWindow | undefined;
      if (kind === "agent") {
        const startedAgent = await createAgentWindow(connection, sessionStore, sessionId, agentCommand, getConnection);
        agentCreated = startedAgent;
        created = startedAgent;
        void startedAgent.commandDelivery.then((delivery) => {
          if (delivery.ok) return;
          toast(`Terminal opened, but the agent command could not be sent: ${delivery.error.message}`);
        });
      } else {
        created = await createTerminalWindow(connection, sessionStore, sessionId);
      }
      const paneObserved = await waitForPane(created.paneId, NEW_PANE_TOPOLOGY_WAIT_MS);
      if (agentCreated && !isConnectionScopeCurrent(agentCreated.scope, getConnection)) {
        log(`create.window ui.navigation-skipped kind=${kind} reason=connection_scope_changed pane=${created.paneId}`);
        return;
      }
      log(`create.window ui.navigate kind=${kind} session=${sessionId} window=${created.windowId} pane=${created.paneId} paneObserved=${paneObserved ? "yes" : "no"} topology=${sessionStore.getState().topologyGeneration}`);
      router.push({ pathname: "/terminal/[paneId]", params: { paneId: toRouteParam(created.paneId), sessionId: toRouteParam(sessionId) } });
    } catch (error) {
      log(`create.window ui.failed kind=${kind} code=${diagnosticErrorCode(error)} topology=${sessionStore.getState().topologyGeneration}`);
      toast(`Couldn't open ${kind === "agent" ? "an agent" : "a terminal"}: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      creatingRef.current = null;
      setCreating(null);
    }
  }, [agentCommand, onDismiss, router, sessionId]);

  return (
    <Sheet onDismiss={onDismiss} title={title} visible={visible}>
      <SheetAction busy={creating === "terminal"} disabled={!connected || !sessionId || creating !== null} label="New terminal" onPress={() => void createWindow("terminal")} />
      <Hairline />
      <SheetAction busy={creating === "agent"} disabled={!connected || !sessionId || creating !== null} label="New agent" onPress={() => void createWindow("agent")} />
      <Text style={styles.command} numberOfLines={1}>Runs: {agentCommand || "Not configured"}</Text>
    </Sheet>
  );
}

function SheetAction({ label, onPress, disabled, busy }: { label: string; onPress(): void; disabled: boolean; busy: boolean }) {
  return (
    <Pressable accessibilityRole="button" disabled={disabled} onPress={onPress} style={({ pressed }) => [styles.action, pressed && styles.pressed, disabled && styles.disabled]}>
      <Text style={styles.actionLabel}>{label}</Text>
      {busy ? <ActivityIndicator color={colors.accent} size="small" /> : null}
    </Pressable>
  );
}

function waitForPane(paneId: string, timeoutMs: number): Promise<boolean> {
  return new Promise((resolve) => {
    if (sessionStore.getState().panes[paneId]) return resolve(true);
    const timer = setTimeout(() => {
      unsubscribe();
      resolve(false);
    }, timeoutMs);
    const unsubscribe = sessionStore.subscribe((state) => {
      if (!state.panes[paneId]) return;
      clearTimeout(timer);
      unsubscribe();
      resolve(true);
    });
  });
}

function diagnosticErrorCode(error: unknown): string {
  return error && typeof error === "object" && "code" in error && typeof error.code === "string"
    ? error.code
    : error instanceof Error ? error.name : typeof error;
}

const styles = StyleSheet.create({
  action: { alignItems: "center", flexDirection: "row", minHeight: 52, paddingVertical: 12 },
  actionLabel: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.rowTitle },
  command: { color: colors.chromeDim, fontSize: typeScale.meta, paddingBottom: 4, paddingTop: 8 },
  pressed: { opacity: 0.75 },
  disabled: { opacity: 0.45 },
});
