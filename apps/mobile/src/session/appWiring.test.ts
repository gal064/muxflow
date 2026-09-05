import { describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  startNotifications: vi.fn(),
  onToast: vi.fn(),
  setForegroundService: vi.fn(),
  setTransportFactory: vi.fn(),
  muxflowSsh: vi.fn(() => ({ native: true })),
  sshTransportFactory: vi.fn(),
  toastShow: vi.fn(),
}));

vi.mock("react-native", () => ({
  ToastAndroid: { SHORT: 0, show: mocks.toastShow },
}));
vi.mock("../features/notifications", () => ({ startNotifications: mocks.startNotifications }));
vi.mock("../ssh/MuxflowSsh", () => ({ muxflowSsh: mocks.muxflowSsh }));
vi.mock("../ssh/registerTransport", () => ({ sshTransportFactory: mocks.sshTransportFactory }));
vi.mock("./connectionManager", () => ({
  onToast: mocks.onToast,
  setForegroundService: mocks.setForegroundService,
  setTransportFactory: mocks.setTransportFactory,
}));

const { wireApp } = await import("./appWiring");

describe("wireApp", () => {
  it("starts agent notifications with the rest of the app wiring, once", () => {
    wireApp();
    wireApp();

    expect(mocks.startNotifications).toHaveBeenCalledTimes(1);
    expect(mocks.setTransportFactory).toHaveBeenCalledWith(mocks.sshTransportFactory);
    expect(mocks.muxflowSsh).toHaveBeenCalledTimes(1);
    expect(mocks.setForegroundService).toHaveBeenCalledWith({ native: true });
  });
});
