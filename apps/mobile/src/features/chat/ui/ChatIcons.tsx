import Svg, { Path } from "react-native-svg";

// The chat's own marks (mobile-chat-mock.html), stroked on a 24-unit grid like
// the ones in ui/components/MediaIcons.tsx.

function Stroke({ d, size, color }: { d: string; size: number; color: string }) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d={d} fill="none" stroke={color} strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.8" />
    </Svg>
  );
}

/** An arrow pointing down: to the newest message. */
export function ArrowDownIcon({ size = 16, color }: { size?: number; color: string }) {
  return <Stroke color={color} d="M12 5v14M6 13l6 6 6-6" size={size} />;
}

/** A chevron pointing down: fold this away. */
export function ChevronDownIcon({ size = 16, color }: { size?: number; color: string }) {
  return <Stroke color={color} d="M6 9l6 6 6-6" size={size} />;
}
