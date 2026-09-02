// Files voice mode touches: the recording it reads and the one MP3 per
// session it keeps under `<cache>/voice/` (docs/mobile/voice-mode-plan.md §1
// "latest reply only"). `VoiceFiles` is what the controller drives;
// `createExpoFiles` is the expo-file-system implementation.

import { Directory, File, Paths } from "expo-file-system";

export interface VoiceFiles {
  read(uri: string): Promise<Uint8Array>;
  /** Writes the reply MP3 for `agentId`, replacing any previous one, and returns its uri. */
  writeReply(agentId: string, bytes: Uint8Array): string;
  delete(uri: string): void;
}

export function createExpoFiles(): VoiceFiles {
  let directory: Directory | undefined;
  const voiceDirectory = (): Directory => {
    if (directory) return directory;
    directory = new Directory(Paths.cache, "voice");
    if (!directory.exists) directory.create({ intermediates: true });
    return directory;
  };
  return {
    read(uri) {
      return new File(uri).bytes();
    },
    writeReply(agentId, bytes) {
      const file = new File(voiceDirectory(), `${replyFileName(agentId)}.mp3`);
      if (file.exists) file.delete();
      file.write(bytes);
      return file.uri;
    },
    delete(uri) {
      try {
        const file = new File(uri);
        if (file.exists) file.delete();
      } catch {
        // A file the OS already evicted from the cache is the outcome we wanted.
      }
    },
  };
}

/** Agent ids carry `:` and `%`; keep the file name to a safe alphabet. */
export function replyFileName(agentId: string): string {
  let out = "";
  for (let index = 0; index < agentId.length; index += 1) {
    const ch = agentId[index] as string;
    out += /[A-Za-z0-9_-]/.test(ch) ? ch : `~${ch.charCodeAt(0).toString(16)}`;
  }
  return out;
}
