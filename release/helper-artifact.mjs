// Identity of a compatible Linux helper, shared by its builder and packagers.
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const inputs = ['Cargo.lock', 'Cargo.toml', 'rust-toolchain.toml', 'rustfmt.toml',
  'apps/host', 'crates', 'release/linux/build-compatible-host.sh', 'release/helper-artifact.mjs'];
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

export function sourceDigest(repo = root) {
  let files;
  if (existsSync(join(repo, '.git'))) {
    files = execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard', '-z', '--', ...inputs],
      { cwd: repo, encoding: 'utf8' }).split('\0').filter(Boolean);
  } else {
    const walk = (path) => statSync(join(repo, path)).isDirectory()
      ? readdirSync(join(repo, path)).filter((name) => !['target', 'tmp'].includes(name)).flatMap((name) => walk(`${path}/${name}`))
      : [path];
    files = inputs.flatMap(walk);
  }
  const hash = createHash('sha256');
  for (const path of [...new Set(files)].sort()) {
    if (!existsSync(join(repo, path)) || !statSync(join(repo, path)).isFile()) continue;
    hash.update(path).update('\0').update(sha256(readFileSync(join(repo, path)))).update('\0');
  }
  return hash.digest('hex');
}

export function metadata(binary, architecture, repo = root, perf = process.env.MUXFLOW_PERF_BUILD === '1', source = sourceDigest(repo)) {
  if (!['x86_64', 'aarch64'].includes(architecture)) throw new Error(`unsupported helper architecture: ${architecture}`);
  if (!/^[a-f0-9]{64}$/.test(source)) throw new Error('invalid helper source digest');
  return { source, architecture, perf, sha256: sha256(readFileSync(binary)) };
}

export function verify(binary, architecture, repo = root, perf) {
  const expected = metadata(binary, architecture, repo, perf);
  const actual = JSON.parse(readFileSync(`${binary}.json`, 'utf8'));
  for (const key of Object.keys(expected)) {
    if (actual[key] !== expected[key]) throw new Error(`Linux helper ${binary}: ${key} mismatch; rebuild with release/linux/build-compatible-host.sh`);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [command, binary, architecture, source] = process.argv.slice(2);
  try {
    if (command === 'digest') console.log(sourceDigest());
    else if (command === 'write') {
      if (!source) throw new Error('write requires the source digest captured before building');
      writeFileSync(`${binary}.json`, `${JSON.stringify(metadata(binary, architecture, root, undefined, source))}\n`);
    }
    else if (command === 'verify') verify(binary, architecture);
    else throw new Error('usage: helper-artifact.mjs digest | write|verify BINARY ARCH');
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
