import Svg, { Path, Rect } from "react-native-svg";

// The chat's own marks (mobile-chat-mock.html), stroked on a 24-unit grid like
// the ones in ui/components/MediaIcons.tsx.

type IconProps = { size?: number; color: string };

function Stroke({ d, size, color }: { d: string; size: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d={d} fill="none" stroke={color} strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.8" />
    </Svg>
  );
}

/** An arrow pointing down: to the newest message. */
export function ArrowDownIcon({ size = 16, color }: IconProps) {
  return <Stroke color={color} d="M12 5v14M6 13l6 6 6-6" size={size} />;
}

/** A chevron pointing down: fold this away. */
export function ChevronDownIcon({ size = 16, color }: IconProps) {
  return <Stroke color={color} d="M6 9l6 6 6-6" size={size} />;
}

/** A chevron pointing right: there is more under this line. */
export function ChevronRightIcon({ size = 16, color }: IconProps) {
  return <Stroke color={color} d="M9 6l6 6-6 6" size={size} />;
}

/** Stacked sheets: work running in the background. */
export function LayersIcon({ size = 16, color }: IconProps) {
  return <Stroke color={color} d="M12 3l9 5-9 5-9-5 9-5zM3 13l9 5 9-5" size={size} />;
}

/** A prompt: a command. */
export function TerminalIcon({ size = 16, color }: IconProps) {
  return <Stroke color={color} d="M5 7l5 5-5 5M13 17h6" size={size} />;
}

/** A robot's head: a subagent. */
export function BotIcon({ size = 16, color }: IconProps) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Rect fill="none" height="11" rx="3" stroke={color} strokeLinejoin="round" strokeWidth="1.8" width="14" x="5" y="8" />
      <Path d="M12 4v4M9 13v2M15 13v2" fill="none" stroke={color} strokeLinecap="round" strokeWidth="1.8" />
    </Svg>
  );
}
