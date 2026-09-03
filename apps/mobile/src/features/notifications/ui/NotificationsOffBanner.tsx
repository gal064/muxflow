import { useStore } from "zustand";
import { Linking, Pressable, StyleSheet, Text, View } from "react-native";

import { notificationsUiStore, showNotificationsOffBanner } from "../permissionStore";
import { colors, radii, typeScale } from "../../../ui/tokens";

/**
 * §13: "if denied, show once on the Agents tab a dismissible banner
 * `Notifications are off. You won't hear when an agent needs you.` with
 * `Open settings`." Copy is final.
 */
export function NotificationsOffBanner() {
  const shown = useStore(notificationsUiStore, showNotificationsOffBanner);
  const dismiss = useStore(notificationsUiStore, (state) => state.dismissBanner);
  if (!shown) return null;
  return (
    <View style={styles.root}>
      <Text style={styles.body}>Notifications are off. You won&apos;t hear when an agent needs you.</Text>
      <View style={styles.actions}>
        <Pressable hitSlop={8} onPress={() => void Linking.openSettings()}>
          <Text style={styles.action}>Open settings</Text>
        </Pressable>
        <Pressable hitSlop={8} onPress={dismiss}>
          <Text style={styles.dismiss}>Dismiss</Text>
        </Pressable>
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  root: {
    backgroundColor: colors.dangerWash,
    borderRadius: radii.card,
    gap: 8,
    margin: 12,
    padding: 12,
  },
  body: { color: colors.chromeInk, fontSize: typeScale.body },
  actions: { flexDirection: "row", gap: 20 },
  action: { color: colors.accent, fontSize: typeScale.body, fontWeight: "600" },
  dismiss: { color: colors.chromeDim, fontSize: typeScale.body, fontWeight: "600" },
});
