import { useEffect, useMemo, useRef, useState } from "react";
import { Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";

import { muxflowSsh, type SshEvent } from "../../src/ssh/MuxflowSsh";

/** Colours from design doc §10.1, hardcoded: this screen is a bench tool, not product UI. */
const C = {
  bg: "#282c34",
  raised: "#2c313a",
  border: "#3e4451",
  ink: "#c4c8c6",
  inkStrong: "#ffffff",
  dim: "#8f96a1",
  accent: "#7aa6da",
  accentInk: "#282c34",
};

/** Manual check that the native module talks to a real sshd. Not linked from the product UI. */
export default function SshDebugScreen() {
  const ssh = useMemo(() => muxflowSsh(), []);
  const [host, setHost] = useState("10.0.2.2");
  const [port, setPort] = useState("22222");
  const [user, setUser] = useState("muxflow");
  const [lines, setLines] = useState<string[]>([]);
  const [pending, setPending] = useState<{ connectionId: string; fingerprint: string } | null>(null);
  const trusted = useRef<string | null>(null);

  const log = (line: string) => setLines((previous) => [...previous.slice(-199), line]);

  useEffect(
    () =>
      ssh.addListener((event: SshEvent) => {
        switch (event.type) {
          case "hostKey":
            setPending({ connectionId: event.connectionId, fingerprint: event.fingerprintSha256 });
            log(`host key ${event.algorithm} ${event.fingerprintSha256}`);
            break;
          case "connected":
            log("connected");
            break;
          case "data":
            log(`stdout ${JSON.stringify(decode(event.base64))}`);
            break;
          case "stderr":
            log(`stderr ${event.text}`);
            break;
          case "closed":
            setPending(null);
            log(`closed reason=${event.reason} exit=${String(event.exitCode)}`);
            break;
        }
      }),
    [ssh],
  );

  const run = async (label: string, action: () => Promise<unknown>) => {
    try {
      const result = await action();
      log(result === undefined ? `${label} ok` : `${label} ${String(result)}`);
    } catch (error) {
      log(`${label} failed: ${String(error)}`);
    }
  };

  const echoHi = () =>
    run("connect", () =>
      ssh.connect(`debug-${Date.now()}`, { host, port: Number(port) || 22, user }, "echo hi", trusted.current),
    );

  return (
    <ScrollView style={styles.screen} contentContainerStyle={styles.content}>
      <Text style={styles.heading}>muxflow-ssh bench</Text>
      <Field label="Host" value={host} onChange={setHost} />
      <Field label="Port" value={port} onChange={setPort} />
      <Field label="User" value={user} onChange={setUser} />

      <Button
        label="Generate key"
        onPress={() =>
          run("generateKeyPair", async () => (await ssh.generateKeyPair()).publicKeyOpenSsh)
        }
      />
      <Button
        label="Show public key"
        onPress={() => run("getPublicKey", async () => (await ssh.getPublicKey()) ?? "(none)")}
      />
      <Button label="Run `echo hi`" onPress={echoHi} />
      <Button label="Clear output" onPress={() => setLines([])} />
      {pending !== null ? (
        <Button
          label={`Trust ${pending.fingerprint}`}
          onPress={() =>
            run("trustHostKey", async () => {
              trusted.current = pending.fingerprint;
              await ssh.trustHostKey(pending.connectionId, pending.fingerprint);
              setPending(null);
            })
          }
        />
      ) : null}
      <View style={styles.output}>
        {lines.length === 0 ? <Text style={styles.dim}>No output yet.</Text> : null}
        {lines.map((line, index) => (
          <Text key={`${index}-${line}`} style={styles.mono}>{line}</Text>
        ))}
      </View>
    </ScrollView>
  );
}

function decode(base64: string): string {
  try {
    return globalThis.atob(base64);
  } catch {
    return `<${base64.length} base64 chars>`;
  }
}

function Field(props: { label: string; value: string; onChange: (next: string) => void }) {
  return (
    <View style={styles.field}>
      <Text style={styles.dim}>{props.label}</Text>
      <TextInput
        style={styles.input}
        value={props.value}
        onChangeText={props.onChange}
        autoCapitalize="none"
        autoCorrect={false}
      />
    </View>
  );
}

function Button(props: { label: string; onPress: () => void }) {
  return (
    <Pressable style={styles.button} onPress={props.onPress}>
      <Text style={styles.buttonLabel} numberOfLines={1}>{props.label}</Text>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  screen: { backgroundColor: C.bg, flex: 1 },
  content: { gap: 8, padding: 16 },
  heading: { color: C.inkStrong, fontSize: 20, marginBottom: 8 },
  field: { gap: 4 },
  dim: { color: C.dim, fontSize: 13 },
  input: {
    backgroundColor: C.raised,
    borderColor: C.border,
    borderRadius: 8,
    borderWidth: 1,
    color: C.ink,
    fontFamily: "monospace",
    padding: 10,
  },
  button: { alignItems: "center", backgroundColor: C.accent, borderRadius: 8, marginTop: 8, padding: 12 },
  buttonLabel: { color: C.accentInk, fontSize: 15 },
  output: {
    backgroundColor: C.raised,
    borderColor: C.border,
    borderRadius: 8,
    borderWidth: 1,
    gap: 2,
    marginTop: 16,
    minHeight: 160,
    padding: 10,
  },
  mono: { color: C.ink, fontFamily: "monospace", fontSize: 12 },
});
