import { router } from "expo-router";
import { StyleSheet, Text, View } from "react-native";

import { connectHost } from "../../session/connectionManager";
import { findHost, hostsStore } from "../../store/hostsStore";
import { Button } from "../../ui/components/Button";
import { colors, typeScale } from "../../ui/tokens";
import type { ConnectionErrorInfo } from "./errorMatrix";
import { useConnectionSheet } from "./ConnectionSheet";
import { useHosts } from "./hooks";

export interface ConnectionErrorScreenProps {
  failure: ConnectionErrorInfo;
  hostId: string | undefined;
}

/**
 * The full-screen rows of the error matrix (design.md §12): auth rejected,
 * helper missing, helper incompatible (§7.3), and the changed host key of
 * §9.10. Covers the screen it is mounted on.
 */
export function ConnectionErrorScreen({ failure, hostId }: ConnectionErrorScreenProps) {
  const host = useHosts((state) => findHost(state, hostId));
  const sheet = useConnectionSheet();

  const act = () => {
    switch (failure.action?.kind) {
      case "sshKey":
        router.push("/key");
        return;
      case "reconnect":
        if (host) connectHost(host);
        return;
      case "forgetHostKey":
        if (!host) return;
        // §9.10: forgetting the key is what lets the user re-trust — the next
        // connection presents the new fingerprint in the trust dialog.
        hostsStore.getState().setTrustedHostKeyFingerprint(host.id, null);
        connectHost(host);
    }
  };

  return (
    <View style={styles.root}>
      <Text style={styles.glyph}>{"!"}</Text>
      <Text style={styles.message}>{failure.message}</Text>
      <View style={styles.actions}>
        {failure.action ? (
          <Button label={failure.action.label} onPress={act} disabled={failure.action.kind !== "sshKey" && !host} />
        ) : null}
        <Button label="Details" variant="text" onPress={sheet.open} />
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  root: {
    bottom: 0, left: 0, position: "absolute", right: 0, top: 0,
    alignItems: "center",
    backgroundColor: colors.chromeBg,
    // The chrome is mounted first so the strip sits under the app bar, which
    // also puts this row behind its screen's content in paint order.
    elevation: 8,
    gap: 16,
    justifyContent: "center",
    padding: 32,
    zIndex: 10,
  },
  glyph: {
    color: colors.danger,
    fontSize: 28,
    fontWeight: "700",
  },
  message: { color: colors.chromeInk, fontSize: typeScale.body, lineHeight: 21, textAlign: "center" },
  actions: { alignItems: "center", gap: 4, marginTop: 8 },
});
