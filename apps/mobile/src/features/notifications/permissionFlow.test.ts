import { describe, expect, it, vi } from "vitest";

import type { NotificationPermission } from "./host";
import { createPermissionFlow } from "./permissionFlow";
import { createNotificationsUiStore, showNotificationsOffBanner } from "./permissionStore";

function flow(answers: { get: NotificationPermission[]; request?: NotificationPermission[] }) {
  const get = vi.fn(async () => answers.get.shift() ?? "granted");
  const request = vi.fn(async () => answers.request?.shift() ?? "granted");
  const store = createNotificationsUiStore();
  return {
    get,
    request,
    store,
    banner: () => showNotificationsOffBanner(store.getState()),
    flow: createPermissionFlow({
      host: { getPermission: get, requestPermission: request },
      setPermission: (permission) => store.getState().setPermission(permission),
    }),
  };
}

describe("§13 permission flow", () => {
  it("asks on the first `connected` and not again", async () => {
    const h = flow({ get: ["undetermined"], request: ["granted"] });
    await h.flow.onConnected();
    await h.flow.onConnected();
    expect(h.request).toHaveBeenCalledTimes(1);
    expect(h.store.getState().permission).toBe("granted");
    expect(h.banner()).toBe(false);
  });

  it("does not prompt when the answer is already known", async () => {
    const h = flow({ get: ["granted"] });
    await h.flow.onConnected();
    expect(h.request).not.toHaveBeenCalled();
    expect(h.store.getState().permission).toBe("granted");
  });

  it("raises the banner when the prompt is refused, and lets it be dismissed", async () => {
    const h = flow({ get: ["undetermined"], request: ["denied"] });
    await h.flow.onConnected();
    expect(h.banner()).toBe(true);
    h.store.getState().dismissBanner();
    expect(h.banner()).toBe(false);
  });

  it("clears the banner once the permission is granted from system settings", async () => {
    const h = flow({ get: ["undetermined", "granted"], request: ["denied"] });
    await h.flow.onConnected();
    expect(h.banner()).toBe(true);
    await h.flow.onAppActive();
    expect(h.store.getState().permission).toBe("granted");
    expect(h.banner()).toBe(false);
  });

  it("keeps the banner up when a resume cannot prove the permission works", async () => {
    // Android reports a once-refused permission as `undetermined` (it would
    // still show a second prompt), so a resume must not read that as "fine".
    const h = flow({ get: ["undetermined", "undetermined"], request: ["denied"] });
    await h.flow.onConnected();
    await h.flow.onAppActive();
    expect(h.banner()).toBe(true);
  });

  it("says nothing before the first connection", async () => {
    const h = flow({ get: ["granted"] });
    await h.flow.onAppActive();
    expect(h.get).not.toHaveBeenCalled();
    expect(h.store.getState().permission).toBe("unknown");
    expect(h.banner()).toBe(false);
  });
});
