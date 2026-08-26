import { forwardRef, useCallback, useImperativeHandle, useRef } from "react";
import { StyleSheet } from "react-native";
import { WebView, type WebViewMessageEvent } from "react-native-webview";

import { PAGE_RECEIVE_FUNCTION, parseFromPageMessage, type FromPageMessage, type ToPageMessage } from "./bridgeMessages";
import { TERMINAL_HTML } from "./terminalHtml";
import { colors } from "../../ui/tokens";

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
        originWhitelist={["*"]}
        overScrollMode="never"
        ref={webview}
        setSupportMultipleWindows={false}
        source={{ html: TERMINAL_HTML }}
        style={styles.webview}
        textZoom={100}
      />
    );
  },
);

const styles = StyleSheet.create({
  webview: { backgroundColor: colors.chromeBg, flex: 1 },
});
