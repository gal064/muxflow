import { useCallback, useContext, useEffect, useRef, useState } from "react";
import { Image, Pressable, StyleSheet, Text, View } from "react-native";

import { colors, typeScale } from "../../../ui/tokens";
import { displayMessageFor } from "../../files/errors";
import { isImage, sizeLabel } from "../attach/attachState";
import { ChatImagesContext } from "../attach/chatImages";
import { forgetLocalImage, localImageFor } from "../attach/localImages";
import { FileIcon, ImageIcon } from "./AttachIcons";
import { ImageViewer, type ViewedImage } from "./ImageViewer";

/**
 * A file a message carries: `ChatAttachment` as the host's item has it, or
 * the one a message on its way was sent with, which also knows where the file
 * is on this phone.
 */
export interface ShownAttachment {
  /** Absolute, on the host. Empty for an image the agent's record holds inline. */
  path: string;
  name: string;
  /** Such as "image/png". Empty when not known. */
  mediaType: string;
  sizeBytes: number | bigint;
  localUri?: string;
}

export interface AttachmentChipsProps {
  /** None, or absent, draws nothing. */
  attachments: readonly ShownAttachment[] | undefined;
  /** The message has text under the chips, so they keep a gap from it. */
  withText?: boolean;
  /**
   * The chips that can be tapped take a hold too, which would otherwise not
   * reach the message they sit in: pass the message's own.
   */
  onLongPress?: (() => void) | undefined;
}

/** The image the viewer is showing: `fetched` is a file read from the host for it, to be given back. */
interface Viewing {
  name: string;
  image: ViewedImage;
  fetched?: string;
}

/**
 * The files of one of your messages (mobile-chat-mock.html, S7). An image
 * this phone attached is its thumbnail and opens at once. Any other image
 * that is a file on the host is a named chip, read from the host only when
 * tapped. An image the agent's record holds inline, and any other file, is a
 * named chip that does nothing.
 */
export function AttachmentChips({ attachments, withText = false, onLongPress }: AttachmentChipsProps) {
  const images = useContext(ChatImagesContext);
  const [viewing, setViewing] = useState<Viewing | undefined>(undefined);
  // Counts what was asked for, so an image that arrives after the viewer closed is not shown.
  const asked = useRef(0);
  // Thumbnails whose file has gone from the phone: they are named chips from then on.
  const [gone, setGone] = useState<ReadonlySet<string>>(NONE);

  const last = useRef<Viewing | undefined>(undefined);
  last.current = viewing;
  const close = useCallback(() => {
    asked.current += 1;
    const fetched = last.current?.fetched;
    if (fetched !== undefined) images.release(fetched);
    setViewing(undefined);
  }, [images]);
  // The message left the screen with the viewer open.
  const held = useRef(images);
  held.current = images;
  useEffect(() => () => {
    asked.current += 1;
    const fetched = last.current?.fetched;
    if (fetched !== undefined) held.current.release(fetched);
  }, []);

  const load = useCallback((attachment: ShownAttachment) => {
    const token = ++asked.current;
    const name = attachment.name || "Image";
    setViewing({ name, image: { state: "loading" } });
    images.load(attachment.path).then(
      ({ uri }) => {
        if (asked.current !== token) return images.release(uri);
        setViewing({ name, image: { state: "image", uri }, fetched: uri });
      },
      (error: unknown) => {
        if (asked.current === token) setViewing({ name, image: { state: "error", message: displayMessageFor(error) || "The image could not be loaded." } });
      },
    );
  }, [images]);

  if (!attachments || attachments.length === 0) return null;
  return (
    <View style={[styles.chips, withText && styles.chipsOverText]}>
      {attachments.map((attachment, index) => {
        const name = attachment.name || (isImage(attachment) ? "Image" : "File");
        const key = `${index}:${attachment.path}:${attachment.name}`;
        if (!isImage(attachment)) return <NamedChip attachment={attachment} key={key} label={`File ${name}`} name={name} />;
        const local = gone.has(key) ? undefined : attachment.localUri ?? localImageFor(attachment.path, Number(attachment.sizeBytes));
        if (local !== undefined) {
          return (
            <Pressable
              accessibilityHint="Opens it"
              accessibilityLabel={`Image ${name}`}
              accessibilityRole="imagebutton"
              delayLongPress={350}
              key={key}
              onLongPress={onLongPress}
              onPress={() => setViewing({ name, image: { state: "image", uri: local } })}
              style={({ pressed }) => [styles.thumb, pressed && styles.pressed]}
            >
              <Image
                onError={() => {
                  forgetLocalImage(attachment.path);
                  setGone((keys) => new Set(keys).add(key));
                }}
                resizeMethod="resize"
                resizeMode="cover"
                source={{ uri: local }}
                style={styles.thumbImage}
              />
            </Pressable>
          );
        }
        // Inline in the agent's record: there is no file to read.
        if (attachment.path === "") return <NamedChip attachment={attachment} key={key} label={`Image ${name}`} name={name} />;
        return <NamedChip attachment={attachment} key={key} label={`Image ${name}`} name={name} onLongPress={onLongPress} onPress={() => load(attachment)} />;
      })}
      {viewing ? <ImageViewer image={viewing.image} name={viewing.name} onClose={close} /> : null}
    </View>
  );
}

