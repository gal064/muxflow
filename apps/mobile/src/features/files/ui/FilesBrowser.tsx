// The Files screen (design doc §9.6), shared by `/files/[paneId]` and
// `/files/[paneId]/dir`. The only difference between the two is which directory
// they list: the pane's active root, or the path they were pushed with.

import { useCallback } from "react";
import { Stack, useRouter } from "expo-router";
import { FlatList, Pressable, StyleSheet, Text, View } from "react-native";

import { FileKind } from "../../../protocol/gen/envelope_pb";
import { colors, metrics, typeScale } from "../../../ui/tokens";
import { relativeToRoot, rootTitle } from "../activeRoot";
import { formatSize, type DirectoryEntry } from "../entries";
import { FILES_COPY } from "../presentation";
import { useDirectory } from "../useDirectory";
import { FileGlyph, FolderGlyph, MarkdownGlyph, SymlinkGlyph } from "./glyphs";
import { CentredMessage, CentredSpinner, ErrorState, TitleBlock } from "./parts";

export interface FilesBrowserProps {
  paneId: string;
  /** The directory to list; omitted on the root screen, which lists the active root. */
  path?: string | undefined;
  /** The last path segment, when the caller already knows it (the row that was tapped). */
  name?: string | undefined;
}

export function FilesBrowser({ paneId, path, name }: FilesBrowserProps) {
  const router = useRouter();
  const { view, reload } = useDirectory(paneId, path);
  const root = view.status === "ready" ? view.root : undefined;

  const title = path === undefined ? (root ? rootTitle(root.root) : "Files") : (name ?? rootTitle(path));
  const subtitle = path === undefined ? (root?.root ?? "") : root ? relativeToRoot(root.root, path) : path;

  const open = useCallback(
    (entry: DirectoryEntry) => {
      switch (entry.action) {
        case "openDirectory":
          router.push({ pathname: "/files/[paneId]/dir", params: { paneId, path: entry.path, name: entry.name } });
          return;
        case "openFile":
          router.push({ pathname: "/file/[paneId]", params: { paneId, path: entry.path, name: entry.name } });
          return;
        default:
          // §9.6 step 4: an unresolved symlink, a socket or a device does nothing.
          return;
      }
    },
    [paneId, router],
  );

  return (
    <View style={styles.root}>
      <Stack.Screen options={{ headerTitle: () => <TitleBlock title={title} subtitle={subtitle} /> }} />
      {view.status === "loading" ? <CentredSpinner /> : null}
      {view.status === "error" ? <ErrorState message={view.message} onRetry={reload} /> : null}
      {view.status === "ready" ? (
        view.listing.entries.length === 0 ? (
          <CentredMessage message={FILES_COPY.empty} />
        ) : (
          <FlatList
            data={view.listing.entries}
            keyExtractor={(entry) => entry.path}
            renderItem={({ item }) => <EntryRow entry={item} onPress={open} />}
            style={styles.list}
          />
        )
      ) : null}
    </View>
  );
}

function EntryRow({ entry, onPress }: { entry: DirectoryEntry; onPress: (entry: DirectoryEntry) => void }) {
  const directory = entry.kind === FileKind.DIRECTORY;
  const inert = entry.action === "none";
  return (
    <Pressable
      accessibilityRole="button"
      disabled={inert}
      onPress={() => onPress(entry)}
      style={({ pressed }) => [styles.row, pressed && !inert && styles.rowPressed]}
    >
      {directory ? <FolderGlyph /> : entry.markdown ? <MarkdownGlyph /> : <FileGlyph />}
      <View style={styles.nameColumn}>
        <Text
          style={[styles.name, entry.markdown ? styles.nameStrong : null, inert ? styles.nameInert : null]}
          numberOfLines={1}
          ellipsizeMode="middle"
        >
          {entry.name}
        </Text>
        {entry.symlink ? <SymlinkGlyph /> : null}
      </View>
      {directory ? (
        <Text style={styles.chevron} allowFontScaling={false}>
          ›
        </Text>
      ) : (
        <Text style={styles.size}>{formatSize(entry.size)}</Text>
      )}
    </Pressable>
  );
}

const styles = StyleSheet.create({
  root: {
    backgroundColor: colors.chromeBg,
    flex: 1,
  },
  list: {
    flex: 1,
  },
  row: {
    alignItems: "center",
    flexDirection: "row",
    height: metrics.fileRowHeight,
    paddingLeft: 12,
    paddingRight: 16,
  },
  rowPressed: {
    backgroundColor: colors.chromeHover,
  },
  nameColumn: {
    alignItems: "center",
    flexDirection: "row",
    flexShrink: 1,
    flexGrow: 1,
  },
  name: {
    color: colors.chromeInk,
    flexShrink: 1,
    fontSize: typeScale.rowTitle,
  },
  nameStrong: {
    color: colors.chromeInkStrong,
  },
  nameInert: {
    color: colors.chromeFaint,
  },
  size: {
    color: colors.chromeDim,
    fontSize: typeScale.meta,
    marginLeft: 12,
  },
  chevron: {
    color: colors.chromeFaint,
    fontSize: 20,
    marginLeft: 12,
  },
});
