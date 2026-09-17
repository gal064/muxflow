import type { ReactNode } from "react";
import { Modal, Pressable, StyleSheet, Text, View } from "react-native";

import { colors, radii, typeScale } from "../tokens";

export interface SheetProps {
  visible: boolean;
  onDismiss: () => void;
  title?: string;
  children: ReactNode;
}

/** A bottom sheet: 12 dp corners (§10.1), tap outside to dismiss. */
export function Sheet({ visible, onDismiss, title, children }: SheetProps) {
  return (
    <Modal visible={visible} transparent animationType="slide" onRequestClose={onDismiss} statusBarTranslucent>
      <Pressable style={styles.backdrop} onPress={onDismiss} accessibilityLabel="Close" />
      <View style={styles.sheet}>
        <View style={styles.grabber} />
        {title === undefined ? null : <Text style={styles.title}>{title}</Text>}
        {children}
      </View>
    </Modal>
  );
}

const styles = StyleSheet.create({
  backdrop: { backgroundColor: "rgba(0, 0, 0, 0.5)", flex: 1 },
  sheet: {
    backgroundColor: colors.chromeRaised,
    borderTopLeftRadius: radii.sheet,
    borderTopRightRadius: radii.sheet,
    gap: 4,
    paddingBottom: 28,
    paddingHorizontal: 16,
    paddingTop: 8,
  },
  grabber: {
    alignSelf: "center",
    backgroundColor: colors.chromeBorder,
    borderRadius: 2,
    height: 4,
    marginBottom: 8,
    width: 36,
  },
  title: {
    color: colors.chromeInkStrong,
    fontSize: typeScale.appBarTitle,
    fontWeight: "600",
    paddingBottom: 8,
  },
});
