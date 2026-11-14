import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { upstreamNotices } from './license-notices.mjs';

const directory = dirname(fileURLToPath(import.meta.url));
test('every pinned notice is intact and missing versions fail closed', () => {
  const manifest = JSON.parse(readFileSync(join(directory, 'license-notices.json'), 'utf8'));
  for (const label of Object.keys(manifest)) {
    const notices = upstreamNotices(label, directory);
    assert.ok(notices.length);
    for (const notice of notices) assert.ok(notice.text.trim());
  }
  assert.throws(() => upstreamNotices('unrecorded-crate 1.0.0', directory), /missing pinned/);
});

test('changed or missing notice files stop packaging', () => {
  const root = mkdtempSync(join(tmpdir(), 'muxflow-notices-'));
  try {
    const manifest = JSON.parse(readFileSync(join(directory, 'license-notices.json'), 'utf8'));
    const [label, entry] = Object.entries(manifest)[0];
    mkdirSync(join(root, 'licenses'));
    writeFileSync(join(root, 'license-notices.json'), JSON.stringify({ [label]: entry }));
    assert.throws(() => upstreamNotices(label, root), /ENOENT/);
    writeFileSync(join(root, 'licenses', entry.files[0].file), 'tampered notice');
    assert.throws(() => upstreamNotices(label, root), /checksum mismatch/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
