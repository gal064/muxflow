import type { MessageInitShape } from "@bufbuild/protobuf";
import { useCallback, useRef, type ReactNode } from "react";
import { Pressable, ScrollView, StyleSheet, Text, View, type NativeScrollEvent, type NativeSyntheticEvent } from "react-native";

import { ChatConnection, ChatTurnState, type ChatCommand, type ChatItem, type ChatOptionsSchema } from "../../../protocol/gen/chat_pb";
import { BackIcon } from "../../../ui/components/MediaIcons";
import { colors, fixedChromeText, metrics, typeScale } from "../../../ui/tokens";
import { WorkingIndicator } from "../../voice/WorkingIndicator";
import { sideQuestion, type ChatPane, type CommandGroup } from "../chatModel";
import { ChatTimeline } from "./ChatTimeline";
import { Composer } from "./Composer";
import { DetailsSheet } from "./DetailsSheet";
import { PendingCard, type PendingAnswer } from "./PendingCard";

export interface ChatViewProps {
  title: string;
  pane: ChatPane | undefined;
  connected: boolean;
  hostLabel: string;
  folder: string;
  draft: string;
  onDraft: (text: string) => void;
  commands: CommandGroup[];
  onPickCommand: (command: ChatCommand) => void;
  /** The user message being edited, when one is. */
  editing: ChatItem | undefined;
  onEdit: (item: ChatItem) => void;
  onCancelEdit: () => void;
  /** A send, answer or option change is on its way. */
  busy: boolean;
  onSend: () => void;
  onStop: () => void;
  onAnswer: (answer: PendingAnswer) => void;
  onLoadOlder: () => void;
  onRetry: () => void;
  onBack: () => void;
  detailsOpen: boolean;
  onDetails: (open: boolean) => void;
  onSetOptions: (options: MessageInitShape<typeof ChatOptionsSchema>) => void;
  voiceMode: boolean;
  onVoiceMode: (on: boolean) => void;
  /** Voice mode only: the readiness card, the newest reply's player, and the bar that replaces the composer. */
  voice?: { status: ReactNode; reply: ReactNode; bar: ReactNode } | undefined;
}

/** The chat screen (chat-mock.html, "Phone"): header, conversation, request card, composer or voice bar. */
export function ChatView(props: ChatViewProps) {
  const { title, pane, connected, voiceMode, voice } = props;
  const list = useRef<ScrollView>(null);
  // New text follows the end only while the reader is there; reading further up stays put.
  const atEnd = useRef(true);
  const toEnd = useCallback((animated: boolean) => {
    if (atEnd.current) list.current?.scrollToEnd({ animated });
  }, []);
  const onScroll = useCallback((event: NativeSyntheticEvent<NativeScrollEvent>) => {
    const { contentOffset, contentSize, layoutMeasurement } = event.nativeEvent;
    atEnd.current = contentOffset.y + layoutMeasurement.height >= contentSize.height - 80;
  }, []);
  const state = pane?.state;
  const capabilities = pane?.catalog?.capabilities;
  const turn = state?.turn ?? ChatTurnState.UNSPECIFIED;
  const working = turn === ChatTurnState.WORKING;
  const pending = turn === ChatTurnState.WAITING ? state?.pending : undefined;
  const ended = pane?.status === "gone" || state?.connection === ChatConnection.ENDED || state?.connection === ChatConnection.FAILED;
  const usable = connected && pane?.status === "open" && !ended;
  const banner = bannerFor(pane, connected);

  return (
    <View style={styles.root}>
      <View style={styles.header}>
        <Pressable accessibilityLabel="Back" accessibilityRole="button" onPress={props.onBack} style={styles.iconButton}>
          <BackIcon color={colors.chromeInkStrong} />
        </Pressable>
        <Text {...fixedChromeText} accessibilityRole="header" numberOfLines={1} style={styles.title}>{title}</Text>
        <Pressable
          accessibilityLabel="Voice mode: read replies aloud"
          accessibilityRole="switch"
          accessibilityState={{ checked: voiceMode }}
          onPress={() => props.onVoiceMode(!voiceMode)}
          style={styles.iconButton}
        >
          <View style={[styles.round, voiceMode && styles.roundOn]}>
            <Text {...fixedChromeText} style={[styles.roundGlyph, voiceMode && styles.roundGlyphOn]}>♪</Text>
          </View>
        </Pressable>
        <Pressable accessibilityLabel="Details" accessibilityRole="button" onPress={() => props.onDetails(true)} style={styles.iconButton}>
          <View style={styles.round}>
            <Text {...fixedChromeText} style={styles.roundGlyph}>i</Text>
          </View>
        </Pressable>
      </View>
      {banner ? (
        <View style={[styles.banner, banner.tone === "danger" && styles.bannerDanger]}>
          <Text style={styles.bannerText}>{banner.text}</Text>
          {banner.retry ? (
            <Pressable accessibilityRole="button" hitSlop={8} onPress={props.onRetry} style={styles.bannerButton}>
              <Text {...fixedChromeText} style={styles.bannerAction}>Retry</Text>
            </Pressable>
          ) : null}
        </View>
      ) : null}

      <ScrollView
        contentContainerStyle={styles.listContent}
        keyboardShouldPersistTaps="handled"
        onContentSizeChange={() => toEnd(true)}
        onLayout={() => toEnd(false)}
        onScroll={onScroll}
        ref={list}
        scrollEventThrottle={100}
        style={styles.list}
      >
        {pane && pane.status !== "opening" ? (
          <ChatTimeline
            canEdit={capabilities?.rewind === true && usable}
            hasOlder={pane.olderCursor !== ""}
            historyIncomplete={state?.historyComplete === false}
            items={pane.items}
            loadingOlder={pane.loadingOlder}
            onEdit={props.onEdit}
            onLoadOlder={props.onLoadOlder}
            outgoing={pane.outgoing}
          />
        ) : null}
        {pending ? <PendingCard busy={props.busy || !usable} onAnswer={props.onAnswer} request={pending} /> : null}
        <WorkingIndicator working={working} />
        {voiceMode ? voice?.reply : null}
      </ScrollView>

      {voiceMode && voice ? (
        <>
          {voice.status}
          {voice.bar}
        </>
      ) : (
        <Composer
          canSendMidTurn={capabilities?.sendMidTurn === true}
          commands={props.commands}
          disabled={!usable}
          draft={props.draft}
          editing={props.editing !== undefined}
          onCancelEdit={props.onCancelEdit}
          onDraft={props.onDraft}
          onMic={() => props.onVoiceMode(true)}
          onPickCommand={props.onPickCommand}
          onSend={props.onSend}
          onStop={props.onStop}
          sending={props.busy}
          sideQuestion={props.editing === undefined && sideQuestion(pane?.catalog, props.draft) !== undefined}
          waiting={pending !== undefined}
          working={working}
        />
      )}

      <DetailsSheet
        busy={props.busy}
        folder={props.folder}
        hostLabel={props.hostLabel}
        onDismiss={() => props.onDetails(false)}
        onSetOptions={props.onSetOptions}
        pane={pane}
        visible={props.detailsOpen}
      />
    </View>
  );
}

