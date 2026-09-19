// Run each existing real surface sequentially: each owns its daemon and
// event-gated UDS client, while the outer runner owns and clears Neo4j.
// This is a direct executable, not a node:test file or a nested test runner.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { join } from 'node:path';
const evidence = process.env.G003_V01_EVIDENCE;
assert.ok(evidence, 'current-built acceptance workspace required');
const env = { ...process.env, G003_PUBLICATION_BUNDLE: join(evidence, 'fixture.mjs'),
  TOKENIZER_EVIDENCE: evidence, TOKENIZER_PHASE: 'green', TOKENIZER_COLLISIONS: '0' };
const surfaces = [
  ['app/anamnesis/g003-publication.surface.mjs'],
  ['app/anamnesis/embedding-recall.surface.mjs'],
  ['app/anamnesis/tokenizer.surface.mjs'],
];
for (const args of surfaces) {
  const child = spawn(process.execPath, args, { cwd: evidence, env, stdio: ['ignore', 'pipe', 'pipe'] });
  const closed = once(child, 'close');
  child.stdout.pipe(process.stdout); child.stderr.pipe(process.stderr);
  const [code, signal] = await closed;
  assert.equal(code, 0, `integrated surface failed: ${args.join(' ')} signal=${signal}`);
}
console.log(JSON.stringify({ checkpoint: 'g003-v01-surfaces', surfaces: surfaces.map(args => args.join(' ')), event_gated: true, real_node_uds: true, concurrency: 1 }));
