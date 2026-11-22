// A file on the phone as the upload's byte source, read a chunk at a time
// through expo-file-system so a large file is never held whole. In its own
// module because it is the only one here that needs the native side: the
// tests of the upload and the queue do not import it.

import { File, FileMode } from "expo-file-system";
import type { AttachmentSource } from "./attachmentQueue";

/**
 * The file a picker or the camera answered with. `known` carries what the
 * picker said of it; its name wins over the URI's last segment, which for a
 * `content://` URI is not a file name.
 */
export function localFileSource(uri: string, known: { name?: string; size?: number } = {}): AttachmentSource {
  const file = new File(uri);
  if (!file.exists) throw new Error("That file is no longer on the phone.");
  return {
    name: known.name || file.name,
    size: known.size ?? file.size,
    localUri: uri,
    // Opened for each read and closed after it, so a chip that is removed or
    // an upload that fails leaves no handle open.
    async read(offset, length) {
      const handle = file.open(FileMode.ReadOnly);
      try {
        handle.offset = offset;
        const first = await handle.readBytes(length);
        if (first.length === length) return first;
        // A read may answer fewer bytes than asked before the end of the file.
        const bytes = new Uint8Array(length);
        bytes.set(first);
        let filled = first.length;
        while (filled < length) {
          const more = await handle.readBytes(length - filled);
          if (more.length === 0) break;
          bytes.set(more, filled);
          filled += more.length;
        }
        return bytes.subarray(0, filled);
      } finally {
        handle.close();
      }
    },
  };
}
