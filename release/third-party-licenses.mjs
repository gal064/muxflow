// Writes the THIRD-PARTY-LICENSES file every package ships: the licence text
// and copyright notices of each crate compiled into the app (`muxflow`) or the
// helper (`muxflow-host`), and of the fonts the app embeds.
//
// Usage: node release/third-party-licenses.mjs OUTPUT
//
// It runs at package time from Cargo.lock, so it cannot go stale. cargo-about
// (pinned below, MIT OR Apache-2.0) finds each crate's licence files. The
// output holds no paths or dates, so a package stays reproducible.
import { execFileSync } from 'node:child_process';
import { chmodSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { upstreamNotices } from './license-notices.mjs';

const ABOUT_VERSION = '0.9.3';
const output = process.argv[2];
if (!output) throw new Error('usage: node release/third-party-licenses.mjs OUTPUT');
const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const fonts = join(repo, 'apps/desktop/assets/fonts');

const run = (args) =>
  execFileSync('cargo', ['about', ...args], { cwd: repo, encoding: 'utf8', maxBuffer: 1 << 30 });
let version = '';
try {
  version = run(['--version']).trim();
} catch {}
if (version !== `cargo-about ${ABOUT_VERSION}`) {
  console.error(
    `cargo-about ${ABOUT_VERSION} is required (found: ${version || 'none'}); ` +
      `install it with: cargo install --locked --features cli cargo-about@${ABOUT_VERSION}`,
  );
  process.exit(69);
}

// cargo-about scans one root crate at a time, so the app and the helper each
// get a scan. `--fail` stops on any crate whose licence cannot be determined,
// rather than shipping without it.
const scans = ['apps/desktop', 'apps/host'].map((manifest) =>
  JSON.parse(
    run([
      'generate', '--locked', '--fail', '--format', 'json',
      '--config', 'release/about.toml', '--manifest-path', `${manifest}/Cargo.toml`,
    ]),
  ),
);

// Muxflow's own crates are not third-party.
const firstParty = (krate) =>
  krate.source === null &&
  ['apps', 'crates'].some((dir) => krate.manifest_path.startsWith(join(repo, dir) + '/'));
const compare = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
const label = (krate) => `${krate.name} ${krate.version}`;
const normalize = (text) => text.replace(/\r\n?/g, '\n').replace(/\s+$/, '') + '\n';

const crates = new Map(); // label -> { krate, license, found: bool, generic: bool (lacks its copyright line) }
const texts = new Map(); // licence text -> { name, crates: Set of labels }
for (const scan of scans) {
  for (const { package: krate, license } of scan.crates) {
    const name = label(krate);
    if (!firstParty(krate) && !crates.has(name)) crates.set(name, { krate, license, found: false, generic: false });
  }
  for (const entry of scan.licenses) {
    for (const { crate: krate } of entry.used_by) {
      const info = crates.get(label(krate));
      if (!info) continue;
      info.found = true;
      // No source file: the standard text stands in for one the crate lacks.
      // Apache-2.0's text holds no copyright line, so only the others miss one.
      if (!entry.source_path && !entry.id.startsWith('Apache-2.0')) info.generic = true;
      const text = normalize(entry.text);
      if (!texts.has(text)) texts.set(text, { name: entry.name, crates: new Set() });
      texts.get(text).crates.add(label(krate));
    }
  }
}
for (const [name, info] of crates) {
  if (!info.found) throw new Error(`no licence text found for ${name}`);
  if (info.krate.name === 'wayland-protocols-plasma') {
    throw new Error('the shipped graph must not include KDE blur protocol bindings');
  }
}

// Files cargo-about does not ship but the crate's licence asks for: notices of
// code or assets the crate includes from elsewhere (gpui-kit-assets embeds the
// Lucide icons), and, where a standard text without the crate's copyright line
// stands in for its own, every licence file it has, since those carry it.
const notice = /^(notice|copyright|licen[cs]e-(third.?party|lucide))/i;
const licenceFile = /^(licen[cs]e|copying|copyright|notice|unlicense)/i;
// Notices below a crate's root for code compiled into the binaries.
const nested = {
  'tree-sitter': ['src/unicode/LICENSE'], // ICU headers
  'regex-syntax': ['src/unicode_tables/LICENSE-UNICODE'],
  'wayland-protocols': ['protocols/COPYING'],
};
const extras = [];
for (const name of [...crates.keys()].sort(compare)) {
  const { krate, generic } = crates.get(name);
  const dir = dirname(krate.manifest_path);
  const root = readdirSync(dir, { withFileTypes: true })
    .filter((entry) => entry.isFile() && (notice.test(entry.name) || (generic && licenceFile.test(entry.name))))
    .map((entry) => entry.name);
  if (generic && root.length === 0) {
    extras.push(...upstreamNotices(name, join(repo, 'release')));
  }
  // readFileSync throws if a listed nested notice has moved, so a bump fails.
  for (const file of [...root.sort(compare), ...(nested[krate.name] ?? [])]) {
    extras.push({ name, file, text: normalize(readFileSync(join(dir, file), 'utf8')) });
  }
}

const fontLicences = readdirSync(fonts).filter((file) => /^LICENSE-.*\.txt$/.test(file)).sort(compare);
if (!fontLicences.length) throw new Error(`no font licences in ${fonts}`);

const rule = '='.repeat(78);
const out = [
  'Third-party software in Muxflow',
  '',
  'The Muxflow app (muxflow) and its helper (muxflow-host) are built from the Rust',
  'crates listed below, and the app embeds the fonts listed at the end. Each',
  'licence text is followed by the crates it applies to.',
  '',
  'Both also include the Rust standard library (https://github.com/rust-lang/rust),',
  'Copyright (c) The Rust Project Contributors, licensed under the Apache License',
  '2.0 or the MIT license at your option; both texts are below.',
  '',
  rule,
  'Crates',
  rule,
  '',
];
for (const name of [...crates.keys()].sort(compare)) {
  const { krate, license } = crates.get(name);
  out.push(`${name}  ${license}  ${krate.repository || `https://crates.io/crates/${krate.name}`}`);
}
const entries = [...texts].sort(([textA, a], [textB, b]) => compare(a.name, b.name) || compare(textA, textB));
for (const [text, { name, crates: users }] of entries) {
  out.push('', rule, name, rule, '', 'Used by:');
  for (const user of [...users].sort(compare)) {
    const authors = crates.get(user).krate.authors;
    out.push(`  ${user}${authors.length ? ` (${authors.join(', ')})` : ''}`);
  }
  out.push('', text.trimEnd());
}
for (const { name, file, text } of extras) {
  out.push('', rule, `${name}: ${file}`, rule, '', text.trimEnd());
}
for (const file of fontLicences) {
  const family = file.slice('LICENSE-'.length, -'.txt'.length);
  const files = readdirSync(fonts).filter((font) => font.startsWith(`${family}-`) && font.endsWith('.ttf'));
  out.push('', rule, `Font: ${family} (${files.sort(compare).join(', ')})`, rule, '');
  out.push(normalize(readFileSync(join(fonts, file), 'utf8')).trimEnd());
}
writeFileSync(output, out.join('\n') + '\n');
// The package records the mode, so it must not depend on the umask.
chmodSync(output, 0o644);
