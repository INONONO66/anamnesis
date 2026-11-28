import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { SCHEMA_ID, type RpcRecallResult } from "@anamnesis/protocol";
import neo4j from "neo4j-driver";
import { z } from "zod";
import { RpcClient } from "../../app/anamnesis/client.ts";

const categories = z.enum(["owner_fact", "agent_work", "temporal", "person", "multi_hop"]);
const keySchema = z.object({ source: z.string(), room: z.string(), thread: z.string().nullable(), message: z.string() });
const questionSchema = z.object({
  id: z.string().min(1), category: categories, question: z.string().min(1), gold: z.array(keySchema).min(1),
  lang: z.string().min(1), lexical_overlap: z.number().min(0).max(1),
  session: z.object({ source: z.string(), session: z.string() }).optional(),
});
type Key = z.infer<typeof keySchema>;
const keyId = (key: Key): string => JSON.stringify([key.source, key.room, key.thread, key.message]);
const threadId = (key: Key): string => JSON.stringify([key.source, key.room, key.thread]);
type Position = Readonly<{ key: Key; index: number }>;

export function scoreRanks(gold: readonly Position[], results: readonly (Position | null)[], radius: 0 | 2) {
  // Match each gold at most once. Augmenting paths keep the most gold matched at every prefix,
  // including when a result is within two positions of more than one gold.
  const rankings = results.slice(0, 10);
  const assigned = new Map<number, number>();
  const seenRecords = new Set<string>();
  function match(rank: number, seen: Set<number>): boolean {
    const result = rankings[rank];
    if (!result) return false;
    for (const [index, target] of gold.entries()) {
      if (seen.has(index) || threadId(result.key) !== threadId(target.key) ||
          Math.abs(result.index - target.index) > radius) continue;
      seen.add(index);
      const previous = assigned.get(index);
      if (previous === undefined || match(previous, seen)) {
        assigned.set(index, rank);
        return true;
      }
    }
    return false;
  }
  const ranks: number[] = [];
  for (const [rank, result] of rankings.entries()) {
    if (!result || seenRecords.has(keyId(result.key))) continue;
    seenRecords.add(keyId(result.key));
    if (match(rank, new Set())) ranks.push(rank + 1);
  }
  const dcg = (values: readonly number[]): number => values.reduce((sum, rank) => sum + 1 / Math.log2(rank + 1), 0);
  const first = ranks[0] ?? Infinity;
  return {
    ranks, recallAt1: Number(first <= 1), recallAt5: Number(first <= 5), recallAt10: Number(first <= 10),
    mrrAt10: 1 / first, ndcgAt10: dcg(ranks) / dcg(Array.from({ length: Math.min(10, gold.length) }, (_, i) => i + 1)),
  };
}

async function readPositions(slice: string): Promise<Map<string, Position>> {
  const positions = new Map<string, Position>();
  for (const source of await readdir(slice, { withFileTypes: true })) {
    if (!source.isDirectory()) continue;
    const directory = join(slice, source.name);
    for (const file of await readdir(directory)) {
      if (!file.endsWith(".jsonl") || file === "_rooms.jsonl") continue;
      const indexes = new Map<string, number>();
      for (const line of (await readFile(join(directory, file), "utf8")).split("\n")) {
        if (!line) continue;
        const key = keySchema.parse(JSON.parse(line));
        const id = threadId(key), index = indexes.get(id) ?? 0;
        if (positions.has(keyId(key))) throw new Error(`duplicate slice key: ${keyId(key)}`);
        positions.set(keyId(key), { key, index });
        indexes.set(id, index + 1);
      }
    }
  }
  return positions;
}

const originSchema = z.object({ id: z.string(), source: z.string(), session: z.string(), message: z.string(), properties: z.string() });
const originProperties = z.object({ room: z.string(), thread: z.string().optional() });

type QueryResult = Readonly<{
  id: string; category: z.infer<typeof categories>; latencyMs: number; empty: boolean; scoped: boolean;
  channels: RpcRecallResult["diagnostics"]["channels_used"];
  vectorReason: RpcRecallResult["diagnostics"]["vector_reason"];
  results: readonly Readonly<{ key: Key | null; kind: "Episode" | "Fact"; channels: RpcRecallResult["results"][number]["channels"] }>[];
  strict: ReturnType<typeof scoreRanks>; lenient: ReturnType<typeof scoreRanks>;
}>;

