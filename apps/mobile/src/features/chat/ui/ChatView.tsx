import type { MessageInitShape } from "@bufbuild/protobuf";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Keyboard, Pressable, StyleSheet, Text, View } from "react-native";

import { ChatConnection, ChatTurnState, type ChatBackgroundTask, type ChatCommand, type ChatItem, type ChatOptionsSchema } from "../../../protocol/gen/chat_pb";
import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";
import { filesState } from "../attach/attachState";
import type { ChatAttachments } from "../attach/useAttachments";
import { sideQuestion, type ChatPane, type CommandGroup } from "../chatModel";
import { chatStatus, followTurn, newTurnClock } from "../chatStatus";
import type { GitDetails } from "../gitDetails";
import { olderState, timelineRows, type TimelineRow } from "../timelineRows";
import { AttachSheet } from "./AttachSheet";
import { AttachStrip } from "./AttachStrip";
import { ChatHeader } from "./ChatHeader";
import { BackgroundTasks } from "./BackgroundTasks";
import { ChatTimeline } from "./ChatTimeline";
import { Composer } from "./Composer";
import type { ComposerDictation } from "./ComposerMic";
import { DetailsSheet } from "./DetailsSheet";
import type { PendingAnswer } from "./PendingCard";
import { RequestDock } from "./RequestDock";
import { StatusLine } from "./StatusLine";
import { useMessageActions } from "./useMessageActions";

export interface ChatViewProps {
  title: string;
  pane: ChatPane | undefined;
  connected: boolean;
  hostLabel: string;
  folder: string;
  /** What the host said about the folder's repository, for Details. */
  git?: GitDetails | undefined;
  draft: string;
  onDraft: (text: string) => void;
  commands: CommandGroup[];
  onPickCommand: (command: ChatCommand) => void;
  /** The user message being edited, when one is. */
  editing: ChatItem | undefined;
  onEdit: (item: ChatItem) => void;
  onCancelEdit: () => void;
  /**
   * Fork from here was picked: forks from the item and opens the fork.
   * Answers whether it was opened; a refusal is said by whoever forks. Absent
   * offers no fork.
   */
  onFork?: ((itemId: string) => Promise<boolean>) | undefined;
  /** The item whose fork is on its way, or "". */
  forking?: string | undefined;
  /**
   * The files on the reply field, and how they get there and leave it. Absent
   * offers no attaching; so does an agent without the `attachments` capability.
   */
  attach?: Pick<ChatAttachments, "entries" | "choose" | "remove"> | undefined;
  /** A send, answer or option change is on its way. */
  busy: boolean;
  onSend: () => void;
  onStop: () => void;
  /** Stop was pressed and the turn has not stopped yet. */
  stopping: boolean;
  /** Stop was pressed for a background task. A promise that rejects says the host refused. */
  onStopTask: (taskId: string) => Promise<void> | void;
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
  /** This screen is the one in front (absent: it is). Under another screen it does not take the system Back button. */
  focused?: boolean | undefined;
  /** The reply field's mic was tapped: voice mode on, or off when it was on and voice was not ready. */
  onVoiceMode: (on: boolean) => void;
  /**
   * The reply field's hold to dictate, and voice mode's parts: the readiness
   * card, the newest reply's player, and the talk pane that replaces the
   * reply field once voice is `ready` on the host.
   */
  voice?: { status: ReactNode; reply: ReactNode; pane: ReactNode; ready: boolean; dictation: ComposerDictation } | undefined;
}

const NO_ROWS: TimelineRow[] = [];
const NO_TASKS: ChatBackgroundTask[] = [];

