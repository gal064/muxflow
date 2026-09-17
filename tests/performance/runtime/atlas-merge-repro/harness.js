import { Terminal } from "@xterm/xterm";
import { WebglAddon } from "@xterm/addon-webgl";

const panes = new Map();

function mk(id, opts = {}) {
  const el = document.getElementById(id);
  const term = new Terminal({
    rows: opts.rows ?? 24,
    cols: opts.cols ?? 80,
    fontFamily: "monospace",
    fontSize: 14,
    lineHeight: 1,
    letterSpacing: 0,
    allowTransparency: false,
    scrollback: 1000,
    theme: { background: "#000000", foreground: "#ffffff" },
  });
  term.open(el);
  const addon = new WebglAddon();
  term.loadAddon(addon);
  panes.set(id, { term, addon, el });
  return id;
}

function internals(id) {
  const p = panes.get(id);
  const r = p?.addon?._renderer;
  const atlas = r?._charAtlas;
  const gr = r?._glyphRenderer?.value;
  return { p, r, atlas, gr };
}

function stats(id) {
  const { atlas, gr } = internals(id);
  return {
    pages: atlas?.pages?.length,
    pageWidths: atlas?.pages?.map((x) => x.canvas.width),
    requestClearModel: atlas?._requestClearModel,
    rendererVersion: gr?._atlasClearModelVersion,
    atlasId: atlas?.__id,
  };
}

function tagAtlases() {
  let n = 0;
  for (const id of panes.keys()) {
    const { atlas } = internals(id);
    if (atlas && atlas.__id === undefined) atlas.__id = ++n;
  }
  const ids = {};
  for (const id of panes.keys()) ids[id] = internals(id).atlas?.__id;
  return ids;
}

function write(id, data) {
  const { term } = panes.get(id);
  return new Promise((res) => term.write(data, res));
}

function raf(n = 2) {
  return new Promise((res) => {
    let i = 0;
    const step = () => (++i >= n ? res() : requestAnimationFrame(step));
    requestAnimationFrame(step);
  });
}

// Wait until one full render pass has happened for this pane.
function rendered(id) {
  const { term } = panes.get(id);
  return new Promise((res) => {
    const d = term.onRender(() => {
      d.dispose();
      res();
    });
    setTimeout(() => {
      d.dispose();
      res();
    }, 500);
  });
}

function refresh(id) {
  const { term } = panes.get(id);
  term.refresh(0, term.rows - 1);
  return rendered(id);
}

// Shrink the merge threshold so page merges are reachable in a test. The
// merge code path is identical; only the page count that triggers it changes.
function setMaxAtlasPages(id, n) {
  const { atlas } = internals(id);
  if (!atlas) return null;
  const ctor = atlas.constructor;
  ctor.maxAtlasPages = n;
  return ctor.maxAtlasPages;
}

// A distinct glyph per (char, fg colour) pair; truecolor SGR gives us as many
// distinct rasterisations as we like without changing cell metrics.
function fillGlyphs(id, count, startAt = 0) {
  const { term } = panes.get(id);
  const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  let s = "";
  for (let i = 0; i < count; i++) {
    const k = startAt + i;
    const r = (k * 7) % 256;
    const g = (k * 13) % 256;
    const b = (k * 29) % 256;
    s += `\x1b[38;2;${r};${g};${b}m${chars[k % chars.length]}`;
  }
  s += "\x1b[0m";
  return new Promise((res) => term.write(s, res));
}

const BOX = (() => {
  const rows = [];
  rows.push("┌────────────┬────────────┬────────────┐");
  for (let i = 0; i < 8; i++) {
    rows.push(`│ row ${String(i).padStart(2, "0")} val │ 19 20 21   │ ${String(1900 + i)}       │`);
  }
  rows.push("└────────────┴────────────┴────────────┘");
  return rows.join("\r\n");
})();

window.H = {
  mk,
  stats,
  write,
  refresh,
  rendered,
  raf,
  fillGlyphs,
  setMaxAtlasPages,
  tagAtlases,
  BOX,
  panes,
  internals: (id) => {
    const { r, atlas, gr } = internals(id);
    return {
      hasRenderer: !!r,
      hasAtlas: !!atlas,
      hasGlyphRenderer: !!gr,
      requestClearModelType: typeof atlas?._requestClearModel,
      rendererVersionType: typeof gr?._atlasClearModelVersion,
    };
  },
  // Install a hook that records every atlas invalidation with the frame it
  // happened on, so we can tell a mid-frame merge from a between-frames merge.
  instrument(id) {
    const { atlas, gr } = internals(id);
    if (!atlas) return false;
    window.__events = [];
    window.__frame = 0;
    const origBegin = gr.beginFrame.bind(gr);
    // count frames per pane
    for (const [pid, p] of panes) {
      const g = p.addon?._renderer?._glyphRenderer?.value;
      if (!g || g.__wrapped) continue;
      g.__wrapped = true;
      const ob = g.beginFrame.bind(g);
      g.beginFrame = function () {
        window.__frame++;
        const res = ob();
        window.__events.push({ t: "beginFrame", pane: pid, frame: window.__frame, full: res, atlasCount: atlas._requestClearModel });
        return res;
      };
    }
    let last = atlas._requestClearModel;
    const iv = setInterval(() => {
      if (atlas._requestClearModel !== last) {
        window.__events.push({ t: "invalidate", from: last, to: atlas._requestClearModel, frame: window.__frame });
        last = atlas._requestClearModel;
      }
    }, 0);
    window.__stopInstrument = () => clearInterval(iv);
    void origBegin;
    return true;
  },
  events: () => window.__events ?? [],
  // The candidate fix, expressed only in the addon's public API: any atlas
  // event (page added / pages merged) schedules one full repaint of the pane.
  installFix(id) {
    const p = panes.get(id);
    if (!p || p.__fixed) return false;
    p.__fixed = true;
    p.__atlasEvents = 0;
    p.addon.onAddTextureAtlasCanvas(() => {
      p.__atlasEvents++;
      p.term.refresh(0, p.term.rows - 1);
    });
    return true;
  },
  atlasEvents: (id) => panes.get(id)?.__atlasEvents,
};
window.__ready = true;