async function main(): Promise<void> {
  const [questionsPath, slice, socket, tokenFile, output, passwordFile, ...rest] = process.argv.slice(2);
  if (!questionsPath || !slice || !socket || !tokenFile || !output || !passwordFile || rest.length) {
    throw new Error("usage: bun scripts/eval/recall-eval.ts <questions.jsonl> <slice-clean> <socket> <token-file> <out-dir> <neo4j-password-file>");
  }
  const questions = (await readFile(questionsPath, "utf8")).split("\n").filter(Boolean)
    .map(line => questionSchema.parse(JSON.parse(line)));
  if (!questions.length || new Set(questions.map(q => q.id)).size !== questions.length) throw new Error("questions must be nonempty with unique ids");
  const positions = await readPositions(slice);
  const queryGold = questions.map(q => q.gold.map(key => {
    const p = positions.get(keyId(key));
    if (!p) throw new Error(`missing gold for ${q.id}: ${keyId(key)}`);
    return p;
  }));
  if (questions.some(q => new Set(q.gold.map(keyId)).size !== q.gold.length)) throw new Error("duplicate gold within a question");
  const driver = neo4j.driver(process.env["ANAMNESIS_EVAL_NEO4J_URI"] ?? "bolt://127.0.0.1:17687",
    neo4j.auth.basic("neo4j", (await readFile(passwordFile, "utf8")).trim()));
  const diagnostics: QueryResult[] = [];
  const episodes = new Map<string, Position | null>();
  try {
    const token = (await readFile(tokenFile, "utf8")).trim();
    {
      for (const [index, q] of questions.entries()) {
        const gold = queryGold[index];
        if (!gold) throw new Error(`missing gold positions: ${q.id}`);
        // One connection per question: the daemon's socket idle deadline does not exempt a request in flight, so one
        // slow recall on a shared connection would close it for every later question.
        const client = await RpcClient.connect(socket, token);
        let latencyMs = 0;
        const start = performance.now();
        const response = await client.request("recall", { query: q.question, limit: 10, ...(q.session && { session: q.session }) })
          .finally(() => { latencyMs = performance.now() - start; return client.close(); });
        // Recall exposes Episode IDs, but no origins. Resolve those IDs in one read-only query
        // on cache misses; the provenance lookup is outside the RPC latency measurement.
        const ids = response.results.map(item => item.kind === "Episode" ? item.id : item.provenance.derived_from[0]?.id);
        const missing = [...new Set(ids.filter((id): id is string => id !== undefined && !episodes.has(id)))];
        if (missing.length) {
          const { records } = await driver.executeQuery(
            "UNWIND $ids AS id MATCH (e:Element:Episode {id:id}) " +
            "RETURN id, e.origin_source AS source, e.origin_session AS session, e.properties AS properties, " +
            "e.origin_record AS message, e.schema AS schema",
            { ids: missing }, { routing: neo4j.routing.READ, database: "neo4j" });
          for (const row of records) {
            const id = z.string().parse(row.get("id"));
            if (row.get("schema") !== SCHEMA_ID.ORIGINAL_MESSAGE) { episodes.set(id, null); continue; }
            const origin = originSchema.parse(row.toObject());
            const { room, thread } = originProperties.parse(JSON.parse(origin.properties));
            if (origin.session !== (thread === undefined ? room : `${room}/${thread}`)) {
              throw new Error(`origin/session mismatch: ${id}`);
            }
            const position = positions.get(keyId({ ...origin, room, thread: thread ?? null }));
            if (!position) throw new Error(`Episode is outside the slice: ${id}`);
            episodes.set(id, position);
          }
          if (missing.some(id => !episodes.has(id))) throw new Error("recall referenced an unknown Episode");
        }
        const ranked = ids.map(id => id === undefined ? null : episodes.get(id) ?? null);
        diagnostics.push({
          id: q.id, category: q.category, latencyMs, empty: ranked.length === 0,
          scoped: q.session !== undefined, channels: response.diagnostics.channels_used, vectorReason: response.diagnostics.vector_reason,
          results: response.results.map((item, i) => ({ key: ranked[i]?.key ?? null, kind: item.kind, channels: item.channels })),
          strict: scoreRanks(gold, ranked, 0), lenient: scoreRanks(gold, ranked, 2),
        });
      }
    }
  } finally { await driver.close(); }
  function summarize(rows: typeof diagnostics) {
    const average = (select: (row: typeof diagnostics[number]) => number): number =>
      rows.reduce((sum, row) => sum + select(row), 0) / rows.length;
    const quality = (mode: "strict" | "lenient") => ({
      recallAt1: average(row => row[mode].recallAt1), recallAt5: average(row => row[mode].recallAt5),
      recallAt10: average(row => row[mode].recallAt10), mrrAt10: average(row => row[mode].mrrAt10),
      ndcgAt10: average(row => row[mode].ndcgAt10),
    });
    const times = rows.map(row => row.latencyMs).sort((a, b) => a - b);
    return { count: rows.length, strict: quality("strict"), lenient: quality("lenient"),
      emptyRate: average(row => Number(row.empty)), latencyP50Ms: times[Math.ceil(times.length * 0.50) - 1],
      latencyP95Ms: times[Math.ceil(times.length * 0.95) - 1] };
  }
  const summary = { overall: summarize(diagnostics), ...Object.fromEntries(categories.options.flatMap(category => {
    const rows = diagnostics.filter(row => row.category === category);
    return rows.length ? [[category, summarize(rows)] as const] : [];
  })) };
  const table = [
    "| Category | Match | N | R@1 | R@5 | R@10 | MRR@10 | nDCG@10 | Empty | p50 ms | p95 ms |",
    "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
  ];
  for (const [category, row] of Object.entries(summary)) for (const mode of ["strict", "lenient"] as const) {
    const q = row[mode];
    table.push(`| ${category} | ${mode} | ${row.count} | ${[
      q.recallAt1, q.recallAt5, q.recallAt10, q.mrrAt10, q.ndcgAt10, row.emptyRate,
    ].map(n => n.toFixed(4)).join(" | ")} | ${row.latencyP50Ms?.toFixed(1)} | ${row.latencyP95Ms?.toFixed(1)} |`);
  }
  await mkdir(output, { recursive: true, mode: 0o700 });
  await writeFile(join(output, "report.json"), `${JSON.stringify({ summary, diagnostics }, null, 2)}\n`, { mode: 0o600 });
  await writeFile(join(output, "report.md"), `# Recall evaluation\n\n${table.join("\n")}\n`, { mode: 0o600 });
}

if (import.meta.main) await main();
