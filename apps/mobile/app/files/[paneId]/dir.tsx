import { useLocalSearchParams } from "expo-router";

import { FilesBrowser } from "../../../src/features/files/ui/FilesBrowser";

/**
 * Files, one directory below the root — design.md §9.6 (`dir?path=`).
 *
 * `path` is the exact string the host returned in `FileMetadata.path`; §7.5
 * forbids rebuilding it by joining.
 */
export default function FilesDirScreen() {
  const { paneId, path, name } = useLocalSearchParams<{ paneId: string; path: string; name?: string }>();
  return <FilesBrowser paneId={paneId} path={path} name={name} />;
}
