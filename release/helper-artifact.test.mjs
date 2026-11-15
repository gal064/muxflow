import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { metadata, sourceDigest, verify } from './helper-artifact.mjs';

test('supplied helpers must match sources, architecture, features, and bytes', (t) => {
  const repo = mkdtempSync(join(tmpdir(), 'muxflow-helper-'));
  t.after(() => rmSync(repo, { recursive: true, force: true }));
  for (const directory of ['apps/host', 'crates', 'release/linux']) mkdirSync(join(repo, directory), { recursive: true });
  for (const file of ['Cargo.lock', 'Cargo.toml', 'rust-toolchain.toml', 'rustfmt.toml',
    'apps/host/main.rs', 'crates/protocol.rs', 'release/linux/build-compatible-host.sh', 'release/helper-artifact.mjs']) {
    writeFileSync(join(repo, file), file);
  }
  const binary = join(repo, 'helper');
  writeFileSync(binary, 'binary');
  const manifest = metadata(binary, 'x86_64', repo, false);
  assert.throws(() => verify(binary, 'x86_64', repo, false), /ENOENT/);
  writeFileSync(`${binary}.json`, JSON.stringify(manifest));
  verify(binary, 'x86_64', repo, false);
  assert.throws(() => verify(binary, 'aarch64', repo, false), /architecture mismatch/);
  assert.throws(() => verify(binary, 'x86_64', repo, true), /perf mismatch/);
  writeFileSync(binary, 'other binary');
  assert.throws(() => verify(binary, 'x86_64', repo, false), /sha256 mismatch/);
  writeFileSync(binary, 'binary');
  writeFileSync(join(repo, 'crates/protocol.rs'), 'new protocol');
  // A save after the build's final source check cannot certify old bytes as new.
  const captured = metadata(binary, 'x86_64', repo, false, manifest.source);
  assert.equal(captured.source, manifest.source);
  writeFileSync(`${binary}.json`, JSON.stringify(captured));
  assert.throws(() => verify(binary, 'x86_64', repo, false), /source mismatch/);
  writeFileSync(join(repo, 'crates/protocol.rs'), 'crates/protocol.rs');

  // A checkout and the same exported sources have the same identity.
  execFileSync('git', ['init', '-q'], { cwd: repo });
  assert.equal(sourceDigest(repo), manifest.source);
  execFileSync('git', ['add', 'Cargo.lock'], { cwd: repo });
  rmSync(join(repo, 'Cargo.lock'));
  const deleted = sourceDigest(repo);
  assert.notEqual(deleted, manifest.source);
  writeFileSync(join(repo, 'Cargo.lock'), 'Cargo.lock');
  assert.equal(sourceDigest(repo), manifest.source);
  writeFileSync(join(repo, 'apps/host/new.rs'), 'new input');
  assert.notEqual(sourceDigest(repo), manifest.source);
});
