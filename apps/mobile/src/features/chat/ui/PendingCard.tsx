import { useState } from "react";
import { Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";

import { ChatRequestKind, type ChatPendingRequest, type ChatQuestion } from "../../../protocol/gen/chat_pb";
import { colors, fonts, typeScale } from "../../../ui/tokens";
import { ChevronDownIcon } from "./ChatIcons";

/** What the user chose for one question. */
export interface QuestionChoice {
  optionIds: string[];
  other: string;
  /** The "Other" row is selected. */
  otherOn: boolean;
}

export type PendingAnswer =
  | { kind: "permission"; allow: boolean }
  | { kind: "questions"; answers: { questionId: string; optionIds: string[]; otherText: string }[] }
  | { kind: "skip" };

export interface PendingCardProps {
  request: ChatPendingRequest;
  /** An answer is on its way; the buttons wait for it. */
  busy: boolean;
  onAnswer: (answer: PendingAnswer) => void;
  /** Folds the card away; absent, its header has no arrow. */
  onFold?: (() => void) | undefined;
}

/** Whether the request is a question the card asks, rather than a permission it grants. */
export function isQuestion(request: ChatPendingRequest): boolean {
  return request.kind === ChatRequestKind.QUESTION && request.questions.length > 0;
}

/**
 * The request the agent stopped on, as a card: a header that stays, and a
 * body that scrolls inside the card when the card is given less room than it
 * needs.
 */
export function PendingCard({ request, busy, onAnswer, onFold }: PendingCardProps) {
  if (isQuestion(request)) {
    return <QuestionCard busy={busy} key={request.requestId} onAnswer={onAnswer} onFold={onFold} request={request} />;
  }
  return <PermissionCard busy={busy} onAnswer={onAnswer} onFold={onFold} request={request} />;
}

/** The header's arrow. Its drawing is small, so the touch target reaches past it. */
function FoldButton({ onFold }: { onFold: () => void }) {
  return (
    <Pressable accessibilityLabel="Collapse request" accessibilityRole="button" hitSlop={12} onPress={onFold} style={({ pressed }) => [styles.fold, pressed && styles.pressed]}>
      <ChevronDownIcon color={colors.chromeFaint} size={18} />
    </Pressable>
  );
}

/** Auto mode declined to decide: a plain Allow / Deny so the turn does not hang. */
function PermissionCard({ request, busy, onAnswer, onFold }: PendingCardProps) {
  return (
    <View accessibilityLabel={`${request.title || "Permission request"}${request.detail ? `: ${request.detail}` : ""}, waiting for you`} style={[styles.card, styles.askCard]}>
      <View style={[styles.head, styles.askHead]}>
        <Text style={[styles.headTitle, styles.flex]}>{request.title || "Allow this action?"}</Text>
        {onFold ? <FoldButton onFold={onFold} /> : null}
      </View>
      <ScrollView contentContainerStyle={styles.body} keyboardShouldPersistTaps="handled" style={styles.scroll}>
        {request.detail ? <Text selectable style={styles.command}>{request.detail}</Text> : null}
        <View style={styles.buttons}>
          <CardButton disabled={busy} label="Allow" onPress={() => onAnswer({ kind: "permission", allow: true })} primary />
          <CardButton disabled={busy} label="Deny" onPress={() => onAnswer({ kind: "permission", allow: false })} />
        </View>
      </ScrollView>
    </View>
  );
}

/** One question at a time, "1 of 2"; Submit answers them all together. */
function QuestionCard({ request, busy, onAnswer, onFold }: PendingCardProps) {
  const questions = request.questions;
  const [step, setStep] = useState(0);
  const [choices, setChoices] = useState<QuestionChoice[]>(() => questions.map(() => ({ optionIds: [], other: "", otherOn: false })));
  const question = questions[step]!;
  const choice = choices[step]!;
  const last = step === questions.length - 1;
  const set = (next: QuestionChoice) => setChoices((all) => all.map((entry, index) => (index === step ? next : entry)));
  const toggle = (optionId: string) => {
    if (question.multiSelect) {
      const on = choice.optionIds.includes(optionId);
      set({ ...choice, optionIds: on ? choice.optionIds.filter((id) => id !== optionId) : [...choice.optionIds, optionId] });
    } else {
      set({ ...choice, optionIds: [optionId], otherOn: false });
    }
  };
  const toggleOther = () => set(question.multiSelect ? { ...choice, otherOn: !choice.otherOn } : { ...choice, optionIds: [], otherOn: true });
  const answered = choice.optionIds.length > 0 || (choice.otherOn && choice.other.trim() !== "");
  const submit = () => onAnswer({
    kind: "questions",
    answers: questions.map((entry, index) => ({
      questionId: entry.id,
      optionIds: choices[index]!.optionIds,
      otherText: choices[index]!.otherOn ? choices[index]!.other.trim() : "",
    })),
  });
  return (
    <View style={styles.card}>
      <View style={styles.head}>
        <Text style={[styles.headTitle, styles.flex]}>{question.prompt || question.header || request.title}</Text>
        {questions.length > 1 ? <Text style={styles.headNote}>{step + 1} of {questions.length}</Text> : null}
        {onFold ? <FoldButton onFold={onFold} /> : null}
      </View>
      <ScrollView contentContainerStyle={styles.body} keyboardShouldPersistTaps="handled" style={styles.scroll}>
        {question.multiSelect ? <Text style={styles.hint}>Choose any that apply.</Text> : null}
        {question.options.map((option) => (
          <OptionRow
            description={option.description}
            key={option.id}
            label={option.label}
            multi={question.multiSelect}
            on={choice.optionIds.includes(option.id)}
            onPress={() => toggle(option.id)}
          />
        ))}
        {question.allowOther ? (
          <>
            <OptionRow description="Type your own answer." label="Other" multi={question.multiSelect} on={choice.otherOn} onPress={toggleOther} />
            {choice.otherOn ? (
              <TextInput
                accessibilityLabel="Your answer"
                autoFocus
                multiline
                onChangeText={(other) => set({ ...choice, other })}
                placeholder="Your answer"
                placeholderTextColor={colors.chromeFaint}
                style={styles.other}
                value={choice.other}
              />
            ) : null}
          </>
        ) : null}
        <View style={styles.buttons}>
          {last
            ? <CardButton disabled={busy || !allAnswered(questions, choices)} label="Submit" onPress={submit} primary />
            : <CardButton disabled={!answered} label="Next" onPress={() => setStep(step + 1)} primary />}
          {step > 0 ? <CardButton disabled={busy} label="Back" onPress={() => setStep(step - 1)} /> : null}
          <CardButton disabled={busy} ghost label="Skip" onPress={() => onAnswer({ kind: "skip" })} />
        </View>
      </ScrollView>
    </View>
  );
}

export function allAnswered(questions: ChatQuestion[], choices: QuestionChoice[]): boolean {
  return questions.every((_, index) => {
    const choice = choices[index];
    return choice !== undefined && (choice.optionIds.length > 0 || (choice.otherOn && choice.other.trim() !== ""));
  });
}

function OptionRow({ label, description, on, multi, onPress }: { label: string; description: string; on: boolean; multi: boolean; onPress: () => void }) {
  return (
    <Pressable
      accessibilityLabel={description ? `${label}. ${description}` : label}
      accessibilityRole={multi ? "checkbox" : "radio"}
      accessibilityState={{ checked: on }}
      onPress={onPress}
      style={({ pressed }) => [styles.option, on && styles.optionOn, pressed && styles.pressed]}
    >
      <View style={[styles.mark, multi && styles.markSquare, on && styles.markOn]} />
      <View style={styles.flex}>
        <Text style={styles.optionLabel}>{label}</Text>
        {description ? <Text style={styles.optionDescription}>{description}</Text> : null}
      </View>
    </Pressable>
  );
}

function CardButton({ label, onPress, disabled, primary, ghost }: { label: string; onPress: () => void; disabled?: boolean; primary?: boolean; ghost?: boolean }) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityState={{ disabled: disabled === true }}
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [styles.button, primary && styles.buttonPrimary, ghost && styles.buttonGhost, disabled && styles.disabled, pressed && styles.pressed]}
    >
      <Text style={[styles.buttonLabel, primary && styles.buttonLabelPrimary, ghost && styles.buttonLabelGhost]}>{label}</Text>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  flex: { flex: 1, minWidth: 0 },
  // Shrinks to the room it is given; the body scrolls then.
  card: { backgroundColor: colors.chromeBg, borderColor: colors.chromeBorder, borderRadius: 9, borderWidth: 1, flexShrink: 1, overflow: "hidden" },
  scroll: { flexGrow: 0, flexShrink: 1 },
  fold: { alignItems: "center", height: 28, justifyContent: "center", width: 28 },
  askCard: { borderColor: colors.danger },
  head: { alignItems: "center", borderBottomColor: colors.chromeHairline, borderBottomWidth: 1, flexDirection: "row", gap: 8, paddingHorizontal: 12, paddingVertical: 8 },
  askHead: { backgroundColor: colors.dangerWash },
  headTitle: { color: colors.chromeInkStrong, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  headNote: { color: colors.chromeFaint, fontSize: typeScale.meta },
  body: { gap: 8, padding: 12 },
  hint: { color: colors.chromeDim, fontSize: typeScale.meta },
  command: { backgroundColor: colors.chromeRaised, borderColor: colors.chromeHairline, borderRadius: 5, borderWidth: 1, color: colors.chromeInkStrong, fontFamily: fonts.mono, fontSize: 12, paddingHorizontal: 8, paddingVertical: 6 },
  buttons: { flexDirection: "row", flexWrap: "wrap", gap: 8, paddingTop: 2 },
  button: { alignItems: "center", borderColor: colors.chromeBorder, borderRadius: 6, borderWidth: 1, justifyContent: "center", minHeight: 44, minWidth: 72, paddingHorizontal: 14 },
  buttonPrimary: { backgroundColor: colors.accent, borderColor: colors.accent },
  buttonGhost: { borderColor: "transparent" },
  buttonLabel: { color: colors.chromeInk, fontSize: typeScale.body },
  buttonLabelPrimary: { color: colors.accentInk, fontWeight: "600" },
  buttonLabelGhost: { color: colors.chromeDim },
  disabled: { opacity: 0.45 },
  pressed: { opacity: 0.75 },
  option: { alignItems: "flex-start", borderColor: colors.chromeHairline, borderRadius: 7, borderWidth: 1, flexDirection: "row", gap: 10, minHeight: 44, paddingHorizontal: 10, paddingVertical: 8 },
  optionOn: { backgroundColor: colors.accentWash, borderColor: colors.accent },
  mark: { borderColor: colors.chromeFaint, borderRadius: 8, borderWidth: 1.5, height: 16, marginTop: 2, width: 16 },
  markSquare: { borderRadius: 3 },
  markOn: { backgroundColor: colors.accent, borderColor: colors.accent },
  optionLabel: { color: colors.chromeInkStrong, fontSize: typeScale.body, fontWeight: "500" },
  optionDescription: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, lineHeight: 18 },
  other: { backgroundColor: colors.chromeRaised, borderColor: colors.chromeBorder, borderRadius: 7, borderWidth: 1, color: colors.chromeInkStrong, fontSize: typeScale.body, minHeight: 44, paddingHorizontal: 10, paddingVertical: 8 },
});
