import * as Clipboard from "expo-clipboard";
import { Modal, Pressable, ScrollView, StyleSheet, Text, View } from "react-native";

import { Button } from "../../ui/components/Button";
import { colors, fonts, typeScale } from "../../ui/tokens";
import { diagnosticHeader } from "../../session/diagnosticSnapshot";
import { logText } from "../../session/log";
import { useLog } from "./hooks";

export interface LogViewerProps {
  visible: boolean;
  onDismiss: () => void;
}

/**
 * The process-local diagnostic flight recorder, copyable from the Connection
 * sheet. Nothing leaves the phone unless the user taps Copy.
 */
export function LogViewer({ visible, onDismiss }: LogViewerProps) {
  return (
    <Modal visible={visible} animationType="slide" onRequestClose={onDismiss} statusBarTranslucent>
      {visible ? <LogContents onDismiss={onDismiss} /> : null}
    </Modal>
  );
}

/** Unmounted while hidden, so normal diagnostic writes do not cause React renders. */
function LogContents({ onDismiss }: { onDismiss: () => void }) {
  const lines = useLog((state) => state.lines);
  const clear = useLog((state) => state.clear);

  return (
    <View style={styles.root}>
      <View style={styles.bar}>
        <Pressable onPress={onDismiss} accessibilityRole="button" hitSlop={12}>
          <Text style={styles.back}>{"←"}</Text>
        </Pressable>
        <Text style={styles.title}>Log</Text>
      </View>
      <ScrollView style={styles.scroll} contentContainerStyle={styles.content}>
        {lines.length === 0 ? (
          <Text style={styles.empty}>Nothing logged yet.</Text>
        ) : (
          lines.map((line, index) => (
            <Text key={`${index}-${line}`} style={styles.line} selectable>
              {line}
            </Text>
          ))
        )}
      </ScrollView>
      <View style={styles.actions}>
        <Button label="Clear" variant="text" onPress={clear} />
        <Button label="Copy" onPress={() => void Clipboard.setStringAsync(logText(undefined, diagnosticHeader()))} />
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
  bar: {
    alignItems: "center",
    backgroundColor: colors.chromeBg,
    flexDirection: "row",
    gap: 16,
    height: 56,
    paddingHorizontal: 16,
    paddingTop: 8,
  },
  back: { color: colors.chromeInkStrong, fontSize: 20 },
  title: { color: colors.chromeInkStrong, fontSize: typeScale.appBarTitle, fontWeight: "600" },
  scroll: { flex: 1 },
  content: { padding: 12 },
  empty: { color: colors.chromeDim, fontSize: typeScale.body },
  line: { color: colors.chromeInk, fontFamily: fonts.mono, fontSize: typeScale.keyMono, marginBottom: 2 },
  actions: {
    alignItems: "center",
    borderTopColor: colors.chromeHairline,
    borderTopWidth: StyleSheet.hairlineWidth,
    flexDirection: "row",
    gap: 12,
    justifyContent: "flex-end",
    padding: 12,
  },
});
