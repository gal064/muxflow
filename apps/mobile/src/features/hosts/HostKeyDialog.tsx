import { StyleSheet, Text } from "react-native";

import { Dialog } from "../../ui/components/Dialog";
import { colors, fonts, typeScale } from "../../ui/tokens";
import { useHostKeyPrompt } from "./hooks";

/**
 * Host key trust dialog — design.md §9.10. Trust-on-first-use with an explicit
 * fingerprint (§14): `Trust` pins the fingerprint on the saved host and lets
 * the handshake continue, `Cancel` fails the connection with
 * `hostKeyNotTrusted`.
 */
export function HostKeyDialog() {
  const pending = useHostKeyPrompt((state) => state.pending);
  const answer = useHostKeyPrompt((state) => state.answer);
  const prompt = pending?.prompt;

  return (
    <Dialog
      visible={pending !== null}
      title="Trust this host?"
      onDismiss={() => answer(false)}
      actions={[
        { label: "Cancel", onPress: () => answer(false), variant: "secondary" },
        { label: "Trust", onPress: () => answer(true), variant: "primary" },
      ]}
    >
      <Text style={styles.body}>
        {`${prompt?.target.host ?? ""} presented an ${prompt?.algorithm ?? ""} key with fingerprint`}
      </Text>
      <Text style={styles.fingerprint} selectable>
        {prompt?.fingerprintSha256 ?? ""}
      </Text>
      <Text style={styles.body}>
        Compare it with the key in ~/.ssh/known_hosts on a machine that already trusts this host.
      </Text>
    </Dialog>
  );
}

const styles = StyleSheet.create({
  body: { color: colors.chromeInk, fontSize: typeScale.body, lineHeight: 20 },
  fingerprint: {
    color: colors.chromeInkStrong,
    fontFamily: fonts.mono,
    fontSize: typeScale.keyMono,
  },
});
