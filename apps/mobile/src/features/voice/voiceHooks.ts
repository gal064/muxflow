import { useStore } from "zustand";
import { voiceStore, type VoiceActions, type VoiceState } from "./voiceStore";

/** Subscribe a component to a slice of the app-wide voice store. */
export function useVoice<T>(selector: (state: VoiceState & VoiceActions) => T): T {
  return useStore(voiceStore, selector);
}
