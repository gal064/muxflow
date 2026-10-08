import { colors, stateColors, terminalTheme } from "@muxflow/client-core";
export { colors, stateColors, terminalTheme };

/**
 * System sans for chrome, JetBrains Mono for the terminal, paths, keys and the
 * plain file view. Android has no font asset named `System`, so that value
 * falls through to the platform default typeface, which is Roboto — the face
 * §10.1 asks for. `mono` is the family name registered by the `expo-font`
 * plugin block in `app.json`; the plugin emits a `ReactFontManager`
 * registration for it, so `fontWeight: "700"` picks the Bold face.
 */
export const fonts = {
  sans: "System",
  mono: "JetBrains Mono",
} as const;

/** Heights in dp, from the per-screen specs in §9. */
export const metrics = {
  appBarHeight: 56,
  connectionStripHeight: 28,
  tabBarHeight: 56,
  terminalHeaderHeight: 48,
  keyChipRowHeight: 40,
  inputBarHeight: 56,
  hostRowHeight: 72,
  agentRowHeight: 76,
  agentAvatarSize: 36,
  sessionRowHeight: 64,
  windowRowHeight: 64,
  actionRowHeight: 48,
  fileRowHeight: 52,
  attentionEdgeBarWidth: 3,
  hairlineWidth: 1,
} as const;

/** Corner radii in dp (§10.1): 8 on cards and pills, 12 on sheets. */
export const radii = {
  card: 8,
  pill: 8,
  sheet: 12,
} as const;

/** Type scale in sp, from the per-screen specs in §9. */
export const typeScale = {
  appBarTitle: 17,
  rowTitle: 16,
  body: 14,
  rowSecondary: 13,
  meta: 12,
  keyMono: 11,
  terminal: 13,
} as const;

/**
 * Text inside fixed-height chrome may follow Android's font scale up to 1.3×.
 * Beyond that, a 28–56 dp control cannot grow with its label, so React Native
 * clips or paints the label into the neighbouring row. Reading surfaces and
 * dialog copy remain uncapped.
 */
export const fixedChromeText = { maxFontSizeMultiplier: 1.3 } as const;

export const tokens = {
  colors,
  terminalTheme,
  stateColors,
  fonts,
  metrics,
  radii,
  typeScale,
  fixedChromeText,
} as const;

export default tokens;
