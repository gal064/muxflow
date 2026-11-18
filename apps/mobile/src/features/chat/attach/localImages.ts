// Where the images this phone attached are on the phone, by their path on the
// host (docs/design/mobile-chat-plan.md §3 "Attachments"): a message's chip
// draws its thumbnail from here, with no bytes fetched. Kept while the app
// runs, and no longer: after that the chip is a named one, as for an image
// attached anywhere else.

/** More than a chat shows at once; the oldest goes first. */
const MOST = 200;

const byHostPath = new Map<string, { localUri: string; sizeBytes: number }>();

export function rememberLocalImage(hostPath: string, localUri: string, sizeBytes: number): void {
  if (hostPath === "" || localUri === "") return;
  byHostPath.delete(hostPath);
  byHostPath.set(hostPath, { localUri, sizeBytes });
  if (byHostPath.size > MOST) byHostPath.delete(byHostPath.keys().next().value as string);
}

/**
 * The phone's own file for the image at `hostPath`. `sizeBytes` is the size
 * the host gives for it, 0 when it gave none: a file of another size is
 * another file under a name the host has used again, and has no thumbnail
 * here.
 */
export function localImageFor(hostPath: string, sizeBytes: number): string | undefined {
  const held = hostPath === "" ? undefined : byHostPath.get(hostPath);
  if (!held) return undefined;
  return sizeBytes > 0 && held.sizeBytes > 0 && sizeBytes !== held.sizeBytes ? undefined : held.localUri;
}

/** The file is gone from the phone (the cache was cleared): its chip goes back to a named one. */
export function forgetLocalImage(hostPath: string): void {
  byHostPath.delete(hostPath);
}

/** Test-only. */
export function clearLocalImages(): void {
  byHostPath.clear();
}
