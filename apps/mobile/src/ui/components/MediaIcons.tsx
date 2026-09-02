import Svg, { Path, Rect } from "react-native-svg";

/**
 * Microphone and transport marks drawn as SVG so they take token colours and
 * render the same on every device (an emoji or a dingbat glyph does neither).
 * `size` is the box in dp; every path is designed on a 24-unit grid.
 */
export function MicIcon({ size = 24, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Rect fill={color} height="12" rx="3.5" width="7" x="8.5" y="2.5" />
      <Path d="M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v3.5M8.5 21.5h7" fill="none" stroke={color} strokeLinecap="round" strokeWidth="2" />
    </Svg>
  );
}

export function PlayIcon({ size = 24, color }: { size?: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M8 5.5v13l10.5-6.5z" fill={color} />
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
