/** Links the mobile UI is allowed to hand to an external device app. */
export function externalLinkTarget(href: string): string | undefined {
  const target = href.trim();
  return /^(?:https?|mailto):/iu.test(target) && !/[\u0000-\u0020\u007f]/u.test(target) ? target : undefined;
}

/** The bundled Markdown document itself stays in-place; all real URLs leave the WebView. */
export function markdownNavigationStaysInApp(url: string): boolean {
  return url === "about:blank" || url.startsWith("about:");
}
