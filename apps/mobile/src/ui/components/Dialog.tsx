import type { ReactNode } from "react";
import { Modal, Pressable, StyleSheet, Text, View } from "react-native";

import { colors, radii, typeScale } from "../tokens";
import { Button, type ButtonVariant } from "./Button";

export interface DialogAction {
  label: string;
  onPress: () => void;
  variant?: ButtonVariant;
}

export interface DialogProps {
  visible: boolean;
  title: string;
  /** Plain body copy; pass `children` instead when the body needs its own layout. */
  message?: string;
  children?: ReactNode;
  actions: DialogAction[];
  onDismiss: () => void;
}

/** A centred modal dialog: §9.10's host key prompt and every confirmation in §9.1/§9.9. */
export function Dialog({ visible, title, message, children, actions, onDismiss }: DialogProps) {
  return (
    <Modal visible={visible} transparent animationType="fade" onRequestClose={onDismiss} statusBarTranslucent>
      <Pressable style={styles.backdrop} onPress={onDismiss} accessibilityLabel="Close" />
      <View style={styles.wrapper} pointerEvents="box-none">
        <View style={styles.card}>
          <Text style={styles.title}>{title}</Text>
          {message === undefined ? null : <Text style={styles.message}>{message}</Text>}
          {children}
          <View style={styles.actions}>
            {actions.map((action) => (
              <Button
                key={action.label}
                label={action.label}
                onPress={action.onPress}
                variant={action.variant ?? "secondary"}
                style={styles.action}
              />
            ))}
          </View>
        </View>
      </View>
    </Modal>
  );
}

const styles = StyleSheet.create({
  backdrop: { backgroundColor: "rgba(0, 0, 0, 0.5)", bottom: 0, left: 0, position: "absolute", right: 0, top: 0 },
  wrapper: { alignItems: "center", flex: 1, justifyContent: "center", padding: 24 },
  card: {
    backgroundColor: colors.chromeRaised,
    borderRadius: radii.sheet,
    gap: 12,
    maxWidth: 420,
    padding: 20,
    width: "100%",
  },
  title: { color: colors.chromeInkStrong, fontSize: typeScale.appBarTitle, fontWeight: "600" },
  message: { color: colors.chromeInk, fontSize: typeScale.body, lineHeight: 20 },
  actions: { flexDirection: "row", gap: 8, justifyContent: "flex-end", marginTop: 4 },
  action: { minWidth: 96 },
});
