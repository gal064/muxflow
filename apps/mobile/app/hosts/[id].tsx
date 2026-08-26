import { router, Stack, useLocalSearchParams } from "expo-router";
import { useMemo, useState } from "react";
import { KeyboardAvoidingView, Platform, ScrollView, StyleSheet, Text, View } from "react-native";

import { HostFormField } from "../../src/features/hosts/HostFormField";
import { useHosts } from "../../src/features/hosts/hooks";
import { KeyCard } from "../../src/features/hosts/KeyCard";
import { useSshKey } from "../../src/features/hosts/useSshKey";
import {
  emptyHostForm,
  hostFormValues,
  validateHostForm,
  type HostFormValues,
} from "../../src/features/hosts/validation";
import { findHost, hostsStore } from "../../src/store/hostsStore";
import { Button } from "../../src/ui/components/Button";
import { colors, fonts, radii, typeScale } from "../../src/ui/tokens";

/** Add / edit host — design.md §9.2. The id is `new` when adding. */
export default function HostEditScreen() {
  const params = useLocalSearchParams<{ id: string }>();
  const id = params.id === "new" ? null : (params.id ?? null);
  const existing = useHosts((state) => findHost(state, id));
  const keyHandle = useSshKey();
  const [values, setValues] = useState<HostFormValues>(() =>
    existing ? hostFormValues(existing) : emptyHostForm(),
  );
  // Errors stay hidden until a field has been visited, so a blank form does not
  // open covered in red.
  const [touched, setTouched] = useState<Record<string, boolean>>({});
  const validation = useMemo(() => validateHostForm(values), [values]);

  const set = (field: keyof HostFormValues) => (next: string) => {
    setTouched((previous) => ({ ...previous, [field]: true }));
    setValues((previous) => ({ ...previous, [field]: next }));
  };

  const save = () => {
    const draft = validation.draft;
    if (!draft) return;
    const store = hostsStore.getState();
    if (existing) store.updateHost(existing.id, draft);
    else store.addHost(draft);
    router.back();
  };

  return (
    <KeyboardAvoidingView
      style={styles.root}
      behavior={Platform.OS === "ios" ? "padding" : undefined}
    >
      <Stack.Screen options={{ title: existing ? "Edit host" : "Add host" }} />
      <ScrollView contentContainerStyle={styles.content} keyboardShouldPersistTaps="handled">
        <HostFormField
          label="Host"
          placeholder="devbox or 10.0.0.5"
          value={values.host}
          onChangeText={set("host")}
          error={touched.host ? validation.errors.host : undefined}
        />
        <HostFormField
          label="Port"
          placeholder="22"
          value={values.port}
          onChangeText={set("port")}
          keyboardType="number-pad"
          error={touched.port ? validation.errors.port : undefined}
        />
        <HostFormField
          label="User"
          placeholder="dev"
          value={values.user}
          onChangeText={set("user")}
          error={touched.user ? validation.errors.user : undefined}
        />
        <HostFormField
          label="Label"
          placeholder="Defaults to host"
          value={values.label}
          onChangeText={set("label")}
        />

        <KeyCard keyHandle={keyHandle} />

        {existing ? (
          <View style={styles.card}>
            <Text style={styles.cardTitle}>Trusted host key</Text>
            <Text style={styles.fingerprint} selectable>
              {existing.trustedHostKeyFingerprint ?? "No host key trusted yet"}
            </Text>
            {existing.trustedHostKeyFingerprint === null ? null : (
              <Button
                label="Forget host key"
                variant="dangerText"
                style={styles.forget}
                onPress={() => hostsStore.getState().setTrustedHostKeyFingerprint(existing.id, null)}
              />
            )}
          </View>
        ) : null}

        <Button label="Save" onPress={save} disabled={!validation.valid} />
      </ScrollView>
    </KeyboardAvoidingView>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
  content: { gap: 16, padding: 16, paddingBottom: 48 },
  card: { backgroundColor: colors.chromeRaised, borderRadius: radii.card, gap: 10, padding: 16 },
  cardTitle: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle, fontWeight: "600" },
  fingerprint: { color: colors.chromeInk, fontFamily: fonts.mono, fontSize: typeScale.keyMono },
  forget: { alignSelf: "flex-start" },
});
