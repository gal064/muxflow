import { router } from "expo-router";
import { useState } from "react";
import { StyleSheet, Text, View } from "react-native";
import { createStore } from "zustand/vanilla";
import { useStore } from "zustand";

import { connectHost, disconnectHost, getConnection } from "../../session/connectionManager";
import { findHost, hostAddress } from "../../store/hostsStore";
import { Button, Hairline } from "../../ui/components/Button";
import { Sheet } from "../../ui/components/Sheet";
import { colors, fonts, typeScale } from "../../ui/tokens";
import { connectionStateLabel } from "./connectionLabels";
import { useDiagnostics, useHosts, useSession } from "./hooks";
import { LogViewer } from "./LogViewer";

interface SheetState {
  visible: boolean;
  open(): void;
  close(): void;
}

const sheetStore = createStore<SheetState>((set) => ({
  visible: false,
  open: () => set({ visible: true }),
  close: () => set({ visible: false }),
}));

/**
 * Opens the §9.8 sheet from anywhere — the connection dot on Home, the strip's
 * "Details", the full-screen rows of §12. The sheet itself is mounted once, by
 * `ConnectionModals` in the root layout.
 */
export function useConnectionSheet(): SheetState {
  const visible = useStore(sheetStore, (state) => state.visible);
  const { open, close } = sheetStore.getState();
  return { visible, open, close };
}

/** Connection sheet — design.md §9.8. */
export function ConnectionSheet() {
  const { visible, close } = useConnectionSheet();
  const [logVisible, setLogVisible] = useState(false);
  const connection = useSession((state) => state.connection);
  const lastClose = useDiagnostics((state) => state.lastClose);
  const savedHost = useHosts((state) => findHost(state, connection.host?.id));
  const hello = getConnection()?.serverHello;
  const connected = connection.state === "connected";

  const reconnect = () => {
    if (!savedHost) return;
    close();
    connectHost(savedHost);
  };

  const disconnect = () => {
    close();
    disconnectHost();
    router.dismissTo("/");
  };

  return (
    <>
      <Sheet visible={visible} onDismiss={close} title="Connection">
        <Row label="Host" value={connection.host ? hostAddress(connection.host) : "—"} mono />
        <Hairline />
        <Row label="Helper" value={hello?.helperVersion || "—"} />
        <Hairline />
        <Row label="tmux" value={hello?.tmuxVersion || "—"} />
        <Hairline />
        <Row label="State" value={connectionStateLabel(connection.state)} />
        <Hairline />
        <Row label="Last error" value={connection.message ?? lastClose?.message ?? "—"} />
        <View style={styles.actions}>
          <Button label="Show log" variant="text" onPress={() => setLogVisible(true)} />
          <View style={styles.spacer} />
          {connected ? null : (
            <Button label="Reconnect now" variant="secondary" onPress={reconnect} disabled={!savedHost} />
          )}
          <Button label="Disconnect" variant="danger" onPress={disconnect} />
        </View>
      </Sheet>
      <LogViewer visible={logVisible} onDismiss={() => setLogVisible(false)} />
    </>
  );
}

function Row({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <View style={styles.row}>
      <Text style={styles.label}>{label}</Text>
      <Text style={[styles.value, mono ? styles.mono : null]} numberOfLines={3}>
        {value}
      </Text>
    </View>
  );
}

const styles = StyleSheet.create({
  row: { flexDirection: "row", gap: 16, paddingVertical: 12 },
  label: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, width: 88 },
  value: { color: colors.chromeInk, flex: 1, fontSize: typeScale.rowSecondary },
  mono: { fontFamily: fonts.mono },
  actions: { alignItems: "center", flexDirection: "row", flexWrap: "wrap", gap: 8, marginTop: 16 },
  spacer: { flex: 1 },
});
