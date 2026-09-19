import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
export const sha = value => createHash('sha256').update(value).digest('hex');
export async function install(root, code) {
  code ??= await readFile('app/anamnesis/tokenizer.fixture.cjs');
  const path = join(root, 'encoder.cjs'), assetPath = join(root, 'vocabulary.json');
  const vocabulary = await readFile('app/anamnesis/tokenizer-vocabulary.fixture.json');
  await writeFile(path, code); await writeFile(assetPath, vocabulary);
  return { id: 'synthetic-byte-merge-v1@sha256:' + sha(JSON.stringify(['anamnesis-tokenizer-v1', sha(code), [['vocabulary', sha(vocabulary)]]])),
    path: resolve(path), assets: [{ name: 'vocabulary', path: resolve(assetPath) }] };
}
