import { useEffect, useRef, type ReactNode } from "react";
import { Platform, Pressable, StyleSheet, Text, View } from "react-native";

import { Sheet } from "../../../ui/components/Sheet";
import { colors, typeScale } from "../../../ui/tokens";
import type { AttachChoice } from "../attach/pickers";
import { CameraIcon, FileIcon, ImageIcon } from "./AttachIcons";

export interface AttachSheetProps {
  visible: boolean;
  onDismiss: () => void;
  /** One of the three was chosen; the sheet has been asked to close. */
  onChoose: (choice: AttachChoice) => void;
}

/** Longer than the sheet takes to slide away. */
const CLOSE_WAIT_MS = 700;

/** What the plus button opens (mobile-chat-mock.html, S7): where the file comes from. */
export function AttachSheet({ visible, onDismiss, onChoose }: AttachSheetProps) {
  // iOS shows one thing at a time: its picker opens once the sheet has closed,
  // or after a moment if the sheet never says that it has.
  const waiting = useRef<{ choice: AttachChoice; timer: ReturnType<typeof setTimeout> } | undefined>(undefined);
  useEffect(() => () => clearTimeout(waiting.current?.timer), []);
  const closed = (): void => {
    const held = waiting.current;
    if (!held) return;
    waiting.current = undefined;
    clearTimeout(held.timer);
    onChoose(held.choice);
  };
  const choose = (choice: AttachChoice): void => {
    onDismiss();
    if (Platform.OS !== "ios") return onChoose(choice);
    clearTimeout(waiting.current?.timer);
    waiting.current = { choice, timer: setTimeout(closed, CLOSE_WAIT_MS) };
  };
  return (
    <Sheet onClosed={closed} onDismiss={onDismiss} title="Attach" visible={visible}>
      <Choice icon={<ImageIcon color={colors.chromeInkStrong} size={20} />} label="Photo library" onPress={() => choose("library")} />
      <Choice icon={<CameraIcon color={colors.chromeInkStrong} size={20} />} label="Take a photo" onPress={() => choose("camera")} />
      <Choice icon={<FileIcon color={colors.chromeInkStrong} size={20} />} label="File" last onPress={() => choose("file")} />
    </Sheet>
  );
}

function Choice({ icon, label, last, onPress }: { icon: ReactNode; label: string; last?: boolean; onPress: () => void }) {
  return (
    <Pressable accessibilityLabel={label} accessibilityRole="button" onPress={onPress} style={({ pressed }) => [styles.row, last && styles.rowLast, pressed && styles.pressed]}>
      <View style={styles.icon}>{icon}</View>
      <Text style={styles.label}>{label}</Text>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  row: { alignItems: "center", borderBottomColor: colors.chromeHairline, borderBottomWidth: 1, flexDirection: "row", gap: 12, minHeight: 52, paddingHorizontal: 2 },
  rowLast: { borderBottomWidth: 0 },
  pressed: { opacity: 0.6 },
  icon: { alignItems: "center", width: 24 },
  label: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.body },
});
