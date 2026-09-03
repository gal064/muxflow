import * as Clipboard from "expo-clipboard";
import { useState } from "react";
import { ScrollView, StyleSheet, Text, View } from "react-native";

import { useSshKey } from "../src/features/hosts/useSshKey";
import { Button } from "../src/ui/components/Button";
import { Dialog } from "../src/ui/components/Dialog";
import { colors, fonts, radii, typeScale } from "../src/ui/tokens";

/** Your SSH key — design.md §9.9. */
export default function KeyScreen() {
  const { publicKey, busy, error, generate } = useSshKey();
  const [confirming, setConfirming] = useState(false);

  return (
    <ScrollView style={styles.root} contentContainerStyle={styles.content}>
      <View style={styles.keyBox}>
        {publicKey === undefined ? (
          <Text style={styles.dim}>Reading the key…</Text>
        ) : publicKey === null ? (
          <Text style={styles.dim}>
            This phone has no SSH key. Hosts on your tailnet with Tailscale SSH don&apos;t need one;
            generate a key for any other host.
          </Text>
        ) : (
          <Text style={styles.key} selectable>
            {publicKey}
          </Text>
        )}
      </View>

      <View style={styles.actions}>
        <Button
          label="Copy"
          onPress={() => {
            if (publicKey) void Clipboard.setStringAsync(publicKey);
          }}
          disabled={!publicKey}
        />
        <Button
          label={publicKey === null ? "Generate key" : "Regenerate key"}
          variant={publicKey === null ? "secondary" : "danger"}
          disabled={busy}
          onPress={() => {
            if (publicKey === null) generate();
            else setConfirming(true);
          }}
        />
      </View>
      {error === null ? null : <Text style={styles.error}>{error}</Text>}

      <View style={styles.steps}>
        <Text style={styles.step}>Over Tailscale SSH the phone signs in with its tailnet identity — skip these steps.</Text>
        <Text style={styles.step}>1. Copy the key.</Text>
        <Text style={styles.step}>2. On the host, append it to ~/.ssh/authorized_keys.</Text>
        <Text style={styles.step}>3. Make sure Muxflow desktop has installed the helper on that host.</Text>
      </View>

      <Dialog
        visible={confirming}
        title="Regenerate key"
        message="Regenerate the key? Hosts that trust the current key will stop accepting this phone until you add the new key."
        onDismiss={() => setConfirming(false)}
        actions={[
          { label: "Cancel", onPress: () => setConfirming(false), variant: "secondary" },
          {
            label: "Regenerate",
            variant: "danger",
            onPress: () => {
              setConfirming(false);
              generate();
            },
          },
        ]}
      />
    </ScrollView>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
  content: { gap: 20, padding: 16 },
  keyBox: { backgroundColor: colors.chromeRaised, borderRadius: radii.card, padding: 14 },
  key: { color: colors.chromeInk, fontFamily: fonts.mono, fontSize: typeScale.keyMono, lineHeight: 16 },
  dim: { color: colors.chromeDim, fontSize: typeScale.body },
  actions: { flexDirection: "row", gap: 12 },
  error: { color: colors.dangerInk, fontSize: typeScale.rowSecondary },
  steps: { gap: 8 },
  step: { color: colors.chromeDim, fontSize: typeScale.body, lineHeight: 20 },
});
