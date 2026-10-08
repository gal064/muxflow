// Android-only service and wake APIs. iOS never loads this native surface.
import { fanOut, type NativeSubscription } from "../ssh/MuxflowSsh";

export const NATIVE_WAKE_EVENT_NAME = "onWake";
export const NATIVE_DISCONNECT_EVENT_NAME = "onDisconnectRequested";

export interface AndroidConnectionServices {
  startForegroundService(title: string, body: string): Promise<void>;
  stopForegroundService(): Promise<void>;
  setServiceNotification(title: string, body: string): Promise<void>;
  addDisconnectListener(listener: () => void): () => void;
  scheduleWake(token: string, delayMs: number): Promise<void>;
  cancelWake(token: string): Promise<void>;
  addWakeListener(listener: (token: string) => void): () => void;
}

export interface NativeAndroidConnectionServices {
  startForegroundService(title: string, body: string): Promise<void>;
  stopForegroundService(): Promise<void>;
  setServiceNotification(title: string, body: string): Promise<void>;
  scheduleWake(token: string, delayMs: number): Promise<void>;
  cancelWake(token: string): Promise<void>;
  addListener(eventName: string, listener: (payload: unknown) => void): NativeSubscription;
}

export function createAndroidConnectionServices(native: NativeAndroidConnectionServices): AndroidConnectionServices {
  return {
    startForegroundService: (title, body) => native.startForegroundService(title, body),
    stopForegroundService: () => native.stopForegroundService(),
    setServiceNotification: (title, body) => native.setServiceNotification(title, body),
    scheduleWake: (token, delayMs) => native.scheduleWake(token, delayMs),
    cancelWake: (token) => native.cancelWake(token),
    addWakeListener: fanOut((deliver) => native.addListener(NATIVE_WAKE_EVENT_NAME, deliver), (payload) => {
      if (typeof payload !== "object" || payload === null) return null;
      const token = (payload as { token?: unknown }).token;
      return typeof token === "string" ? token : null;
    }),
    addDisconnectListener: fanOut<true>(
      (deliver) => native.addListener(NATIVE_DISCONNECT_EVENT_NAME, deliver),
      () => true,
    ),
  };
}

let instance: AndroidConnectionServices | undefined;
export function androidConnectionServices(): AndroidConnectionServices {
  if (!instance) {
    const native = require("../../modules/muxflow-ssh").default as NativeAndroidConnectionServices;
    instance = createAndroidConnectionServices(native);
  }
  return instance;
}