const NONE: ReadonlySet<string> = new Set();

/** A mark, the name and the size. With `onPress` it is an image that opens when tapped. */
function NamedChip({ attachment, name, label, onPress, onLongPress }: { attachment: ShownAttachment; name: string; label: string; onPress?: () => void; onLongPress?: (() => void) | undefined }) {
  const Mark = isImage(attachment) ? ImageIcon : FileIcon;
  const size = Number(attachment.sizeBytes) > 0 ? sizeLabel(attachment.sizeBytes) : "";
  const inside = (
    <>
      <Mark color={colors.chromeDim} size={18} />
      <View style={styles.chipText}>
        <Text ellipsizeMode="middle" numberOfLines={1} style={styles.chipName}>{name}</Text>
        {size ? <Text numberOfLines={1} style={styles.chipSize}>{size}</Text> : null}
      </View>
    </>
  );
  if (!onPress) return <View accessibilityLabel={label} accessible style={styles.chip}>{inside}</View>;
  return (
    <Pressable
      accessibilityHint="Loads it from the host and opens it"
      accessibilityLabel={label}
      accessibilityRole="button"
      delayLongPress={350}
      onLongPress={onLongPress}
      onPress={onPress}
      style={({ pressed }) => [styles.chip, pressed && styles.pressed]}
    >
      {inside}
    </Pressable>
  );
}

const styles = StyleSheet.create({
  // Above the message's text, against the same edge. Side by side where they
  // fit, so a message of many images is not one tall column of them.
  chips: { flexDirection: "row", flexWrap: "wrap", gap: 6, justifyContent: "flex-end", paddingVertical: 2 },
  chipsOverText: { paddingBottom: 6 },
  pressed: { opacity: 0.75 },
  thumb: { backgroundColor: colors.chromeBg, borderColor: colors.chromeBorder, borderRadius: 9, borderWidth: 1, height: 96, overflow: "hidden", width: 136 },
  thumbImage: { height: "100%", width: "100%" },
  chip: { alignItems: "center", borderColor: colors.chromeBorder, borderRadius: 8, borderWidth: 1, flexDirection: "row", gap: 8, maxWidth: "100%", minHeight: 44, paddingHorizontal: 10, paddingVertical: 5 },
  chipText: { flexShrink: 1 },
  chipName: { color: colors.chromeInk, fontSize: typeScale.meta, lineHeight: 16 },
  chipSize: { color: colors.chromeFaint, fontSize: typeScale.keyMono, lineHeight: 15 },
});
