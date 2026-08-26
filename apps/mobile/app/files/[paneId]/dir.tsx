import { Redirect, useLocalSearchParams } from "expo-router";

import { FilesBrowser } from "../../../src/features/files/ui/FilesBrowser";

/**
 * Files, one directory below the root — design.md §9.6 (`dir?path=`).
 *
 * `path` is the exact string the host returned in `FileMetadata.path`; §7.5
 * forbids rebuilding it by joining. A deep link that omits it names no
 * directory, so it answers with the pane's active root instead.
 */
export default function FilesDirScreen() {
  const { paneId, path, name } = useLocalSearchParams<{ paneId: string; path?: string; name?: string }>();
  if (!path) return <Redirect href={{ pathname: "/files/[paneId]", params: { paneId } }} />;
  return <FilesBrowser paneId={paneId} path={path} name={name} />;
}
