import { beforeEach, describe, expect, it } from "vitest";

import { chromeRegistry, modalOwner, type Token } from "./primaryChrome";

const owner = (): Token | null => modalOwner(chromeRegistry.getState());

describe("which mount renders the §9.8 sheet and the §9.10 dialog", () => {
  beforeEach(() => {
    chromeRegistry.setState({ roots: [], screens: [] });
  });

  it("has no owner before anything has mounted", () => {
    expect(owner()).toBeNull();
  });

  it("gives the modals to a lone screen mount", () => {
    const screen = {};
    chromeRegistry.getState().claim(screen, "screen");
    expect(owner()).toBe(screen);
  });

  it("prefers the root layout, which react-native-screens never detaches", () => {
    const screen = {};
    const root = {};
    chromeRegistry.getState().claim(screen, "screen");
    chromeRegistry.getState().claim(root, "root");
    expect(owner()).toBe(root);
  });

  it("names exactly one owner however many screens are alive", () => {
    const screens = [{}, {}, {}];
    for (const screen of screens) chromeRegistry.getState().claim(screen, "screen");
    expect(screens.filter((screen) => owner() === screen)).toHaveLength(1);
    expect(owner()).toBe(screens[0]);
  });

  it("ignores a repeated claim from the same mount", () => {
    const root = {};
    chromeRegistry.getState().claim(root, "root");
    chromeRegistry.getState().claim(root, "root");
    expect(chromeRegistry.getState().roots).toEqual([root]);
  });

  it("hands the modals back to a screen when the root unmounts", () => {
    const screen = {};
    const root = {};
    chromeRegistry.getState().claim(screen, "screen");
    chromeRegistry.getState().claim(root, "root");
    chromeRegistry.getState().release(root);
    expect(owner()).toBe(screen);
  });

  it("leaves no owner once every mount is gone", () => {
    const screen = {};
    chromeRegistry.getState().claim(screen, "screen");
    chromeRegistry.getState().release(screen);
    expect(owner()).toBeNull();
  });
});
