// One place to turn a rejected file request into the string §9.6/§9.7 show.

import { HostError, RequestTimeoutError } from "../../protocol/HostConnection";

/** The host's own `displayMessage` where there is one; §12's timeout copy otherwise. */
export function displayMessageFor(error: unknown): string {
  if (error instanceof HostError) return error.message;
  if (error instanceof RequestTimeoutError) return "The host didn't answer in time.";
  if (error instanceof Error) return error.message;
  return String(error);
}
