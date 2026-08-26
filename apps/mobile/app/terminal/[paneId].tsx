import { PlaceholderScreen } from "../../src/ui/components/PlaceholderScreen";

/** Terminal — design.md §9.5. The one screen without the standard app bar. */
export default function TerminalScreen() {
  return <PlaceholderScreen title="Terminal" route="/terminal/[paneId]" />;
}
