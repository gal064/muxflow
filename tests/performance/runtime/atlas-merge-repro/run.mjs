/**
 * Reproduces, in real WebGL, the terminal artifact where rows of clean content
 * are painted with scattered and duplicated glyphs and stay that way with no
 * new output.
 *
 * Two terminals share one texture atlas (same font config, as every pane in the
 * app does). Pane A is fed screen after screen of freshly coloured glyphs until
 * the atlas runs out of pages and merges four of them into one. That merge
 * rewrites the texture coordinates of every glyph already rasterised — and it
 * happens *inside* the paint of the row that asked for the new glyph, so the
 * frame A presents is half pre-merge and half post-merge. Nothing schedules
 * another frame, so it stays on screen.
 *
 * Run with FIX=0 to see the artifact, FIX=1 to see the repaint that answers it.
 * The pass condition for the fix is `A after 2s idle vs clean: 0 differing`.
 *
 * Not wired into CI: it needs Playwright and a Chromium build, neither of which
 * this repo depends on. See README.md.
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const DIR = path.dirname(fileURLToPath(import.meta.url));
const OUT = process.env.OUT_DIR ?? path.join(DIR, "evidence");
const MAX_PAGES = Number(process.env.MAX_PAGES ?? 4);
const DPR = Number(process.env.DPR ?? 1);
const FIX = process.env.FIX === "1";
const CHROME = process.env.CHROME_PATH;

const { chromium } = await import(process.env.PLAYWRIGHT_PATH ?? "playwright");

fs.mkdirSync(OUT, { recursive: true });

/**
 * A screen of readable box-drawing content. Every row gets its own truecolor
 * foreground, so every row rasterises glyphs the atlas has never seen — which
 * is what drives it towards a page merge without changing cell metrics.
 */
function screen(gen) {
  const col = (k) => `\x1b[38;2;${64 + (k & 127)};${64 + ((k >> 7) & 127)};${64 + ((k >> 14) & 127)}m`;
  const rows = ["┌──────────┬──────────┬──────────┬──────────┬──────────┬──────────┐"];
  for (let i = 0; i < 22; i++) {
    const n = String(19 + ((gen + i) % 9)).padStart(2, "0");
    rows.push(`│ ${n} ${n} ${n} │ ${n} ${n} ${n} │ ${n} ${n} ${n} │ ${n} ${n} ${n} │ ${n} ${n} ${n} │ ${n} ${n} ${n} │`);
  }
  rows.push("└──────────┴──────────┴──────────┴──────────┴──────────┴──────────┘");
  return "\x1b[2J\x1b[H" + rows.map((r, i) => col(gen * 24 + i) + r).join("\r\n") + "\x1b[0m";
}

const browser = await chromium.launch({
  headless: true,
  ...(CHROME ? { executablePath: CHROME } : {}),
  // SwiftShader, so the run does not depend on the host's GPU. The bug is in
  // which texture coordinates are handed to the shader, not in the shader.
  args: ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--disable-lcd-text"],
});
const page = await browser.newPage({ viewport: { width: 1440, height: 480 }, deviceScaleFactor: DPR });
page.on("pageerror", (e) => console.log("[pageerror]", e.message));
await page.goto("file://" + path.join(DIR, "index.html"));
await page.waitForFunction(() => window.__ready === true, null, { timeout: 15_000 });

const setup = await page.evaluate(async ([maxPages, fix]) => {
  H.mk("A", { rows: 24, cols: 80 });
  H.mk("B", { rows: 24, cols: 80 });
  await H.raf(5);
  const ids = H.tagAtlases();
  // Merges happen at 16 pages on real hardware and would need ~25k distinct
  // glyphs to reach. The merge code path does not care how many pages there
  // were, so the threshold is lowered instead of the test being made slow.
  const cap = H.setMaxAtlasPages("A", maxPages);
  if (fix) {
    H.installFix("A");
    H.installFix("B");
  }
  return { ids, cap, fix, statsA: H.stats("A") };
}, [MAX_PAGES, FIX]);
console.log("setup:", JSON.stringify(setup));
if (setup.ids.A !== setup.ids.B) throw new Error("panes did not share an atlas; the repro is meaningless");

// Pane B paints a stable table once and is never touched again.
await page.evaluate(async (payload) => {
  await H.write("B", payload);
  await H.rendered("B");
  await H.raf(3);
}, screen(999));
const bBefore = await page.locator("#B").screenshot();
fs.writeFileSync(path.join(OUT, "B-before.png"), bBefore);

