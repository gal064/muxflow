import { PlaceholderScreen } from "../../src/ui/components/PlaceholderScreen";

/** File viewer — design.md §9.7 (`?path=`). */
export default function FileViewerScreen() {
  return <PlaceholderScreen title="File" route="/file/[paneId]?path=" />;
}
