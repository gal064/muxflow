import Svg, { Circle, Path, Rect } from "react-native-svg";

// The marks of a message's actions (mobile-chat-mock.html, S5), stroked on a
// 24-unit grid like the ones in ui/components/MediaIcons.tsx.

interface IconProps {
  size?: number;
  color: string;
}

/** Every shape here is an outline of the same weight. */
function outline(color: string) {
  return { fill: "none", stroke: color, strokeLinecap: "round", strokeLinejoin: "round", strokeWidth: "1.8" } as const;
}

/** Two sheets, one behind the other: copy. */
export function CopyIcon({ size = 18, color }: IconProps) {
  const line = outline(color);
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Rect height="11" rx="2" width="11" x="9" y="9" {...line} />
      <Path d="M5 15V6a2 2 0 0 1 2-2h9" {...line} />
    </Svg>
  );
}

/** One line that came from two: fork the conversation. */
export function ForkIcon({ size = 18, color }: IconProps) {
  const line = outline(color);
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Circle cx="6" cy="5" r="2.2" {...line} />
      <Circle cx="18" cy="5" r="2.2" {...line} />
      <Circle cx="12" cy="19" r="2.2" {...line} />
      <Path d="M6 7.2v2.3a3 3 0 0 0 3 3h6a3 3 0 0 0 3-3V7.2M12 12.5v4.3" {...line} />
    </Svg>
  );
}

/** A pencil: edit and resend. */
export function EditIcon({ size = 18, color }: IconProps) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M4 20h4L19 9l-4-4L4 16v4zM13.5 6.5l4 4" {...outline(color)} />
    </Svg>
  );
}

/** The corners of a selection around a block: select text. */
export function SelectIcon({ size = 18, color }: IconProps) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M5 7V5h2M17 5h2v2M19 17v2h-2M7 19H5v-2M9 9h6v6H9z" {...outline(color)} />
    </Svg>
  );
}

/** A tick: done, copied. */
export function CheckIcon({ size = 18, color }: IconProps) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M5 12.5l4.5 4.5L19 7.5" {...outline(color)} />
    </Svg>
  );
}
