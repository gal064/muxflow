import { useEffect, useState } from "react";
import { ActivityIndicator, Image, Modal, Pressable, StyleSheet, Text, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";
import { CloseIcon } from "./AttachIcons";

/** What the viewer has of the image: still being read, a file on the phone, or why it could not be read. */
export type ViewedImage = { state: "loading" } | { state: "image"; uri: string } | { state: "error"; message: string };

export interface ImageViewerProps {
  name: string;
  image: ViewedImage;
  onClose: () => void;
}

/** Said when the phone has the file and cannot draw it. */
export const IMAGE_NOT_DRAWN = "This image can't be shown on the phone.";

/** An attached image over the whole screen, with its name and a way out. Back closes it too. */
export function ImageViewer({ name, image, onClose }: ImageViewerProps) {
  const insets = useSafeAreaInsets();
  const uri = image.state === "image" ? image.uri : "";
  // The file is there and is not something the phone can draw (an SVG, say).
  const [undrawn, setUndrawn] = useState(false);
  useEffect(() => setUndrawn(false), [uri]);
  const message = image.state === "error" ? image.message : undrawn ? IMAGE_NOT_DRAWN : undefined;
  return (
    <Modal animationType="fade" onRequestClose={onClose} statusBarTranslucent visible>
      <View style={[styles.root, { paddingBottom: insets.bottom, paddingTop: insets.top }]}>
        <View style={styles.bar}>
          <Text {...fixedChromeText} ellipsizeMode="middle" numberOfLines={1} style={styles.name}>{name}</Text>
          <Pressable accessibilityLabel="Close" accessibilityRole="button" onPress={onClose} style={({ pressed }) => [styles.close, pressed && styles.pressed]}>
            <CloseIcon color={colors.chromeInkStrong} size={20} />
          </Pressable>
        </View>
        <View style={styles.stage}>
          {message !== undefined ? (
            <Text accessibilityRole="alert" style={styles.message}>{message}</Text>
          ) : image.state === "loading" ? (
            <>
              <ActivityIndicator color={colors.chromeDim} />
              <Text style={styles.loading}>Loading the image…</Text>
            </>
          ) : (
            <Image accessibilityLabel={name} onError={() => setUndrawn(true)} resizeMethod="resize" resizeMode="contain" source={{ uri }} style={styles.image} />
          )}
        </View>
      </View>
    </Modal>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: "#000", flex: 1 },
  bar: { alignItems: "center", flexDirection: "row", gap: 8, minHeight: 48, paddingLeft: 16, paddingRight: 2 },
  name: { color: colors.chromeInk, flex: 1, fontSize: typeScale.rowSecondary },
  close: { alignItems: "center", height: 48, justifyContent: "center", width: 48 },
  pressed: { opacity: 0.6 },
  stage: { alignItems: "center", flex: 1, gap: 12, justifyContent: "center" },
  image: { alignSelf: "stretch", flex: 1 },
  message: { color: colors.chromeInk, fontSize: typeScale.body, lineHeight: 20, paddingHorizontal: 28, textAlign: "center" },
  loading: { color: colors.chromeDim, fontSize: typeScale.rowSecondary },
});
