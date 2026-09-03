import Svg, { Path, Rect } from "react-native-svg";

/**
 * UI marks drawn as SVG so they take token colours and render the same on
 * every device (an emoji or a font glyph does neither). `size` is the box in
 * dp; every path is designed on a 24-unit grid.
 */
/** The conventional outlined Shift mark, centred independently of font metrics. */
export function ShiftIcon({ size = 18, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M12 3 4 11h4v9h8v-9h4z" fill="none" stroke={color} strokeLinejoin="round" strokeWidth="2" />
    </Svg>
  );
}

export function MicIcon({ size = 24, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Rect fill={color} height="12" rx="3.5" width="7" x="8.5" y="2.5" />
      <Path d="M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v3.5M8.5 21.5h7" fill="none" stroke={color} strokeLinecap="round" strokeWidth="2" />
    </Svg>
  );
}

/** The app bar's back arrow: a stroke on the same grid as the transport marks, so it centres on the title's line. */
export function BackIcon({ size = 24, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M19 12H5.5M11.5 6l-6 6 6 6" fill="none" stroke={color} strokeLinecap="round" strokeLinejoin="round" strokeWidth="2.2" />
    </Svg>
  );
}

export function PlayIcon({ size = 24, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      {/* Nudged one unit right: a centred triangle reads left of centre in a disc. */}
      <Path d="M9 5.5v13l10.5-6.5z" fill={color} />
    </Svg>
  );
}

export function PauseIcon({ size = 24, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Rect fill={color} height="14" rx="1" width="4" x="6.5" y="5" />
      <Rect fill={color} height="14" rx="1" width="4" x="13.5" y="5" />
    </Svg>
  );
}

export function StopIcon({ size = 24, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Rect fill={color} height="12" rx="1.5" width="12" x="6" y="6" />
    </Svg>
  );
}

export function FolderIcon({ size = 24, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h4.6l2 2H19.5A1.5 1.5 0 0 1 21 8.5v9a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z" fill="none" stroke={color} strokeLinejoin="round" strokeWidth="2" />
    </Svg>
  );
}
