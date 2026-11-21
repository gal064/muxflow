// What kind of file a name says it is. The desktop's rule (`media_type_of`
// in apps/desktop-gpui/src/chat/attach.rs) with HEIC added, which is what an
// iPhone's camera writes. The host reads the file's own bytes before it hands
// one to the agent as an image; this only decides how the phone treats it.

const IMAGE_TYPES: Readonly<Record<string, string>> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  heic: "image/heic",
};

/** An image type such as "image/png" for a file named `name`, from its extension; "" for any other file. */
export function mediaTypeForName(name: string): string {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return "";
  return IMAGE_TYPES[name.slice(dot + 1).toLowerCase()] ?? "";
}
