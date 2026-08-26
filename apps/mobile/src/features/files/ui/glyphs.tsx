// The three row glyphs §9.6 step 3 asks for, plus the symlink marker in step 4.
//
// Drawn from Views and text rather than an icon font: the app has no icon
// package, and three shapes do not justify one.

import { StyleSheet, Text, View } from "react-native";

import { colors, fonts } from "../../../ui/tokens";

export const GLYPH_COLUMN_WIDTH = 30;

export function FolderGlyph() {
  return (
    <View style={styles.column}>
      <View style={styles.folderTab} />
      <View style={styles.folderBody} />
    </View>
  );
}

export function MarkdownGlyph() {
  return (
    <View style={styles.column}>
      <Text style={styles.markdown} allowFontScaling={false}>
        M↓
      </Text>
    </View>
  );
}

export function FileGlyph() {
  return (
    <View style={styles.column}>
      <View style={styles.file} />
    </View>
  );
}

/** §9.6 step 4: symlinks are shown with a small link glyph. */
export function SymlinkGlyph() {
  return (
    <Text style={styles.symlink} allowFontScaling={false}>
      ↗
    </Text>
  );
}

const styles = StyleSheet.create({
  column: {
    alignItems: "center",
    height: 20,
    justifyContent: "center",
    width: GLYPH_COLUMN_WIDTH,
  },
  folderTab: {
    backgroundColor: colors.accent,
    borderTopLeftRadius: 2,
    borderTopRightRadius: 2,
    height: 3,
    marginBottom: -1,
    marginRight: 9,
    width: 8,
  },
  folderBody: {
    backgroundColor: colors.accent,
    borderRadius: 2,
    height: 12,
    width: 18,
  },
  file: {
    borderColor: colors.chromeFaint,
    borderRadius: 2,
    borderWidth: 1.5,
    height: 16,
    width: 13,
  },
  markdown: {
    color: colors.chromeInkStrong,
    fontFamily: fonts.mono,
    fontSize: 12,
    fontWeight: "700",
  },
  symlink: {
    color: colors.chromeFaint,
    fontSize: 12,
    marginLeft: 6,
  },
});
