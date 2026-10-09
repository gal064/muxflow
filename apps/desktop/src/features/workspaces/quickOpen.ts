import { fuzzyMatch } from "../../commands/fuzzy";
import type { SelectableTab } from "../shell/model";
import type { FileSearchMatch } from "../files/types";
import type { MergedWorkspaceRow } from "./mergedWorkspaceRows";
import { workspaceMetaLine } from "./workspaceRows";

interface Item { key: string; title: string; detail: string; current: boolean }
export type QuickOpenItem = Item & (
  | { kind: "workspace"; workspace: MergedWorkspaceRow }
  | { kind: "tab"; tab: SelectableTab }
  | { kind: "file"; file: FileSearchMatch }
);

export function quickOpenScore(title: string, detail: string, query: string): number | undefined {
  const name = fuzzyMatch(title, query);
  return name ? name.score + 1000 : fuzzyMatch(detail, query)?.score;
}

export function quickOpenItems(
  workspaces: readonly MergedWorkspaceRow[], tabs: readonly SelectableTab[], files: readonly FileSearchMatch[],
  query: string, activeTabKey: string | undefined, hostLabel: (id: string) => string,
  selectedKey?: string,
): QuickOpenItem[] {
  const items: QuickOpenItem[] = tabs.map((tab) => ({
    kind: "tab", key: tab.key, tab, title: tab.title, current: tab.key === activeTabKey,
    detail: tab.kind === "terminal" ? "Terminal" : tab.resource,
  }));
  items.push(...workspaces.map((workspace): QuickOpenItem => ({
    kind: "workspace", key: `workspace:${workspace.key}`, workspace,
    title: workspace.session.name, detail: [workspaceMetaLine(workspace), hostLabel(workspace.hostProfileId)].filter(Boolean).join(" · "),
    current: workspace.active,
  })));
  const openPaths = new Set(tabs.flatMap((tab) => tab.kind === "app" && (tab.appKind === "file" || tab.appKind === "markdown") ? [tab.resource] : []));
  if (query) items.push(...files.filter((file) => !openPaths.has(file.path)).map((file): QuickOpenItem => ({
    kind: "file", key: `file:${file.path}`, file, title: file.relativePath.split("/").at(-1)!,
    detail: file.relativePath, current: false,
  })));
  if (!query) return items.slice(0, 75);
  const ranked = items.map((item, index) => ({ item, index, score: item.kind === "file" ? item.file.score : quickOpenScore(item.title, item.detail, query) }))
    .filter((row): row is typeof row & { score: number } => row.score !== undefined)
    .sort((a, b) => b.score - a.score || a.index - b.index)
    .map((row) => row.item);
  const visible = ranked.slice(0, 75);
  const selected = ranked.find((item) => item.key === selectedKey);
  if (selected && !visible.some((item) => item.key === selectedKey)) visible[visible.length - 1] = selected;
  return visible;
}
