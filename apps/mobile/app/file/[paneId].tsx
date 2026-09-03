import { useLocalSearchParams } from "expo-router";
import { StyleSheet, View } from "react-native";

import { FILE_VIEWER_COPY } from "../../src/features/files/presentation";
import { CentredMessage } from "../../src/features/files/ui/parts";
import { FileViewer } from "../../src/features/files/ui/FileViewer";
import { fromRouteParam } from "../../src/navigation/routeParams";
import { colors } from "../../src/ui/tokens";

/** File viewer — design.md §9.7 (`?path=&name=`). */
export default function FileViewerScreen() {
  const params = useLocalSearchParams<{ paneId: string; path?: string; name?: string }>();
  const paneId = fromRouteParam(params.paneId);
  const path = fromRouteParam(params.path);
  const name = fromRouteParam(params.name);
  // Every route is deep-linkable under the `muxflow` scheme, so `path` is not
  // guaranteed by the pushes inside the app.
  if (!path) {
    return (
      <View style={styles.root}>
        <CentredMessage message={FILE_VIEWER_COPY.unavailable} />
      </View>
    );
  }
  return <FileViewer paneId={paneId} path={path} name={name ?? lastSegment(path)} />;
}

function lastSegment(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash === -1 ? path : path.slice(slash + 1);
}

const styles = StyleSheet.create({
  root: {
    backgroundColor: colors.chromeBg,
    flex: 1,
  },
});