/** One line above the conversation when the chat cannot be used as normal. */
export function bannerFor(pane: ChatPane | undefined, connected: boolean): { text: string; tone: "info" | "danger"; retry?: boolean } | undefined {
  if (!connected) return { text: "Not connected. The chat comes back when the phone reconnects.", tone: "info" };
  if (!pane || pane.status === "opening") return { text: "Opening the chat…", tone: "info" };
  if (pane.status === "error") return { text: pane.error || "This chat can't be opened.", tone: "danger", retry: true };
  if (pane.status === "gone") return { text: "This chat's pane has closed.", tone: "info" };
  if (pane.resyncing) return { text: "Catching up…", tone: "info" };
  switch (pane.state?.connection) {
    case ChatConnection.CONNECTING:
      return { text: "Connecting to the agent…", tone: "info" };
    case ChatConnection.ENDED:
      return { text: "The agent has exited.", tone: "info" };
    case ChatConnection.FAILED:
      return { text: pane.state.failure || "The agent stopped working.", tone: "danger" };
    default:
      return undefined;
  }
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
  header: { alignItems: "center", backgroundColor: colors.chromeBg, borderBottomColor: colors.chromeHairline, borderBottomWidth: 1, flexDirection: "row", height: metrics.terminalHeaderHeight + 4, paddingHorizontal: 4 },
  iconButton: { alignItems: "center", height: 48, justifyContent: "center", width: 44 },
  title: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.appBarTitle, fontWeight: "600", minWidth: 0 },
  round: { alignItems: "center", borderColor: colors.chromeBorder, borderRadius: 15, borderWidth: 1, height: 30, justifyContent: "center", width: 30 },
  roundOn: { backgroundColor: colors.accentWash, borderColor: colors.accent },
  roundGlyph: { color: colors.chromeDim, fontSize: 13, fontWeight: "600" },
  roundGlyphOn: { color: colors.accent },
  banner: { alignItems: "center", backgroundColor: colors.accentWash, borderBottomColor: colors.chromeHairline, borderBottomWidth: 1, flexDirection: "row", gap: 12, paddingHorizontal: 14, paddingVertical: 8 },
  bannerDanger: { backgroundColor: colors.dangerWash },
  bannerText: { color: colors.chromeInk, flex: 1, fontSize: typeScale.rowSecondary, lineHeight: 18 },
  bannerButton: { justifyContent: "center", minHeight: 44, paddingHorizontal: 4 },
  bannerAction: { color: colors.accent, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  list: { flex: 1 },
  listContent: { flexGrow: 1, gap: 12, paddingHorizontal: 12, paddingVertical: 12 },
});