let hit = null;
for (let gen = 0; gen < 600 && !hit; gen++) {
  const r = await page.evaluate(async (payload) => {
    const before = H.stats("A");
    await H.write("A", payload);
    await H.rendered("A");
    return { before, after: H.stats("A") };
  }, screen(gen));
  if (r.after.requestClearModel > r.before.requestClearModel) hit = { gen, ...r };
}
if (!hit) {
  console.log("NO MERGE TRIGGERED — the repro did not get far enough to prove anything");
  await browser.close();
  process.exit(1);
}
console.log("merge:", JSON.stringify(hit));

const aPostMerge = await page.locator("#A").screenshot();
fs.writeFileSync(path.join(OUT, "A-postmerge.png"), aPostMerge);
const bAfter = await page.locator("#B").screenshot();
fs.writeFileSync(path.join(OUT, "B-after.png"), bAfter);

// The user's condition: no writes, no scroll, no input. Whatever is on screen
// after this is what they keep looking at.
await page.waitForTimeout(2_000);
const aSettled = await page.locator("#A").screenshot();
fs.writeFileSync(path.join(OUT, "A-settled.png"), aSettled);
console.log("stats after 2s idle:", JSON.stringify(await page.evaluate(() => H.stats("A"))));

// Ground truth: the same unchanged content, painted again from scratch.
await page.evaluate(async () => {
  await H.refresh("A");
  await H.raf(3);
});
const aClean = await page.locator("#A").screenshot();
fs.writeFileSync(path.join(OUT, "A-clean.png"), aClean);

/** Pixel-diffs two PNG buffers in a throwaway page, so there is no native dep. */
async function diff(a, b, outPath) {
  const p = await browser.newPage();
  const res = await p.evaluate(
    async ([x, y]) => {
      const load = (d) => new Promise((ok, no) => {
        const img = new Image();
        img.onload = () => ok(img);
        img.onerror = no;
        img.src = "data:image/png;base64," + d;
      });
      const [ia, ib] = await Promise.all([load(x), load(y)]);
      if (ia.width !== ib.width || ia.height !== ib.height) return { differing: -1, total: -1, pct: -1, png: null };
      const data = (img) => {
        const c = document.createElement("canvas");
        c.width = img.width;
        c.height = img.height;
        const ctx = c.getContext("2d");
        ctx.drawImage(img, 0, 0);
        return ctx.getImageData(0, 0, img.width, img.height);
      };
      const da = data(ia), db = data(ib);
      const out = document.createElement("canvas");
      out.width = ia.width;
      out.height = ia.height;
      const octx = out.getContext("2d");
      const od = octx.createImageData(ia.width, ia.height);
      let differing = 0;
      for (let i = 0; i < da.data.length; i += 4) {
        const delta = Math.abs(da.data[i] - db.data[i]) + Math.abs(da.data[i + 1] - db.data[i + 1]) + Math.abs(da.data[i + 2] - db.data[i + 2]);
        if (delta > 24) {
          differing++;
          od.data[i] = 255; od.data[i + 3] = 255;
        } else {
          od.data[i] = da.data[i] >> 2; od.data[i + 1] = da.data[i + 1] >> 2; od.data[i + 2] = da.data[i + 2] >> 2; od.data[i + 3] = 255;
        }
      }
      octx.putImageData(od, 0, 0);
      return { differing, total: ia.width * ia.height, pct: +((differing / (ia.width * ia.height)) * 100).toFixed(4), png: out.toDataURL("image/png").split(",")[1] };
    },
    [a.toString("base64"), b.toString("base64")],
  );
  await p.close();
  if (res.png) fs.writeFileSync(outPath, Buffer.from(res.png, "base64"));
  return { differing: res.differing, total: res.total, pct: res.pct };
}

const dSettled = await diff(aSettled, aClean, path.join(OUT, "A-settled-diff.png"));
const dB = await diff(bBefore, bAfter, path.join(OUT, "B-diff.png"));
console.log("A post-merge vs clean:", JSON.stringify(await diff(aPostMerge, aClean, path.join(OUT, "A-postmerge-diff.png"))));
console.log("A after 2s idle vs clean:", JSON.stringify(dSettled), dSettled.pct === 0 ? "=> SELF-HEALED" : "=> CORRUPTION PERSISTS");
console.log("B idle before vs after:", JSON.stringify(dB));
console.log("atlas events seen:", JSON.stringify(await page.evaluate(() => ({ A: H.atlasEvents("A"), B: H.atlasEvents("B") }))));

await browser.close();
// Exit code carries the verdict: with the fix on, a clean settled frame is the
// pass; with it off, a clean settled frame means the repro stopped working.
const healed = dSettled.pct === 0;
process.exit(FIX === healed ? 0 : 1);
