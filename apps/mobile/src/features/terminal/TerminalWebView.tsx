import { forwardRef, useCallback, useImperativeHandle, useRef } from "react";
import { StyleSheet } from "react-native";
import { WebView, type WebViewMessageEvent } from "react-native-webview";

import { PAGE_RECEIVE_FUNCTION, parseFromPageMessage, type FromPageMessage, type ToPageMessage } from "./bridgeMessages";
import { TERMINAL_HTML } from "./terminalHtml";
import { colors } from "../../ui/tokens";

const TERMINAL_DOCUMENT_ORIGIN = "https://terminal.muxflow.invalid";
const TERMINAL_DOCUMENT_URL = `${TERMINAL_DOCUMENT_ORIGIN}/`;

export interface TerminalWebViewHandle {
  send(message: ToPageMessage): void;
}

/**
 * The xterm WebView (§9.5 item 2, §10.2). Loads the build-time HTML string;
 * messages go in through an injected call to the page's receive function and
 * come back through `onMessage`. `textZoom={100}` pins the cell size against
 * the system font scale, which would otherwise break the sizing rule.
 */
export const TerminalWebView = forwardRef<TerminalWebViewHandle, { onMessage: (message: FromPageMessage) => void; onLoadEnd?: () => void }>(
  function TerminalWebView({ onMessage, onLoadEnd }, ref) {
    const webview = useRef<WebView>(null);
    useImperativeHandle(ref, () => ({
      send(message) {
        webview.current?.injectJavaScript(`window.${PAGE_RECEIVE_FUNCTION} && window.${PAGE_RECEIVE_FUNCTION}(${JSON.stringify(message)}); true;`);
      },
    }), []);
    const handleMessage = useCallback((event: WebViewMessageEvent) => {
      // `input` is a privileged bridge message: only the bundled terminal
      // document may send it. Blocking navigation as well keeps an OSC link
      // or `window.open` from replacing that document with an untrusted page.
      // Modern Android reports the sender origin; older message paths report
      // the document URL. Both identify the same fixed bundled document.
      if (event.nativeEvent.url !== TERMINAL_DOCUMENT_ORIGIN && event.nativeEvent.url !== TERMINAL_DOCUMENT_URL) return;
      const message = parseFromPageMessage(event.nativeEvent.data);
      if (message) onMessage(message);
    }, [onMessage]);
    return (
      <WebView
        allowFileAccess={false}
        allowsBackForwardNavigationGestures={false}
        androidLayerType="hardware"
        bounces={false}
        cacheEnabled={false}
        domStorageEnabled={false}
        javaScriptEnabled
        nestedScrollEnabled
        onLoadEnd={onLoadEnd}
        onMessage={handleMessage}
        onShouldStartLoadWithRequest={(request) => request.url === TERMINAL_DOCUMENT_URL}
        originWhitelist={[TERMINAL_DOCUMENT_ORIGIN]}
        overScrollMode="never"
        ref={webview}
        setSupportMultipleWindows={false}
        source={{ html: TERMINAL_HTML, baseUrl: TERMINAL_DOCUMENT_URL }}
        style={styles.webview}
        textZoom={100}
      />
    );
  },
);

const styles = StyleSheet.create({
  webview: { backgroundColor: colors.chromeBg, flex: 1 },
});
