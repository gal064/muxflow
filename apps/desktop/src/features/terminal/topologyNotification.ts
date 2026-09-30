export interface PaneGrid {
  paneId: string;
  columns: number;
  rows: number;
}

/** Layout sizes travel in the existing ordered topology-notification frame. */
export function decodeTopologyNotification(label: string): { name: string; grids?: PaneGrid[] } {
  if (!label.startsWith("layout-change")) return { name: label };
  if (!label.startsWith("layout-change ")) throw new Error("layout notification omitted pane grids");
  const payload: unknown = JSON.parse(label.slice("layout-change ".length));
  if (!Array.isArray(payload)) throw new Error("layout notification pane grids are not an array");
  const seen = new Set<string>();
  const grids = payload.map((entry: unknown): PaneGrid => {
    if (!Array.isArray(entry) || entry.length !== 3) throw new Error("invalid pane grid");
    const [paneId, columns, rows] = entry;
    if (typeof paneId !== "string" || !/^%\d+$/.test(paneId) || seen.has(paneId)
      || !Number.isInteger(columns) || columns < 1 || columns > 65535
      || !Number.isInteger(rows) || rows < 1 || rows > 65535) throw new Error("invalid pane grid");
    seen.add(paneId);
    return { paneId, columns, rows };
  });
  return { name: "layout-change", grids };
}
