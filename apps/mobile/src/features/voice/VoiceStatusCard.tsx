import { useState } from "react";
import { StyleSheet, Text, View } from "react-native";

import { Button } from "../../ui/components/Button";
import { Dialog } from "../../ui/components/Dialog";
import { StreamingBar } from "../files/ui/parts";
import { colors, fonts, radii, typeScale } from "../../ui/tokens";
import { formatBytes, provisionFraction, provisionPhaseLabel } from "./format";
import type { VoiceController } from "./VoiceController";
import { useVoice } from "./voiceHooks";
import type { VoiceHostStatus } from "./voiceStore";

/**
 * The readiness card under the message list (design.md §9.11): the uv install
 * hint, the consent-gated "Set up voice" button with its progress bar, or
 * nothing once the host is ready. Subscribes to `hostStatus` itself so
 * progress lines re-render this card only.
 */
export function VoiceStatusCard({ controller, connected }: { controller: VoiceController; connected: boolean }) {
  const status = useVoice((s) => s.hostStatus);
  const [consent, setConsent] = useState(false);
  const size = status.modelDownloadBytes > 0 ? formatBytes(status.modelDownloadBytes) : "~640 MB";
  return (
    <>
      <ReadinessCard connected={connected} controller={controller} onSetUp={() => setConsent(true)} size={size} status={status} />
      <ConsentDialog
        onCancel={() => setConsent(false)}
        onConfirm={() => {
          setConsent(false);
          void controller.provision();
        }}
        size={size}
        visible={consent}
      />
    </>
  );
}

function ReadinessCard({ status, connected, controller, size, onSetUp }: { status: VoiceHostStatus; connected: boolean; controller: VoiceController; size: string; onSetUp: () => void }) {
  if (status.readiness === "ready") return null;
  if (!connected && status.readiness === "unknown") return null;

  if (status.readiness === "unknown") {
    // Also the resting state after a STATUS that failed (timeout, dropped lane): the button is the retry.
    return (
      <View style={styles.card}>
        <Text style={styles.body}>Checking voice on the host…</Text>
        <Button label="Check again" onPress={() => void controller.refreshStatus(true)} variant="secondary" />
      </View>
    );
  }

  if (status.readiness === "uvMissing") {
    return (
      <View style={styles.card}>
        <Text style={styles.heading}>uv not found on this host</Text>
        <Text style={styles.body}>Voice needs the uv Python runner on the host machine.</Text>
        {status.detail ? <Text numberOfLines={3} selectable style={styles.detail}>{status.detail}</Text> : null}
        <Button label="Check again" onPress={() => void controller.refreshStatus(true)} variant="secondary" />
      </View>
    );
  }

  if (status.readiness === "provisioning") {
    const progress = status.provision;
    const fraction = progress ? provisionFraction(progress.transferredBytes, progress.totalBytes) : undefined;
    const percent = fraction === undefined ? undefined : Math.round(fraction * 100);
    return (
      <View style={styles.card}>
        <Text style={styles.heading}>Setting up voice</Text>
        <Text style={styles.body}>{provisionPhaseLabel(progress?.phase ?? "")}</Text>
        {fraction === undefined ? (
          <View accessibilityLabel="Setup progress" accessibilityRole="progressbar" accessibilityValue={{ text: "in progress" }}>
            <StreamingBar />
          </View>
        ) : (
          <View accessibilityLabel="Setup progress" accessibilityRole="progressbar" accessibilityValue={{ min: 0, max: 100, now: percent }} style={styles.track}>
            <View style={[styles.progress, { width: `${fraction * 100}%` }]} />
          </View>
        )}
        {progress && progress.totalBytes > 0 ? (
          <Text style={styles.meta}>{formatBytes(progress.transferredBytes)} of {formatBytes(progress.totalBytes)} · {percent}%</Text>
        ) : null}
      </View>
    );
  }

  // modelMissing
  const failed = status.provision?.phase === "failed" ? status.provision.error : "";
  return (
    <View style={styles.card}>
      <Text style={styles.heading}>Voice isn't set up on this host</Text>
      <Text style={styles.body}>The speech model is downloaded once, onto the host, and stays there.</Text>
      {failed ? <Text style={styles.error}>Last attempt failed: {failed}</Text> : null}
      <Button disabled={!connected} label="Set up voice" onPress={onSetUp} />
    </View>
  );
}

/** The consent dialog, mounted whatever the readiness so closing it is a `visible={false}` render, not an unmount mid-fade. */
function ConsentDialog({ visible, size, onCancel, onConfirm }: { visible: boolean; size: string; onCancel: () => void; onConfirm: () => void }) {
  return (
    <Dialog
      actions={[
        { label: "Cancel", onPress: onCancel },
        { label: "Download", onPress: onConfirm, variant: "primary" },
      ]}
      message={`This downloads the speech model (${size}) to the host and installs its Python runtime with uv. Nothing is downloaded to this phone.`}
      onDismiss={onCancel}
      title="Set up voice on the host?"
      visible={visible}
    />
  );
}

const styles = StyleSheet.create({
  card: { backgroundColor: colors.chromeRaised, borderRadius: radii.card, gap: 10, marginHorizontal: 12, marginVertical: 8, padding: 16 },
  heading: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle, fontWeight: "600" },
  body: { color: colors.chromeInk, fontSize: typeScale.body },
  detail: { color: colors.chromeDim, fontFamily: fonts.mono, fontSize: typeScale.keyMono },
  error: { color: colors.dangerInk, fontSize: typeScale.rowSecondary },
  meta: { color: colors.chromeDim, fontSize: typeScale.meta },
  track: { backgroundColor: colors.chromeBorder, borderRadius: 2, height: 4, overflow: "hidden" },
  progress: { backgroundColor: colors.accent, height: 4 },
});
