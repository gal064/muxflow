// Pinned upstream notices for crates that omit them from their published archive.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

export function upstreamNotices(label, directory) {
  const manifest = JSON.parse(readFileSync(join(directory, 'license-notices.json'), 'utf8'));
  const entry = manifest[label];
  if (!entry) throw new Error(`missing pinned upstream notice for ${label}`);
  return entry.files.map(({ file, source, sha256 }) => {
    const bytes = readFileSync(join(directory, 'licenses', file));
    if (createHash('sha256').update(bytes).digest('hex') !== sha256) {
      throw new Error(`upstream notice checksum mismatch: ${file}`);
    }
    return { name: label, file: `${file}\nSource: ${source}`, text: bytes.toString('utf8') };
  });
}
