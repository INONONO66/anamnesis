import { isEpisodeSchema } from "@anamnesis/protocol";
import neo4j, { Driver, type ManagedTransaction, type RecordShape } from "neo4j-driver";
import { homedir } from "node:os";
import { join } from "node:path";
import { Generation } from "@anamnesis/protocol";
import { ObjectStore } from "../objects.ts";
import { z } from "zod";
import { type EmbeddingProvider } from "../embedding.ts";
import { type Tokenizers } from "../recall.ts";
import { sha256, canonicalJson } from "./digest.ts";
import { receiptTime, RecallReceipt, ReceiptError } from "./receipts.ts";
import { type InstallationContext, PolicyEvent, policySelector, policyBody, type PolicyState, requireInstallation } from "./policy.ts";
import { type QueryParameters, recordsToObjects } from "./records.ts";
import { EmbeddingLedger } from "./embedding-ledger.ts";

export interface StoreOptions {
  uri: string;
  user: string;
  password: string;
  database?: string;
  objectsRoot?: string;
  /** Server clock only, never accepted from a feedback request. */
  clock?: () => number;
  embeddingProvider?: EmbeddingProvider;
  embeddingLedgerPath?: string;
  tokenizers?: Tokenizers;
  recallDefaultBytes?: number;
  /** Trusted runtime injection only; never loaded from an RPC or arbitrary command. */
  /** True when the extraction provider answers `judge_relations`: validated claims
   * then wait for Fact->Fact verdicts before any Fact of their source is written (D53). */
  relationJudge?: boolean;
}

type PolicyEventRow = { key: string; revision: number; body: string; digest: string };

/** The index-th stored policy event: a well-formed event whose stored key, revision, body and digest all agree with it. */
function decodePolicyEvent(row: PolicyEventRow, index: number): PolicyEvent {
  let decoded: unknown;
  try { decoded = JSON.parse(row.body); }
  catch (error) { if (!(error instanceof SyntaxError)) throw error; throw new ReceiptError("policy_unavailable"); }
  const parsed = PolicyEvent.safeParse(decoded);
  if (!parsed.success) throw new ReceiptError("policy_unavailable");
  const event = parsed.data, body = policyBody(event);
  if (event.revision !== index + 1 || row.revision !== event.revision
    || row.key !== `${event.policy_id}:${event.action}`
    || row.body !== body || row.digest !== sha256(body)) throw new ReceiptError("policy_unavailable");
  return event;
}

/** A deny opens a policy once; a revoke closes an open, unrevoked deny with the same selector and scope. At most 256 stay open. */
function foldPolicyEvent(event: PolicyEvent, denies: Map<string, PolicyEvent>, revoked: Set<string>): void {
  const deny = denies.get(event.policy_id);
  if (event.action === "deny") {
    if (deny) throw new ReceiptError("policy_unavailable");
    denies.set(event.policy_id, event);
  } else {
    if (!deny || revoked.has(event.policy_id) || canonicalJson(policySelector(deny.selector)) !== canonicalJson(policySelector(event.selector)) || deny.scope !== event.scope) throw new ReceiptError("policy_unavailable");
    revoked.add(event.policy_id);
  }
  if (denies.size - revoked.size > 256) throw new ReceiptError("policy_unavailable");
}

