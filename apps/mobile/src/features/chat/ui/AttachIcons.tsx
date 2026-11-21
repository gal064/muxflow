import Svg, { Circle, Path, Rect } from "react-native-svg";

// The marks of attaching a file (mobile-chat-mock.html, S7), stroked on a
// 24-unit grid like the chat's others in ChatIcons.tsx.

type IconProps = { size?: number; color: string };

const stroke = (color: string) => ({ fill: "none", stroke: color, strokeLinecap: "round", strokeLinejoin: "round", strokeWidth: "1.8" }) as const;

/** A plus: attach something. */
export function PlusIcon({ size = 18, color }: IconProps) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M12 5v14M5 12h14" {...stroke(color)} />
    </Svg>
  );
}

/** A framed picture: the photo library, or an image. */
export function ImageIcon({ size = 18, color }: IconProps) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Rect height="16" rx="2.5" width="18" x="3" y="4" {...stroke(color)} />
      <Circle cx="9" cy="10" r="1.6" {...stroke(color)} />
      <Path d="M4 18l5-5 4 4 3-3 4 4" {...stroke(color)} />
    </Svg>
  );
}

/** A camera: take a photo. */
export function CameraIcon({ size = 18, color }: IconProps) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M4 8h3l2-3h6l2 3h3v11H4z" {...stroke(color)} />
      <Circle cx="12" cy="13" r="3.2" {...stroke(color)} />
    </Svg>
  );
}

/** A sheet with a folded corner: a file. */
export function FileIcon({ size = 18, color }: IconProps) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M7 3h7l5 5v13H7zM14 3v5h5" {...stroke(color)} />
    </Svg>
  );
}

/** A cross: take this away, or close. */
export function CloseIcon({ size = 14, color }: IconProps) {
  return (
    <Svg height={size} viewBox="0 0 24 24" width={size}>
      <Path d="M6 6l12 12M18 6L6 18" {...stroke(color)} />
    </Svg>
  );
}
