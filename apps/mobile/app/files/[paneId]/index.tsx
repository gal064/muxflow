import { useLocalSearchParams } from "expo-router";

import { FilesBrowser } from "../../../src/features/files/ui/FilesBrowser";
import { fromRouteParam } from "../../../src/navigation/routeParams";

/** Files, rooted at the pane's active root — design.md §9.6. */
export default function FilesRootScreen() {
  const params = useLocalSearchParams<{ paneId: string }>();
  const paneId = fromRouteParam(params.paneId);
  return <FilesBrowser paneId={paneId} />;
}
