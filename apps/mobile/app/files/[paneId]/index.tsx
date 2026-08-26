import { useLocalSearchParams } from "expo-router";

import { FilesBrowser } from "../../../src/features/files/ui/FilesBrowser";

/** Files, rooted at the pane's active root — design.md §9.6. */
export default function FilesRootScreen() {
  const { paneId } = useLocalSearchParams<{ paneId: string }>();
  return <FilesBrowser paneId={paneId} />;
}
