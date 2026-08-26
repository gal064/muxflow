import { Link, useRouter } from "expo-router";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { connectHost } from "../src/session/connectionManager";

import { colors, radii, typeScale } from "../src/ui/tokens";

/**
 * Hosts — design.md §9.1. M0 renders the empty state only; the host list, the
 * long-press sheet and the FAB arrive with M6.
 */
export default function HostsScreen() {
  const router = useRouter();
  return (
    <View style={styles.root}>
      <Text style={styles.glyph}>{">_"}</Text>
      <Text style={styles.heading}>No hosts yet</Text>
      <Text style={styles.body}>
        Add the machine where Muxflow desktop runs. The app connects over SSH, the same way you
        would from a terminal.
      </Text>
      <Link href="/hosts/new" style={styles.button}>
        Add host
      </Link>
      <Link href="/key" style={styles.footerLink}>
        Your SSH key
      </Link>
      {__DEV__ ? <DevBridgeLink onConnect={() => router.push("/home")} /> : null}
    </View>
  );
}

const styles = StyleSheet.create({
  root: {
    alignItems: "center",
    backgroundColor: colors.chromeBg,
    flex: 1,
    gap: 12,
    justifyContent: "center",
    padding: 32,
  },
  glyph: {
    color: colors.chromeFaint,
    fontSize: 40,
  },
  heading: {
    color: colors.chromeInkStrong,
    fontSize: typeScale.rowTitle,
    fontWeight: "600",
  },
  body: {
    color: colors.chromeDim,
    fontSize: typeScale.body,
    textAlign: "center",
  },
  button: {
    backgroundColor: colors.accent,
    borderRadius: radii.card,
    color: colors.accentInk,
    fontSize: typeScale.body,
    fontWeight: "600",
    marginTop: 12,
    overflow: "hidden",
    paddingHorizontal: 20,
    paddingVertical: 12,
  },
  footerLink: {
    color: colors.accent,
    fontSize: typeScale.meta,
    marginTop: 16,
  },
});

/**
 * Development builds only: connects to `scripts/dev-tcp-bridge.mjs` on the
 * host machine (a real muxflow-host over plain TCP) so the terminal can be
 * exercised before the SSH module lands. Not compiled into release builds.
 */
function DevBridgeLink({ onConnect }: { onConnect: () => void }) {
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const { DEV_HOST } = require("../src/session/devTransport") as typeof import("../src/session/devTransport");
  return (
    <Pressable
      onPress={() => {
        void connectHost(DEV_HOST).catch(() => undefined);
        onConnect();
      }}
      style={devStyles.devLink}
    >
      <Text style={styles.footerLink}>Dev: connect to 10.0.2.2:7777</Text>
    </Pressable>
  );
}

const devStyles = StyleSheet.create({ devLink: { marginTop: 8 } });
