import type { MessageInitShape } from "@bufbuild/protobuf";
import { useFocusEffect, useIsFocused, useRouter } from "expo-router";
import { useCallback, useEffect, useMemo, useState } from "react";
import { StyleSheet } from "react-native";
import Animated, { useAnimatedKeyboard, useAnimatedStyle } from "react-native-reanimated";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { useStore } from "zustand";

import { ChatTurnState, type ChatCommand, type ChatItem, type ChatOptionsSchema } from "../../protocol/gen/chat_pb";
import { toRouteParam } from "../../navigation/routeParams";
import { toast } from "../../session/connectionManager";
import { colors } from "../../ui/tokens";
import { useSession } from "../../ui/hooks";
import { agentForPane, agentTitle } from "../agents/agentViews";
import { ConnectionStrip } from "../hosts/ConnectionStrip";
import { notificationAttention } from "../notifications/attention";
import { waitForPane } from "../terminal/WorkspaceActionsSheet";
import { attachDeps, chatImages } from "./attach/attachRuntime";
import { filesRefusal, filesState } from "./attach/attachState";
import { ChatImagesContext } from "./attach/chatImages";
import { sendWithFiles, useAttachments } from "./attach/useAttachments";
import { sideQuestion, slashMatches, turnOf, withDictation, withPickedCommand } from "./chatModel";
import { chatClient, chatStore, setVoiceMode } from "./chatRuntime";
import { refusalText } from "./refusals";
import { ChatView } from "./ui/ChatView";
import type { PendingAnswer } from "./ui/PendingCard";
import { useChatVoice } from "./useChatVoice";
import { useFork } from "./useFork";
import { useGitDetails } from "./useGitDetails";
import { useReplyDraft } from "./useReplyDraft";

const FORK_PANE_WAIT_MS = 3_000;
/** A Stop the host has not answered, with the turn still working, is Stop again after this long. */
const STOP_WAIT_MS = 10_000;

export interface ChatScreenProps {
  paneId: string;
  sessionId: string;
  /** Opened from an agent's Talk: start in voice mode. */
  startInVoice?: boolean;
}

