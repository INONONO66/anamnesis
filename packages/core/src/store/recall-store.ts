import { EPISODE_SCHEMAS, SCHEMA_ID } from "@anamnesis/protocol";
import neo4j, { type ManagedTransaction } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { z } from "zod";
import { solvePpr } from "../dynamics/ppr.ts";
import { normalizedRrf } from "../dynamics/ranking.ts";
import { initialStability, retention } from "../dynamics/retention.ts";
import { RpcRecallParams, RpcRecallResult, type RpcRecallItem } from "@anamnesis/protocol";
import { EmbeddingError, embeddingProfileId, validateVector } from "../embedding.ts";
import { admittedBudget, packRecall, canonicalContext, type RecallBundle } from "../recall.ts";
import { sha256, tupleHash } from "./digest.ts";
import { luceneQuery, receiptTime, IssueReceiptInput, ReceiptError } from "./receipts.ts";
import { type InstallationContext, PolicyEvent, policySelector, type PolicyState, requireInstallation } from "./policy.ts";
import { type ElementNode, nodeProps, toElement } from "./records.ts";
import type { StoreCore } from "./core.ts";
import type { ExtractionStore } from "./extraction-store.ts";
import type { ReceiptStore } from "./receipt-store.ts";

type RecallLists = Record<string, { id: string }[]>;
interface RecallScope { T: string; denies: ReturnType<typeof policySelector>[]; schemas: string[] }

/** An Episode is servable when it is visible at T, no active deny selects it, and no effective INVALIDATES targets it. */
const RECALL_ALLOWED = `e.schema IN $schemas AND e.time_utc <= $T
        AND NONE(d IN $denies WHERE (d.episode_id IS NULL OR d.episode_id=e.id) AND (d.source IS NULL OR d.source=e.origin_source))
        AND NOT EXISTS { MATCH ()-[inv:INVALIDATES]->() WHERE inv.target_id=e.id AND inv.effective_time_utc <= $T AND inv.id IS NOT NULL }`;

/** Original Episodes remain candidates through identity, BM25 and session. */
async function episodeChannelsTx(tx: ManagedTransaction, scope: RecallScope, request: RpcRecallParams) {
  const lists: RecallLists = {}, nodes = new Map<string, ElementNode>();
  const channel = async (name: string, query: string, params: Record<string, unknown>) => {
    const rows = await tx.run<{ e: ElementNode }>(query, { ...scope, ...params });
    lists[name] = rows.records.map(row => { const e = row.get("e"); const id = String(e.properties["id"]); nodes.set(id, e); return { id }; });
  };
  await channel("identity", `MATCH (e:Element:Episode {id:$id}) WHERE ${RECALL_ALLOWED} RETURN e LIMIT 1`, { id: request.query });
  const q = luceneQuery(request.query);
  if (q) await channel("bm25", `CALL db.index.fulltext.queryNodes('element_content',$q,{limit:256}) YIELD node,score
    WITH node AS e,score WHERE e:Episode AND ${RECALL_ALLOWED} RETURN e ORDER BY score DESC,e.id ASC LIMIT 64`, { q });
  if (request.session) await channel("session", `MATCH (e:Episode {session_key:$session}) WHERE ${RECALL_ALLOWED}
    RETURN e ORDER BY e.time_utc DESC,e.ingest_seq DESC,e.id ASC LIMIT 32`, { session: tupleHash([request.session.source, request.session.session]) });
  return { lists, nodes };
}

/** Every candidate Episode as a ranked item: retention-decayed mass, RRF relevance across channels and cached utility. */
async function rankedEpisodesTx(tx: ManagedTransaction, nodes: Map<string, ElementNode>, lists: RecallLists, now: number): Promise<RpcRecallItem[]> {
  const cacheRows = await tx.run<{ id: string; s: number | null; last: number | null; utility: number | null }>(
    `UNWIND $ids AS id OPTIONAL MATCH (c:HitCache {episode_id:id})
     RETURN id,c.s AS s,c.t_last_hit AS last,c.utility AS utility`, { ids: [...nodes.keys()] });
  const caches = new Map(cacheRows.records.map(row => [row.get("id"), row]));
  const ranked: RpcRecallItem[] = [];
  for (const [id, node] of nodes) {
    const props = nodeProps(node), e = toElement(props), cache = caches.get(id)!;
    const mass = e.mass * retention(Math.max(0, now - (cache.get("last") ?? Number(props["ingested_at"]))) / 86400000, cache.get("s") ?? initialStability(e.mass));
    const relevance = normalizedRrf(lists, id), utility = cache.get("utility") ?? 0;
    const item = { id, kind: "Episode", schema: e.schema, epistemic: "observed", content: e.content, time: e.time,
      mass, utility, relevance, score: relevance * Math.sqrt(Math.max(mass, 0.02)) * (1 + 0.25 * utility), sources: [id],
      provenance: { derived_from: [{ id, kind: "Episode", visible_at_T: true }], supersedes: [], supersedes_redacted: false, contrasts: [], warnings: [] },
      channels: Object.keys(lists).filter(name => lists[name]!.some(candidate => candidate.id === id)) };
    ranked.push(RpcRecallResult.shape.results.element.parse(item));
  }
  return ranked;
}

