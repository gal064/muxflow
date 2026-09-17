// Markdown rendering (design doc D4, §10.3).
//
// `renderSafeMarkdown` below is copied **verbatim** from
// apps/desktop/src/features/files/markdown.ts, including `SAFE_LINK_URL` and
// `SAFE_EMBEDDED_IMAGE`. A Markdown file that renders on the desktop renders
// the same on the phone because it is the same function, the same `marked`
// options and the same DOMPurify configuration. Do not "improve" it here;
// change it on the desktop and copy it again.
//
// It runs inside the Markdown WebView (webview/markdown/entry.ts), not in the
// React Native runtime: it needs a DOM, and the WebView is the only DOM the app
// has. It is imported directly by the unit test under jsdom.

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
