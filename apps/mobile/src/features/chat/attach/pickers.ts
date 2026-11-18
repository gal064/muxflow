// The three ways to choose a file to attach (docs/design/mobile-chat-mock.html,
// S7): the photo library, the camera, and the system's file picker. Each
// answers the files as the picker described them; nothing here reads a byte.
//
// The pickers answer a `file://` copy in the app's cache: the image picker
// always does, and the file picker is asked to (`copyToCacheDirectory`), so
// expo-file-system can read what Android hands over as a `content://` URI.
// Neither needs a storage permission; only the camera asks for one.

import * as DocumentPicker from "expo-document-picker";
import * as ImagePicker from "expo-image-picker";

export type AttachChoice = "library" | "camera" | "file";

/** A chosen file. `name` and `size` are the picker's word, when it gave any. */
export interface PickedFile {
  uri: string;
  name?: string;
  size?: number;
}

/** Said when the camera may not be used. */
export const CAMERA_REFUSED = "Muxflow may not use the camera. Allow it in the phone's settings to take a photo.";

/**
 * Opens the picker for `choice` and answers what was chosen, at most `limit`
 * files; nothing when the user backed out. Rejects with a sentence to show.
 */
export async function pickFiles(choice: AttachChoice, limit: number, now: Date = new Date()): Promise<PickedFile[]> {
  if (limit <= 0) return [];
  switch (choice) {
    case "library": {
      // `quality: 1` and no editing: the file goes as it is, and a GIF stays one.
      const result = await ImagePicker.launchImageLibraryAsync({
        mediaTypes: ["images"],
        allowsMultipleSelection: true,
        selectionLimit: limit,
        quality: 1,
        exif: false,
      });
      if (result.canceled) return [];
      return result.assets.slice(0, limit).map((asset) => picked(asset.uri, asset.fileName, asset.fileSize));
    }
    case "camera": {
      const permission = await ImagePicker.requestCameraPermissionsAsync();
      if (!permission.granted) throw new Error(CAMERA_REFUSED);
      const result = await ImagePicker.launchCameraAsync({ mediaTypes: ["images"], quality: 1, exif: false });
      if (result.canceled) return [];
      // The camera writes a file with a random name; the message shows a readable one.
      return result.assets.slice(0, 1).map((asset) => picked(asset.uri, photoName(asset.uri, now), asset.fileSize));
    }
    case "file": {
      const result = await DocumentPicker.getDocumentAsync({ type: "*/*", copyToCacheDirectory: true, multiple: true });
      if (result.canceled) return [];
      // The system picker has no limit of its own: the queue refuses what does not fit.
      return result.assets.map((asset) => picked(asset.uri, asset.name, asset.size));
    }
  }
}

function picked(uri: string, name: string | null | undefined, size: number | null | undefined): PickedFile {
  return {
    uri,
    ...(name ? { name } : {}),
    ...(typeof size === "number" && size >= 0 ? { size } : {}),
  };
}

/** "Photo 2026-11-18 14.03.22.jpg": a photo just taken, named by when, with the extension the camera gave its file. */
export function photoName(uri: string, now: Date): string {
  const two = (value: number): string => String(value).padStart(2, "0");
  const day = `${now.getFullYear()}-${two(now.getMonth() + 1)}-${two(now.getDate())}`;
  const time = `${two(now.getHours())}.${two(now.getMinutes())}.${two(now.getSeconds())}`;
  const extension = /\.([A-Za-z0-9]{1,5})$/.exec(uri.split(/[?#]/)[0] ?? "")?.[1]?.toLowerCase() ?? "jpg";
  return `Photo ${day} ${time}.${extension}`;
}