export class StoreCore {
  readonly driver: Driver;
  readonly database: string;
  readonly objects: ObjectStore;
  writerEpoch: number | undefined;
  readonly clock: () => number;
  readonly embeddingProvider: EmbeddingProvider | undefined;
  readonly embeddingLedger: EmbeddingLedger;
  readonly tokenizers: Tokenizers;
  readonly recallDefaultBytes: number;
  readonly relationJudge: boolean;
  constructor(opts: StoreOptions, driver?: Driver) {
    this.clock = opts.clock ?? Date.now;
    this.relationJudge = opts.relationJudge ?? false;
    this.embeddingProvider = opts.embeddingProvider;
    this.embeddingLedger = new EmbeddingLedger(opts.embeddingLedgerPath);
    this.tokenizers = opts.tokenizers ?? new Map();
    this.recallDefaultBytes = z.number().int().min(0).max(1024 * 1024).parse(opts.recallDefaultBytes ?? 65536);
    this.driver =
      driver ??
      neo4j.driver(opts.uri, neo4j.auth.basic(opts.user, opts.password), {
        disableLosslessIntegers: true,
      });
    this.database = opts.database ?? "neo4j";
    this.objects = new ObjectStore(
      opts.objectsRoot ?? join(homedir(), ".anamnesis", "objects"),
    );
  }
  async claimWriterEpoch(): Promise<number> {
    const session = this.driver.session({ database: this.database });
    try {
      const epoch = await session.executeWrite(async (tx) => {
        const result = await tx.run<{ epoch: number }>(
          `MERGE (m:Meta {key: 'meta'})
           ON CREATE SET m.ingest_seq = 0, m.writer_epoch = 0
           SET m.writer_epoch = coalesce(m.writer_epoch, 0) + 1
           RETURN m.writer_epoch AS epoch`,
        );
        const record = result.records[0];
        if (!record) throw new Error("writer epoch claim returned no epoch");
        return record.get("epoch");
      });
      this.writerEpoch = epoch;
      return epoch;
    } finally {
      await session.close();
    }
  }
  /** Meta's write lock is held through validation, feedback/cache writes and
   * commit. Policy commands use the same fence, so policy cannot race effects. */
  async receiptLockTx(tx: ManagedTransaction): Promise<PolicyState> {
    const rows = await tx.run<{ structure: number | null; policy: number | null; format: string | null; legacy: number }>(
      `MATCH (m:Meta {key:'meta'}) SET m.ingest_seq=m.ingest_seq
       WITH m OPTIONAL MATCH (p:PolicyAuthority {key:'installation'})
       CALL () { MATCH (e:Element {schema:'anamnesis.memory-policy/1'}) RETURN count(e) AS legacy }
       RETURN m.structure_revision AS structure,m.policy_revision AS policy,p.format AS format,legacy`);
    const row = rows.records[0];
    const revision = receiptTime.safeParse(row?.get("policy"));
    if (!row || !revision.success || row.get("format") !== "episode-source-v1" || row.get("legacy") !== 0) throw new ReceiptError("policy_unavailable");
    // Fold immutable controls, not a caller policy callback or an optional cache.
    // Validate the complete contiguous history even at an unchanged revision.
    const events = await tx.run<{ key: string; revision: number; body: string; digest: string }>(
      `MATCH (p:PolicyEvent) RETURN p.key AS key,p.revision AS revision,p.body AS body,p.body_digest AS digest ORDER BY p.revision`);
    if (events.records.length !== revision.data) throw new ReceiptError("policy_unavailable");
    const denies = new Map<string, PolicyEvent>(), revoked = new Set<string>();
    for (const [index, record] of events.records.entries()) foldPolicyEvent(decodePolicyEvent(record.toObject(), index), denies, revoked);
    return { structure_revision: row.get("structure"), policy_revision: revision.data, denies, revoked };
  }
  async authorizeEpisodesTx(tx: ManagedTransaction, ids: string[], policy: PolicyState): Promise<void> {
    const rows = await tx.run<{ id: string; source: string; schema: string }>(
      `MATCH (e:Element:Episode) WHERE e.id IN $ids RETURN e.id AS id,e.origin_source AS source,e.schema AS schema`, { ids });
    if (rows.records.length !== ids.length) throw new ReceiptError("invalid_selection");
    for (const row of rows.records) {
      if (!isEpisodeSchema(row.get("schema"))) throw new ReceiptError("invalid_selection");
      for (const deny of policy.denies.values()) {
        if (policy.revoked.has(deny.policy_id)) continue;
        if ((deny.selector.episode_id === undefined || deny.selector.episode_id === row.get("id"))
          && (deny.selector.source === undefined || deny.selector.source === row.get("source"))) throw new ReceiptError("policy_denied");
      }
    }
  }
  async authorizeReceiptTx(tx: ManagedTransaction, receipt: RecallReceipt, policy: PolicyState, context: InstallationContext): Promise<void> {
    if (receipt.principal !== context.principal) throw new ReceiptError("unauthenticated");
    // Null identifies an old, unauthorised bootstrap receipt, not revision zero.
    if (receipt.policy_revision === null || receipt.policy_revision > policy.policy_revision) throw new ReceiptError("policy_unavailable");
    await this.authorizeEpisodesTx(tx, [...new Set([...receipt.primary_ids,
      ...receipt.primaries.flatMap(item => [item.id, ...item.sources]),
      ...(receipt.serving?.response.results.flatMap(item => item.provenance.supersedes.map(prior => prior.id)) ?? [])])], policy);
  }
  async withReadTx<Result>(
    work: (tx: ManagedTransaction) => Promise<Result>,
  ): Promise<Result> {
    const session = this.driver.session({ database: this.database });
    try { return await session.executeRead(work); }
    finally { await session.close(); }
  }
  async withWriteTx<Result>(
    work: (tx: ManagedTransaction) => Promise<Result>,
  ): Promise<Result> {
    const session = this.driver.session({ database: this.database });
    try {
      return await session.executeWrite(async (tx) => {
        const epoch = this.writerEpoch;
        if (epoch === undefined) {
          // Compatibility Store writers also serialize physical/cache changes.
          await tx.run(`MERGE (m:Meta {key:'meta'}) ON CREATE SET m.ingest_seq=0
            SET m.conducting_write_lock=true REMOVE m.conducting_write_lock`);
        } else {
          // Acquire Meta's write lock before reading the epoch, and hold it
          // through commit against concurrent claims.
          const fence = await tx.run<{ epoch: number }>(
            `MATCH (m:Meta {key: 'meta'})
             SET m.writer_epoch = m.writer_epoch
             RETURN m.writer_epoch AS epoch`,
          );
          if (fence.records[0]?.get("epoch") !== epoch) {
            throw new Error("stale_writer_epoch");
          }
        }
        return work(tx);
      });
    } finally {
      await session.close();
    }
  }
  /** Internal installation API only. No unfenced compatibility writer is allowed
   * for extraction, including status reads that may reveal retained content. */
  async extractionTx<T>(context: InstallationContext, work: (tx: ManagedTransaction, policy: PolicyState) => Promise<T>): Promise<T> {
    requireInstallation(context);
    if (this.writerEpoch === undefined) throw new Error("writer_epoch_required");
    return this.withWriteTx(async tx => work(tx, await this.receiptLockTx(tx)));
  }
  async extractionRecordTx<T>(tx: ManagedTransaction, label: "ExtractionGeneration" | "ExtractionAttempt" | "ModelTask" | "ExtractionJudgeInput", id: string, schema: z.ZodType<T>): Promise<T> {
    const rows = await tx.run<{ body: string }>(`MATCH (n:${label} {id:$id}) RETURN n.body AS body`, { id });
    if (!rows.records[0]) throw new Error(`unknown_${label}`);
    return schema.parse(JSON.parse(rows.records[0].get("body")));
  }
  async writableExtractionGenerationTx(tx: ManagedTransaction, id: string): Promise<Generation> {
    const generation = await this.extractionRecordTx(tx, "ExtractionGeneration", id, Generation);
    if (!["active", "catching_up"].includes(generation.state)) throw new Error("generation_not_writable");
    return generation;
  }
  async close(): Promise<void> {
    await this.driver.close();
  }
  async run<Row extends RecordShape>(
    cypher: string,
    params: QueryParameters = {},
  ): Promise<Row[]> {
    const res = await this.driver.executeQuery<Row>(cypher, params, {
      database: this.database,
    });
    return recordsToObjects(res.records);
  }
}
