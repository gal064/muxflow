import { summarizeSurfaceError } from "./errorSummary";

interface Props {
  /** Whatever the surface caught, verbatim. */
  detail: string;
  /**
   * `alert` interrupts, `status` waits its turn. A rejection the user just
   * caused is an alert; a condition they merely walked into is a status.
   * `"none"` is for a container that already carries the live-region role —
   * nesting one alert inside another announces the same text twice.
   */
  role?: "alert" | "status" | "none";
  className?: string;
  /**
   * A sentence the surface already knows to be better than anything derivable
   * from `detail` — a helper rollback saying what state the host was left in,
   * say. The raw detail still goes behind the disclosure.
   */
  summary?: string;
  /**
   * Makes the banner closeable. Only for surfaces where nothing else clears it
   * — a rejection that survives until the *next* attempt, on a surface a user
   * may never use again, is one they cannot get rid of.
   */
  onDismiss?(): void;
}

/**
 * The one shape every rejection takes: a sentence, then the diagnostic behind a
 * disclosure (11.4.4, closing M10-E059).
 *
 * The `<details>` is a real disclosure rather than a truncation, so the exact
 * text a bug report needs is one click away and is still selectable and
 * copyable. It is closed on every render of a *new* error because the element
 * is keyed by the detail text — a fresh rejection must not inherit the previous
 * one's opened state.
 */
export function SurfaceError({ className = "surface-error", detail, onDismiss, role = "alert", summary }: Props) {
  const derived = summarizeSurfaceError(detail);
  const headline = summary ?? derived.summary;
  const disclosed = summary ? (detail.trim() === summary ? undefined : detail.trim()) : derived.detail;
  return <div className={className} role={role === "none" ? undefined : role}>
    <span className="surface-error-summary">{headline}</span>
    {disclosed && <details className="surface-error-details" key={disclosed}>
      <summary>Details</summary>
      <pre>{disclosed}</pre>
    </details>}
    {onDismiss && <button aria-label="Dismiss the error" className="surface-error-dismiss" onClick={onDismiss} type="button">Dismiss</button>}
  </div>;
}