/** Same-session context is closest-first on the wire, farthest-first when the packer must drop it. */
async function windowTx(tx: ManagedTransaction, primary: RpcRecallItem, scope: RecallScope, window: number, now: number): Promise<RpcRecallItem[]> {
  if (primary.kind !== "Fact" || window === 0) return [];
  const nodes = new Map<string, ElementNode>();
  const side = async (comparison: "<" | ">", order: "DESC" | "ASC") => {
    const rows = await tx.run<{ e: ElementNode }>(`MATCH (s:Element:Episode {id:$source})
      MATCH (e:Element:Episode {session_key:s.session_key})
      WHERE e.ingest_seq ${comparison} s.ingest_seq AND ${RECALL_ALLOWED}
      RETURN e ORDER BY e.ingest_seq ${order},e.id ASC LIMIT $window`,
      { ...scope, source: primary.window.source, window: neo4j.int(window) });
    return rows.records.map(row => {
      const node = row.get("e"), id = String(node.properties["id"]);
      nodes.set(id, node); return id;
    });
  };
  const before = await side("<", "DESC"), after = await side(">", "ASC");
  primary.window = { source: primary.window.source, before, after };
  const items = new Map((await rankedEpisodesTx(tx, nodes, {}, now)).map(item => [item.id, item]));
  const optional: RpcRecallItem[] = [];
  for (let distance = window - 1; distance >= 0; distance--) {
    for (const id of [after[distance], before[distance]]) {
      const item = id === undefined ? undefined : items.get(id);
      if (item) optional.push(item);
    }
  }
  return optional;
}

/** BM25 and vector hits share source resolution and ranking in the selected generation. */
async function derivedCandidatesTx(tx: ManagedTransaction, query: string, generation: string, T: string, lists: RecallLists,
  queryVector: number[] | null, profileId: string | null): Promise<Map<string, ElementNode>> {
  const derivedNodes = new Map<string, ElementNode>(), q = luceneQuery(query);
  const channel = async (name: string, cypher: string, params: Record<string, unknown>) => {
    const rows = await tx.run<{ f: ElementNode }>(cypher, { generation, T, ...params });
    const list = lists[name] ?? (lists[name] = []);
    for (const row of rows.records) {
      const fact = row.get("f"), id = String(fact.properties["id"]);
      derivedNodes.set(id, fact); list.push({ id });
    }
  };
  if (q) await channel("bm25", `CALL db.index.fulltext.queryNodes('element_content',$q,{limit:256}) YIELD node,score
    WITH node AS f,score WHERE f:Fact AND f.generation=$generation AND f.time_utc <= $T
    RETURN f ORDER BY score DESC,f.id ASC LIMIT 64`, { q });
  // Non-Episode Elements have no revision_key; embedding pins their canonical digest instead.
  if (queryVector && profileId) await channel("vector", `CALL db.index.vector.queryNodes($index,256,$vector) YIELD node,score
    MATCH (f:Element:Fact {id:node.fact_id})
    WHERE node.profile_id=$profile AND node.input_revision=coalesce(f.revision_key,f.digest)
      AND node.input_digest=f.digest AND f.generation=$generation AND f.time_utc <= $T
    RETURN f ORDER BY score DESC,f.id ASC LIMIT 64`,
    { index: `vec_fact_${profileId}`, vector: queryVector, profile: profileId });
  return derivedNodes;
}

/** Personalized PageRank over the generation's conducting arcs around every candidate; null when nothing seeds it. */
async function pprScoresTx(tx: ManagedTransaction, generation: string, lists: RecallLists, derivedNodes: Map<string, ElementNode>): Promise<Map<string, number> | null> {
  const seeds = [...new Set(Object.values(lists).flat().map(hit => hit.id).concat([...derivedNodes.keys()]))];
  if (seeds.length === 0) return null;
  const graph = await tx.run(`MATCH (a:ConductingArc)
    WHERE a.generation=$generation AND (a.source_id IN $seeds OR a.peer_id IN $seeds)
    RETURN a.source_id AS source,a.peer_id AS peer,a.role AS role,a.link_id AS id LIMIT 1024`, { generation, seeds });
  const arcs = graph.records.map(record => ({ from: record.get("source"), to: record.get("peer"), role: record.get("role"), id: record.get("id") }));
  const graphNodes = [...new Set(seeds.concat(arcs.flatMap(arc => [arc.from, arc.to])))];
  const solved = solvePpr({ nodes: graphNodes, arcs, seeds: new Map(seeds.map(id => [id, 1])) });
  const sorted = [...graphNodes].sort();
  return new Map(sorted.map((id, index) => [id, solved.values[index]!]));
}

