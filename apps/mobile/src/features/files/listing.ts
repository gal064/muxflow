// Directory listing with paging (design doc §9.6 step 2, §7.5 `listDirectory`).
//
// `LIST_DIRECTORY` is a control-lane operation: it stays on the control
// connection, unlike file bodies (§11.1). The host answers one page at a time
// and sets `complete: false` with a `nextPageToken` when more follow
// (`snapshot()` in apps/host/src/service/filesystem/listing.rs), so a listing
// is "fetch until complete, appending".

import type { FileMetadata, Response } from "../../protocol/gen/envelope_pb";
import { listDirectory, newOperationId, type RootedPath } from "../../protocol/requests";
import { visibleEntries, type DirectoryEntry } from "./entries";

/**
 * A hard stop on how many pages one listing will ask for.
 *
 * The host clamps a page to 10 000 entries and refuses to enumerate more than
 * that in one scan, so 40 pages is 20 000 entries at the phone's page size of
 * 500 — far past anything a phone can scroll, and a bound that turns a host
 * that never sets `complete` into an error instead of an infinite loop.
 */
export const MAX_LISTING_PAGES = 40;

export interface DirectoryListing {
  /** The directory the entries belong to, as the host spelled it. */
  path: string;
  entries: DirectoryEntry[];
  /** Pages fetched, for the log line and the tests. */
  pages: number;
  /** True when the host stopped before the whole directory was read. */
  truncated: boolean;
}

/** The one call this module needs from `HostConnection`; injected so it is testable. */
export type RequestFn = (request: ReturnType<typeof listDirectory>) => Promise<Response>;

export class DirectoryListingError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DirectoryListingError";
  }
}

/**
 * Fetches one directory, following `nextPageToken` until `complete`.
 *
 * Hidden names are dropped and the rows are ordered only once, after the last
 * page, so a name on page two still sorts ahead of one on page one.
 */
export async function fetchDirectory(
  request: RequestFn,
  target: RootedPath,
  expectedServerIdentity: string,
): Promise<DirectoryListing> {
  const collected: FileMetadata[] = [];
  let pageToken = "";
  let pages = 0;
  let truncated = false;
  for (;;) {
    const response = await request(listDirectory(newOperationId(), target, expectedServerIdentity, pageToken));
    const directory = response.file?.directory;
    if (!directory) throw new DirectoryListingError("the host answered without a directory listing");
    pages += 1;
    collected.push(...directory.entries);
    if (directory.complete || !directory.nextPageToken) break;
    if (pages >= MAX_LISTING_PAGES) {
      truncated = true;
      break;
    }
    pageToken = directory.nextPageToken;
  }
  return { path: target.path, entries: visibleEntries(collected), pages, truncated };
}
