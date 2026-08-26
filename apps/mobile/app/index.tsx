import { router, Stack } from "expo-router";
import { useEffect, useState } from "react";
import { FlatList, Pressable, StyleSheet, Text, View } from "react-native";

import { ConnectionChrome } from "../src/features/hosts/ConnectionChrome";
import { ConnectionDot } from "../src/features/hosts/ConnectionDot";
import { useHosts } from "../src/features/hosts/hooks";
import { connectHost, getConnectedHost } from "../src/session/connectionManager";
import { hostAddress, hostsStore, type SavedHost } from "../src/store/hostsStore";
import { sessionStore } from "../src/store/sessionStore";
import { Button, Hairline } from "../src/ui/components/Button";
import { Dialog } from "../src/ui/components/Dialog";
import { Sheet } from "../src/ui/components/Sheet";
import { colors, metrics, typeScale } from "../src/ui/tokens";

/** Hosts — design.md §9.1. The first screen when no host is connected. */
export default function HostsScreen() {
  const hosts = useHosts((state) => state.hosts);
  const hydrated = useHosts((state) => state.hydrated);
  const [actionsFor, setActionsFor] = useState<SavedHost | null>(null);
  const [forgetting, setForgetting] = useState<SavedHost | null>(null);

  useEffect(() => {
    void hostsStore.getState().hydrate();
  }, []);
  useAutoConnect(hydrated);

  const connect = (host: SavedHost) => {
    // A cold start already connected to `lastHostId` (§12); tapping its row
    // must not tear that connection down and dial it again.
    if (!isLive(host.id)) connectHost(host);
    // §9.1: navigate as soon as the state is `sshConnecting`, which `connect()`
    // sets synchronously.
    router.push("/home");
  };

  return (
    <View style={styles.root}>
      <Stack.Screen options={{ headerRight: () => <ConnectionDot /> }} />
      <ConnectionChrome returnOnFailure />
      {hosts.length === 0 ? (
        <EmptyState />
      ) : (
        <FlatList
          data={hosts}
          keyExtractor={(host) => host.id}
          ItemSeparatorComponent={Hairline}
          renderItem={({ item }) => (
            <HostRow host={item} onPress={() => connect(item)} onLongPress={() => setActionsFor(item)} />
          )}
          ListFooterComponent={<KeyLink />}
        />
      )}

      <Pressable
        accessibilityLabel="Add host"
        accessibilityRole="button"
        onPress={() => router.push("/hosts/new")}
        style={({ pressed }) => [styles.fab, pressed ? styles.fabPressed : null]}
      >
        <Text style={styles.fabLabel}>+</Text>
      </Pressable>

      <Sheet visible={actionsFor !== null} onDismiss={() => setActionsFor(null)} title={actionsFor?.label}>
        <SheetAction
          label="Edit"
          onPress={() => {
            const host = actionsFor;
            setActionsFor(null);
            if (host) router.push({ pathname: "/hosts/[id]", params: { id: host.id } });
          }}
        />
        <Hairline />
        <SheetAction
          label="Forget host"
          danger
          onPress={() => {
            setForgetting(actionsFor);
            setActionsFor(null);
          }}
        />
      </Sheet>

      <Dialog
        visible={forgetting !== null}
        title="Forget host"
        message={`Forget ${forgetting?.label ?? ""}? The host key trust and connection history for this host are removed. The SSH key on this phone is kept.`}
        onDismiss={() => setForgetting(null)}
        actions={[
          { label: "Cancel", onPress: () => setForgetting(null), variant: "secondary" },
          {
            label: "Forget",
            variant: "danger",
            onPress: () => {
              if (forgetting) hostsStore.getState().removeHost(forgetting.id);
              setForgetting(null);
            },
          },
        ]}
      />
    </View>
  );
}