/** The bounded prior-revision view of a primary: up to eight superseded Episodes, a withheld one or an overflow flagged by warning. */
async function supersedesTx(tx: ManagedTransaction, primary: RpcRecallItem, denies: PolicyEvent[]): Promise<void> {
  const supersedes = await tx.run<{ e: ElementNode }>(`MATCH (:Element:Episode {id:$id})-[:INVALIDATES]->(e:Element:Episode)
    RETURN DISTINCT e ORDER BY e.id ASC LIMIT 9`, { id: primary.id });
  for (const row of supersedes.records.slice(0, 8)) {
    const old = toElement(nodeProps(row.get("e")));
    const denied = denies.some(deny => (deny.selector.episode_id === undefined || deny.selector.episode_id === old.id)
      && (deny.selector.source === undefined || deny.selector.source === old.origin.source));
    if (denied) primary.provenance.supersedes_redacted = true;
    else primary.provenance.supersedes.push({ id: old.id, content: old.content });
  }
  if (primary.provenance.supersedes_redacted) primary.provenance.warnings.push({ code: "supersedes_withheld", content: "Prior revision content is withheld by current policy." });
  if (supersedes.records.length > 8) primary.provenance.warnings.push({ code: "supersedes_incomplete", content: "Prior revision provenance exceeds the bounded eight-entry view." });
}

export class RecallStore {
  constructor(private readonly core: StoreCore, private readonly extraction: ExtractionStore, private readonly receipts: ReceiptStore) {}

