import { StyleSheet, Text, TextInput, View, type KeyboardTypeOptions } from "react-native";

import { colors, radii, typeScale } from "../../ui/tokens";

export interface HostFormFieldProps {
  label: string;
  value: string;
  placeholder: string;
  onChangeText: (next: string) => void;
  error?: string | undefined;
  keyboardType?: KeyboardTypeOptions;
}

/** One row of the §9.2 form: label, full-width field, and its validation message. */
export function HostFormField({
  label,
  value,
  placeholder,
  onChangeText,
  error,
  keyboardType,
}: HostFormFieldProps) {
  return (
    <View style={styles.field}>
      <Text style={styles.label}>{label}</Text>
      <TextInput
        style={[styles.input, error === undefined ? null : styles.inputError]}
        value={value}
        onChangeText={onChangeText}
        placeholder={placeholder}
        placeholderTextColor={colors.chromeFaint}
        autoCapitalize="none"
        autoCorrect={false}
        keyboardType={keyboardType ?? "default"}
        accessibilityLabel={label}
      />
      {error === undefined ? null : <Text style={styles.error}>{error}</Text>}
    </View>
  );
}

const styles = StyleSheet.create({
  field: { gap: 6 },
  label: { color: colors.chromeDim, fontSize: typeScale.rowSecondary },
  input: {
    backgroundColor: colors.chromeRaised,
    borderColor: colors.chromeBorder,
    borderRadius: radii.card,
    borderWidth: 1,
    color: colors.chromeInkStrong,
    fontSize: typeScale.rowTitle,
    paddingHorizontal: 12,
    paddingVertical: 12,
  },
  inputError: { borderColor: colors.danger },
  error: { color: colors.dangerInk, fontSize: typeScale.meta },
});
