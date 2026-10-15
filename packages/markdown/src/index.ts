import DOMPurify from "dompurify";
import { marked } from "marked";

const SAFE_LINK_URL = /^(?:https?:|mailto:|#)/i;
const SAFE_EMBEDDED_IMAGE = /^data:image\/(?:png|gif|jpe?g|webp);base64,[a-z\d+/]+={0,2}$/i;

/** Embedded raster images only; the whole payload must stay base64. */
export function isEmbeddedMarkdownImage(src: string): boolean {
  return SAFE_EMBEDDED_IMAGE.test(src);
}

/** File paths are inert links; schemes and network-path references are not. */
export function isMarkdownFileLink(href: string): boolean {
  return href.length > 0 && href.trim() === href
    && !/^(?:[a-z][a-z\d+.-]*:|\/\/|[?#])/i.test(href)
    && !/[\u0000-\u001f\u007f\\]/.test(href);
}

export function renderSafeMarkdown(source: string): string {
  const parsed = marked.parse(source, { async: false, gfm: true, breaks: false }) as string;
  const clean = DOMPurify.sanitize(parsed, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: ["style", "iframe", "object", "embed", "form", "input", "button", "svg", "math"],
    FORBID_ATTR: ["style", "srcdoc"],
  });
  const document = new DOMParser().parseFromString(clean, "text/html");
  for (const element of document.querySelectorAll<HTMLElement>("[href], [src]")) {
    for (const attribute of ["href", "src"] as const) {
      const value = element.getAttribute(attribute);
      if (value && !(attribute === "href"
        ? SAFE_LINK_URL.test(value) || isMarkdownFileLink(value)
        : isEmbeddedMarkdownImage(value))) element.removeAttribute(attribute);
    }
    if (element.tagName === "A" && element.hasAttribute("href")) {
      element.setAttribute("rel", "noopener noreferrer");
    }
  }
  // WebKitGTK's AT-SPI effectiveRoleName aborts on implicitly inferred table
  // headers. Explicit roles preserve the same semantics and avoid that path.
  for (const table of document.querySelectorAll("table")) {
    let occupiedUntil: number[] = [];
    let group: HTMLElement | null = null;
    for (const row of table.rows) {
      // A rowspan cannot extend into another row group; rowspan=0 means the
      // rest of this group. cellIndex alone ignores cells spanning from above.
      if (row.parentElement !== group) { occupiedUntil = []; group = row.parentElement; }
      let column = 0;
      for (const cell of row.cells) {
        while (occupiedUntil[column] > row.rowIndex) column += 1;
        const firstColumn = column === 0;
        const end = column + cell.colSpan;
        for (; column < end; column += 1) occupiedUntil[column] = cell.rowSpan ? row.rowIndex + cell.rowSpan : Infinity;
        if (cell.tagName !== "TH" || cell.hasAttribute("role")) continue;
        const scope = cell.getAttribute("scope")?.toLowerCase();
        const rowScope = scope === "row" || scope === "rowgroup";
        const columnScope = scope === "col" || scope === "colgroup";
        const automatic = !rowScope && !columnScope;
        // Match WebKit's implicit orientation: thead/first row are columns;
        // first-column body/footer headers are rows. Other th cells stay cells.
        const isColumn = columnScope || (automatic
          && (group?.tagName === "THEAD" || (group?.tagName !== "TFOOT" && row.rowIndex === 0)));
        const isRow = rowScope || (automatic && firstColumn);
        cell.setAttribute("role", isColumn ? "columnheader" : isRow ? "rowheader" : "cell");
      }
    }
  }
  return document.body.innerHTML;
}
