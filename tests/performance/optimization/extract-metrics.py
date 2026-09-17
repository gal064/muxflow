import json
import sys
from pathlib import Path


EXPECTED: dict[str, tuple[str, int, set[str]]] = {
    "terminalFrontend": ("terminal-frontend.log", 3, {"chunkBytes", "eventCount", "byteExact", "counters", "highWater"}),
    "explorerWide": ("explorer-wide.log", 1, {"entries", "logicalRowsHighWater", "renderedRowsHighWater", "rowParity"}),
    "explorerListWatch": ("explorer-wide.log", 1, {"directoryListRequests", "watchRequests", "hostRequestAttempts", "hostRequestSuccesses", "hostRequestFailures", "hostRequestCancellations", "mappedPayloadBytes"}),

    "paneResource": ("pane-resource.log", 3, {"paneCount", "retainedBytes", "fullAccountingScans", "lruEntriesVisited", "revealParity"}),
    "paneResourceEviction": ("pane-resource.log", 1, {"evictions", "lruPops", "oldPaneReleased", "newPaneRetained"}),
    "transferAdmission": ("transfer-admission.log", 1, {"activeHighWater", "queuedHighWater", "accepted", "rejected", "completedSuccesses", "completedFailures", "completionParity"}),
    "git32Consumers": ("git-consumers.log", 1, {"consumers", "nativeWatcherCreations", "nativeWatchers", "gitProcesses", "statusProcesses", "diffProcesses", "mutationProcesses"}),
}

# Lanes a later source may emit that the preserved Stage 0 baseline predates.
# Absent, they are not a failure; present, they must carry their full record.
OPTIONAL: dict[str, tuple[str, set[str]]] = {
    "explorerPaginatedWatchTraffic": ("explorer-wide.log", {
        "entries", "hostPageSize", "heldListingComplete",
        "expandDirectoryListRequests", "expandedRows",
        "changeDirectoryListRequests", "externalChangeRows",
        # Identity, not arithmetic. One create and one delete leave the row
        # count unchanged, so both-patched and neither-patched score the same;
        # these name the two rows the events actually described.
        "createdRowPath", "deletedRowPath",
    }),
    "explorerWideWatchTraffic": ("explorer-wide.log", {
        "entries", "rootWatchRequests", "expandWatchRequests", "directoryListRequests",
        "collapseWatchReleases", "expandedRows", "externalChangeRows", "cachedRevisitRows",
        "cachedRevisitPaintedBeforeRevalidation", "externalChangeRowPath",
        # Recorded, never gated: a jsdom commit is not a browser paint. Their
        # presence is the assertion — a budget nothing publishes a sample for
        # cannot be checked anywhere, which is what these two were.
        "jsdomExpandToPaintP95Ms", "jsdomExternalChangeToPaintP95Ms",
    }),
}