  /** Derived Facts as ranked items: each is rechecked against policy through its source Episode, its contrast peers
   * become companions, and every served Fact joins `ranked`. */
  private async factItemsTx(tx: ManagedTransaction, policy: PolicyState, generation: string, scope: RecallScope, derivedNodes: Map<string, ElementNode>,
    pprScores: Map<string, number>, ranked: RpcRecallItem[], lists: RecallLists): Promise<Map<string, RpcRecallItem>> {
    const factItems = new Map<string, RpcRecallItem>();
    const factItem = async (node: ElementNode): Promise<RpcRecallItem | null> => {
      const id = String(node.properties["id"]), cached = factItems.get(id);
      if (cached) return cached;
      const sourceRows = await tx.run(`MATCH (f:Fact {id:$id})-[:DERIVED_FROM]->(e:Element:Episode)
        WHERE ${RECALL_ALLOWED} RETURN e.id AS source ORDER BY e.id LIMIT 2`, { ...scope, id });
      const sourceId = sourceRows.records[0]?.get("source");
      if (typeof sourceId !== "string") return null;
      try { await this.core.authorizeEpisodesTx(tx, [z.uuidv7().parse(sourceId)], policy); }
      catch (error) { if (error instanceof ReceiptError && error.code === "policy_denied") return null; throw error; }
      const fact = toElement(nodeProps(node)), ppr = pprScores.get(id) ?? 0;
      const item = RpcRecallResult.shape.results.element.parse({ id, kind: "Fact", schema: SCHEMA_ID.CLAIM, epistemic: "derived",
        content: fact.content, time: fact.time!, mass: Math.max(0, Math.min(1, ppr || fact.mass)), utility: 0,
        relevance: Math.max(0, ppr), score: Math.max(0, ppr || fact.mass), sources: [z.uuidv7().parse(sourceId)],
        window: { source: sourceId, before: [], after: [] },
        provenance: { derived_from: [{ id: z.uuidv7().parse(sourceId), kind: "Episode", visible_at_T: true }], supersedes: [], supersedes_redacted: false,
          contrasts: [], warnings: [] }, channels: Object.keys(lists).filter(name => lists[name]!.some(candidate => candidate.id === id)) });
      factItems.set(id, item); return item;
    };
    for (const [id, node] of derivedNodes) {
      const item = await factItem(node); if (!item) continue;
      const peers = await tx.run<{ other: ElementNode }>(`MATCH (f:Fact {id:$id})-[:CONTRASTS]-(other:Fact {generation:$generation})
        WHERE other.time_utc <= $T RETURN DISTINCT other ORDER BY other.id LIMIT 4`, { id, generation, T: scope.T });
      for (const row of peers.records) {
        const peer = await factItem(row.get("other"));
        if (peer) item.provenance.contrasts.push(peer.id);
      }
      ranked.push(item);
    }
    return factItems;
  }
  /** Candidate reads, policy revalidation, source resolution, packing and receipt
   * issuance share the Meta write barrier; policy commands acknowledge after
   * the daemon writes the response. */
  async recall(input: z.input<typeof RpcRecallParams>, context: InstallationContext): Promise<RpcRecallResult> {
    requireInstallation(context);
    const request = RpcRecallParams.parse(input), budget = admittedBudget(request.budget, this.core.tokenizers, this.core.recallDefaultBytes);
    const now = receiptTime.parse(this.core.clock()), T = request.T ?? now, recallId = uuidv7();
    const provider = this.core.embeddingProvider, profileId = provider ? embeddingProfileId(provider.profile) : null;
    let queryVector: number[] | null = null;
    let vectorReason: RpcRecallResult["diagnostics"]["vector_reason"] = provider ? "not_requested" : "not_configured";
    if (provider && request.limit > 0 && budget.limit > 0) {
      try { queryVector = validateVector(await provider.embed(request.query, "query"), provider.profile); vectorReason = "available"; }
      catch (error) { if (!(error instanceof EmbeddingError)) throw error; vectorReason = error.reason; }
    }
    return this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      const selection = await this.extraction.extractionSelectionTx(tx);
      const denies = [...policy.denies.values()].filter(deny => !policy.revoked.has(deny.policy_id));
      // AND semantics stay identical to feedback authority, including policies
      // that specify both source and Episode ID.
      const scope: RecallScope = { T: new Date(T).toISOString(), denies: denies.map(deny => policySelector(deny.selector)), schemas: [...EPISODE_SCHEMAS] };
      const serving = request.limit > 0 && budget.limit > 0;
      const { lists, nodes } = serving ? await episodeChannelsTx(tx, scope, request) : { lists: {}, nodes: new Map<string, ElementNode>() };
      const ranked = await rankedEpisodesTx(tx, nodes, lists, now);
      const derivedNodes = selection.generation_id && serving
        ? await derivedCandidatesTx(tx, request.query, selection.generation_id, scope.T, lists, queryVector, profileId) : new Map<string, ElementNode>();
      const pprScores = selection.generation_id ? await pprScoresTx(tx, selection.generation_id, lists, derivedNodes) : null;
      // Derived serving is strictly generation-selected and policy-authorized.
      // Every source Episode is rechecked for primaries AND their mandatory peers.
      const factItems = selection.generation_id && serving
        ? await this.factItemsTx(tx, policy, selection.generation_id, scope, derivedNodes, pprScores ?? new Map(), ranked, lists) : new Map<string, RpcRecallItem>();
      // Originals are retained in `nodes`; Facts are served from `derivedNodes`.
      ranked.sort((a, b) => b.score - a.score || b.relevance - a.relevance || b.mass - a.mass || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
      const bundles: RecallBundle[] = [];
      for (const candidate of ranked) {
        const primary = { ...candidate };
        await supersedesTx(tx, primary, denies);
        const optional = await windowTx(tx, primary, scope, request.window, now);
        bundles.push({ primary, companions: primary.provenance.contrasts.map(id => factItems.get(id)!), optional });
      }
      const result = RpcRecallResult.parse(packRecall(bundles, {
        recall_id: recallId, expires_at: now + 3600000, results: [], companions: [], entities: [], context_text: "", used_budget: 0,
        budget, renderer: "canonical-jsonl-v1", diagnostics: { pipeline: "derived-hybrid-v1", now, T,
          policy_revision: policy.policy_revision, channels_used: Object.keys(lists).filter(name => lists[name]!.length > 0) as RpcRecallResult["diagnostics"]["channels_used"],
          vector_reason: vectorReason, embedding_profile_id: profileId, candidate_count: ranked.length, skipped_bundles: 0,
          ppr_used: pprScores !== null, identity_mode: "exact_episode_id" },
      }, request.limit, this.core.tokenizers));
      await this.core.authorizeEpisodesTx(tx, [...new Set(result.results.flatMap(item => item.sources))], policy);
      await this.receipts.issueReceiptTx(tx, IssueReceiptInput.parse({ recall_id: recallId, primary_ids: result.results.map(item => item.id) }), context, policy,
        { response: result, context_digest: sha256(result.context_text), result_digest: sha256(canonicalContext({ results: result.results, companions: result.companions })), query: request.query, query_vector: queryVector, candidates: ranked.map(({ id, score, relevance, mass, utility }) => ({ id, score, relevance, mass, utility })) });
      return result;
    });
  }
}
