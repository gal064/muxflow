// The file viewer (design doc §9.7) and the Markdown WebView bridge (§10.3).

import { useCallback, useEffect, useRef, useState } from "react";
import { Stack } from "expo-router";
import { Directory, File, Paths } from "expo-file-system";
import { Image, Linking, Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import Pdf from "react-native-pdf";
import { WebView, type WebViewMessageEvent } from "react-native-webview";

import { useStore } from "zustand";

import { colors, fonts, radii, terminalTheme, typeScale } from "../../../ui/tokens";
import { externalLinkTarget, markdownNavigationStaysInApp } from "../../../ui/externalLinks";
import { MARKDOWN_HTML } from "../../../webview/markdownBundle";
import { relativeToRoot, subtitleFor } from "../activeRoot";
import { filesStore } from "../filesStore";
import { FILE_VIEWER_COPY, type FilePresentation, type ViewerMode } from "../presentation";
import { useFileBody } from "../useFileBody";
import { CentredMessage, CentredSpinner, ErrorState, StreamingBar, TitleBlock } from "./parts";

export interface FileViewerProps {
  paneId: string;
  /** Exactly the `FileMetadata.path` the listing row carried. */
  path: string;
  name: string;
}

export function FileViewer({ paneId, path, name }: FileViewerProps) {
  const { view, reload } = useFileBody(paneId, path, name);
  const [mode, setMode] = useState<ViewerMode>("rendered");
  const markdown = view.status === "ready" && view.presentation.kind === "markdown";
  // The root is known before the body is: the screen that pushed this one
  // resolved it. Reading it from the store keeps §9.7's "path relative to root"
  // true while the file is still streaming and after it failed to open.
  const rootPath = useStore(filesStore, (state) => state.roots[paneId]?.root ?? "");
  const subtitle = subtitleFor(name, rootPath ? relativeToRoot(rootPath, path) : path);

  return (
    <View style={styles.root}>
      <Stack.Screen
        options={{
          headerTitle: () => <TitleBlock title={name} subtitle={subtitle} />,
          headerRight: markdown ? () => <ModeToggle mode={mode} onChange={setMode} /> : undefined,
        }}
      />
      {view.status === "loading" ? (
        <>
          <StreamingBar />
          <CentredSpinner />
        </>
      ) : null}
      {view.status === "error" ? <ErrorState message={view.message} onRetry={reload} /> : null}
      {view.status === "ready" ? <Body presentation={view.presentation} mode={mode} /> : null}
    </View>
  );
}

function Body({ presentation, mode }: { presentation: FilePresentation; mode: ViewerMode }) {
  switch (presentation.kind) {
    case "markdown":
      return mode === "rendered" ? <MarkdownView source={presentation.text} /> : <PlainView text={presentation.text} />;
    case "plain":
      return <PlainView text={presentation.text} />;
    case "image":
      return <ImagePreview bytes={presentation.bytes} />;
    case "pdf":
      return <PdfPreview bytes={presentation.bytes} />;
    case "placeholder":
      return <CentredMessage message={presentation.message} />;
  }
}

/** §9.7: `Rendered | Source`, in the app bar, Rendered first. */
function ModeToggle({ mode, onChange }: { mode: ViewerMode; onChange: (mode: ViewerMode) => void }) {
  return (
    <View style={styles.toggle}>
      {(["rendered", "source"] as const).map((value) => (
        <Pressable
          accessibilityRole="button"
          accessibilityState={{ selected: mode === value }}
          // The segments have to fit a 56 dp app bar, so the touchable area is
          // grown past the painted pill rather than the pill past the bar.
          hitSlop={{ bottom: 12, left: 8, right: 8, top: 12 }}
          key={value}
          onPress={() => onChange(value)}
          style={[styles.toggleOption, mode === value && styles.toggleOptionSelected]}
        >
          <Text style={[styles.toggleLabel, mode === value && styles.toggleLabelSelected]}>
            {value === "rendered" ? "Rendered" : "Source"}
          </Text>
        </Pressable>
      ))}
    </View>
  );
}

/**
 * §9.7: monospace, no wrapping, horizontal scroll, no line numbers.
 *
 * The horizontal scroller sits inside the vertical one so a long line pans
 * without the whole page moving.
 */
function PlainView({ text }: { text: string }) {
  return (
    <ScrollView style={styles.plainRoot} contentContainerStyle={styles.plainContent}>
      <ScrollView horizontal showsHorizontalScrollIndicator contentContainerStyle={styles.plainRow}>
        <Text style={styles.plainText} allowFontScaling={false} selectable>
          {text}
        </Text>
      </ScrollView>
    </ScrollView>
  );
}

/** Where a preview's bytes are on the phone. */
type CacheFile = { state: "writing" } | { state: "written"; uri: string } | { state: "failed" };

let previews = 0;

/**
 * Writes `bytes` to a cache file of their own for as long as the caller is
 * mounted: `Image` and react-native-pdf draw from a file, not from bytes. A
 * fresh name per open keeps the image cache, which goes by URI, from showing
 * an earlier file. The file is deleted when the view goes.
 */
function useCacheFile(bytes: Uint8Array, extension: string): CacheFile {
  const [file, setFile] = useState<CacheFile>({ state: "writing" });
  useEffect(() => {
    let live = true;
    setFile({ state: "writing" });
    const directory = new Directory(Paths.cache, "file-previews");
    // The first preview of this run clears what a run that ended with one open left behind.
    const sweep = previews === 0;
    previews += 1;
    const target = new File(directory, `${Date.now().toString(36)}-${previews}.${extension}`);
    const discard = () => {
      try {
        if (target.exists) target.delete();
      } catch {
        // Already cleared from the cache, which is what was wanted.
      }
    };
    const write = async () => {
      if (sweep && directory.exists) directory.delete();
      if (!directory.exists) directory.create({ intermediates: true });
      await target.write(bytes);
    };
    write().then(
      () => (live ? setFile({ state: "written", uri: target.uri }) : discard()),
      () => {
        if (live) setFile({ state: "failed" });
        discard();
      },
    );
    return () => {
      live = false;
      discard();
    };
  }, [bytes, extension]);
  return file;
}

/** An image fitted to the screen. */
function ImagePreview({ bytes }: { bytes: Uint8Array }) {
  const file = useCacheFile(bytes, "img");
  // The file is there and is not something the phone can draw (an SVG, say).
  const [undrawn, setUndrawn] = useState<string>();
  if (file.state === "writing") return <CentredSpinner />;
  if (file.state === "failed" || undrawn === file.uri) return <CentredMessage message={FILE_VIEWER_COPY.imageNotShown} />;
  const { uri } = file;
  return (
    <Image
      onError={() => setUndrawn(uri)}
      resizeMethod="resize"
      resizeMode="contain"
      source={{ uri }}
      style={styles.preview}
    />
  );
}

/** A PDF, with react-native-pdf's own scrolling, paging and pinch zoom. */
function PdfPreview({ bytes }: { bytes: Uint8Array }) {
  const file = useCacheFile(bytes, "pdf");
  // Damaged, encrypted, or otherwise not a PDF the platform can open.
  const [undrawn, setUndrawn] = useState<string>();
  if (file.state === "writing") return <CentredSpinner />;
  if (file.state === "failed" || undrawn === file.uri) return <CentredMessage message={FILE_VIEWER_COPY.pdfNotShown} />;
  const { uri } = file;
  return (
    <Pdf
      onError={() => setUndrawn(uri)}
      onPressLink={openExternal}
      source={{ uri }}
      style={styles.preview}
      trustAllCerts={false}
    />
  );
}

/** Opens an `http(s)`/`mailto` link with the phone; anything else is ignored. */
function openExternal(href: string) {
  const target = externalLinkTarget(href);
  if (target) void Linking.openURL(target).catch(() => {});
}

type Inbound = { t?: unknown; href?: unknown };

/**
 * The Markdown page (§10.3). The document is inlined at build time and handed
 * to the WebView as a string, so nothing is ever fetched: every navigation
 * except the initial `about:blank` load is refused, and `http(s)`/`mailto`
 * links are opened by the phone instead.
 */
function MarkdownView({ source }: { source: string }) {
  const webView = useRef<WebView<object>>(null);
  const ready = useRef(false);

  // `render` is idempotent, so posting it from both `ready` and `onLoadEnd`
  // costs nothing and stops one lost message from leaving a blank page with no
  // way back short of leaving the screen.
  const send = useCallback(() => {
    webView.current?.postMessage(JSON.stringify({ t: "render", source }));
  }, [source]);

  useEffect(() => {
    if (ready.current) send();
  }, [send]);

  const onMessage = useCallback(
    (event: WebViewMessageEvent) => {
      let message: Inbound;
      try {
        message = JSON.parse(event.nativeEvent.data) as Inbound;
      } catch {
        return;
      }
      if (message.t === "ready") {
        ready.current = true;
        send();
        return;
      }
      if (message.t === "link" && typeof message.href === "string") {
        const target = externalLinkTarget(message.href);
        if (target) void Linking.openURL(target).catch(() => {});
      }
    },
    [send],
  );

  const allowNavigation = useCallback((url: string) => {
    if (markdownNavigationStaysInApp(url)) return true;
    const target = externalLinkTarget(url);
    if (target) void Linking.openURL(target).catch(() => {});
    return false;
  }, []);

  // react-native-webview 14.0.1 declares `WebView<P = undefined>`, which makes its props `never` (upstream #3970); name the generic.

  return (
    <WebView<object>
      ref={webView}
      allowFileAccess={false}
      allowFileAccessFromFileURLs={false}
      allowUniversalAccessFromFileURLs={false}
      androidLayerType="hardware"
      javaScriptEnabled
      onLoadEnd={send}
      onMessage={onMessage}
      onShouldStartLoadWithRequest={(request) => allowNavigation(request.url)}
      originWhitelist={["about:blank"]}
      setSupportMultipleWindows={false}
      source={{ html: MARKDOWN_HTML }}
      style={styles.webView}
      containerStyle={styles.webViewContainer}
    />
  );
}

const styles = StyleSheet.create({
  root: {
    backgroundColor: colors.chromeBg,
    flex: 1,
  },
  webView: {
    backgroundColor: colors.chromeBg,
  },
  preview: {
    backgroundColor: colors.chromeBg,
    flex: 1,
    width: "100%",
  },
  webViewContainer: {
    backgroundColor: colors.chromeBg,
    flex: 1,
  },
  plainRoot: {
    backgroundColor: terminalTheme.background,
    flex: 1,
  },
  plainContent: {
    paddingVertical: 12,
  },
  plainRow: {
    paddingHorizontal: 16,
  },
  plainText: {
    color: colors.chromeInk,
    fontFamily: fonts.mono,
    fontSize: 12,
    lineHeight: 17,
  },
  toggle: {
    backgroundColor: colors.chromeRaised,
    borderRadius: radii.pill,
    flexDirection: "row",
    overflow: "hidden",
    padding: 2,
  },
  toggleOption: {
    borderRadius: radii.pill - 2,
    paddingHorizontal: 10,
    paddingVertical: 5,
  },
  toggleOptionSelected: {
    backgroundColor: colors.accentWash,
  },
  toggleLabel: {
    color: colors.chromeDim,
    fontSize: typeScale.meta,
    fontWeight: "600",
  },
  toggleLabelSelected: {
    color: colors.accent,
  },
});
