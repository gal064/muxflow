import { Stack, useLocalSearchParams, useRouter } from "expo-router";
import { useCallback, useState } from "react";
import { ActivityIndicator, FlatList, Pressable, StyleSheet, Text, View } from "react-native";

import { agentTitle, agentsInWindow, loudestPill } from "../../src/features/agents/agentViews";
import { stripAgentStatusGlyphs } from "../../src/features/agents/agentLabels";
import { createTerminalWindow } from "../../src/features/terminal/createWindow";
import { activePaneForWindow } from "../../src/features/terminal/panes";
import { getConnection, toast } from "../../src/session/connectionManager";
import { sessionStore } from "../../src/store/sessionStore";
import { ConnectionStrip } from "../../src/features/hosts/ConnectionStrip";
import { ListRow } from "../../src/ui/components/ListRow";
import { StatusPill } from "../../src/ui/components/StatusPill";
import { useSession } from "../../src/ui/hooks";
import { colors, metrics, typeScale } from "../../src/ui/tokens";

/** How long the New-terminal row waits for the created pane to reach the topology before navigating. */
const NEW_PANE_TOPOLOGY_WAIT_MS = 3_000;

/** Workspace — design.md §9.4. */
export default function WorkspaceScreen() {
  const { sessionId } = useLocalSearchParams<{ sessionId: string }>();
  const router = useRouter();
  const state = useSession((s) => s);
  const session = sessionId ? state.sessions[sessionId] : undefined;
  const windows = Object.values(state.windows).filter((w) => w.sessionId === sessionId).sort((a, b) => a.index - b.index);
  const connected = state.connection.state === "connected";
  const [creating, setCreating] = useState(false);

  const openPane = useCallback((paneId: string) => {
    router.push({ pathname: "/terminal/[paneId]", params: { paneId, sessionId: sessionId ?? "" } });
  }, [router, sessionId]);

  const browseFiles = useCallback(() => {
    if (!sessionId) return;
    const activeWindow = windows.find((w) => w.active) ?? windows[0];
    const pane = activeWindow ? activePaneForWindow(sessionStore.getState(), activeWindow.id) : undefined;
    if (!pane) return;
    router.push({ pathname: "/files/[paneId]", params: { paneId: pane.id } });
  }, [router, sessionId, windows]);

  const newTerminal = useCallback(async () => {
    const connection = getConnection();
    if (!sessionId || !connection || creating) return;
    setCreating(true);
    try {
      const created = await createTerminalWindow(connection, sessionStore, sessionId);
      await waitForPane(created.paneId);
      openPane(created.paneId);
    } catch (error) {
      toast(`Couldn't open a terminal: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      setCreating(false);
    }
  }, [creating, openPane, sessionId]);

  return (
    <View style={styles.root}>
      <Stack.Screen options={{ title: session ? stripAgentStatusGlyphs(session.name) : "Workspace" }} />
      <ConnectionStrip />
      <ActionRow disabled={!connected} glyph="▤" label="Browse files" onPress={browseFiles} />
      <ActionRow busy={creating} disabled={!connected || creating} glyph=">_" label="New terminal" onPress={() => void newTerminal()} />
      <FlatList
        data={windows}
        keyExtractor={(w) => w.id}
        renderItem={({ item }) => {
          const agents = agentsInWindow(state, item.id);
          const pane = activePaneForWindow(state, item.id);
          const subtitle = agents.length > 0 ? agents.map((a) => agentTitle(state, a)).join(", ") : pane?.currentCommand ?? "";
          const pill = loudestPill(agents);
          return (
            <ListRow
              height={metrics.windowRowHeight}
              onPress={pane ? () => openPane(pane.id) : undefined}
              subtitle={subtitle}
              title={`${item.index}: ${stripAgentStatusGlyphs(item.name)}`}
              trailing={pill ? <StatusPill state={pill} /> : undefined}
            />
          );
        }}
        style={styles.list}
      />
    </View>
  );
}

function waitForPane(paneId: string): Promise<void> {
  return new Promise((resolve) => {
    if (sessionStore.getState().panes[paneId]) return resolve();
    const timer = setTimeout(() => {
      unsubscribe();
      resolve();
    }, NEW_PANE_TOPOLOGY_WAIT_MS);
    const unsubscribe = sessionStore.subscribe((s) => {
      if (s.panes[paneId]) {
        clearTimeout(timer);
        unsubscribe();
        resolve();
      }
    });
  });
}

/** 48 dp action row on `--chrome-raised` (§9.4). */
function ActionRow({ glyph, label, onPress, disabled, busy }: { glyph: string; label: string; onPress: () => void; disabled?: boolean; busy?: boolean }) {
  return (
    <Pressable accessibilityRole="button" disabled={disabled} onPress={onPress} style={({ pressed }) => [styles.action, pressed && styles.pressed, disabled && !busy && styles.disabled]}>
      <Text style={styles.actionGlyph}>{glyph}</Text>
      <Text style={styles.actionLabel}>{label}</Text>
      {busy ? <ActivityIndicator color={colors.accent} size="small" /> : null}
    </Pressable>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
  list: { flex: 1 },
  action: {
    alignItems: "center",
    backgroundColor: colors.chromeRaised,
    borderBottomColor: colors.chromeHairline,
    borderBottomWidth: metrics.hairlineWidth,
    flexDirection: "row",
    gap: 12,
    height: metrics.actionRowHeight,
    paddingHorizontal: 16,
  },
  pressed: { backgroundColor: colors.chromeHover },
  disabled: { opacity: 0.5 },
  actionGlyph: { color: colors.accent, fontSize: 16, fontWeight: "700", width: 24, textAlign: "center" },
  actionLabel: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.rowTitle },
});
