import type { MessageInitShape } from "@bufbuild/protobuf";
import { useFocusEffect, useRouter } from "expo-router";
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
import { sideQuestion, slashMatches, turnOf, withPickedCommand } from "./chatModel";
import { chatClient, chatStore, setVoiceMode } from "./chatRuntime";
import { refusalText } from "./refusals";
import { ChatView } from "./ui/ChatView";
import type { PendingAnswer } from "./ui/PendingCard";
import { useChatVoice } from "./useChatVoice";
import { useGitDetails } from "./useGitDetails";

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

  const [draft, setDraft] = useState("");
  const [editing, setEditing] = useState<ChatItem | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const commands = useMemo(() => slashMatches(pane?.catalog, draft), [draft, pane?.catalog]);
  const voice = useChatVoice({ paneId, sessionId, serverIdentity, pane, enabled: voiceMode, connected });
  const git = useGitDetails({ paneId, serverIdentity, folder, turn: turnOf(pane), open: detailsOpen, connected });

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

  const send = useCallback(() => {
    const text = draft.trim();
    if (!text) return;
    const target = editing;
    const question = target ? undefined : sideQuestion(pane?.catalog, text);
    void run(async () => {
      if (target) {
        await chatClient.rewindAndSend(paneId, target.id, text);
        setEditing(undefined);
        setDraft("");
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
      // The message shows at once; the draft comes back if it was refused.
      setDraft("");
      try {
        await chatClient.send(paneId, text);
      } catch (error) {
        setDraft((current) => current || text);
        throw error;
      }
    });
  }, [draft, editing, pane?.catalog, paneId, router, run]);

  const answer = useCallback((given: PendingAnswer) => {
    const requestId = pane?.state?.pending?.requestId;
    if (!requestId) return;
    void run(() => chatClient.answer(paneId, requestId,
      given.kind === "permission" ? { allow: given.allow } : given.kind === "skip" ? { skipped: true } : { answers: given.answers }));
  }, [pane?.state?.pending?.requestId, paneId, run]);

  const setOptions = useCallback((options: MessageInitShape<typeof ChatOptionsSchema>) => {
    void run(() => chatClient.setOptions(paneId, options));
  }, [paneId, run]);

  const pickCommand = useCallback((command: ChatCommand) => setDraft((current) => withPickedCommand(current, command)), []);
  const edit = useCallback((item: ChatItem) => {
    setEditing(item);
    setDraft(item.text);
  }, []);
  const cancelEdit = useCallback(() => {
    setEditing(undefined);
    setDraft("");
  }, []);
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

  // A refusal is said as a toast, and passed on so the task's row can be stopped again.
  const stopTask = useCallback(async (taskId: string) => {
    try {
      await chatClient.stopTask(paneId, taskId);
    } catch (error) {
      toast(refusalText(error));
      throw error;
    }
  }, [paneId]);

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
      <ChatView
        busy={busy}
        commands={commands}
        connected={connected}
        detailsOpen={detailsOpen}
        draft={draft}
        editing={editing}
        folder={folder}
        git={git}
        hostLabel={hostLabel}
        onAnswer={answer}
        onBack={() => router.back()}
        onCancelEdit={cancelEdit}
        onDetails={setDetailsOpen}
        onDraft={setDraft}
        onEdit={edit}
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
      />
    </Animated.View>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
});
