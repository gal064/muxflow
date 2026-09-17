import { Redirect, useLocalSearchParams } from "expo-router";

import { FilesBrowser } from "../../../src/features/files/ui/FilesBrowser";
import { fromRouteParam, toRouteParam } from "../../../src/navigation/routeParams";

/**
 * Files, one directory below the root — design.md §9.6 (`dir?path=`).
 *
 * `path` is the exact string the host returned in `FileMetadata.path`; §7.5
 * forbids rebuilding it by joining. A deep link that omits it names no
 * directory, so it answers with the pane's active root instead.
 */
export default function FilesDirScreen() {
  const params = useLocalSearchParams<{ paneId: string; path?: string; name?: string }>();
  const paneId = fromRouteParam(params.paneId);
  const path = fromRouteParam(params.path);
  const name = fromRouteParam(params.name);
  if (!path) return <Redirect href={{ pathname: "/files/[paneId]", params: { paneId: toRouteParam(paneId) } }} />;
  return <FilesBrowser paneId={paneId} path={path} name={name} />;
}
