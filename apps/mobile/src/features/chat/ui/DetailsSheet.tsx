import type { MessageInitShape } from "@bufbuild/protobuf";
import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";

import { ChatPermissionMode, ChatSpeed, type ChatOptionsSchema } from "../../../protocol/gen/chat_pb";
import { Sheet } from "../../../ui/components/Sheet";
import { colors, fixedChromeText, fonts, typeScale } from "../../../ui/tokens";
import { currentModel, effortLabel, modelLabel, usageFraction, usageLabel, type ChatPane } from "../chatModel";

export interface DetailsSheetProps {
  visible: boolean;
  onDismiss: () => void;
  pane: ChatPane | undefined;
  hostLabel: string;
  folder: string;
  /** A change is on its way; the choices wait for it. */
  busy: boolean;
  onSetOptions: (options: MessageInitShape<typeof ChatOptionsSchema>) => void;
}

/**
 * The header's Details: where the chat runs, the context used, and the
 * model, effort, permission and speed controls the desktop has as composer
 * pills.
 * Each control shows only when the provider can change it.
 */
export function DetailsSheet({ visible, onDismiss, pane, hostLabel, folder, busy, onSetOptions }: DetailsSheetProps) {
  return (
    <Sheet onDismiss={onDismiss} title="Details" visible={visible}>
      <DetailsBody busy={busy} folder={folder} hostLabel={hostLabel} onSetOptions={onSetOptions} pane={pane} />
    </Sheet>
  );
}

export function DetailsBody({ pane, hostLabel, folder, busy, onSetOptions }: Omit<DetailsSheetProps, "visible" | "onDismiss">) {
  const capabilities = pane?.catalog?.capabilities;
  const options = pane?.state?.options;
  const models = pane?.catalog?.models ?? [];
  const model = currentModel(pane);
  // An unlisted model allows only the permission mode to change (plan §4, "Options").
  const listed = model !== undefined;
  const usage = usageLabel(pane);
  const fraction = usageFraction(pane);
  return (
    <ScrollView style={styles.scroll}>
      <View style={styles.group}>
        <Line value={hostLabel} />
        {folder ? <Line glyph="/" mono value={folder} /> : null}
      </View>
      {/* Context first: the sheet's controls can run past its height. */}
      {usage ? (
        <View style={[styles.group, styles.contextRow]}>
          <Text {...fixedChromeText} style={styles.caption}>Context</Text>
          {fraction !== undefined ? (
            <View style={styles.meter}><View style={[styles.meterFill, { width: `${Math.round(fraction * 100)}%` }]} /></View>
          ) : <View style={styles.flex} />}
          <Text {...fixedChromeText} style={styles.usage}>{usage}</Text>
        </View>
      ) : null}
      {capabilities?.model && models.length > 0 ? (
        <Choices
          disabled={busy || !listed}
          label="Model"
          note={listed ? undefined : `${modelLabel(pane)} is set by the agent and isn't one it lists.`}
          onPick={(id) => onSetOptions({ model: id })}
          options={models.map((entry) => ({ id: entry.id, label: entry.label || entry.id }))}
          selected={options?.model ?? ""}
        />
      ) : null}
      {capabilities?.effort && listed && model.efforts.length > 0 ? (
        <Choices disabled={busy} label="Effort" onPick={(effort) => onSetOptions({ effort })} options={model.efforts.map((effort) => ({ id: effort, label: effortLabel(effort) }))} selected={options?.effort ?? ""} />
      ) : null}
      {capabilities?.permission ? (
        <Choices
          disabled={busy}
          label="Permissions"
          note={options?.permission === ChatPermissionMode.FULL ? "No checks and no sandbox." : "The agent approves actions it judges safe and asks about the rest."}
          onPick={(id) => onSetOptions({ permission: id === "full" ? ChatPermissionMode.FULL : ChatPermissionMode.AUTO })}
          options={[{ id: "auto", label: "Auto" }, { id: "full", label: "Full" }]}
          selected={options?.permission === ChatPermissionMode.FULL ? "full" : options?.permission === ChatPermissionMode.AUTO ? "auto" : ""}
        />
      ) : null}
      {capabilities?.speed && listed && model.supportsFast ? (
        <Choices
          disabled={busy}
          label="Speed"
          onPick={(id) => onSetOptions({ speed: id === "fast" ? ChatSpeed.FAST : ChatSpeed.STANDARD })}
          options={[{ id: "standard", label: "Standard" }, { id: "fast", label: "Fast" }]}
          selected={options?.speed === ChatSpeed.FAST ? "fast" : "standard"}
        />
      ) : null}
      <Text style={styles.footnote}>Changes apply from the next message.</Text>
    </ScrollView>
  );
}

