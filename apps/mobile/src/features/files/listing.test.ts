import { describe, expect, it, vi } from "vitest";
import { create } from "@bufbuild/protobuf";
import { ResponseSchema, type Request, type Response } from "../../protocol/gen/envelope_pb";
import { DIRECTORY_PAGE_SIZE } from "../../protocol/requests";
import { DirectoryListingError, fetchDirectory, MAX_LISTING_PAGES } from "./listing";
import { directory, listingResponse, metadata } from "./testing";

const TARGET = { root: "/w", rootToken: "token", path: "/w" };

describe("paging (§9.6 step 2)", () => {
  it("follows nextPageToken until complete and sorts the assembled listing once", async () => {
    const seen: Request[] = [];
    const request = vi.fn(async (value: Request): Promise<Response> => {
      seen.push(value);
      if (seen.length === 1) return listingResponse([metadata("zeta.txt"), directory("src")], { nextPageToken: "page-2" });
      if (seen.length === 2) return listingResponse([metadata("alpha.txt"), directory("assets")], { nextPageToken: "page-3" });
      return listingResponse([metadata("README.md")]);
    });

    const listing = await fetchDirectory(request, TARGET, "identity");

    expect(seen).toHaveLength(3);
    expect(seen.map((value) => value.file?.pageToken)).toEqual(["", "page-2", "page-3"]);
    expect(seen.every((value) => value.file?.pageSize === DIRECTORY_PAGE_SIZE)).toBe(true);
    expect(seen.every((value) => value.file?.rootToken === "token")).toBe(true);
    // A name from the last page still sorts ahead of one from the first.
    expect(listing.entries.map((entry) => entry.name)).toEqual([
      "assets",
      "src",
      "README.md",
      "alpha.txt",
      "zeta.txt",
    ]);
    expect(listing.pages).toBe(3);
    expect(listing.truncated).toBe(false);
  });

  it("gives every page its own operation id", async () => {
    const ids: string[] = [];
    const request = vi.fn(async (value: Request): Promise<Response> => {
      ids.push(value.file!.operationId);
      return ids.length === 1 ? listingResponse([], { nextPageToken: "next" }) : listingResponse([]);
    });
    await fetchDirectory(request, TARGET, "identity");
    expect(new Set(ids).size).toBe(2);
  });

  it("stops on `complete` even when the host also sent a token", async () => {
    const request = vi.fn(async (): Promise<Response> => {
      const response = listingResponse([metadata("a.txt")], { nextPageToken: "next" });
      response.file!.directory!.complete = true;
      return response;
    });
    const listing = await fetchDirectory(request, TARGET, "identity");
    expect(listing.pages).toBe(1);
    expect(request).toHaveBeenCalledTimes(1);
  });

  it("stops after the page cap rather than looping on a host that never completes", async () => {
    const request = vi.fn(async (): Promise<Response> => listingResponse([metadata("a.txt")], { nextPageToken: "next" }));
    const listing = await fetchDirectory(request, TARGET, "identity");
    expect(request).toHaveBeenCalledTimes(MAX_LISTING_PAGES);
    expect(listing.truncated).toBe(true);
  });

  it("refuses a response that carries no listing", async () => {
    const request = vi.fn(async (): Promise<Response> => create(ResponseSchema, { ok: true }));
    await expect(fetchDirectory(request, TARGET, "identity")).rejects.toBeInstanceOf(DirectoryListingError);
  });

  it("hides the hidden names across the whole assembled listing", async () => {
    const request = vi.fn(async (): Promise<Response> => {
      const first = listingResponse([directory(".git"), metadata("a.txt")], { nextPageToken: "next" });
      return request.mock.calls.length === 1 ? first : listingResponse([directory("node_modules"), metadata("b.txt")]);
    });
    const listing = await fetchDirectory(request, TARGET, "identity");
    expect(listing.entries.map((entry) => entry.name)).toEqual(["a.txt", "b.txt"]);
  });
});
