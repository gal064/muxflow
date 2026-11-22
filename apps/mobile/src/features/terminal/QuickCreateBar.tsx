// New agent | New terminal, both in the `random` workspace
// (docs/design/quick-session-and-shortcuts-plan.md §4.3). One rounded
// rectangle floating 16 above the tab bar on the Agents and Workspaces tabs;
// both lists end with `metrics.quickCreateClearance` so nothing hides under it.

import { useRouter } from "expo-router";
import { useCallback } from "react";
import { ActivityIndicator, Pressable, StyleSheet, Text, useWindowDimensions, View } from "react-native";
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";

import { toRouteParam } from "../../navigation/routeParams";
import { getConnection, toast } from "../../session/connectionManager";
import { log } from "../../session/log";
import { defaultDirectoryOf, findHost, hostsStore } from "../../store/hostsStore";
import { prefsStore } from "../../store/prefsStore";
import { sessionStore } from "../../store/sessionStore";
import { useSession } from "../../ui/hooks";
import { colors, fixedChromeText, metrics, radii, typeScale } from "../../ui/tokens";
import { isConnectionScopeCurrent, MissingAgentCommandError, openInRandom, RANDOM_WORKSPACE, type CreateWindowKind } from "./createWindow";
import { waitForPane } from "./WorkspaceActionsSheet";

const NEW_PANE_TOPOLOGY_WAIT_MS = 3_000;

/** One create at a time across every bar: the Agents and Workspaces tabs both stay mounted. */
const inFlight = createStore<{ creating: CreateWindowKind | null }>(() => ({ creating: null }));

export function QuickCreateBar() {
  const router = useRouter();
  const { width } = useWindowDimensions();
  const connected = useSession((state) => state.connection.state === "connected");
  const agentCommand = useStore(prefsStore, (state) => state.agentCommand);
  const creating = useStore(inFlight, (state) => state.creating);

  const open = useCallback(async (kind: CreateWindowKind) => {
    const connection = getConnection();
    log(`create.random ui.press kind=${kind} connection=${connection ? "present" : "missing"} alreadyCreating=${inFlight.getState().creating ? "yes" : "no"}`);
    if (!connection || inFlight.getState().creating) return;
    if (kind === "agent" && agentCommand.trim().length === 0) {
      toast("Set an agent command first.");
      router.push("/settings");
      return;
    }
    const hostId = sessionStore.getState().connection.host?.id;
    const directory = defaultDirectoryOf(findHost(hostsStore.getState(), hostId));
    inFlight.setState({ creating: kind });
    try {
      const opened = await openInRandom(connection, sessionStore, { kind, directory, command: agentCommand, getCurrentConnection: getConnection });
      void opened.commandDelivery?.then((delivery) => {
        if (!delivery.ok) toast(`Terminal opened, but the agent command could not be sent: ${delivery.error.message}`);
      });
      const paneObserved = await waitForPane(opened.paneId, NEW_PANE_TOPOLOGY_WAIT_MS);
      if (!isConnectionScopeCurrent(opened.scope, getConnection)) {
        log(`create.random ui.navigation-skipped kind=${kind} reason=connection_scope_changed pane=${opened.paneId}`);
        return;
      }
      log(`create.random ui.navigate kind=${kind} session=${opened.sessionId} pane=${opened.paneId} created=${opened.createdWorkspace ? "yes" : "no"} paneObserved=${paneObserved ? "yes" : "no"}`);
      router.push({ pathname: "/terminal/[paneId]", params: { paneId: toRouteParam(opened.paneId), sessionId: toRouteParam(opened.sessionId) } });
    } catch (error) {
      if (error instanceof MissingAgentCommandError) return;
      log(`create.random ui.failed kind=${kind} code=${error instanceof Error ? error.name : typeof error}`);
      toast(`Couldn't open ${kind === "agent" ? "an agent" : "a terminal"}: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      inFlight.setState({ creating: null });
    }
  }, [agentCommand, router]);

  const disabled = !connected || creating !== null;
  return (
    <View pointerEvents="box-none" style={styles.anchor}>
      <View style={[styles.bar, { width: Math.min(metrics.quickCreateBarWidth, width - 32) }, disabled && styles.disabled]}>
        <Half
          accessibilityLabel={`New agent in the ${RANDOM_WORKSPACE} workspace`}
          busy={creating === "agent"}
          disabled={disabled}
          label="New agent"
          onPress={() => void open("agent")}
          testID="quick-new-agent"
        />
        <View style={styles.divider} />
        <Half
          accessibilityLabel={`New terminal in the ${RANDOM_WORKSPACE} workspace`}
          busy={creating === "terminal"}
          disabled={disabled}
          label="New terminal"
          onPress={() => void open("terminal")}
          testID="quick-new-terminal"
        />
      </View>
    </View>
  );
}

function Half({ label, accessibilityLabel, testID, busy, disabled, onPress }: { label: string; accessibilityLabel: string; testID: string; busy: boolean; disabled: boolean; onPress(): void }) {
  return (
    <Pressable
      accessibilityLabel={accessibilityLabel}
      accessibilityRole="button"
      accessibilityState={{ busy, disabled }}
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [styles.half, pressed && styles.pressed]}
      testID={testID}
    >
      {busy ? <ActivityIndicator color={colors.accentInk} size="small" /> : <Text {...fixedChromeText} numberOfLines={1} style={styles.label}>{label}</Text>}
    </Pressable>
  );
}

const styles = StyleSheet.create({
  anchor: { alignItems: "center", bottom: metrics.quickCreateBarGap, left: 0, position: "absolute", right: 0 },
  bar: {
    backgroundColor: colors.accent,
    borderRadius: radii.quickCreateBar,
    elevation: 4,
    flexDirection: "row",
    height: metrics.quickCreateBarHeight,
    overflow: "hidden",
    shadowColor: "#000000",
    shadowOffset: { height: 2, width: 0 },
    shadowOpacity: 0.25,
    shadowRadius: 6,
  },
  half: { alignItems: "center", flex: 1, justifyContent: "center", paddingHorizontal: 8 },
  divider: { alignSelf: "stretch", backgroundColor: colors.accentInk, marginVertical: 12, opacity: 0.25, width: StyleSheet.hairlineWidth * 2 },
  label: { color: colors.accentInk, fontSize: typeScale.body, fontWeight: "600" },
  pressed: { opacity: 0.75 },
  disabled: { opacity: 0.45 },
});
