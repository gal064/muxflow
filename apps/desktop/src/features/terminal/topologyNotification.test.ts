import { describe, expect, it } from "vitest";
import { decodeTopologyNotification } from "./topologyNotification";
import { decodeTerminalEvent } from "./api";

describe("ordered layout notifications", () => {
  it("decodes pane grids through the native topology frame", () => {
    const label = new TextEncoder().encode('layout-change [["%7",39,24],["%12",80,24]]');
    const frame = new Uint8Array(11 + label.length);
    frame[0] = 3;
    new DataView(frame.buffer).setUint16(1, label.length);
    frame.set(label, 3);
    new DataView(frame.buffer).setBigUint64(3 + label.length, 42n);
    expect(decodeTerminalEvent(frame.buffer)).toEqual({
      kind: "topologyDirty", name: "layout-change", sequence: 42,
      grids: [{ paneId: "%7", columns: 39, rows: 24 }, { paneId: "%12", columns: 80, rows: 24 }],
    });
  });

  it.each([
    'layout-change', 'layout-change {}', 'layout-change [["%1",0,24]]',
    'layout-change [["%1",80,1.5]]', 'layout-change [["%1",80,24],["%1",40,12]]',
  ])("rejects invalid grid metadata: %s", (label) => {
    expect(() => decodeTopologyNotification(label)).toThrow();
  });

  it("keeps other topology notifications unchanged", () => {
    expect(decodeTopologyNotification("window-close")).toEqual({ name: "window-close" });
  });
});
