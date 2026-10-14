import { useDragSelector } from "./pointerDrag";

/** What is being dragged, following the pointer, for as long as a drag lasts; dimmed over a refusal. */
export function DragGhost() {
  const drag = useDragSelector((current) => current);
  if (!drag) return null;
  const refused = drag.target !== undefined && !drag.allowed;
  return <div aria-hidden="true" className={refused ? "drag-ghost refused" : "drag-ghost"} style={{ left: drag.x, top: drag.y }}>
    {drag.payload.label}
  </div>;
}