/** A chat pane on the phone (docs/design/chat-plan.md §5 F). */
export function ChatScreen({ paneId, sessionId, startInVoice }: ChatScreenProps) {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  // Edge-to-edge Android does not resize the window for the keyboard, so pad
  // by the IME inset Reanimated reads, as the terminal does.
  const keyboard = useAnimatedKeyboard({ isStatusBarTranslucentAndroid: true, isNavigationBarTranslucentAndroid: true });
  const keyboardPadding = useAnimatedStyle(() => ({
    paddingBottom: Math.max(insets.bottom, keyboard.height.value),
  }), [insets.bottom]);
  useEffect(() => chatClient.acquire(paneId), [paneId]);
  useEffect(() => {
    if (startInVoice) setVoiceMode(paneId, true);
  }, [paneId, startInVoice]);

  const pane = useStore(chatStore, (s) => s.panes[paneId]);
  const voiceMode = useStore(chatStore, (s) => s.voiceMode[paneId] === true);
  const connected = useSession((s) => s.connection.state === "connected");
  const serverIdentity = useSession((s) => s.serverIdentity);
  const hostLabel = useSession((s) => s.connection.host?.label || s.connection.host?.host || "");
  const folder = useSession((s) => s.panes[paneId]?.currentPath ?? "");
  const agent = useSession((s) => agentForPane(s, paneId));
  const windows = useSession((s) => s.windows);
  const adapters = useSession((s) => s.adapters);
  const windowName = useSession((s) => s.windows[s.panes[paneId]?.windowId ?? ""]?.name ?? "");
  const title = agent ? agentTitle({ windows, adapters }, agent) : windowName || "Chat";

  // The reply field: this chat's draft, kept while the app runs, or the message being edited.
  const { draft, setDraft, editing, setEditing, edit, cancelEdit, sentEdit } = useReplyDraft(serverIdentity, paneId);
  // The files on the reply field (S7). Leaving the chat stops their uploads.
  const files = useAttachments(paneId, attachDeps);
  const images = useMemo(() => chatImages(paneId), [paneId]);
  // An edited message's files come back with its text, as they are on the
  // host, in place of what was on the field; cancelling takes them off again.
  // An image the agent's record holds inline has no file to send again.
  const { replace: replaceFiles, clear: clearFiles } = files;
  const editWithFiles = useCallback((item: ChatItem) => {
    replaceFiles(item.attachments.flatMap((file) => (file.path === "" ? [] : [{ path: file.path, name: file.name, mediaType: file.mediaType, sizeBytes: Number(file.sizeBytes) }])));
    edit(item);
  }, [edit, replaceFiles]);
  const cancelEditWithFiles = useCallback(() => {
    if (editing) clearFiles();
    cancelEdit();
  }, [cancelEdit, clearFiles, editing]);
  const [busy, setBusy] = useState(false);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const commands = useMemo(() => slashMatches(pane?.catalog, draft), [draft, pane?.catalog]);
  const git = useGitDetails({ paneId, serverIdentity, folder, turn: turnOf(pane), open: detailsOpen, connected });

  const focused = useIsFocused();
  const agentId = agent?.id;
  useFocusEffect(useCallback(() => (agentId ? notificationAttention.focusAgent(agentId) : undefined), [agentId]));

  // An edited message that is no longer in the timeline (rewound elsewhere) cannot be edited.
  useEffect(() => {
    if (editing && pane && pane.status === "open" && !pane.items.some((item) => item.id === editing.id)) setEditing(undefined);
  }, [editing, pane]);

  const run = useCallback(async (work: () => Promise<void>) => {
    setBusy(true);
    try {
      await work();
    } catch (error) {
      toast(refusalText(error));
    } finally {
      setBusy(false);
    }
  }, []);

  /** Sends `text` as the message, with the files on the reply field; the draft is cleared. Settles when it has gone or been refused. */
  const sendText = useCallback((text: string): Promise<void> => {
    const target = editing;
    const question = target ? undefined : sideQuestion(pane?.catalog, text);
    // A side question goes alone: the files wait for the next message.
    const waiting = question === undefined ? filesState(files.entries) : "none";
    if (!text && waiting === "none") return Promise.resolve();
    // Send is off while a file uploads or after one failed; said here too, should a send get past that.
    const held = filesRefusal(waiting);
    if (held !== undefined) {
      toast(held);
      return Promise.resolve();
    }
    return run(async () => {
      if (target) {
        await sendWithFiles(files, (attached) => chatClient.rewindAndSend(paneId, target.id, text, attached));
        sentEdit();
        return;
      }
      if (question !== undefined) {
        const forked = await chatClient.sideFork(paneId, question);
        setDraft("");
        if (!forked) return;
        // Opened before the topology names the fork, the chat would read as closed.
        await waitForPane(forked, FORK_PANE_WAIT_MS);
        router.push({ pathname: "/chat/[paneId]", params: { paneId: toRouteParam(forked) } });
        return;
      }
      // The message shows at once, with its files; the draft and the files come back if it was refused.
      setDraft("");
      try {
        await sendWithFiles(files, (attached) => chatClient.send(paneId, text, attached));
      } catch (error) {
        setDraft((current) => current || text);
        throw error;
      }
    });
  }, [editing, files, pane?.catalog, paneId, router, run, setDraft, sentEdit]);
  const send = useCallback(() => void sendText(draft.trim()), [draft, sendText]);
  // A dictation is what was typed with what was said after it, sent as Send
  // sends it. While the files cannot go yet it waits in the field instead.
  const dictate = useCallback(async (spoken: string) => {
    const text = withDictation(draft, spoken);
    // As in sendText: a side question goes alone, so only other messages wait for the files.
    const question = editing ? undefined : sideQuestion(pane?.catalog, text);
    const held = question === undefined ? filesRefusal(filesState(files.entries)) : undefined;
    if (held !== undefined) {
      setDraft(text);
      toast(held);
      return;
    }
    await sendText(text);
  }, [draft, editing, files.entries, pane?.catalog, sendText, setDraft]);

  const answer = useCallback((given: PendingAnswer) => {
    const requestId = pane?.state?.pending?.requestId;
    if (!requestId) return;
    void run(() => chatClient.answer(paneId, requestId,
      given.kind === "permission" ? { allow: given.allow } : given.kind === "skip" ? { skipped: true } : { answers: given.answers }));
  }, [pane?.state?.pending?.requestId, paneId, run]);

  const setOptions = useCallback((options: MessageInitShape<typeof ChatOptionsSchema>) => {
    void run(() => chatClient.setOptions(paneId, options));
  }, [paneId, run]);

  const pickCommand = useCallback((command: ChatCommand) => setDraft((current) => withPickedCommand(current, command)), [setDraft]);
  const retry = useCallback(() => chatClient.retry(paneId), [paneId]);

  // Stop shows at once, until the turn stops working, the host refuses, the
  // connection goes, or no answer comes.
  const [stopping, setStopping] = useState(false);
  const working = turnOf(pane) === ChatTurnState.WORKING;
  useEffect(() => {
    if (!working || !connected) setStopping(false);
  }, [connected, working]);
  useEffect(() => {
    if (!stopping) return undefined;
    const timer = setTimeout(() => setStopping(false), STOP_WAIT_MS);
    return () => clearTimeout(timer);
  }, [stopping]);
  const stop = useCallback(() => {
    // A press the host has not answered yet is not sent twice.
    if (stopping) return;
    setStopping(true);
    void run(async () => {
      try {
        await chatClient.interrupt(paneId);
      } catch (error) {
        setStopping(false);
        throw error;
      }
    });
  }, [paneId, run, stopping]);

  const leaveVoiceMode = useCallback(() => setVoiceMode(paneId, false), [paneId]);
  const voice = useChatVoice({ paneId, sessionId, serverIdentity, pane, voiceMode, connected, stopping, onStop: stop, onKeyboard: leaveVoiceMode, dictate });

  // A refusal is said as a toast, and passed on so the task's row can be stopped again.
  const stopTask = useCallback(async (taskId: string) => {
    try {
      await chatClient.stopTask(paneId, taskId);
    } catch (error) {
      toast(refusalText(error));
      throw error;
    }
  }, [paneId]);

  // Fork from here: the fork opens as a side question's does. A refusal is said as a toast.
  const { forking, fork } = useFork({
    fork: (itemId) => chatClient.fork(paneId, itemId),
    wait: (forked) => waitForPane(forked, FORK_PANE_WAIT_MS),
    open: (forked) => router.push({ pathname: "/chat/[paneId]", params: { paneId: toRouteParam(forked) } }),
    say: toast,
  });

  // Earlier messages load by themselves, so a failure is said in the
  // conversation, where Retry is, and as a toast only when Retry was pressed.
  // A fresh snapshot starts over.
  const [olderFailed, setOlderFailed] = useState(false);
  const snapshotSeq = pane?.snapshotSeq;
  useEffect(() => setOlderFailed(false), [paneId, snapshotSeq]);
  const loadOlder = useCallback((asked: boolean) => {
    setOlderFailed(false);
    chatClient.loadOlder(paneId).catch((error: unknown) => {
      setOlderFailed(true);
      if (asked) toast(refusalText(error));
    });
  }, [paneId]);
  const loadOlderOnReach = useCallback(() => loadOlder(false), [loadOlder]);
  const retryOlder = useCallback(() => loadOlder(true), [loadOlder]);

  return (
    <Animated.View style={[styles.root, { paddingTop: insets.top }, keyboardPadding]}>
      <ConnectionStrip />
      <ChatImagesContext.Provider value={images}>
        <ChatView
          attach={files}
          busy={busy}
          commands={commands}
          connected={connected}
          detailsOpen={detailsOpen}
          draft={draft}
          editing={editing}
          folder={folder}
          forking={forking}
          git={git}
          hostLabel={hostLabel}
          onAnswer={answer}
          onBack={() => router.back()}
          onCancelEdit={cancelEditWithFiles}
          onDetails={setDetailsOpen}
          onDraft={setDraft}
          onEdit={editWithFiles}
          onFork={fork}
          olderFailed={olderFailed}
          onLoadOlder={loadOlderOnReach}
          onPickCommand={pickCommand}
          onRetry={retry}
          onRetryOlder={retryOlder}
          onSend={send}
          onSetOptions={setOptions}
          onStop={stop}
          onStopTask={stopTask}
          onVoiceMode={(on) => setVoiceMode(paneId, on)}
          pane={pane}
          stopping={stopping}
          title={title}
          voice={voice}
          voiceMode={voiceMode}
          focused={focused}
        />
      </ChatImagesContext.Provider>
    </Animated.View>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
});
