import Svg, { Circle, G, Path } from "react-native-svg";

import { CLAUDE_ICON, CODEX_ICON, GENERIC_ICON, isClaudeAdapter, isCodexAdapter, PIN_ICON } from "../agentIconPaths";

/**
 * The adapter marks, drawn from the desktop's geometry (`agentIconPaths.ts`,
 * pinned to `AgentIdentity.tsx` by a test). Claude's four strokes and Codex's
 * blossom are drawn to the same optical weight, not the same box: the blossom
 * fills its viewBox edge to edge, so the desktop insets it ~14% about its own
 * centre, and that transform comes along verbatim.
 */
export function AgentIcon({ adapterId, size, color }: { adapterId: string; size: number; color: string }) {
  if (isClaudeAdapter(adapterId)) {
    return (
      <Svg height={size} viewBox={CLAUDE_ICON.viewBox} width={size}>
        <Path d={CLAUDE_ICON.path} fill="none" stroke={color} strokeLinecap="round" strokeWidth={CLAUDE_ICON.strokeWidth} />
      </Svg>
    );
  }
  if (isCodexAdapter(adapterId)) {
    return (
      <Svg height={size} viewBox={CODEX_ICON.viewBox} width={size}>
        <G transform={CODEX_ICON.transform}>
          <Path d={CODEX_ICON.path} fill={color} />
        </G>
      </Svg>
    );
  }
  return (
    <Svg height={size} viewBox={GENERIC_ICON.viewBox} width={size}>
      <Circle {...GENERIC_ICON.face} fill="none" stroke={color} strokeWidth={GENERIC_ICON.strokeWidth} />
      {GENERIC_ICON.eyes.map((eye) => <Circle key={eye.cx} {...eye} fill={color} />)}
      <Path d={GENERIC_ICON.mouth} fill="none" stroke={color} strokeLinecap="round" strokeWidth={GENERIC_ICON.strokeWidth} />
    </Svg>
  );
}

/** The desktop `Icon` set's pin, beside a pinned row's name. */
export function PinIcon({ size, color }: { size: number; color: string }) {
  return (
    <Svg height={size} viewBox={PIN_ICON.viewBox} width={size}>
      {PIN_ICON.paths.map((d) => <Path d={d} fill="none" key={d} stroke={color} strokeLinecap="round" strokeLinejoin="round" strokeWidth={PIN_ICON.strokeWidth} />)}
    </Svg>
  );
}
