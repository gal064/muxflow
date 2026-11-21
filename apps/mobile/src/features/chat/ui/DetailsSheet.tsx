import type { MessageInitShape } from "@bufbuild/protobuf";
import type { ReactNode } from "react";
import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";

import { ChatPermissionMode, ChatSpeed, type ChatOptionsSchema } from "../../../protocol/gen/chat_pb";
import { FolderIcon } from "../../../ui/components/MediaIcons";
import { Sheet } from "../../../ui/components/Sheet";
import { colors, fixedChromeText, fonts, typeScale } from "../../../ui/tokens";
import { currentModel, effortLabel, modelLabel, usageFraction, usageLabel, type ChatPane } from "../chatModel";
import { folderLabel, gitRows, NO_GIT_DETAILS, READING_MARK, type ChangesRow, type GitDetails } from "../gitDetails";
import { BranchIcon, HostIcon } from "./HeaderIcons";

export interface DetailsSheetProps {
  visible: boolean;
  onDismiss: () => void;
  pane: ChatPane | undefined;
  hostLabel: string;
  /** The pane's folder, as the host gives it. */
  folder: string;
  /** What the host said about the folder's repository. Absent draws no Git row. */
  git?: GitDetails | undefined;
  /** A change is on its way; the choices wait for it. */
  busy: boolean;
  onSetOptions: (options: MessageInitShape<typeof ChatOptionsSchema>) => void;
}

/**
 * The header's Details (mobile-chat-mock.html, S8): where the chat runs (the
 * host, the folder as the desktop names it, the branch and what changed), the
 * context used, and the model, effort, permission and speed controls the
 * desktop has as composer pills.
 * Each control shows only when the provider can change it.
 */
export function DetailsSheet({ visible, onDismiss, pane, hostLabel, folder, git, busy, onSetOptions }: DetailsSheetProps) {
  return (
    <Sheet onDismiss={onDismiss} title="Details" visible={visible}>
      <DetailsBody busy={busy} folder={folder} git={git} hostLabel={hostLabel} onSetOptions={onSetOptions} pane={pane} />
    </Sheet>
  );
}

export function DetailsBody({ pane, hostLabel, folder, git = NO_GIT_DETAILS, busy, onSetOptions }: Omit<DetailsSheetProps, "visible" | "onDismiss">) {
  const capabilities = pane?.catalog?.capabilities;
  const options = pane?.state?.options;
  const models = pane?.catalog?.models ?? [];
  const model = currentModel(pane);
  // An unlisted model allows only the permission mode to change (plan §4, "Options").
  const listed = model !== undefined;
  const usage = usageLabel(pane);
  const fraction = usageFraction(pane);
  const rows = gitRows(git);
  return (
    <ScrollView style={styles.scroll}>
      <View style={styles.group}>
        {hostLabel ? <Line icon={<HostIcon color={colors.chromeFaint} />} label={`Host: ${hostLabel}`} value={hostLabel} /> : null}
        {folder ? (
          <Line
            head
            icon={<FolderIcon color={colors.chromeFaint} size={14} />}
            // The whole path, which the line shortens.
            label={`Folder: ${folder}`}
            mono
            value={folderLabel(folder, git.repository === "git" ? git.root : undefined)}
          />
        ) : null}
        {rows.branch !== undefined ? <Line icon={<BranchIcon color={colors.chromeFaint} />} label={`Branch: ${rows.branch}`} mono value={rows.branch} /> : null}
        {rows.changes ? <Changes row={rows.changes} /> : null}
        {rows.uncommitted ? <Changes row={rows.uncommitted} /> : null}
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

/** A mark and one line of text. `head` cuts a long value at its start, keeping its end. */
function Line({ icon, value, label, mono, head }: { icon: ReactNode; value: string; label: string; mono?: boolean; head?: boolean }) {
  return (
    <View accessibilityLabel={label} accessible style={styles.line}>
      <View style={styles.mark}>{icon}</View>
      <Text ellipsizeMode={head ? "head" : "tail"} numberOfLines={1} style={[styles.value, mono && styles.mono]}>{value}</Text>
    </View>
  );
}

/** "Changes vs main +412 −96 … 14 files", under the branch and with no mark of its own. */
function Changes({ row }: { row: ChangesRow }) {
  const { count } = row;
  return (
    <View accessibilityLabel={row.label} accessible style={styles.line}>
      <View style={styles.mark} />
      <Text numberOfLines={1} style={[styles.changes, count.kind === "none" && styles.quiet]}>{row.title}</Text>
      {count.kind === "lines" ? (
        <>
          <Text style={[styles.count, styles.added]}>{count.added}</Text>
          <Text style={[styles.count, styles.removed]}>{count.removed}</Text>
          <View style={styles.flex} />
          <Text style={styles.files}>{count.files}</Text>
        </>
      ) : (
        <Text style={[styles.count, styles.quiet]}>{count.kind === "none" ? "none" : READING_MARK}</Text>
      )}
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
  mark: { alignItems: "center", width: 16 },
  value: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.body },
  mono: { fontFamily: fonts.mono, fontSize: 12.5 },
  changes: { color: colors.chromeInkStrong, flexShrink: 1, fontSize: typeScale.body },
  count: { fontSize: typeScale.rowSecondary, fontVariant: ["tabular-nums"] },
  added: { color: colors.okInk },
  removed: { color: colors.dangerInk },
  quiet: { color: colors.chromeDim },
  files: { color: colors.chromeDim, fontSize: typeScale.meta, fontVariant: ["tabular-nums"] },
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
