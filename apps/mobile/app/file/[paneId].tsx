import { useLocalSearchParams } from "expo-router";

import { FileViewer } from "../../src/features/files/ui/FileViewer";

/** File viewer — design.md §9.7 (`?path=&name=`). */
export default function FileViewerScreen() {
  const { paneId, path, name } = useLocalSearchParams<{ paneId: string; path: string; name?: string }>();
  return <FileViewer paneId={paneId} path={path} name={name ?? lastSegment(path)} />;
}

function lastSegment(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash === -1 ? path : path.slice(slash + 1);
}
