import { useRef } from "react";
import { Image, Pressable, ScrollView, StyleSheet, Text, View, type ScrollViewInstance } from "react-native";

import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";
import type { AttachmentEntry } from "../attach/attachmentQueue";
import { chipLine, isImage, uploadShare } from "../attach/attachState";
import { CloseIcon, FileIcon, ImageIcon } from "./AttachIcons";

export interface AttachStripProps {
  /** The files waiting on the reply field, in the order they were added; none draws nothing. */
  entries: readonly AttachmentEntry[];
  /** The cross on a chip: takes the file away, stopping its upload. */
  onRemove: (id: string) => void;
}

/**
 * The chips above the reply field (mobile-chat-mock.html, S7): a thumbnail
 * for an image, a mark and the name for any other file, a bar while it
 * uploads, and why when it could not be sent. Each has a cross that takes it
 * away.
 */
export function AttachStrip({ entries, onRemove }: AttachStripProps) {
  const strip = useRef<ScrollViewInstance>(null);
  // A chip just added is at the end of the row: the row is taken to it, so
  // its upload, or why it was refused, is in view.
  const count = useRef(entries.length);
  const grown = entries.length > count.current;
  count.current = entries.length;
  const toAdded = useRef(false);
  if (grown) toAdded.current = true;
  if (entries.length === 0) return null;
  return (
    <ScrollView
      contentContainerStyle={styles.strip}
      horizontal
      keyboardShouldPersistTaps="always"
      onContentSizeChange={() => {
        if (!toAdded.current) return;
        toAdded.current = false;
        strip.current?.scrollToEnd({ animated: true });
      }}
      ref={strip}
      showsHorizontalScrollIndicator={false}
      style={styles.scroll}
    >
      {entries.map((entry) => <Chip entry={entry} key={entry.id} onRemove={onRemove} />)}
    </ScrollView>
  );
}

function Chip({ entry, onRemove }: { entry: AttachmentEntry; onRemove: (id: string) => void }) {
  const failed = entry.status === "failed";
  const moving = entry.status === "queued" || entry.status === "uploading";
  const line = chipLine(entry);
  const label = `${entry.name}, ${line}`;
  // A picture that can go is its own thumbnail. One that failed needs the room for the reason.
  const thumbnail = !failed && entry.localUri !== undefined && isImage(entry);
  const Mark = isImage(entry) ? ImageIcon : FileIcon;
  return (
    <View style={[styles.chip, failed && styles.chipFailed]}>
      {thumbnail ? (
        <Image accessibilityLabel={label} accessible resizeMethod="resize" resizeMode="cover" source={{ uri: entry.localUri }} style={styles.thumb} />
      ) : (
        <View accessibilityLabel={label} accessible style={styles.body}>
          <Mark color={failed ? colors.dangerInk : colors.chromeDim} size={18} />
          <View style={styles.text}>
            <Text {...fixedChromeText} ellipsizeMode="middle" numberOfLines={1} style={styles.name}>{entry.name}</Text>
            <Text {...fixedChromeText} numberOfLines={failed ? 2 : 1} style={[styles.line, failed && styles.lineFailed]}>{line}</Text>
          </View>
        </View>
      )}
      <Pressable
        accessibilityLabel={`Remove ${entry.name}`}
        accessibilityRole="button"
        hitSlop={REMOVE_SLOP}
        onPress={() => onRemove(entry.id)}
        style={({ pressed }) => [styles.remove, pressed && styles.pressed]}
      >
        <CloseIcon color={colors.chromeFaint} size={14} />
      </Pressable>
      {moving ? <View style={[styles.bar, { width: `${Math.round(uploadShare(entry) * 100)}%` }]} /> : null}
    </View>
  );
}

const CHIP = 48;
const REMOVE = 36;
// The cross's column is the chip's full height and narrower than a touch
// target. The slop reaches over the chip's own body, which takes no touch: a
// slop past the chip's edge would not be honoured on Android.
const REMOVE_SLOP = { left: 44 - REMOVE } as const;

const styles = StyleSheet.create({
  scroll: { flexGrow: 0 },
  strip: { alignItems: "flex-start", gap: 6, paddingHorizontal: 9, paddingTop: 8 },
  pressed: { opacity: 0.6 },
  chip: { alignItems: "stretch", borderColor: colors.chromeBorder, borderRadius: 8, borderWidth: 1, flexDirection: "row", maxWidth: 240, minHeight: CHIP, overflow: "hidden" },
  chipFailed: { backgroundColor: colors.dangerWash, borderColor: colors.danger, maxWidth: 280 },
  thumb: { backgroundColor: colors.chromeRaised, height: CHIP, width: CHIP },
  body: { alignItems: "center", flexDirection: "row", flexShrink: 1, gap: 7, paddingLeft: 9, paddingVertical: 5 },
  text: { flexShrink: 1 },
  name: { color: colors.chromeInk, fontSize: typeScale.meta, lineHeight: 16 },
  line: { color: colors.chromeFaint, fontSize: typeScale.keyMono, lineHeight: 15 },
  lineFailed: { color: colors.dangerInk },
  remove: { alignItems: "center", justifyContent: "center", width: REMOVE },
  bar: { backgroundColor: colors.accent, bottom: 0, height: 2, left: 0, position: "absolute" },
});
