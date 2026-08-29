import * as Clipboard from "expo-clipboard";
import { StyleSheet, Text, View } from "react-native";

import { Button } from "../../ui/components/Button";
import { colors, fonts, radii, typeScale } from "../../ui/tokens";
import type { SshKeyHandle } from "./useSshKey";

export interface KeyCardProps {
  keyHandle: SshKeyHandle;
}

/**
 * The `SSH key` card of design.md §9.2: the public line with a `Copy` button,
 * or a `Generate key` button when this phone has no key yet.
 */
export function KeyCard({ keyHandle }: KeyCardProps) {
  const { publicKey, busy, error, generate } = keyHandle;
  return (
    <View style={styles.card}>
      <Text style={styles.title}>SSH key</Text>
      <Text style={styles.body}>
        Not needed over Tailscale SSH. Otherwise this phone&apos;s key is added to the host&apos;s
        ~/.ssh/authorized_keys.
      </Text>
      {publicKey === undefined ? (
        <Text style={styles.body}>Reading the key…</Text>
      ) : publicKey === null ? (
        <Button label={busy ? "Generating…" : "Generate key"} onPress={generate} disabled={busy} />
      ) : (
        <>
          <Text style={styles.key} selectable>
            {publicKey}
          </Text>
          <Button
            label="Copy"
            variant="secondary"
            onPress={() => void Clipboard.setStringAsync(publicKey)}
            style={styles.copy}
          />
        </>
      )}
      {error === null ? null : <Text style={styles.error}>{error}</Text>}
    </View>
  );
}

const styles = StyleSheet.create({
  card: {
    backgroundColor: colors.chromeRaised,
    borderRadius: radii.card,
    gap: 10,
    padding: 16,
  },
  title: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle, fontWeight: "600" },
  body: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, lineHeight: 18 },
  key: {
    backgroundColor: colors.chromeBg,
    borderRadius: radii.card,
    color: colors.chromeInk,
    fontFamily: fonts.mono,
    fontSize: typeScale.keyMono,
    lineHeight: 15,
    padding: 10,
  },
  copy: { alignSelf: "flex-start" },
  error: { color: colors.dangerInk, fontSize: typeScale.rowSecondary },
});
