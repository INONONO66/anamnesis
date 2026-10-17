import { expect, test } from "bun:test";
import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { canonicalExtractionBody, extractionBodyDigest, type Generation, type SemanticClaim, type SemanticReviewProvider } from "@anamnesis/protocol";
import { Engine } from "../../packages/core/src/engine.ts";
import type { ExtractionProviderInput } from "../../packages/core/src/extraction.ts";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const OUT = process.env["ANAMNESIS_G10_SURFACE_OUT"]
  ?? "/Users/ino/Develop/anamnesis/.omo/evidence/ulw/01a11b8c-9f0b-71f3-ac0c-cb1048614a4e/G005-g10-projection/surface";
const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const now = Date.UTC(2030, 0, 1);
// Same fixture as packages/core/src/projection-store.test.ts; that file seeds at module scope, so importing it would re-run its tests here.
const fixtures = [
  ["Alice", "Alice moved to Lisbon.", "2026-09-01T00:00:00Z"], ["Alice", "Alice adopted a cat.", "2026-09-02T00:00:00Z"],
  ["Alice", "Alice moved to Porto.", "2026-09-03T00:00:00Z"], ["Bob", "Bob plays the cello.", "2026-09-04T00:00:00Z"],
  ["Bob", "Bob runs marathons.", "2026-09-05T00:00:00Z"], ["Carol", "Carol writes Rust.", "2026-09-06T00:00:00Z"],
  ["Carol", "Carol keeps bees.", "2026-09-07T00:00:00Z"],
] as const;

