/**
 * The native supervisor appends why *this side* tore a bridge down to the
 * error the dying bridge reports — `"… (torn down locally: <reason>)"`, see
 * `name_local_teardown` in connection/bridge.rs. That suffix is for the
 * journal; the words a person reads stay the plain failure.
 */
export const LOCAL_TEARDOWN_MARKER = " (torn down locally:";

export function userFacingBridgeFailure(detail: string): string {
  const marker = detail.indexOf(LOCAL_TEARDOWN_MARKER);
  return marker === -1 ? detail : detail.slice(0, marker);
}