def main() -> int:
    argv = sys.argv[1:]
    # Which lanes this run is answerable for.
    #
    # A run that produces only the Explorer logs was previously still measured
    # against every lane the extractor knows about, so it wrote an artifact
    # stamped `"valid": false` listing six absent lanes it was never asked to
    # produce. A reader who opened the artifact rather than the runner's stdout
    # got the opposite verdict from the same run. Named scopes make the
    # artifact's own verdict mean what it says.
    scope: str | None = None
    if argv and argv[0].startswith("--scope="):
        scope = argv.pop(0).split("=", 1)[1]
    if len(argv) < 2:
        return 2
    in_scope = (lambda lane: lane.startswith(scope)) if scope else (lambda _lane: True)
    output = Path(argv[0])
    records: list[dict[str, object]] = []
    malformed: list[str] = []
    for name in argv[1:]:
        path = Path(name)
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            marker = "PHASE14_METRIC "
            if marker not in line:
                continue
            payload = line.split(marker, 1)[1].strip()
            try:
                record = json.loads(payload)
                record["sourceLog"] = path.name
                records.append(record)
            except json.JSONDecodeError:
                malformed.append(f"{path.name}:{payload}")
    validation_errors = list(malformed)
    by_lane: dict[str, list[dict[str, object]]] = {}
    for record in records:
        lane = record.get("lane")
        if not isinstance(lane, str) or (lane not in EXPECTED and lane not in OPTIONAL):
            validation_errors.append(f"unexpected lane: {lane!r}")
            continue
        by_lane.setdefault(lane, []).append(record)
    for lane, (source, fields) in OPTIONAL.items():
        for record in by_lane.get(lane, []):
            if record.get("sourceLog") != source:
                validation_errors.append(f"{lane}: expected source {source}, found {record.get('sourceLog')!r}")
            missing = fields - record.keys()
            if missing:
                validation_errors.append(f"{lane}: missing fields {sorted(missing)}")
    for lane, (source, cardinality, fields) in EXPECTED.items():
        if not in_scope(lane):
            continue
        values = by_lane.get(lane, [])
        if len(values) != cardinality:
            validation_errors.append(f"{lane}: expected {cardinality} record(s), found {len(values)}")
        for record in values:
            if record.get("sourceLog") != source:
                validation_errors.append(f"{lane}: expected source {source}, found {record.get('sourceLog')!r}")
            missing = fields - record.keys()
            if missing:
                validation_errors.append(f"{lane}: missing fields {sorted(missing)}")
    exact_values = {
        "terminalFrontend": {"eventCount": 8, "byteExact": True},
        "explorerWide": {"entries": 4_096, "logicalRowsHighWater": 4_096, "rowParity": True},
        "paneResourceEviction": {"evictions": 1, "lruPops": 1, "oldPaneReleased": True, "newPaneRetained": True},
        "transferAdmission": {"activeHighWater": 2, "rejected": 1, "completionParity": True},
        "git32Consumers": {
            "consumers": 32,
        },
    }
    for lane, expected in exact_values.items():
        for record in by_lane.get(lane, []):
            for field, value in expected.items():
                if record.get(field) != value:
                    validation_errors.append(f"{lane}.{field}: expected {value!r}, found {record.get(field)!r}")
    pane_counts = {record.get("paneCount") for record in by_lane.get("paneResource", [])}
    if in_scope("paneResource") and pane_counts != {32, 256, 1_024}:
        validation_errors.append(f"paneResource.paneCount: expected [32, 256, 1024], found {sorted(pane_counts, key=str)}")
    for record in by_lane.get("paneResource", []):
        if record.get("revealParity") is not True:
            validation_errors.append("paneResource.revealParity: expected true")
    for record in by_lane.get("explorerWide", []):
        rendered = record.get("renderedRowsHighWater")
        logical = record.get("logicalRowsHighWater")
        if not all(isinstance(value, int) and not isinstance(value, bool) for value in (rendered, logical)):
            validation_errors.append("explorerWide: row counts must be integers")
        elif not 0 < rendered <= logical:
            validation_errors.append("explorerWide: rendered row cost must be positive and no larger than logical rows")
        # A later source windows the tree, and windowing means the mounted row
        # cost is a function of the viewport rather than of the directory.
        # `rendered <= logical` holds with no windowing at all, so the same tree
        # is measured four times larger and must mount exactly as much. Absent
        # from the preserved Stage 0 baseline, which predates windowing.
        quadrupled = record.get("mountedRowsAtFourTimesTheEntries")
        if quadrupled is not None and quadrupled != rendered:
            validation_errors.append(
                "explorerWide: the mounted band grew with the directory rather than the viewport")
    # An after-only lane: the Stage 0 baseline predates it, so its absence is
    # not a validation failure. Present, it must describe a windowed tree with
    # one watch per directory and no directory list at all.
    for record in by_lane.get("explorerWideWatchTraffic", []):
        # One watch per visible directory, no directory list at all, and one
        # release per collapse. Anything else is a list or watch storm.
        if record.get("rootWatchRequests") != 1 or record.get("expandWatchRequests") != 1:
            validation_errors.append("explorerWideWatchTraffic: expansion must cost exactly one watch bootstrap")
        if record.get("directoryListRequests") != 0:
            validation_errors.append("explorerWideWatchTraffic: the watch bootstrap must be the listing")
        if record.get("collapseWatchReleases") != 1:
            validation_errors.append("explorerWideWatchTraffic: collapse must be exactly one unwatch")
        if record.get("expandedRows") != 4_096 or record.get("externalChangeRows") != 4_097:
            validation_errors.append("explorerWideWatchTraffic: a precise change must patch exactly one row")
        if not record.get("externalChangeRowPath"):
            validation_errors.append("explorerWideWatchTraffic: the extra row was not the one the event named")
        if record.get("cachedRevisitPaintedBeforeRevalidation") is not True:
            validation_errors.append("explorerWideWatchTraffic: a cached revisit must paint before revalidation")
        for span in ("jsdomExpandToPaintP95Ms", "jsdomExternalChangeToPaintP95Ms"):
            value = record.get(span)
            if not isinstance(value, (int, float)) or isinstance(value, bool):
                validation_errors.append(f"explorerWideWatchTraffic: {span} must be a published sample")
    # The size class the 4,096-entry lane cannot see: the host's page is 4,096
    # entries, so a larger directory arrives incomplete and stays incomplete.
    # A precise change inside the rows the tree holds must still cost no list.
    for record in by_lane.get("explorerPaginatedWatchTraffic", []):
        if record.get("entries") != 12_288 or record.get("hostPageSize") != 4_096:
            validation_errors.append("explorerPaginatedWatchTraffic: the lane must span three host pages")
        # The state the lane exists for. At two pages the prefetched page
        # finished the directory and every patch took the *complete* branch —
        # the same one the 4,096-entry lane takes — so the lane measured a size
        # and nothing else.
        if record.get("heldListingComplete") is not False:
            validation_errors.append("explorerPaginatedWatchTraffic: the held listing was complete, so no incomplete-listing patch was exercised")
        if record.get("expandDirectoryListRequests") != 1:
            validation_errors.append("explorerPaginatedWatchTraffic: expansion must cost the bootstrap plus one prefetched page")
        if record.get("expandedRows") != 8_192:
            validation_errors.append("explorerPaginatedWatchTraffic: the prefetched page did not land")
        if record.get("changeDirectoryListRequests") != 0:
            validation_errors.append("explorerPaginatedWatchTraffic: a single-file change re-listed a paginated directory")
        if record.get("externalChangeRows") != 8_192:
            validation_errors.append("explorerPaginatedWatchTraffic: a precise change was not patched in place")
        # The count above passes equally when *neither* patch landed. These do
        # not: the lane names the row it created and the row it deleted, and a
        # `null` is the test reporting that it could not find, or still found,
        # the row in question.
        if not record.get("createdRowPath"):
            validation_errors.append("explorerPaginatedWatchTraffic: the created row was never patched into the held pages")
        if not record.get("deletedRowPath"):
            validation_errors.append("explorerPaginatedWatchTraffic: the deleted row was still on screen")
    for record in by_lane.get("transferAdmission", []):
        completed_successes = record.get("completedSuccesses")
        completed_failures = record.get("completedFailures")
        if not all(isinstance(value, int) and not isinstance(value, bool) for value in (
            completed_successes, completed_failures, record.get("accepted")
        )) or record.get("accepted") != completed_successes + completed_failures:
            validation_errors.append("transferAdmission: accepted and completed totals differ")
    for record in by_lane.get("explorerListWatch", []):
        attempts = record.get("hostRequestAttempts")
        successes = record.get("hostRequestSuccesses")
        failures = record.get("hostRequestFailures")
        cancellations = record.get("hostRequestCancellations")
        if not all(isinstance(value, int) and not isinstance(value, bool) for value in (
            attempts, successes, failures, cancellations
        )):
            validation_errors.append("explorerListWatch: host request outcomes must be integers")
        elif attempts != successes + failures + cancellations:
            validation_errors.append("explorerListWatch: attempts must equal all terminal outcomes")
    for record in by_lane.get("git32Consumers", []):
        watchers = record.get("nativeWatchers")
        creations = record.get("nativeWatcherCreations")
        git_processes = record.get("gitProcesses")
        status_processes = record.get("statusProcesses")
        diff_processes = record.get("diffProcesses")
        mutation_processes = record.get("mutationProcesses")
        if not all(isinstance(value, int) and not isinstance(value, bool) for value in (
            watchers, creations, git_processes, status_processes, diff_processes, mutation_processes
        )):
            validation_errors.append("git32Consumers: operation counts must be integers")
        elif not (1 <= watchers <= 32 and 1 <= creations <= 32 and status_processes > 0
                  and git_processes >= status_processes + diff_processes + mutation_processes):
            validation_errors.append("git32Consumers: operation-count relationships are invalid")
    output.write_text(json.dumps({
        "schemaVersion": 3,
        "laneScope": scope or "all",
        "records": records,
        "malformed": malformed,
        "validationErrors": validation_errors,
        "valid": not validation_errors,
    }, indent=2) + "\n", encoding="utf-8")
    return 1 if validation_errors else 0


raise SystemExit(main())
