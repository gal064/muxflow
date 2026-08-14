import type { Platform } from "./registry";

/**
 * Renders a stored shortcut in the notation the platform's own menus use.
 *
 * Shortcuts are *stored* in a portable, parseable form (`Meta+Shift+P`) because
 * that is what the shortcut editor round-trips and what `commandForKeyboardEvent`
 * matches against. Showing the user that form was finding M10-E052: macOS has
 * never written a shortcut as `Meta+Shift+P` anywhere, so the palette and menus
 * read as debug output.
 *
 * macOS gets glyphs with no separators, in the fixed order the approved mock
 * uses throughout: ⌃⌥⌘⇧ (`⌘⇧P`, `⌥⌘B`, `⌃⌘⇧=`). That is Command-before-Shift,
 * which differs from Apple's own menu order (⌃⌥⇧⌘ → `⇧⌘P`); the mock is the
 * acceptance reference and the phase's gate is written against `⌘⇧P`, so the
 * mock's order wins and is applied consistently rather than per-shortcut.
 * Linux gets words joined by `+`, which is the GTK/Qt convention — glyphs
 * there would be the same mistake in the other direction.
 */

const MAC_MODIFIERS: Record<string, string> = {
  Ctrl: "⌃",
  Alt: "⌥",
  Shift: "⇧",
  Meta: "⌘",
};

const LINUX_MODIFIERS: Record<string, string> = {
  Ctrl: "Ctrl",
  Alt: "Alt",
  Shift: "Shift",
  Meta: "Super",
};

/** The mock's order (⌃⌥⌘⇧), which is not the order shortcuts are stored in. */
const MAC_MODIFIER_ORDER = ["Ctrl", "Alt", "Meta", "Shift"];
/** Linux menus write the modifier words in this order. */
const LINUX_MODIFIER_ORDER = ["Ctrl", "Alt", "Meta", "Shift"];

const KEY_GLYPHS: Record<string, string> = {
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
  Enter: "↩",
  Return: "↩",
  Escape: "⎋",
  Backspace: "⌫",
  Delete: "⌦",
  Tab: "⇥",
  " ": "Space",
  Space: "Space",
  PageUp: "⇞",
  PageDown: "⇟",
  Home: "↖",
  End: "↘",
};

const LINUX_KEY_NAMES: Record<string, string> = {
  ArrowLeft: "Left",
  ArrowRight: "Right",
  ArrowUp: "Up",
  ArrowDown: "Down",
  " ": "Space",
};

/**
 * `undefined` in, `undefined` out: an unbound command renders nothing at all,
 * rather than an empty lozenge (the mock calls those out by name).
 */
export function shortcutGlyphs(shortcut: string | undefined, platform: Platform): string | undefined {
  if (!shortcut) return undefined;
  const parts = shortcut.split("+").filter(Boolean);
  const order = platform === "mac" ? MAC_MODIFIER_ORDER : LINUX_MODIFIER_ORDER;
  const table = platform === "mac" ? MAC_MODIFIERS : LINUX_MODIFIERS;
  const modifiers = order.filter((modifier) => parts.includes(modifier)).map((modifier) => table[modifier]);
  const rawKey = parts.find((part) => !order.includes(part));
  const key = rawKey === undefined
    ? ""
    : platform === "mac"
      ? KEY_GLYPHS[rawKey] ?? (rawKey.length === 1 ? rawKey.toUpperCase() : rawKey)
      : LINUX_KEY_NAMES[rawKey] ?? (rawKey.length === 1 ? rawKey.toUpperCase() : rawKey);
  const pieces = [...modifiers, key].filter(Boolean);
  if (pieces.length === 0) return undefined;
  return platform === "mac" ? pieces.join("") : pieces.join("+");
}

/**
 * The same shortcut, spoken. A screen reader reading `⌘⇧P` announces nothing
 * useful, so every chip carries this on its `aria-label` while the glyphs stay
 * visual-only.
 */
export function shortcutSpoken(shortcut: string | undefined, platform: Platform): string | undefined {
  if (!shortcut) return undefined;
  const spokenModifiers: Record<string, string> = platform === "mac"
    ? { Ctrl: "Control", Alt: "Option", Shift: "Shift", Meta: "Command" }
    : { Ctrl: "Control", Alt: "Alt", Shift: "Shift", Meta: "Super" };
  const parts = shortcut.split("+").filter(Boolean);
  const modifiers = MAC_MODIFIER_ORDER.filter((modifier) => parts.includes(modifier)).map((modifier) => spokenModifiers[modifier]);
  const rawKey = parts.find((part) => !MAC_MODIFIER_ORDER.includes(part));
  const key = rawKey === undefined ? "" : rawKey.length === 1 ? rawKey.toUpperCase() : rawKey.replace(/([a-z])([A-Z])/gu, "$1 $2");
  const pieces = [...modifiers, key].filter(Boolean);
  return pieces.length ? pieces.join(" ") : undefined;
}
