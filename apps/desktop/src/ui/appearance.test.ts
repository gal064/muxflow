// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import paletteCss from "@muxflow/client-core/palette.css?raw";
import indexHtml from "../../index.html?raw";
import { applyInterfaceScale, applyTheme, currentTheme, interfaceScale, onAppearanceChange, rememberedTheme, rememberTheme, resolvedTheme } from "./appearance";

function block(selector: string): Map<string, string> {
  const start = paletteCss.indexOf(`${selector} {`);
  const body = paletteCss.slice(start, paletteCss.indexOf("}", start));
  return new Map([...body.matchAll(/^\s*(--[\w-]+):\s*([^;]+);/gmu)].map(([, name, value]) => [name, value.trim()]));
}

describe("appearance", () => {
  const root = document.documentElement;
  afterEach(() => { delete root.dataset.theme; });

  it("resolves System from the OS and leaves an explicit choice alone", () => {
    expect(resolvedTheme("system", true)).toBe("dark");
    expect(resolvedTheme("system", false)).toBe("light");
    expect(resolvedTheme("light", true)).toBe("light");
    expect(resolvedTheme("dark", false)).toBe("dark");
  });

  it("sets the root attribute and tells subscribers only when it changes", () => {
    const listener = vi.fn();
    const unsubscribe = onAppearanceChange(listener);
    applyTheme("system", false, root);
    expect(root.dataset.theme).toBe("light");
    expect(currentTheme(root)).toBe("light");
    applyTheme("light", true, root);
    expect(listener).toHaveBeenCalledTimes(1);
    applyTheme("dark", false, root);
    expect(currentTheme(root)).toBe("dark");
    expect(listener).toHaveBeenCalledTimes(2);
    unsubscribe();
    applyTheme("light", false, root);
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it("remembers the last choice and ignores anything it did not write", () => {
    const store = new Map<string, string>();
    const storage = { getItem: (key: string) => store.get(key) ?? null, setItem: (key: string, value: string) => { store.set(key, value); } };
    expect(rememberedTheme(storage)).toBeUndefined();
    rememberTheme("light", storage);
    expect(rememberedTheme(storage)).toBe("light");
    store.set("muxflow.theme", "sepia");
    expect(rememberedTheme(storage)).toBeUndefined();
  });

  it("gives the light theme every color the dark one has, as hex", () => {
    // Monaco throws on a rule color that is not six or eight hex digits, and
    // a token missing here would show the dark value inside the light theme.
    const dark = block(':root, [data-palette="dark"]');
    const light = block(':root[data-theme="light"], [data-palette="light"]');
    expect([...light.keys()].sort()).toEqual([...dark.keys()].sort());
    for (const [name, value] of light) expect(value, name).toMatch(/^#(?:[0-9a-f]{6}|[0-9a-f]{8})$/u);
  });
});

describe("interface scale", () => {
  it("reads back what was applied, and 100% before anything was", () => {
    const root = document.createElement("html");
    expect(interfaceScale(root)).toBe(1);
    const storage = new Map<string, string>();
    applyInterfaceScale(1.2, root, { setItem: (key, value) => storage.set(key, value) });
    expect(interfaceScale(root)).toBe(1.2);
    expect(storage.get("muxflow.uiScale")).toBe("1.2");
  });
});

describe("index.html", () => {
  // Packaging adds a CSP nonce to any inline style element, and a nonce in
  // style-src disables the 'unsafe-inline' that Monaco and CodeMirror need for
  // the styles they generate: Live stops scrolling and Source loses its lines.
  it("has no inline style for packaging to give a nonce", () => {
    expect(indexHtml).not.toMatch(/<style[\s>]/iu);
    expect(indexHtml).toContain('<link rel="stylesheet" href="/theme.css" />');
  });
});
