import { Terminal as HeadlessTerminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";
import { terminalLinksForBufferLine } from "./terminalLinks";

describe("terminal links across rendered rows", () => {
  it("joins a parenthesized relative path whose first hard row is not a path by itself", async () => {
    const lines = [
      "PowerPoint (sampleco-projectx-strategic-mapping/slides/partnerco-september-2026/output/Sampleco-Partnerco-2026-09-10.pptx) · PDF (sampleco-projectx-strategic-",
      "  mapping/slides/partnerco-september-2026/tmp/Sampleco-Partnerco-2026-09-10.pdf)",
    ];
    const terminal = await terminalWith(lines.join("\r\n"), 200);
    const powerpoint = "sampleco-projectx-strategic-mapping/slides/partnerco-september-2026/output/Sampleco-Partnerco-2026-09-10.pptx";
    const pdf = "sampleco-projectx-strategic-mapping/slides/partnerco-september-2026/tmp/Sampleco-Partnerco-2026-09-10.pdf";
    const expectedPdf = {
      kind: "file",
      text: pdf,
      range: {
        start: { x: lines[0].lastIndexOf("(") + 2, y: 1 },
        end: { x: lines[1].indexOf(")"), y: 2 },
      },
    };

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)).toEqual([
      {
        kind: "file",
        text: powerpoint,
        range: {
          start: { x: lines[0].indexOf(powerpoint) + 1, y: 1 },
          end: { x: lines[0].indexOf(powerpoint) + powerpoint.length, y: 1 },
        },
      },
      expectedPdf,
    ]);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([expectedPdf]);
  });

  it("joins an unparenthesized relative path that wraps before its first slash", async () => {
    const lines = [
      "• Added >> responses beneath every comment in sampleco-projectx-strategic-",
      "  mapping/meetings/2026-09-10-partnerco/DECK-PLAN.md.",
    ];
    const path = "sampleco-projectx-strategic-mapping/meetings/2026-09-10-partnerco/DECK-PLAN.md";
    const terminal = await terminalWith(lines.join("\r\n"), 100);
    const expected = {
      kind: "file",
      text: path,
      range: {
        start: { x: lines[0].indexOf("sampleco-") + 1, y: 1 },
        end: { x: lines[1].lastIndexOf("."), y: 2 },
      },
    };

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)).toEqual([expected]);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([expected]);
  });

  it("uses generic list-marker and indentation evidence for an unparenthesized path", async () => {
    const terminal = await terminalWith("● Saved report-\r\n    output/final.md.", 80);
    const expected = "report-output/final.md";

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)[0]?.text).toBe(expected);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)[0]?.text).toBe(expected);
  });

  it.each([
    ["ordinary prose", "Stored at /home/dev/report-", "  final.ts:11.", "/home/dev/report-final.ts", "/home"],
    ["a bullet", "• Stored ~/reports/report-", "  final.ts:11.", "~/reports/report-final.ts", "~/"],
    ["an indented list item", "    - Stored src/report-", "      final.ts:11.", "src/report-final.ts", "src/"],
    ["a numbered item", "  1. Stored /home/dev/report-", "     final.ts:11.", "/home/dev/report-final.ts", "/home"],
    ["a standalone target", "  /home/dev/report-", "  final.ts:11.", "/home/dev/report-final.ts", "/home"],
  ])(
    "joins a recognized path across hard rows in %s",
    async (_context, origin, continuation, path, firstFragment) => {
      const terminal = await terminalWith(`${origin}\r\n${continuation}`, 80);
      const expected = {
        kind: "file",
        text: path,
        range: {
          start: { x: origin.indexOf(firstFragment) + 1, y: 1 },
          end: { x: continuation.indexOf(":11"), y: 2 },
        },
      };

      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)).toEqual([expected]);
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([expected]);
    },
  );

  it("joins the reported migration path embedded in a hard-wrapped paragraph", async () => {
    const lines = [
      "  The new attempt table stores requested/accepted/running/completed/failed/unavailable state, result stage, candidate identity, verification, inferred-",
      "  unavailable state and recovery deadlines at /home/dev/dev/sampleco-demo-repo-pr4807-integration-v2/packages/core-services/src/migrations/1803020000000-",
      "  CreateTestRunHealingAttempts.ts:11.",
    ];
    const path = "/home/dev/dev/sampleco-demo-repo-pr4807-integration-v2/packages/core-services/src/migrations/1803020000000-CreateTestRunHealingAttempts.ts";
    const terminal = await terminalWith(lines.join("\r\n"), 200);
    const expected = {
      kind: "file",
      text: path,
      range: {
        start: { x: lines[1].indexOf("/home") + 1, y: 2 },
        end: { x: lines[2].indexOf(":11"), y: 3 },
      },
    };

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([expected]);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 3)).toEqual([expected]);
  });

  it("does not append indented prose to a standalone directory", async () => {
    const terminal = await terminalWith("  /home/dev/reports/\r\n  Read the summary next.", 80);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)[0]?.text)
      .toBe("/home/dev/reports/");
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([]);
  });

  it("returns both links when one hard-wrapped target ends where another begins", async () => {
    const lines = [
      "• PowerPoint (sampleco-projectx-strategic-mapping/slides/projectx-update/output/2026-08-11/Sampleco-Strategic-Projectx-Update-",
      "  2026-08-11.pptx) · PDF (sampleco-projectx-strategic-mapping/slides/projectx-update/output/2026-08-11/Sampleco-Strategic-",
      "  Projectx-Update-2026-08-11.pdf)",
    ];
    const powerpoint = "sampleco-projectx-strategic-mapping/slides/projectx-update/output/2026-08-11/Sampleco-Strategic-Projectx-Update-2026-08-11.pptx";
    const pdf = "sampleco-projectx-strategic-mapping/slides/projectx-update/output/2026-08-11/Sampleco-Strategic-Projectx-Update-2026-08-11.pdf";
    const terminal = await terminalWith(lines.join("\r\n"), 160);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2).map((link) => link.text)).toEqual([
      powerpoint,
      pdf,
    ]);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 3).map((link) => link.text)).toEqual([pdf]);
  });

  it("joins the exact three hard rows painted by Codex in an 85-column pane", async () => {
    const lines = [
      "• Or manually transfer and install /home/dev/dev/dev-app-mobile-layout-scroll-",
      "  diagnostics/apps/mobile/android/app/build/outputs/apk/release/app-",
      "  release.apk.",
    ];
    expect(lines.map((line) => line.length)).toEqual([78, 68, 14]);
    const terminal = await terminalWith(lines.join("\r\n"), 85);
    const expected = {
      kind: "file",
      text: "/home/dev/dev/dev-app-mobile-layout-scroll-diagnostics/apps/mobile/android/app/build/outputs/apk/release/app-release.apk",
      range: {
        start: { x: 36, y: 1 },
        end: { x: 13, y: 3 },
      },
    };

    for (const row of [1, 2, 3]) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)).toEqual([expected]);
    }
  });

  it("joins the reported TravelClick URL across Codex hard rows", async () => {
    const lines = [
      "   Property       Lake Natoma Inn (https://bookings.travelclick.com/13381?",
      "                  adults=2&children=1&DateIn=09/05/2026&DateOut=09/06/2026&HotelId=13381&languageid=1&rooms=1#/accommodation/room)",
    ];
    const url = "https://bookings.travelclick.com/13381?adults=2&children=1&DateIn=09/05/2026&DateOut=09/06/2026&HotelId=13381&languageid=1&rooms=1#/accommodation/room";
    const terminal = await terminalWith(lines.join("\r\n"), 160);
    const expected = {
      kind: "web",
      text: url,
      range: {
        start: { x: lines[0].indexOf("https://") + 1, y: 1 },
        end: { x: lines[1].length - 1, y: 2 },
      },
    };

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)).toEqual([expected]);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([expected]);
  });

  it.each(["?", "/", "#", "=", "%", "-", "."])(
    "joins a hard-rendered URL after the %s separator",
    async (separator) => {
      const terminal = await terminalWith(`Open https://example.com/path${separator}\r\n  continuation`, 80);
      const expected = `https://example.com/path${separator}continuation`;

      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)[0]?.text).toBe(expected);
    },
  );

  it("keeps raw URL punctuation across an intermediate hard row", async () => {
    const lines = [
      "Open https://example.com/path/",
      "  item?",
      "  query=value",
    ];
    const terminal = await terminalWith(lines.join("\r\n"), 80);
    const expected = "https://example.com/path/item?query=value";

    for (const row of [1, 2, 3]) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)[0]?.text).toBe(expected);
    }
  });

  it("does not append indented prose to a completed URL", async () => {
    const url = "https://example.com/path";
    const terminal = await terminalWith(`${url}\r\n  This is a new paragraph.`, 80);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)[0]?.text).toBe(url);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([]);
  });

  it("does not continue a URL after its closing delimiter", async () => {
    const url = "https://example.com/path";
    const terminal = await terminalWith(`(${url})\r\n  query=value`, 80);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)[0]?.text).toBe(url);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([]);
  });

  it.each([")", "]", "}"])(
    "does not bypass the %s closing delimiter when sentence punctuation follows it",
    async (delimiter) => {
      const url = "https://example.com/path";
      const terminal = await terminalWith(`${url}${delimiter}.\r\n  query=value`, 80);

      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)[0]?.text).toBe(url);
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([]);
    },
  );

  it("joins a file path across a proven soft wrap and links both rows", async () => {
    const terminal = await terminalWith("  /home/dev/dev/dev-app/apps/mobile/android/app-release.apk", 32);
    const expectedText = "/home/dev/dev/dev-app/apps/mobile/android/app-release.apk";

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)).toEqual([{
      kind: "file",
      text: expectedText,
      range: {
        start: { x: 3, y: 1 },
        end: { x: 27, y: 2 },
      },
    }]);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)[0]?.text).toBe(expectedText);
  });

  it("joins a URL across a proven soft wrap", async () => {
    const url = "https://example.com/releases/a-very-long-build-artifact.apk?download=1";
    const terminal = await terminalWith(`Open ${url}`, 30);

    const links = terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2);
    expect(links).toHaveLength(1);
    expect(links[0]).toMatchObject({
      kind: "web",
      text: url,
      range: { start: { x: 6, y: 1 }, end: { y: 3 } },
    });
  });

  it("does not join tokens separated by a real newline", async () => {
    const terminal = await terminalWith("/home/dev/dev/dev-app/apps/mobile/android/app-re\r\nlease.apk", 80);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)[0]?.text)
      .toBe("/home/dev/dev/dev-app/apps/mobile/android/app-re");
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([]);
  });

  it("joins a recognized path continuation without relying on its output source", async () => {
    const terminal = await terminalWith("/tmp/app-\r\n  release.apk", 85);
    const expected = {
      kind: "file",
      text: "/tmp/app-release.apk",
      range: {
        start: { x: 1, y: 1 },
        end: { x: 13, y: 2 },
      },
    };

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)).toEqual([expected]);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([expected]);
  });

  it("does not invent a parenthesized hard-wrapped path without a closing delimiter", async () => {
    const terminal = await terminalWith("See (sampleco-projectx-strategic-\r\n  mapping/slides/report.pdf", 80);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)).toEqual([]);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)[0]?.text)
      .toBe("mapping/slides/report.pdf");
  });

  it.each([
    ["an inner pair on the continuation", "See (assets/foo-\r\n  image_(dark).pdf)", "assets/foo-image_(dark).pdf"],
    ["an inner opening on the origin", "See (docs/release_(final-\r\n  candidate)/report.pdf)", "docs/release_(final-candidate)/report.pdf"],
    ["an inner pair at the target end", "See (assets/foo-\r\n  image_(dark))", "assets/foo-image_(dark)"],
    ["an alphanumeric row boundary", "See (workspacepro\r\n  ject/report.pdf)", "workspaceproject/report.pdf"],
  ])("balances the outer wrapper around a path with %s", async (_label, text, expected) => {
    const terminal = await terminalWith(text, 80);

    for (const row of [1, 2]) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)[0]?.text).toBe(expected);
    }
  });

  it("joins application hard rows after xterm soft-wraps each of them", async () => {
    const lines = [
      "See (workspace/a-very-long-project-",
      "  name/with/a-very-long-directory/report.pdf)",
    ];
    const expected = "workspace/a-very-long-project-name/with/a-very-long-directory/report.pdf";
    const terminal = await terminalWith(lines.join("\r\n"), 20, 10);

    for (let row = 1; row <= 5; row += 1) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)
        .some((link) => link.text === expected), `row=${row}`).toBe(true);
    }
  });

  it("joins a hard-wrapped URL after xterm soft-wraps its application rows", async () => {
    const lines = [
      "Open https://example.com/a-very-long-",
      "  path/with/a-very-long-query?value=one",
    ];
    const expected = "https://example.com/a-very-long-path/with/a-very-long-query?value=one";
    const terminal = await terminalWith(lines.join("\r\n"), 20, 10);
    const link = terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)
      .find((candidate) => candidate.text === expected);

    expect(link?.range.end.y).toBeGreaterThan(2);
    for (let row = link?.range.start.y ?? 0; row <= (link?.range.end.y ?? -1); row += 1) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)
        .some((candidate) => candidate.text === expected), `row=${row}`).toBe(true);
    }
  });

  it("joins a parenthesized URL split at an alphanumeric boundary", async () => {
    const expected = "https://example.com/releases/file.pdf";
    const terminal = await terminalWith("Open (https://example.com/relea\r\n  ses/file.pdf)", 80);

    for (const row of [1, 2]) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)).toEqual([{
        kind: "web",
        text: expected,
        range: {
          start: { x: 7, y: 1 },
          end: { x: 14, y: 2 },
        },
      }]);
    }
  });

  it.each([
    ["file", "See (docs/re-\r\n  port.pdf.)", "docs/re-port.pdf"],
    ["web", "See (https://example.com/re-\r\n  port.)", "https://example.com/re-port"],
  ])("keeps trailing prose punctuation outside a hard-wrapped %s target", async (_kind, text, expected) => {
    const terminal = await terminalWith(text, 80);

    for (const row of [1, 2]) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)[0]?.text).toBe(expected);
    }
  });

  it("counts application rows separately from narrow-pane physical rows", async () => {
    const lines = [
      "See (workspace/a-very-long-project-",
      "  name/with/a-very-long-directory/report.pdf)",
    ];
    const expected = "workspace/a-very-long-project-name/with/a-very-long-directory/report.pdf";
    const terminal = await terminalWith(lines.join("\r\n"), 3, 50);
    const first = terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)
      .find((link) => link.text === expected);

    expect(first?.range.end.y).toBeGreaterThan(20);
    for (let row = first?.range.start.y ?? 0; row <= (first?.range.end.y ?? -1); row += 1) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)
        .some((link) => link.text === expected), `row=${row}`).toBe(true);
    }
  });

  it("bounds parenthesized hard-wrapped path scans", async () => {
    const lines = [
      "See (sampleco-projectx-strategic-",
      ...Array.from({ length: 20 }, (_, index) => `  part${index}-`),
      "  mapping/slides/report.pdf)",
    ];
    const terminal = await terminalWith(lines.join("\r\n"), 80, lines.length + 1);

    for (const row of [1, lines.length]) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)
        .some((link) => link.text.startsWith("sampleco-projectx-strategic-"))).toBe(false);
    }
  });

  it("accepts a parenthesized path closing on the twentieth application row", async () => {
    const parts = Array.from({ length: 18 }, (_, index) => `part${index}/`);
    const lines = ["See (workspace/", ...parts.map((part) => `  ${part}`), "  report.pdf)"];
    const expected = `workspace/${parts.join("")}report.pdf`;
    const terminal = await terminalWith(lines.join("\r\n"), 80, lines.length + 1);

    for (const row of [1, 10, 20]) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)[0]?.text).toBe(expected);
    }
  });

  it("joins an indented dash list item that wraps after a slash", async () => {
    const lines = [
      "    - /home/dev/dev/ai-projects/sampleco-projectx-strategic-mapping/meetings/2026-09-10-partnerco/",
      "      RUNBOOK.md is the concise operator checklist.",
    ];
    const path = "/home/dev/dev/ai-projects/sampleco-projectx-strategic-mapping/meetings/2026-09-10-partnerco/RUNBOOK.md";
    const terminal = await terminalWith(lines.join("\r\n"), 200);

    for (const row of [1, 2]) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)[0]?.text).toBe(path);
    }
  });

  it("does not splice a sibling list item into the item above it", async () => {
    const lines = ["  - /home/dev/dev/one/", "  - /home/dev/dev/two/"];
    const terminal = await terminalWith(lines.join("\r\n"), 80);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1).map((link) => link.text))
      .toEqual(["/home/dev/dev/one/"]);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2).map((link) => link.text))
      .toEqual(["/home/dev/dev/two/"]);
  });

  it("does not splice a sibling list item into a relative path above it", async () => {
    const lines = ["  - src/components/", "  - src/features/"];
    const terminal = await terminalWith(lines.join("\r\n"), 80);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1).map((link) => link.text))
      .toEqual(["src/components/"]);
    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2).map((link) => link.text))
      .toEqual(["src/features/"]);
  });

  it("does not swallow the next word of a sentence as a path continuation", async () => {
    const terminal = await terminalWith("  - the files live in src/components/\r\n    more text here", 80);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1).map((link) => link.text))
      .toEqual(["src/components/"]);
  });

  it("keeps joining across hops whose fragments carry no separator or extension", async () => {
    const terminal = await terminalWith("● /home/dev/a-\r\n  b-\r\n  c.md", 80);

    for (const row of [1, 2, 3]) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, row)[0]?.text)
        .toBe("/home/dev/a-b-c.md");
    }
  });

  it("joins an extensionless tail when the row broke inside the token", async () => {
    const terminal = await terminalWith("● Saved /home/dev/dev/my-\r\n  project", 80);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)[0]?.text)
      .toBe("/home/dev/dev/my-project");
  });

  it("does not splice a sibling item marked with a plus", async () => {
    const lines = ["  + /home/dev/dev/one/", "  + /home/dev/dev/two/"];
    const terminal = await terminalWith(lines.join("\r\n"), 80);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1).map((link) => link.text))
      .toEqual(["/home/dev/dev/one/"]);
  });

  it("joins a multi-level numbered item that wraps", async () => {
    const terminal = await terminalWith("  1.1. Open /home/dev/dev/notes-\r\n       archive/today.md.", 80);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 1)[0]?.text)
      .toBe("/home/dev/dev/notes-archive/today.md");
  });

  it("maps UTF-16 text offsets to cells across wrapped rows", async () => {
    const terminal = await terminalWith("界界 /tmp/this-path-wraps", 12);

    expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, 2)).toEqual([{
      kind: "file",
      text: "/tmp/this-path-wraps",
      range: {
        start: { x: 6, y: 1 },
        end: { x: 1, y: 3 },
      },
    }]);
  });
});

async function terminalWith(text: string, columns: number, rows = 6): Promise<HeadlessTerminal> {
  const terminal = new HeadlessTerminal({
    allowProposedApi: true,
    cols: columns,
    rows,
    scrollback: 100,
  });
  await new Promise<void>((resolve) => terminal.write(text, resolve));
  return terminal;
}
