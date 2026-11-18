import Svg, { Circle, Path, Rect } from "react-native-svg";

// The marks of the chat's header and its Details sheet
// (mobile-chat-mock.html, S8), stroked on a 24-unit grid like the ones in
// ui/components/MediaIcons.tsx.

interface IconProps {
  size?: number;
  color: string;
}

/** Every shape here is an outline of the same weight. */
function outline(color: string) {
  return { fill: "none", stroke: color, strokeLinecap: "round", strokeLinejoin: "round", strokeWidth: "1.8" } as const;
}

/** Headphones: voice mode, replies read aloud. */
export function HeadphonesIcon({ size = 18, color }: IconProps) {
  const line = outline(color);
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M4 14v-2a8 8 0 0 1 16 0v2" {...line} />
      <Rect height="6" rx="1.5" width="4" x="3" y="14" {...line} />
      <Rect height="6" rx="1.5" width="4" x="17" y="14" {...line} />
    </Svg>
  );
}

/** An i in a circle: Details. */
export function InfoIcon({ size = 18, color }: IconProps) {
  const line = outline(color);
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Circle cx="12" cy="12" r="9" {...line} />
      <Path d="M12 11v5M12 7.5v.5" {...line} />
    </Svg>
  );
}

/** A screen on its stand: the host. */
export function HostIcon({ size = 14, color }: IconProps) {
  const line = outline(color);
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Rect height="12" rx="2" width="18" x="3" y="5" {...line} />
      <Path d="M8 21h8M12 17v4" {...line} />
    </Svg>
  );
}

/** A Git branch. */
export function BranchIcon({ size = 14, color }: IconProps) {
  const line = outline(color);
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Circle cx="6" cy="5" r="2.2" {...line} />
      <Circle cx="6" cy="19" r="2.2" {...line} />
      <Circle cx="18" cy="8" r="2.2" {...line} />
      <Path d="M6 7.2v9.6M18 10.2c0 4-6 3-12 6" {...line} />
    </Svg>
  );
}
