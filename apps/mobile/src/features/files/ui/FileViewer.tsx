// The file viewer (design doc §9.7) and the Markdown WebView bridge (§10.3).

import { useCallback, useEffect, useRef, useState } from "react";
import { Stack } from "expo-router";
import { Linking, Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { WebView, type WebViewMessageEvent } from "react-native-webview";

import { useStore } from "zustand";

import { colors, fonts, radii, terminalTheme, typeScale } from "../../../ui/tokens";
import { MARKDOWN_HTML } from "../../../webview/markdownBundle";
import { relativeToRoot } from "../activeRoot";
import { filesStore } from "../filesStore";
import type { FilePresentation, ViewerMode } from "../presentation";
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
  const subtitle = rootPath ? relativeToRoot(rootPath, path) : path;

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

type Inbound = { t?: unknown; href?: unknown };

/**
 * The Markdown page (§10.3). The document is inlined at build time and handed
 * to the WebView as a string, so nothing is ever fetched: every navigation
 * except the initial `about:blank` load is refused, and `http(s)`/`mailto`
 * links are opened by the phone instead.
 */
function MarkdownView({ source }: { source: string }) {
  const webView = useRef<WebView>(null);
  const ready = useRef(false);

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
      if (message.t === "link" && typeof message.href === "string" && /^(?:https?|mailto):/iu.test(message.href)) {
        void Linking.openURL(message.href).catch(() => {});
      }
    },
    [send],
  );

  return (
    <WebView
      ref={webView}
      allowFileAccess={false}
      allowFileAccessFromFileURLs={false}
      allowUniversalAccessFromFileURLs={false}
      androidLayerType="hardware"
      javaScriptEnabled
      onMessage={onMessage}
      onShouldStartLoadWithRequest={(request) => request.url === "about:blank" || request.url.startsWith("about:")}
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
    paddingHorizontal: 12,
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
