import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import neo4j from 'neo4j-driver';
import { Engine } from '../../packages/core/src/engine.ts';
import { exportFixedCsr, solveFixedCsr } from '../../packages/core/src/dynamics/ppr.ts';
const uuid = () => `01900000-0000-7000-8000-${randomBytes(6).toString('hex')}`;
const hash = v => createHash('sha256').update(v).digest('hex');
const context = { principal: 'installation', commit_mode: 'receipt' };
const g = state => ({ id: uuid(), stream: 'extraction', incarnation: hash('g004-real'), state, covered_ingest_seq: 0, created_at: 1, updated_at: 1 });

test('owned Neo4j audit coverage refuses activation and fixed CSR/PPR remains available', async () => {
  const root = await mkdtemp('/tmp/g004-graph-real-');
  const options = { uri: process.env.ANAMNESIS_TEST_NEO4J_URI, password: process.env.ANAMNESIS_TEST_NEO4J_PASSWORD };
  assert.ok(options.uri && options.password, 'owned Neo4j credentials required');
  const driver = neo4j.driver(options.uri, neo4j.auth.basic('neo4j', options.password), { disableLosslessIntegers: true });
  const engine = new Engine({ ...options, objectsRoot: root });
  const query = async (cypher, params = {}) => (await driver.executeQuery(cypher, params)).records.map(r => r.toObject());
  try {
    await query('MATCH (n) DETACH DELETE n'); await engine.init(); await engine.claimWriterEpoch();
    const first = g('catching_up'), second = g('catching_up');
    await engine.store.createExtractionGeneration(first, context); await engine.store.createExtractionGeneration(second, context);
    const seedCoverage = async id => { for (const partition of ['episodes', 'active_extraction']) await query('CREATE (:ExtractionCoverage {key:$key,generation_id:$id,body:$body,covered_ingest_seq:0})', { key: `${id}:${partition}`, id, body: JSON.stringify({ generation_id:id, partition, required_ingest_seq:0, covered_ingest_seq:0, omission_digest:hash(''), updated_at:1 }) }); };
    await seedCoverage(first.id); await seedCoverage(second.id);
    const before = await query('MATCH (n) RETURN elementId(n) AS id,properties(n) AS properties ORDER BY id');
    await assert.rejects(engine.store.cutoverExtractionGeneration({generation_id:first.id,expected_generation_id:null,expected_selector_version:0},context),{code:'activation_prerequisite_unavailable'});
    assert.deepEqual(await query('MATCH (n) RETURN elementId(n) AS id,properties(n) AS properties ORDER BY id'),before);
    await assert.rejects(engine.store.readExtractionCoverage(first.id, context), /generation_not_selected/);
    await assert.rejects(engine.store.rollbackExtractionGeneration({generation_id:first.id,expected_generation_id:null,expected_selector_version:0},context), /rollback_requires_retired/);
    const csr = exportFixedCsr({ nodes: ['a','b','c'], arcs: [{from:'a',to:'b',role:'X',id:'2'},{from:'a',to:'c',role:'X',id:'1'}] });
    const ppr = solveFixedCsr(csr, new Map([['a', 1]])); assert.equal(csr.offsets.length, 4); assert.equal(ppr.values.length, 3); assert.ok(ppr.mass > 0);
    console.log(JSON.stringify({ checkpoint:'generation-activation-fail-closed-csr-ppr', selected:null, activation_supported:false, csr, ppr:{ mass:ppr.mass, iterations:ppr.iterations, residualL1:ppr.residualL1 } }));
  } finally { await driver.close(); await engine.close(); await rm(root, { recursive:true, force:true }); }
});