/** Seeds 3 Entities / 7 Facts through an in-process Engine, invalidates Fact 0 by Fact 2, then closes so the daemon owns the writer epoch. */
async function seed(uri: string, password: string, objectsRoot: string, admin: neo4j.Driver) {
  const extractionProvider = {
    model: "projection-claim-fixture", modelIncarnation: extractionBodyDigest("projection-claim-fixture"),
    async extract(input: ExtractionProviderInput) {
      if (input.task === "claim") return { task: "claim", language: "en", modality: "text",
        claims: [{ text: input.text, evidence: { start: 0, end: Buffer.byteLength(input.text), text: input.text } }] };
      if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected task ${input.task}`);
      return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context.body_digest,
        decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
    },
  };
  const review: SemanticReviewProvider = {
    profileId: extractionBodyDigest("projection-review-fixture"),
    async resolve() { return { entity_resolutions: [], attribution_speakers: [], allow_no_single_locus: false, content_language: "en" }; },
    async review(input) { return { disposition: "retain", semantic_claim: input.premises.request.semantic_claim, reason: "fixture retained" }; },
  };
  const engine = new Engine({ uri, password, objectsRoot, extractionProvider, semanticReviewProvider: review, clock: () => now });
  const generation: Generation = { id: uuidv7(), stream: "extraction", incarnation: extractionProvider.modelIncarnation,
    state: "catching_up", covered_ingest_seq: 0, created_at: now, updated_at: now };
  const entities: Record<string, string> = { Alice: uuidv7(), Bob: uuidv7(), Carol: uuidv7() };
  const sources: string[] = [], facts: string[] = [];
  try {
    await engine.init(); await engine.claimWriterEpoch();
    await engine.store.createExtractionGeneration(generation, context);
    for (const [index, [, content, value]] of fixtures.entries()) {
      const episode = await engine.remember({ content, time: { value, precision: "second" },
        origin: { source: "g10-surface", session: "projection", actor: "user", record: String(index) } },
      { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
      sources.push(episode.id);
      const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: episode.id }, context);
      const audit = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "qa", lease_ms: 30000 }, context);
      assert(audit.state === "known" && audit.judge_attempt?.state === "succeeded" && !audit.semantic_writes);
      const source = await engine.store.semanticEpisode(episode.id, context);
      const semantic: SemanticClaim = {
        content, content_language: "en", sub_kind: "fact", modality: "asserted", confidence: 0.7,
        time: { time_value: source.time.time_value, time_utc: source.time.time_utc, time_precision: "inherited",
          resolution: "inherited", anchor_time_utc: source.time.time_utc },
        entities: [], subject_keys: null, predicate_text: content, scope_complete: false,
        scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] },
        evidence: { kind: "source_locus", span: { start: 0, end: Buffer.byteLength(content) } },
      };
      const request = { proposal_id: uuidv7(), generation_id: generation.id, source_episode_id: episode.id,
        judge_attempt_id: audit.judge_attempt.id, claim_index: 0, semantic_claim: semantic };
      await engine.proposeRetainedClaim(request, context);
      await engine.reviewRetainedClaim({ review_id: uuidv7(), proposal_id: request.proposal_id, action: "accept", reason: "fixture accepted" }, context);
      facts.push((await engine.materializeRetainedClaim({ ...request, operation_id: uuidv7() }, context)).fact_id);
    }
    await admin.executeQuery(`UNWIND $rows AS row MERGE (e:Element:Entity {id:row.entity})
      ON CREATE SET e.content=row.name,e.generation=$generation,e.schema='anamnesis.entity/1',e.properties='{"entity_kind":"person"}'
      WITH e,row MATCH (f:Element:Fact {id:row.fact}) CREATE (f)-[:MENTIONS]->(e)`,
    { generation: generation.id, rows: fixtures.map(([name], index) => ({ entity: entities[name], name, fact: facts[index] })) });
    await admin.executeQuery(`MATCH (g:ExtractionGeneration {id:$id}),(s:Meta {key:'extraction_selector'})
      SET g.state='active',g.body=$body,s.generation_id=$id,s.selector_version=1`,
    { id: generation.id, body: canonicalExtractionBody({ ...generation, state: "active" }) });
    await engine.link({ id: uuidv7(), from: facts[2]!, to: facts[0]!, role: "INVALIDATES", content: "Alice moved again" });
  } finally { await engine.close(); }
  return { entities, sources, facts };
}

async function daemon(env: NodeJS.ProcessEnv) {
  const child = spawn("node", ["dist/anamnesis-daemon.mjs"], { cwd: ROOT, stdio: ["ignore", "pipe", "pipe"], env });
  const exited = once(child, "exit");
  child.stderr.on("data", chunk => process.stderr.write(chunk));
  const lines = createInterface({ input: child.stdout });
  const listening = new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(Error("listening deadline")), 20000);
    lines.on("line", line => { if (JSON.parse(line).event === "listening") { clearTimeout(timer); resolve(); } });
  });
  await Promise.race([listening, exited.then(([code]) => { throw Error(`daemon exit ${code}`); })]);
  return { async stop() { if (child.exitCode === null && child.signalCode === null) { child.kill("SIGTERM"); await exited; } lines.close(); } };
}

test("ops projection write renders the valid Facts, rewrites identical bytes and drops a denied Episode's Facts on the real daemon", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
  if (!uri || !password) throw new Error("isolated graph credentials required");
  await mkdir(OUT, { recursive: true });
  const admin = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const root = await mkdtemp("/tmp/g10-projection-runtime-"), objects = await mkdtemp("/tmp/g10-projection-objects-"), token = randomUUID();
  const dirs = [1, 2, 3].map(n => `${root}/out-${n}`);
  const env = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: token, ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_PASSWORD: password };
  const write = (name: string, body: unknown) => writeFile(`${OUT}/${name}.json`, JSON.stringify(body, null, 2) + "\n");
  const ops = async (dir: string) => {
    const run = await promisify(execFile)(process.execPath, ["app/anamnesis/ops.ts", "projection", "write", dir], { cwd: ROOT, env });
    return { exit: 0, stdout: run.stdout, report: JSON.parse(run.stdout.trim().split("\n").at(-1)!) };
  };
  const contents = async (dir: string) => Object.fromEntries(await Promise.all((await readdir(dir)).sort().map(async name => [name, await readFile(`${dir}/${name}`, "utf8")] as const)));
  let running: Awaited<ReturnType<typeof daemon>> | undefined;
  let client: { request(method: string, params: unknown): Promise<any>; close(): Promise<void> } | undefined;
  try {
    await admin.executeQuery("MATCH (n) DETACH DELETE n");
    const { sources, facts } = await seed(uri, password, objects, admin);
    running = await daemon(env);

    const one = await ops(dirs[0]!), first = await contents(dirs[0]!);
    const bodies = Object.values(first).join("");
    const factLines = bodies.split("\n").filter(line => line.startsWith("- ")).length, invalidatedHits = bodies.split(facts[0]!).length - 1;
    await write("01-write", { stdout: one.stdout, report: one.report, files: Object.keys(first), file_count: Object.keys(first).length,
      fact_lines: factLines, invalidated_fact_id: facts[0], invalidated_grep_count: invalidatedHits });
    expect(one.report).toEqual({ entities: 3, facts: 6, written: 3, removed: 0, dir: dirs[0] });
    expect(Object.keys(first)).toHaveLength(3);
    expect(factLines).toBe(6);
    expect(invalidatedHits).toBe(0);

    const two = await ops(dirs[1]!);
    const diff = await promisify(execFile)("diff", ["-r", dirs[0]!, dirs[1]!]).then(run => ({ code: 0, stdout: run.stdout }), (error: { code: number; stdout: string }) => ({ code: error.code, stdout: error.stdout }));
    await write("02-idempotent", { report: two.report, diff_exit: diff.code, diff_output: diff.stdout });
    expect(two.exit).toBe(0);
    expect(diff).toEqual({ code: 0, stdout: "" });

    const RpcClient = (await import(`${ROOT}dist/anamnesis-client.mjs`)).RpcClient;
    client = await RpcClient.connect(`${root}/anamnesis.sock`, token, "receipt");
    const denied = sources[4]!, deniedFacts = [facts[4]!];
    const policy = await client!.request("policy.set", { policy_id: uuidv7(), selector: { episode_id: denied }, scope: "content" });
    const three = await ops(dirs[2]!), third = await contents(dirs[2]!);
    const bob = Object.keys(first).find(name => name.startsWith("bob-"))!;
    const absent = deniedFacts.filter(id => !Object.values(third).join("").includes(id));
    await write("03-policy-deny", { denied_episode_id: denied, denied_fact_ids: deniedFacts, policy, report: three.report,
      affected_file: bob, before: first[bob], after: third[bob], ids_absent_after: absent });
    expect(first[bob]).toContain(deniedFacts[0]!);
    expect(three.report).toEqual({ entities: 3, facts: 5, written: 3, removed: 0, dir: dirs[2] });
    expect(absent).toEqual(deniedFacts);
  } finally {
    await client?.close();
    await running?.stop();
    await admin.close();
    await rm(root, { recursive: true, force: true }); await rm(objects, { recursive: true, force: true });
  }
}, 180000);