function HostRow({
  host,
  onPress,
  onLongPress,
}: {
  host: SavedHost;
  onPress: () => void;
  onLongPress: () => void;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      onPress={onPress}
      onLongPress={onLongPress}
      style={({ pressed }) => [styles.row, pressed ? styles.rowPressed : null]}
    >
      <View style={styles.rowText}>
        <Text style={styles.rowTitle} numberOfLines={1}>
          {host.label}
        </Text>
        <Text style={styles.rowSubtitle} numberOfLines={1}>
          {hostAddress(host)}
        </Text>
      </View>
      <Text style={styles.chevron}>{"›"}</Text>
    </Pressable>
  );
}

function EmptyState() {
  return (
    <View style={styles.empty}>
      <Text style={styles.glyph}>{">_"}</Text>
      <Text style={styles.heading}>No hosts yet</Text>
      <Text style={styles.body}>
        Add the machine where Muxflow desktop runs. The app connects over SSH, the same way you
        would from a terminal.
      </Text>
      <Button label="Add host" onPress={() => router.push("/hosts/new")} style={styles.emptyButton} />
      <KeyLink />
    </View>
  );
}

function KeyLink() {
  return (
    <Pressable onPress={() => router.push("/key")} accessibilityRole="link" style={styles.keyLink}>
      <Text style={styles.keyLinkLabel}>Your SSH key</Text>
    </Pressable>
  );
}

function SheetAction({ label, onPress, danger = false }: { label: string; onPress: () => void; danger?: boolean }) {
  return (
    <Pressable onPress={onPress} accessibilityRole="button" style={styles.sheetAction}>
      <Text style={[styles.sheetActionLabel, danger ? styles.sheetActionDanger : null]}>{label}</Text>
    </Pressable>
  );
}

/** True while this host owns a connection that is up or on its way up. */
function isLive(hostId: string): boolean {
  if (getConnectedHost()?.id !== hostId) return false;
  const { state } = sessionStore.getState().connection;
  return state !== "idle" && state !== "failed" && state !== "incompatible";
}

/**
 * §12's last row: after Android kills the process, `lastHostId` is still set,
 * so a cold start reconnects on its own. Once per process.
 */
let autoConnected = false;
function useAutoConnect(hydrated: boolean): void {
  useEffect(() => {
    if (!hydrated || autoConnected) return;
    autoConnected = true;
    const { hosts, lastHostId } = hostsStore.getState();
    const host = hosts.find((candidate) => candidate.id === lastHostId);
    if (!host || sessionStore.getState().connection.state !== "idle") return;
    connectHost(host);
  }, [hydrated]);
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
  row: {
    alignItems: "center",
    flexDirection: "row",
    gap: 12,
    height: metrics.hostRowHeight,
    paddingHorizontal: 16,
  },
  rowPressed: { backgroundColor: colors.chromeHover },
  rowText: { flex: 1, gap: 2 },
  rowTitle: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle },
  rowSubtitle: { color: colors.chromeDim, fontSize: typeScale.rowSecondary },
  chevron: { color: colors.chromeFaint, fontSize: 22 },
  empty: { alignItems: "center", flex: 1, gap: 12, justifyContent: "center", padding: 32 },
  glyph: { color: colors.chromeFaint, fontSize: 40 },
  heading: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle, fontWeight: "600" },
  body: { color: colors.chromeDim, fontSize: typeScale.body, lineHeight: 20, textAlign: "center" },
  emptyButton: { marginTop: 12 },
  keyLink: { alignItems: "center", padding: 20 },
  keyLinkLabel: { color: colors.accent, fontSize: typeScale.meta },
  fab: {
    alignItems: "center",
    backgroundColor: colors.accent,
    borderRadius: 28,
    bottom: 24,
    height: 56,
    justifyContent: "center",
    position: "absolute",
    right: 24,
    width: 56,
  },
  fabPressed: { opacity: 0.8 },
  fabLabel: { color: colors.accentInk, fontSize: 30, lineHeight: 34 },
  sheetAction: { paddingVertical: 16 },
  sheetActionLabel: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle },
  sheetActionDanger: { color: colors.dangerInk },
});