/**
 * The chat screen (mobile-chat-mock.html, S1 to S4): header, conversation,
 * then what cannot scroll away: the request the agent waits on, the line that
 * says it works, the chip for what runs in the background, and the composer
 * or voice mode's talk pane.
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
  const rows = useMemo(() => (items && outgoing ? timelineRows(items, outgoing, turn) : NO_ROWS), [items, outgoing, turn]);

  // When the turn began is kept from the moment it is seen, whatever is on
  // screen then, so the status line has it when it shows.
  const clock = useRef(newTurnClock());
  clock.current = followTurn(clock.current, pane, Date.now());
  const status = usable ? chatStatus(pane, clock.current, props.stopping) : undefined;

  // The request folded to one line. Kept by its id, so the next request shows whole.
  const [foldedRequest, setFoldedRequest] = useState("");
  const folded = pending !== undefined && foldedRequest === pending.requestId;

  // Attaching (S7) is offered only to an agent that takes files with a message.
  const attach = capabilities?.attachments === true ? props.attach : undefined;
  const [attaching, setAttaching] = useState(false);

  // What a hold on a message opens, the row under the newest reply, and editing (S5).
  const messages = useMessageActions({
    pane: shown,
    usable,
    editing: props.editing,
    onEdit: props.onEdit,
    onCancelEdit: props.onCancelEdit,
    onBack: props.onBack,
    onFork: props.onFork,
    forking: props.forking,
    field: !(voiceMode && voice?.ready === true),
    focused: props.focused,
  });

  // The talk pane's keyboard button leaves voice mode for the reply field, so the field takes the focus.
  const voicePane = voiceMode && voice?.ready === true;
  const wasVoicePane = useRef(voicePane);
  const field = messages.field;
  useEffect(() => {
    if (wasVoicePane.current && !voiceMode) field.current?.focus();
    wasVoicePane.current = voicePane;
  }, [field, voiceMode, voicePane]);

  return (
    <View style={styles.root}>
      <ChatHeader
        connected={connected}
        onBack={messages.back}
        onDetails={() => props.onDetails(true)}
        pane={pane}
        title={title}
      />
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
        canEdit={capabilities?.rewind === true && usable && !(voiceMode && voice?.ready === true)}
        // Another chat is another conversation: what was open in this one is not open in that.
        key={pane?.paneId ?? ""}
        messages={messages.timeline}
        older={olderState(pane, props.olderFailed)}
        onEdit={messages.edit}
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
      <BackgroundTasks onStop={props.onStopTask} tasks={usable ? state?.background ?? NO_TASKS : NO_TASKS} />

      {/* The setup card shows until voice is ready on the host, and the reply field stays under it until then. */}
      {voiceMode && voice ? voice.status : null}
      {voicePane && voice ? voice.pane : (
        <Composer
          attach={attach ? {
            files: filesState(attach.entries),
            onOpen: () => {
              Keyboard.dismiss();
              setAttaching(true);
            },
            strip: <AttachStrip entries={attach.entries} onRemove={attach.remove} />,
          } : undefined}
          canSendMidTurn={capabilities?.sendMidTurn === true}
          commands={props.commands}
          disabled={!usable}
          draft={props.draft}
          editing={props.editing !== undefined}
          inputRef={messages.field}
          onCancelEdit={props.onCancelEdit}
          onDraft={props.onDraft}
          // Room to read and type: the request folds to its one line.
          onFocus={() => {
            if (pending) setFoldedRequest(pending.requestId);
          }}
          // In voice mode before voice is ready (not set up, not connected) the mic is the way out.
          mic={{
            on: voiceMode,
            onTap: () => {
              if (!voiceMode) Keyboard.dismiss();
              props.onVoiceMode(!voiceMode);
            },
            dictation: voice?.dictation,
          }}
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
        git={props.git}
        hostLabel={props.hostLabel}
        onDismiss={() => props.onDetails(false)}
        onSetOptions={props.onSetOptions}
        pane={pane}
        visible={props.detailsOpen}
      />
      {messages.sheet}
      {attach ? <AttachSheet onChoose={attach.choose} onDismiss={() => setAttaching(false)} visible={attaching} /> : null}
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
  banner: { alignItems: "center", backgroundColor: colors.accentWash, borderBottomColor: colors.chromeHairline, borderBottomWidth: 1, flexDirection: "row", gap: 12, paddingHorizontal: 14, paddingVertical: 8 },
  bannerDanger: { backgroundColor: colors.dangerWash },
  bannerText: { color: colors.chromeInk, flex: 1, fontSize: typeScale.rowSecondary, lineHeight: 18 },
  bannerButton: { justifyContent: "center", minHeight: 44, paddingHorizontal: 4 },
  bannerAction: { color: colors.accent, fontSize: typeScale.rowSecondary, fontWeight: "600" },
});
