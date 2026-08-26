import { Redirect } from "expo-router";

/**
 * `/home` itself has no screen — design.md §9.3 makes it a two-tab container
 * whose initial tab is Agents. §9.1 and §9.3 both navigate to the bare `/home`,
 * so this redirect is what answers that path. It is hidden from the tab bar.
 */
export default function HomeIndexScreen() {
  return <Redirect href="/home/agents" />;
}
