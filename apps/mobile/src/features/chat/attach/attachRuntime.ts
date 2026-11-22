// The chat's files bound to the app: its connections, the phone's pickers and
// its file system. `ChatScreen` is the only one to import this; everything it
// binds is tested on its own with fakes.
//
// An upload runs on a bulk connection of its own (`openUploadConnection`),
// because it closes that connection when it ends and the host keeps a partial
// only for as long as the connection lives. Reading an image back does not:
// it is one stream that leaves its connection open, as the file viewer's is,
// so it shares the viewer's lane (`openBulkConnection`) and pays for no dial.

import { Directory, File, Paths } from "expo-file-system";

import { getConnection, openBulkConnection, openUploadConnection, toast } from "../../../session/connectionManager";
import { sessionStore } from "../../../store/sessionStore";
import type { ChatImages } from "./chatImages";
import { localFileSource } from "./fileSource";
import { pickFiles } from "./pickers";
import { readAttachedImage } from "./readImage";
import { uploadFile } from "./upload";
import type { AttachDeps } from "./useAttachments";

export const attachDeps: AttachDeps = {
  upload: (source, signal, onProgress) => uploadFile({ openLane: openUploadConnection, source, signal, onProgress }),
  pick: pickFiles,
  toSource: (file) => {
    // The size is the copy's own, which is what will be read; the picker's
    // word, which may be of the original, only when the copy cannot say.
    const source = localFileSource(file.uri, file.name === undefined ? {} : { name: file.name });
    return source.size > 0 || file.size === undefined ? source : { ...source, size: file.size };
  },
  say: toast,
};

const EXTENSIONS: Readonly<Record<string, string>> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
  "image/bmp": "bmp",
  "image/svg+xml": "svg",
};

let shown = 0;

/** The images of the chat in `paneId`, read through the app's connections. */
export function chatImages(paneId: string): ChatImages {
  return {
    async load(hostPath) {
      const control = getConnection();
      const session = sessionStore.getState();
      const pane = session.panes[paneId];
      if (!control || control.state !== "connected") throw new Error("Not connected.");
      if (!pane) throw new Error("This chat's pane has closed.");
      const image = await readAttachedImage(
        { control, openBulk: openBulkConnection },
        { pane, serverIdentity: session.serverIdentity, topologyGeneration: session.topologyGeneration },
        hostPath,
      );
      // React Native draws an image from a file, not from bytes. A name of
      // its own each time: the image cache goes by the URI.
      const directory = new Directory(Paths.cache, "chat-images");
      if (!directory.exists) directory.create({ intermediates: true });
      shown += 1;
      const file = new File(directory, `${Date.now().toString(36)}-${shown}.${EXTENSIONS[image.mediaType] ?? "img"}`);
      await file.write(image.bytes);
      return { uri: file.uri };
    },
    release(uri) {
      try {
        const file = new File(uri);
        if (file.exists) file.delete();
      } catch {
        // A file the system already cleared from the cache is gone, which is what was wanted.
      }
    },
  };
}
