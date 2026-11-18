import type { MessageInitShape } from "@bufbuild/protobuf";
import { useMemo, useRef, useState, type ReactNode } from "react";
import { Keyboard, Pressable, StyleSheet, Text, View } from "react-native";

import { ChatConnection, ChatTurnState, type ChatCommand, type ChatItem, type ChatOptionsSchema } from "../../../protocol/gen/chat_pb";
import { BackIcon } from "../../../ui/components/MediaIcons";
import { colors, fixedChromeText, metrics, typeScale } from "../../../ui/tokens";
import { sideQuestion, type ChatPane, type CommandGroup } from "../chatModel";
import { chatStatus, followTurn, newTurnClock } from "../chatStatus";
import { olderState, timelineRows, type TimelineRow } from "../timelineRows";
import { ChatTimeline } from "./ChatTimeline";
import { Composer } from "./Composer";
import { DetailsSheet } from "./DetailsSheet";
import type { PendingAnswer } from "./PendingCard";
import { RequestDock } from "./RequestDock";
import { StatusLine } from "./StatusLine";

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
  /** Stop was pressed and the turn has not stopped yet. */
  stopping: boolean;
  onAnswer: (answer: PendingAnswer) => void;
  /** The top of the conversation came within reach while older messages can be loaded. */
  onLoadOlder: () => void;
  /** The last load of older messages failed; the conversation offers Retry instead of loading by itself. */
  olderFailed: boolean;
  onRetryOlder: () => void;
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

const NO_ROWS: TimelineRow[] = [];

/**
 * The chat screen (mobile-chat-mock.html, S1 and S3): header, conversation,
 * then what cannot scroll away: the request the agent waits on, the line that
 * says it works, and the composer or voice bar.
 */
export function ChatView(props: ChatViewProps) {
  const { title, pane, connected, voiceMode, voice } = props;
  const state = pane?.state;
  const capabilities = pane?.catalog?.capabilities;
  const turn = state?.turn ?? ChatTurnState.UNSPECIFIED;
  const working = turn === ChatTurnState.WORKING;
  const pending = turn === ChatTurnState.WAITING ? state?.pending : undefined;
  const ended = pane?.status === "gone" || state?.connection === ChatConnection.ENDED || state?.connection === ChatConnection.FAILED;
  const usable = connected && pane?.status === "open" && !ended;
  const banner = bannerFor(pane, connected);

  const shown = pane && pane.status !== "opening" ? pane : undefined;
  const items = shown?.items;
  const outgoing = shown?.outgoing;
  const rows = useMemo(() => (items && outgoing ? timelineRows(items, outgoing) : NO_ROWS), [items, outgoing]);

  // When the turn began is kept from the moment it is seen, whatever is on
  // screen then, so the status line has it when it shows.
  const clock = useRef(newTurnClock());
  clock.current = followTurn(clock.current, pane, Date.now());
  const status = usable ? chatStatus(pane, clock.current, props.stopping) : undefined;

  // The request folded to one line. Kept by its id, so the next request shows whole.
  const [foldedRequest, setFoldedRequest] = useState("");
  const folded = pending !== undefined && foldedRequest === pending.requestId;

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

      <ChatTimeline
        canEdit={capabilities?.rewind === true && usable}
        older={olderState(pane, props.olderFailed)}
        onEdit={props.onEdit}
        onLoadOlder={props.onLoadOlder}
        onRetryOlder={props.onRetryOlder}
        rows={rows}
        tail={voiceMode ? voice?.reply : undefined}
      />

      {pending ? (
        <RequestDock
          busy={props.busy || !usable}
          folded={folded}
          key={pending.requestId}
          onAnswer={props.onAnswer}
          onFold={() => setFoldedRequest(pending.requestId)}
          onUnfold={() => {
            setFoldedRequest("");
            Keyboard.dismiss();
          }}
          request={pending}
        />
      ) : null}
      {status ? <StatusLine status={status} /> : null}

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
          // Room to read and type: the request folds to its one line.
          onFocus={() => {
            if (pending) setFoldedRequest(pending.requestId);
          }}
          onMic={() => props.onVoiceMode(true)}
          onPickCommand={props.onPickCommand}
          onSend={props.onSend}
          onStop={props.onStop}
          sending={props.busy}
          sideQuestion={props.editing === undefined && sideQuestion(pane?.catalog, props.draft) !== undefined}
          stopping={props.stopping}
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
});
