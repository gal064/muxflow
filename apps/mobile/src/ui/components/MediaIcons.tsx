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

export function LockIcon({ size = 24, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Rect fill="none" height="11" rx="2" stroke={color} strokeWidth="2" width="14" x="5" y="10" />
      <Path d="M8 10V7a4 4 0 0 1 8 0v3" fill="none" stroke={color} strokeLinecap="round" strokeWidth="2" />
    </Svg>
  );
}

export function TrashIcon({ size = 24, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M4 7h16M9 3h6l1 4H8l1-4M7 7l1 14h8l1-14M10 11v6M14 11v6" fill="none" stroke={color} strokeLinecap="round" strokeLinejoin="round" strokeWidth="2" />
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

export function SettingsIcon({ size = 22, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7Z" fill="none" stroke={color} strokeWidth="2" />
      <Path d="m19.2 13.5 1.3 1-.2 1.4-1.6.6a8 8 0 0 1-1.2 1.6l.2 1.7-1.3.7-1.3-1a8 8 0 0 1-2 .5l-.7 1.5H11L10.3 20a8 8 0 0 1-2-.5l-1.3 1-1.3-.7.2-1.7a8 8 0 0 1-1.2-1.6L3 15.9l-.2-1.4 1.3-1a8 8 0 0 1 0-2l-1.3-1L3 9.1l1.6-.6A8 8 0 0 1 5.9 7l-.2-1.7L7 4.5l1.3 1a8 8 0 0 1 2-.5l.7-1.5h1.5l.7 1.5a8 8 0 0 1 2 .5l1.3-1 1.3.7-.2 1.7a8 8 0 0 1 1.2 1.6l1.6.6.2 1.4-1.3 1a8 8 0 0 1 0 2Z" fill="none" stroke={color} strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
    </Svg>
  );
}
