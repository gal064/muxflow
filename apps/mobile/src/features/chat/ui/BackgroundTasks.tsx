import { useCallback, useEffect, useRef, useState } from "react";
import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";

import { ChatTaskKind, type ChatBackgroundTask } from "../../../protocol/gen/chat_pb";
import { Sheet } from "../../../ui/components/Sheet";
import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";
import { backgroundLabel, startedAt, taskLine } from "../steps";
import { BotIcon, LayersIcon, TerminalIcon } from "./ChatIcons";
import { useRan } from "./useRan";

/** A Stop the host has not answered, with the task still listed, can be pressed again after this long. */
const STOP_WAIT_MS = 10_000;

export interface BackgroundTasksProps {
  /** What the agent runs outside the turn (`ChatState.background`); empty draws nothing. */
  tasks: readonly ChatBackgroundTask[];
  /** Stop was pressed for a task. A promise that rejects says the host refused, and the task can be stopped again. */
  onStop: (taskId: string) => Promise<void> | void;
}

/**
 * The chip above the reply field for as long as anything runs in the
 * background, turn or no turn, and the sheet it opens (mobile-chat-mock.html,
 * S4): each task with what it is and how long it has run, and Stop where the
 * agent can stop it.
 */
export function BackgroundTasks({ tasks, onStop }: BackgroundTasksProps) {
  const [open, setOpen] = useState(false);
  // The last task ended: the sheet has nothing left to show, and the next task does not reopen it.
  useEffect(() => {
    if (tasks.length === 0) setOpen(false);
  }, [tasks.length]);

  // Tasks whose Stop was pressed, each with the timer that gives up on it.
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const [stopping, setStopping] = useState<ReadonlySet<string>>(NONE);
  const settle = useCallback((taskId: string) => {
    clearTimeout(timers.current.get(taskId));
    if (!timers.current.delete(taskId)) return;
    setStopping(new Set(timers.current.keys()));
  }, []);
  useEffect(() => {
    const held = timers.current;
    return () => held.forEach((timer) => clearTimeout(timer));
  }, []);
  // A task that left the list has stopped.
  useEffect(() => {
    for (const taskId of [...timers.current.keys()]) {
      if (!tasks.some((task) => task.id === taskId)) settle(taskId);
    }
  }, [settle, tasks]);
  const stop = useCallback((taskId: string) => {
    // A press the host has not answered yet is not sent twice.
    if (timers.current.has(taskId)) return;
    timers.current.set(taskId, setTimeout(() => settle(taskId), STOP_WAIT_MS));
    setStopping(new Set(timers.current.keys()));
    // Refused, or not sent at all: the row can be stopped again.
    void (async () => onStop(taskId))().catch(() => settle(taskId));
  }, [onStop, settle]);

  const label = backgroundLabel(tasks.length);
  return (
    <>
      {tasks.length > 0 ? (
        <View style={styles.chipLine}>
          <Pressable
            accessibilityHint="Lists them"
            accessibilityLabel={label}
            accessibilityRole="button"
            hitSlop={CHIP_SLOP}
            onPress={() => setOpen(true)}
            style={({ pressed }) => [styles.chip, pressed && styles.pressed]}
          >
            <LayersIcon color={colors.chromeDim} size={14} />
            <Text {...fixedChromeText} numberOfLines={1} style={styles.chipLabel}>{label}</Text>
          </Pressable>
        </View>
      ) : null}
      <Sheet onDismiss={() => setOpen(false)} title="Background tasks" visible={open && tasks.length > 0}>
        <ScrollView style={styles.scroll}>
          {tasks.map((task, index) => (
            <TaskRow key={task.id} last={index === tasks.length - 1} onStop={stop} stopping={stopping.has(task.id)} task={task} />
          ))}
        </ScrollView>
      </Sheet>
    </>
  );
}

const NONE: ReadonlySet<string> = new Set();

function TaskRow({ task, stopping, last, onStop }: { task: ChatBackgroundTask; stopping: boolean; last: boolean; onStop: (taskId: string) => void }) {
  // The time runs while the sheet is open: a closed sheet draws no rows.
  const ran = useRan(startedAt(task.startedAtUnixMillis));
  const Icon = task.kind === ChatTaskKind.COMMAND ? TerminalIcon : task.kind === ChatTaskKind.SUBAGENT ? BotIcon : LayersIcon;
  return (
    <View style={[styles.row, last && styles.rowLast, stopping && styles.rowStopping]}>
      <Icon color={colors.chromeInkStrong} size={18} />
      <View style={styles.rowText}>
        <Text numberOfLines={2} style={styles.description}>{task.description}</Text>
        <Text numberOfLines={1} style={styles.what}>{taskLine(task.kind, ran)}</Text>
      </View>
      {task.stoppable ? (
        <Pressable
          accessibilityLabel={`Stop ${task.description}`}
          accessibilityRole="button"
          accessibilityState={{ disabled: stopping }}
          disabled={stopping}
          hitSlop={STOP_SLOP}
          onPress={() => onStop(task.id)}
          style={({ pressed }) => [styles.stop, pressed && styles.pressed]}
        >
          <Text {...fixedChromeText} style={styles.stopLabel}>Stop</Text>
        </Pressable>
      ) : null}
    </View>
  );
}

const CHIP_HEIGHT = 26;
// Both are slimmer than a touch target; the slop makes each 44 dp tall.
const CHIP_SLOP = { bottom: (44 - CHIP_HEIGHT) / 2, left: 8, right: 8, top: (44 - CHIP_HEIGHT) / 2 } as const;
const STOP_HEIGHT = 32;
const STOP_SLOP = { bottom: (44 - STOP_HEIGHT) / 2, left: 8, right: 8, top: (44 - STOP_HEIGHT) / 2 } as const;

const styles = StyleSheet.create({
  chipLine: { flexDirection: "row", paddingBottom: 6, paddingHorizontal: 14 },
  chip: {
    alignItems: "center",
    backgroundColor: colors.chromeBg,
    borderColor: colors.chromeBorder,
    borderRadius: 999,
    borderWidth: 1,
    flexDirection: "row",
    gap: 6,
    height: CHIP_HEIGHT,
    paddingHorizontal: 10,
  },
  chipLabel: { color: colors.chromeDim, fontSize: typeScale.meta },
  pressed: { opacity: 0.6 },
  scroll: { maxHeight: 420 },
  row: { alignItems: "center", borderBottomColor: colors.chromeHairline, borderBottomWidth: 1, flexDirection: "row", gap: 12, minHeight: 56, paddingHorizontal: 2, paddingVertical: 10 },
  rowLast: { borderBottomWidth: 0 },
  // As a message on its way is: Stop was pressed, and the task has not left the list yet.
  rowStopping: { opacity: 0.6 },
  rowText: { flex: 1, gap: 1, minWidth: 0 },
  description: { color: colors.chromeInkStrong, fontSize: typeScale.body, lineHeight: 20 },
  what: { color: colors.chromeFaint, fontSize: typeScale.meta, fontVariant: ["tabular-nums"], lineHeight: 16 },
  stop: { alignItems: "center", borderColor: colors.chromeBorder, borderRadius: 6, borderWidth: 1, height: STOP_HEIGHT, justifyContent: "center", paddingHorizontal: 12 },
  stopLabel: { color: colors.dangerInk, fontSize: typeScale.rowSecondary },
});
