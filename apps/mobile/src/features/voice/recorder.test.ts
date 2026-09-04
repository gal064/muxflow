import { describe, expect, it } from "vitest";

import { isBluetoothInput, playbackInputAfterBluetooth, preferredExternalInput } from "./inputRouting";

describe("voice recording input routing", () => {
  const builtIn = { name: "Phone microphone", type: "MicrophoneBuiltIn", uid: "phone" };

  it("prefers a Bluetooth speech input over the phone and wired inputs", () => {
    const wired = { name: "Wired headset", type: "MicrophoneWired", uid: "wired" };
    const bluetooth = { name: "Pixel Buds", type: "BluetoothSCO", uid: "buds" };
    expect(preferredExternalInput([builtIn, wired, bluetooth])).toBe(bluetooth);
  });

  it("recognizes the iOS HFP and headset microphone route names", () => {
    const wired = { name: "Headset Microphone", type: "HeadsetMic", uid: "headset" };
    const bluetooth = { name: "AirPods", type: "BluetoothHFP", uid: "airpods" };
    expect(preferredExternalInput([builtIn, wired])).toBe(wired);
    expect(preferredExternalInput([builtIn, bluetooth])).toBe(bluetooth);
  });

  it("leaves the system route alone when only the built-in microphone exists", () => {
    expect(preferredExternalInput([builtIn])).toBeUndefined();
  });

  it("restores a non-SCO route after Bluetooth recording", () => {
    const bluetooth = { name: "Pixel Buds", type: "BluetoothSCO", uid: "buds" };
    expect(isBluetoothInput(bluetooth)).toBe(true);
    expect(playbackInputAfterBluetooth([bluetooth, builtIn])).toBe(builtIn);
  });
});
