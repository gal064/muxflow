// Installed once by app/_layout.tsx: the transport factory for this build and
// the toast sink. The SSH module (§6) is the transport for every host.

import { ToastAndroid } from "react-native";
import { sshTransportFactory } from "../ssh/registerTransport";
import { onToast, setTransportFactory } from "./connectionManager";

let wired = false;

export function wireApp(): void {
  if (wired) return;
  wired = true;
  onToast((message) => ToastAndroid.show(message, ToastAndroid.SHORT));
  setTransportFactory(sshTransportFactory);
}
