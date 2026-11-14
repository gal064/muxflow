// The one recorder, player, file and haptics adapter for the app, created when
// the first voice surface renders (§2c: nothing audio-related loads before
// that). The Voice screen and a chat in voice mode share it.

import { log } from "../../session/log";
import { createExpoFiles } from "./files";
import { createExpoHaptics } from "./haptics";
import { createExpoPlayer } from "./player";
import { createExpoRecorder } from "./recorder";
import { createExpoTones } from "./tones";

let audio: { recorder: ReturnType<typeof createExpoRecorder>; player: ReturnType<typeof createExpoPlayer>; files: ReturnType<typeof createExpoFiles>; haptics: ReturnType<typeof createExpoHaptics>; tones: ReturnType<typeof createExpoTones> } | undefined;

export function sharedAudio() {
  audio ??= { recorder: createExpoRecorder(log), player: createExpoPlayer(), files: createExpoFiles(), haptics: createExpoHaptics(), tones: createExpoTones() };
  return audio;
}
