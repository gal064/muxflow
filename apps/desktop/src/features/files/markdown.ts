import DOMPurify from "dompurify";
import { isMarkdownFileLink } from "@muxflow/markdown";

export { renderSafeMarkdown } from "@muxflow/markdown";

/** Resolve against the document, leaving canonicalization and access to the host. */
export function markdownFilePath(documentPath: string, href: string): string | undefined {
  if (!isMarkdownFileLink(href)) return undefined;
  const path = decodeURIComponent(href.split(/[?#]/, 1)[0]);
  if (!path || /[\u0000-\u001f\u007f]/.test(path)) throw new Error("Invalid Markdown file path.");
  return path.startsWith("/") ? path : `${documentPath.slice(0, documentPath.lastIndexOf("/") + 1)}${path}`;
}

/** Sanitize SVG before placing preview bytes in an image object URL. */
export function renderSafeSvg(source: string): string {
  const clean = DOMPurify.sanitize(source, {
    USE_PROFILES: { svg: true, svgFilters: false },
    FORBID_TAGS: ["script", "style", "foreignObject", "iframe", "object", "embed"],
    FORBID_ATTR: ["style", "onload", "onclick", "onerror"],
  });
  const document = new DOMParser().parseFromString(clean, "image/svg+xml");
  for (const element of document.querySelectorAll("*")) {
    for (const attribute of ["href", "xlink:href", "src"] as const) {
      const value = element.getAttribute(attribute);
      if (value && !/^(?:#|data:image\/(?:png|gif|jpe?g|webp);base64,)/i.test(value)) element.removeAttribute(attribute);
    }
    for (const attribute of [...element.attributes]) {
      if (/^on/i.test(attribute.name)) element.removeAttribute(attribute.name);
    }
  }
  return new XMLSerializer().serializeToString(document.documentElement);
}
