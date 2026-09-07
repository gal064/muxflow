import { useRouter } from "expo-router";
import { useCallback, useState } from "react";
import { ActivityIndicator, Pressable, StyleSheet, Text, View } from "react-native";
import { useStore } from "zustand";

import { toRouteParam } from "../../navigation/routeParams";
import { getConnection, toast } from "../../session/connectionManager";
import { prefsStore } from "../../store/prefsStore";
import { sessionStore } from "../../store/sessionStore";
import { Hairline } from "../../ui/components/Button";
import { Sheet } from "../../ui/components/Sheet";
import { useSession } from "../../ui/hooks";
import { colors, typeScale } from "../../ui/tokens";
import { createTerminalWindow } from "./createWindow";

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

  const createWindow = useCallback(async (kind: "terminal" | "agent") => {
    const connection = getConnection();
    if (!sessionId || !connection || creating) return;
    const command = kind === "agent" ? agentCommand.trim() : "";
    if (kind === "agent" && !command) {
      onDismiss();
      toast("Set an agent command first.");
      router.push("/settings");
      return;
    }
    onDismiss();
    setCreating(kind);
    try {
      const created = await createTerminalWindow(connection, sessionStore, sessionId, command);
      await waitForPane(created.paneId, NEW_PANE_TOPOLOGY_WAIT_MS);
      router.push({ pathname: "/terminal/[paneId]", params: { paneId: toRouteParam(created.paneId), sessionId: toRouteParam(sessionId) } });
    } catch (error) {
      toast(`Couldn't open ${kind === "agent" ? "an agent" : "a terminal"}: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      setCreating(null);
    }
  }, [agentCommand, creating, onDismiss, router, sessionId]);

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

function waitForPane(paneId: string, timeoutMs: number): Promise<void> {
  return new Promise((resolve) => {
    if (sessionStore.getState().panes[paneId]) return resolve();
    const timer = setTimeout(() => {
      unsubscribe();
      resolve();
    }, timeoutMs);
    const unsubscribe = sessionStore.subscribe((state) => {
      if (!state.panes[paneId]) return;
      clearTimeout(timer);
      unsubscribe();
      resolve();
    });
  });
}

const styles = StyleSheet.create({
  action: { alignItems: "center", flexDirection: "row", minHeight: 52, paddingVertical: 12 },
  actionLabel: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.rowTitle },
  command: { color: colors.chromeDim, fontSize: typeScale.meta, paddingBottom: 4, paddingTop: 8 },
  pressed: { opacity: 0.75 },
  disabled: { opacity: 0.45 },
});
