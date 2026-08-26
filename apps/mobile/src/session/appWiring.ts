// Installed once by app/_layout.tsx: the transport factory for this build and
// the toast sink. The SSH module (§6) replaces the dev factory when it lands.

import { ToastAndroid } from "react-native";
import { onToast, setTransportFactory } from "./connectionManager";

let wired = false;

export function wireApp(): void {
  if (wired) return;
  wired = true;
  onToast((message) => ToastAndroid.show(message, ToastAndroid.SHORT));
  if (__DEV__) {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const { devTransportFactory } = require("./devTransport") as typeof import("./devTransport");
    setTransportFactory(devTransportFactory);
  }
}