function Line({ glyph, value, mono }: { glyph?: string; value: string; mono?: boolean }) {
  return (
    <View style={styles.line}>
      {glyph ? <Text {...fixedChromeText} style={styles.glyph}>{glyph}</Text> : null}
      <Text ellipsizeMode="head" numberOfLines={1} style={[styles.value, mono && styles.mono]}>{value}</Text>
    </View>
  );
}

function Choices({ label, options, selected, onPick, disabled, note }: { label: string; options: { id: string; label: string }[]; selected: string; onPick: (id: string) => void; disabled: boolean; note?: string | undefined }) {
  return (
    <View style={styles.group}>
      <Text {...fixedChromeText} style={styles.caption}>{label}</Text>
      <View style={styles.chips}>
        {options.map((option) => {
          const on = option.id === selected;
          return (
            <Pressable
              accessibilityLabel={`${label}: ${option.label}`}
              accessibilityRole="radio"
              accessibilityState={{ checked: on, disabled }}
              disabled={disabled || on}
              key={option.id}
              onPress={() => onPick(option.id)}
              style={({ pressed }) => [styles.chip, on && styles.chipOn, pressed && styles.pressed]}
            >
              <Text {...fixedChromeText} style={[styles.chipLabel, on && styles.chipLabelOn]}>{option.label}</Text>
            </Pressable>
          );
        })}
      </View>
      {note ? <Text style={styles.note}>{note}</Text> : null}
    </View>
  );
}

const styles = StyleSheet.create({
  scroll: { maxHeight: 520 },
  flex: { flex: 1 },
  group: { borderBottomColor: colors.chromeHairline, borderBottomWidth: 1, gap: 8, paddingVertical: 10 },
  line: { alignItems: "center", flexDirection: "row", gap: 10, minHeight: 24 },
  glyph: { color: colors.chromeFaint, fontFamily: fonts.mono, fontSize: 12, textAlign: "center", width: 16 },
  value: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.body },
  mono: { fontFamily: fonts.mono, fontSize: 12.5 },
  caption: { color: colors.chromeFaint, fontSize: 11, letterSpacing: 0.8, textTransform: "uppercase" },
  chips: { flexDirection: "row", flexWrap: "wrap", gap: 8 },
  chip: { alignItems: "center", borderColor: colors.chromeBorder, borderRadius: 999, borderWidth: 1, justifyContent: "center", minHeight: 44, paddingHorizontal: 16 },
  chipOn: { backgroundColor: colors.accentWash, borderColor: colors.accent },
  chipLabel: { color: colors.chromeInk, fontSize: typeScale.rowSecondary },
  chipLabelOn: { color: colors.accent, fontWeight: "600" },
  pressed: { opacity: 0.75 },
  note: { color: colors.chromeDim, fontSize: typeScale.meta, lineHeight: 16 },
  contextRow: { alignItems: "center", flexDirection: "row" },
  meter: { backgroundColor: colors.chromeHairline, borderRadius: 2, flex: 1, height: 4, overflow: "hidden" },
  meterFill: { backgroundColor: colors.chromeDim, height: 4 },
  usage: { color: colors.chromeDim, fontSize: 11, fontVariant: ["tabular-nums"] },
  footnote: { color: colors.chromeFaint, fontSize: typeScale.meta, paddingTop: 10 },
});
