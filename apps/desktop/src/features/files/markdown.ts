import DOMPurify from "dompurify";
import { marked } from "marked";

const SAFE_LINK_URL = /^(?:https?:|mailto:|#|\/|\.\/|\.\.\/)/i;
const SAFE_EMBEDDED_IMAGE = /^data:image\/(?:png|gif|jpe?g|webp);base64,/i;

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
      if (value && !(attribute === "href" ? SAFE_LINK_URL : SAFE_EMBEDDED_IMAGE).test(value)) element.removeAttribute(attribute);
    }
    if (element.tagName === "A" && element.hasAttribute("href")) {
      element.setAttribute("rel", "noopener noreferrer");
    }
  }
  return document.body.innerHTML;
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
