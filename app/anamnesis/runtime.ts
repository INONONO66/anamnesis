import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, rm, stat } from "node:fs/promises";
import { join } from "node:path";
import neo4j, { type Driver, type RecordShape } from "neo4j-driver";
import { Engine, envConfig, type EngineOptions } from "../../packages/core/src/engine.ts";
import { OpenAiChatExtractionProvider } from "../../packages/core/src/openai-extraction-provider.ts";
import { ExtractionScheduler, type ExtractionTurn } from "../../packages/core/src/extraction-scheduler.ts";
import { OpenAiEmbeddingProvider } from "../../packages/core/src/openai-embedding-provider.ts";
import { elementDigest, verifyLineageRetry } from "../../packages/core/src/store.ts";
import { EchoLineage, parseEpisodeLineage } from "../../packages/protocol/src/episode-lineage.ts";
import type { CreateExtractionPipeline, RunExtractionPipeline } from '../../packages/protocol/src/extraction-audit.ts';
import type { InstallationContext, CommitReceiptInput, RecallTransportInput } from "../../packages/core/src/store.ts";
import type { RpcPolicySetParams, RpcPolicyRevokeParams, RpcRecallParams, RpcEmbeddingRecoverParams, RpcDreamAdmitParams, RpcDreamLeaseParams, RpcDreamExpireParams, RpcDreamExecuteParams } from "../../packages/protocol/src/rpc.ts";
import { DurableSpool, type SpoolEntry } from "../../packages/core/src/spool.ts";
import { RPC_LIMITS, RPC_METHODS, RpcRememberParams, type RpcCapabilities, type RpcCommittedResult, type RpcIngestStatusParams, type RpcIngestStatusResult, type RpcStatusResult, type RpcWorkersStatus } from "../../packages/protocol/src/rpc.ts";
import { atomicJson, hasCode, loadProviderConfig, syncDirectory, type Installation } from "./config.ts";
import { Uploads, type UploadLifecycle } from "./objects.ts";
import { loadTokenizers } from "./tokenizer.ts";
import { daemonTiming, runtimeTimed, timingHash } from "./timing.ts";
import { fault, RpcFault, storageUnavailable } from "./wire.ts";
import type { TrustedAuthorityAdapter } from "./backup-restore-orchestrator.ts";
import { backupOwned, restoreOwned } from "./backup-restore-orchestrator.ts";
import { createRuntimeAuthority, manifestTemplate, objectInventory } from "./runtime-authority.ts";
import { NEO4J_IMAGE, NEO4J_VERSION } from "./owned-neo4j-adapter.ts";
import { createDreamLeidenAdapter } from './dream-leiden-runtime.ts';
import { trustedDreamLeiden, DREAM_GDS_IMAGE, DREAM_GDS_VERSION, DREAM_ALGORITHM, DREAM_NETWORK } from '../../packages/core/src/dream-leiden-adapter.ts';

export const capabilities: RpcCapabilities = { methods: [...RPC_METHODS], recall: true, commit: true, policy: true, extraction: false, embeddings: false, writer_fence: "database" };
function canonical(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  return `{${Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([key, member]) => `${JSON.stringify(key)}:${canonical(member)}`).join(",")}}`;
}
const sha = (value: string) => createHash("sha256").update(value).digest("hex");
const originKey = (params: RpcRememberParams) => { const o = params.episode.origin; return sha(JSON.stringify([o.source, o.session, o.actor, o.record])); };
// Origin field order is explicit, not object-insertion dependent.
function revisionKey(params: RpcRememberParams): string {
  const o = params.episode.origin;
  return sha(JSON.stringify([sha(JSON.stringify([o.source, o.session, o.actor, o.record])), params.source_revision]));
}
interface Binding { digest_version: 1; params: RpcRememberParams; body_digest: string; incarnation: string; fs_epoch: string; }
const Binding = { parse(value: unknown): Binding {
  if (!value || typeof value !== "object") throw new RpcFault("spool_corrupt", "invalid delivery binding");
  const data = value as Record<string, unknown>;
  if (data["digest_version"] !== 1 || typeof data["body_digest"] !== "string" || !/^[a-f0-9]{64}$/.test(data["body_digest"]) || typeof data["incarnation"] !== "string" || typeof data["fs_epoch"] !== "string") throw new RpcFault("spool_corrupt", "invalid delivery binding");
  return { digest_version: 1, params: RpcRememberParams.parse(data["params"]), body_digest: data["body_digest"], incarnation: data["incarnation"], fs_epoch: data["fs_epoch"] };
} };
function digest(params: RpcRememberParams): string { return sha(canonical({ digest_version: 1, params })); }
interface StoredEpisode {
  id: string; schema: string; time_value: string; time_precision: string;
  content: string; mass: number; properties: string;
  origin_source: string; origin_session: string; origin_actor: string; origin_record: string;
  source_revision: string; previous_revision_key?: string; payload_hash?: string;
  ingest_seq: number; digest: string; digest_format?: string;
  episode_digest_version?: number; origin_role?: string; lineage_digest?: string;
}
function lineageMetadata(params: RpcRememberParams) {
  return params.origin_role !== undefined || params.lineage_mode !== undefined || params.parent_recall_ids !== undefined
    ? { origin_role: params.origin_role, lineage_mode: params.lineage_mode, parent_recall_ids: params.parent_recall_ids } : undefined;
}
function compatibilityParams(params: RpcRememberParams): RpcRememberParams {
  const { origin_role, lineage_mode, parent_recall_ids, ...legacy } = params;
  return legacy;
}

export interface RuntimeAuthorityOptions {
  /** Injected only by the owning lifecycle. No ambient/global adapter is used. */
  authorityAdapter?: TrustedAuthorityAdapter;
}
/** Background lanes the daemon's single writer interleaves with requests. */
export type BackgroundLane = "spool" | "embedding" | "extraction";
/** Outcome of one bounded embedding batch. "stalled" keeps the wake pending for the next storage recovery. */
export type EmbeddingTurn = "more" | "idle" | "stalled";

export class Runtime {
  readonly capabilities: RpcCapabilities;
  /** Load asynchronous provider assets once, before accepting RPC traffic. */
  static async create(installation: Installation, scheduleDrain: (lane: BackgroundLane) => void = () => {}, uploadLifecycle: UploadLifecycle = {}, authorityOptions: RuntimeAuthorityOptions = {}): Promise<Runtime> {
    const providers: EngineOptions = {};
    if (process.env["ANAMNESIS_LLM_BASE_URL"] !== undefined || process.env["ANAMNESIS_EMBEDDING_BASE_URL"] !== undefined) {
      const config = await loadProviderConfig();
      if (config.llm.baseUrl !== undefined) {
        if (!config.llm.apiKey) throw new Error("ANAMNESIS_LLM_API_KEY_FILE required");
        providers.extractionProvider = new OpenAiChatExtractionProvider({
          ...config.llm, baseUrl: config.llm.baseUrl, apiKey: config.llm.apiKey, systemPrompt: config.systemPrompt, relationPrompt: config.relationPrompt, timeoutMs: 30000,
        });
      }
      if (config.embedding) {
        const { model, dimensions, baseUrl } = config.embedding;
        // Configuration identity only; an alias does not attest immutable server weights.
        const profile = { model, dimensions, model_incarnation: sha(JSON.stringify([baseUrl, model, dimensions])),
          document_prefix: "", query_prefix: "", max_input_bytes: 65536, norm: "unit_l2" as const, norm_tolerance: 0.01 };
        providers.embeddingProvider = new OpenAiEmbeddingProvider({ ...config.embedding, profile, timeoutMs: 30000 });
      }
    }
    return new Runtime(installation, scheduleDrain, uploadLifecycle, authorityOptions, providers);
  }
  readonly uploads: Uploads;
  private readonly engine: Engine;
  private readonly reader: Driver;
  private readonly database: string;
  private readonly spool: DurableSpool;
  private readonly spoolRoot: string;
  private readonly bindings: string;
  private readonly authorityAdapter: TrustedAuthorityAdapter | undefined;
  private readonly backupOperations = new Map<string, { state: "running" | "complete" | "failed"; error?: string }>();
  private readonly restoreOperations = new Map<string, { state: "running" | "complete" | "failed"; error?: string }>();
  private epoch: number | undefined;
  private initialized = false;
  private available = false;
  private readonly blocked = new Map<number, "missing_predecessor" | "dependency_cycle" | "stale_revision" | "revision_conflict">();
  private readonly quarantined = new Map<number, "spool_corrupt" | "incarnation_mismatch">();
  private drainJob: AsyncGenerator<void, void, void> | undefined;
  private drainRequested = false;
  private drainStopped = false;
  private readonly embedding = { requested: false, drained_total: 0, quarantined_total: 0, last_error: null as string | null };
  /** Present only with an extraction provider; the lane is otherwise never scheduled and reports unconfigured. */
  private readonly extraction: ExtractionScheduler | undefined;
  private extractionRequested = false;
  constructor(readonly installation: Installation, private readonly scheduleDrain: (lane: BackgroundLane) => void = () => {}, uploadLifecycle: UploadLifecycle = {}, authorityOptions: RuntimeAuthorityOptions = {}, providers: EngineOptions = {}) {
    this.authorityAdapter = authorityOptions.authorityAdapter;
    const config = { ...envConfig(), ...providers, tokenizers: loadTokenizers(), objectsRoot: join(installation.root, "objects") };
    this.capabilities = { ...capabilities, extraction: !!config.extractionProvider, embeddings: !!config.embeddingProvider };
    const rawDream = createDreamLeidenAdapter();
    const dreamLeidenAdapter = rawDream ? trustedDreamLeiden({ image_digest: DREAM_GDS_IMAGE, plugin_digest: 'sha256:246e3fbbbf733b4def1e7b0a9740a2309f6605ee8a7b46b29fe1de56d0a4b47c', algorithm: DREAM_ALGORITHM, gds_version: DREAM_GDS_VERSION, network: DREAM_NETWORK, adapter: rawDream }) : undefined;
    this.engine = new Engine({ ...config, ...(dreamLeidenAdapter ? { dreamLeidenAdapter } : {}) });
    this.extraction = config.extractionProvider && new ExtractionScheduler(this.engine, { provider: config.extractionProvider,
      context: Object.freeze({ principal: "installation", commit_mode: "auto", client_binding: randomUUID() }),
      read: (query, params) => this.read(query, params), wake: () => this.wakeExtraction() });
    this.database = config.database ?? "neo4j";
    this.reader = neo4j.driver(config.uri, neo4j.auth.basic(config.user, config.password), {
      disableLosslessIntegers: true, connectionTimeout: 1000, connectionAcquisitionTimeout: 1500, maxTransactionRetryTime: 0,
    });
    this.uploads = new Uploads(config.objectsRoot, join(installation.root, "uploads"), uploadLifecycle);
    this.spoolRoot = join(installation.root, "spool");
    this.bindings = join(installation.root, "deliveries");
    this.spool = new DurableSpool(this.spoolRoot, { maxBytes: RPC_LIMITS.spool_bytes, maxFrameBytes: RPC_LIMITS.frame_bytes });
  }
  async init(): Promise<void> {
    await mkdir(this.bindings, { recursive: true, mode: 0o700 });
    await mkdir(this.spoolRoot, { recursive: true, mode: 0o700 });
    await this.uploads.init();
    await syncDirectory(this.installation.root);
    await this.refresh();
  }
  private async read<Row extends RecordShape>(query: string, params: Record<string, unknown> = {}): Promise<Row[]> {
    const session = this.reader.session({ database: this.database, defaultAccessMode: neo4j.session.READ });
    try { return (await runtimeTimed("neo4j.read", () => session.run<Row>(query, params, { timeout: 5000 }), daemonTiming ? timingHash(query) : undefined)).records.map(record => record.toObject()); }
    finally { await runtimeTimed("neo4j.session.close", () => session.close()); }
  }
  /** Reconnect is demand-driven by status/remember/startup, without polling timers. */
  async refresh(): Promise<void> {
    await this.installation.assertOwned();
    try {
      await this.read("RETURN 1 AS connected");
      if (!this.initialized) { await runtimeTimed("engine.init", () => this.engine.init()); this.initialized = true; }
      if (this.epoch === undefined) this.epoch = await runtimeTimed("engine.claimWriterEpoch", () => this.engine.claimWriterEpoch());
      const rows = await this.read<{ epoch: number }>("MATCH (m:Meta {key:'meta'}) RETURN m.writer_epoch AS epoch");
      if (rows[0]?.epoch !== this.epoch) throw new RpcFault("ownership_lost", "database writer epoch changed");
      const recovered = !this.available;
      this.available = true;
      if (recovered) { this.wakeDrain(); this.wakeEmbedding(); this.wakeExtraction(); }
    } catch (error) {
      if (!storageUnavailable(error)) throw error;
      this.available = false;
    }
  }
  private identity(binding: Binding) {
    return { revision_key: revisionKey(binding.params), body_digest: binding.body_digest, data_incarnation: binding.incarnation };
  }
  private spooled(binding: Binding, sequence: number) {
    return { state: "spooled" as const, ...this.identity(binding), fs_epoch: binding.fs_epoch, spool_seq: sequence };
  }
  private async binding(key: string): Promise<Binding | null> {
    try { return Binding.parse(JSON.parse(await readFile(join(this.bindings, key + ".json"), "utf8"))); }
    catch (error) { if (hasCode(error, "ENOENT")) return null; throw error; }
  }
  private async pendingEntry(revision: string): Promise<SpoolEntry | null> {
    // Snapshot the admitted cohort rather than using an unbounded sentinel. The
    // page cursor then gives deterministic continuation without materializing the journal.
    const cohort = (await this.spool.status()).nextSequence - 1;
    const lookup = this.findPending(revision, cohort);
    let step = await lookup.next();
    while (!step.done) step = await lookup.next();
    return step.value;
  }
  private async *findPending(revision: string, cohort: number): AsyncGenerator<void, SpoolEntry | null, void> {
    let cursor: string | undefined;
    while (true) {
      const page = await this.spool.page({ ...(cursor ? { cursor } : {}), limit: 100, maxBytes: 4 * 1024 * 1024 });
      yield;
      const found = page.entries.find(entry => entry.sequence <= cohort && entry.revision === revision);
      if (found) return found;
      if (!page.nextCursor || page.entries.some(entry => entry.sequence >= cohort)) return null;
      cursor = page.nextCursor;
    }
  }
  private async *eachPendingPage(cohort: number, visit: (entry: SpoolEntry) => AsyncGenerator<void, void, void>): AsyncGenerator<void, void, void> {
    let cursor: string | undefined;
    while (true) {
      const page = await this.spool.page({ ...(cursor ? { cursor } : {}), limit: 100, maxBytes: 4 * 1024 * 1024 });
      yield;
      for (const entry of page.entries) {
        if (entry.sequence > cohort) return;
        yield* visit(entry);
        yield; // Invalid/blocked entries also consume a bounded turn.
      }
      if (!page.nextCursor || page.entries.some(entry => entry.sequence >= cohort)) return;
      cursor = page.nextCursor;
    }
  }
  private validated(entry: SpoolEntry): Binding {
    const binding = Binding.parse(entry.body);
    if (entry.incarnation !== this.installation.incarnation || binding.incarnation !== entry.incarnation) throw new RpcFault("incarnation_mismatch", "spool belongs to another installation incarnation");
    if (entry.revision !== revisionKey(binding.params) || entry.predecessor !== binding.params.expected_previous_revision_key || entry.origin !== originKey(binding.params) || binding.body_digest !== digest(binding.params)) {
      throw new RpcFault("spool_corrupt", "spool envelope identity mismatch");
    }
    return binding;
  }
  private async committed(binding: Binding, created = false): Promise<RpcCommittedResult | null> {
    const row = (await this.read<{ e: StoredEpisode }>("MATCH (e:Element:Episode {revision_key:$key}) RETURN properties(e) AS e", { key: revisionKey(binding.params) }))[0]?.e;
    if (!row) return null;
    if (row.episode_digest_version !== undefined && row.episode_digest_version !== 2)
      throw new RpcFault("unsupported_digest_version", "stored Episode version is unsupported");
    let metadata;
    if (row.episode_digest_version === 2) {
      const retained = (await this.read<{ body: string; props: Record<string, unknown> }>(
        "MATCH (l:EchoLineage {episode_id:$id}) RETURN l.body AS body,properties(l) AS props", { id: row.id }))[0];
      if (!retained) throw new RpcFault("lineage_unavailable", "retained lineage missing");
      const lineage = EchoLineage.parse(JSON.parse(retained.body));
      const { body, digest: retainedDigest, ...props } = retained.props;
      if (lineage.episode_id !== row.id || sha(canonical(lineage)) !== row.lineage_digest
        || retainedDigest !== row.lineage_digest || canonical(props) !== body || canonical(lineage) !== body)
        throw new RpcFault("lineage_mismatch", "retained lineage digest mismatch");
      verifyLineageRetry(lineageMetadata(binding.params), row.origin_role ?? null, lineage);
      metadata = { origin_role: row.origin_role, lineage_mode: lineage.lineage_mode, parent_recall_ids: lineage.parent_recall_ids };
    } else if (row.digest_format !== undefined && row.digest_format !== "rfc8785-v1") {
      throw new RpcFault("unsupported_digest_version", "stored digest format is unsupported");
    }
    const params = RpcRememberParams.parse({ episode: {
      schema: row.schema, time: { value: row.time_value, precision: row.time_precision },
      content: row.content, mass: row.mass, properties: JSON.parse(row.properties),
      origin: { source: row.origin_source, session: row.origin_session, actor: row.origin_actor, record: row.origin_record },
    }, source_revision: row.source_revision, expected_previous_revision_key: row.previous_revision_key ?? null,
    ...(row.payload_hash ? { payload_hash: row.payload_hash } : {}), ...metadata });
    const candidate = row.episode_digest_version === 2
      ? { ...binding.params, ...parseEpisodeLineage(lineageMetadata(binding.params)) } : compatibilityParams(binding.params);
    if (digest(params) !== digest(candidate)) throw new RpcFault("revision_conflict", "stored revision has a different delivery body");
    // Verify the core digest as well as the complete RPC envelope (which also
    // binds mass and origin). A revision-only match can never yield success.
    const coreDigest = elementDigest(candidate.episode, { payloadHash: candidate.payload_hash ?? null,
      previousRevisionKey: candidate.expected_previous_revision_key, format: row.digest_format ?? null,
      episodeDigestVersion: row.episode_digest_version ?? null, originRole: row.origin_role ?? null, lineageDigest: row.lineage_digest ?? null });
    if (row.digest !== coreDigest) throw new RpcFault("revision_conflict", "stored digest does not verify");
    return { state: "committed", ...this.identity(binding), id: row.id, created, ingest_seq: row.ingest_seq };
  }
  private async write(binding: Binding, context?: InstallationContext): Promise<RpcCommittedResult> {
    const params = binding.params;
    const existing = await this.committed(binding);
    if (existing) return existing;
    let payload: { payload: Uint8Array<ArrayBuffer>; payload_media_type: string } | undefined;
    if (params.payload_hash) {
      const metadata = await this.uploads.metadata(params.payload_hash);
      if (!metadata) throw new RpcFault("object_not_found", "remember references an uncommitted object");
      payload = { payload: new Uint8Array(await this.uploads.store.get(params.payload_hash)), payload_media_type: metadata.media_type };
    }
    await this.installation.assertOwned();
    const metadata = lineageMetadata(params);
    if (metadata && !context) throw new RpcFault("lineage_binding_mismatch", "new lineage requires authenticated connection custody");
    const result = await runtimeTimed("engine.remember", () => this.engine.remember({ ...params.episode, source_revision: params.source_revision,
      expected_previous_revision_key: params.expected_previous_revision_key, ...payload }, metadata ? { metadata, context: context! } : undefined));
    const committed = await this.committed(binding, result.created);
    if (!committed || committed.id !== result.id) throw new RpcFault("internal_error", "database did not expose the committed delivery");
    if (committed.created) { this.wakeEmbedding(); this.wakeExtraction(); } // Direct and spool-drained commits alike enqueue worker work.
    return committed;
  }
  async remember(params: RpcRememberParams, context?: InstallationContext) {
    await this.installation.assertOwned();
    if ((await this.spool.status()).quarantined) throw new RpcFault("spool_corrupt", "spool is quarantined; admission is stopped");
    const key = revisionKey(params);
    const previous = await this.binding(key);
    const candidate: Binding = { digest_version: 1, params, body_digest: digest(params), incarnation: this.installation.incarnation, fs_epoch: this.installation.epoch };
    // The immutable database version wins before filesystem delivery equality or
    // any new role/parent validation. Never rewrite an old delivery binding.
    await this.refresh();
    if (this.available) {
      const existing = await this.committed(candidate);
      if (existing) {
        if (previous?.incarnation !== undefined && previous.incarnation !== this.installation.incarnation)
          throw new RpcFault("incarnation_mismatch", "delivery belongs to another incarnation");
        if (!previous) await atomicJson(join(this.bindings, key + ".json"), candidate);
        return previous ? { ...existing, ...this.identity(previous) } : existing;
      }
    }
    if (previous && previous.body_digest !== candidate.body_digest) throw new RpcFault("revision_conflict", "revision already binds a different delivery body");
    const binding = previous ?? candidate;
    if (binding.incarnation !== this.installation.incarnation) throw new RpcFault("incarnation_mismatch", "delivery belongs to another incarnation");
    if (params.payload_hash && !await this.uploads.metadata(params.payload_hash)) throw new RpcFault("object_not_found", "remember references an uncommitted object");
    if ((await this.spool.status()).quarantined) throw new RpcFault("spool_corrupt", "spool is quarantined; admission is stopped");
    const metadata = lineageMetadata(params);
    if (metadata) {
      if (!context?.client_binding) throw new RpcFault("lineage_binding_mismatch", "authenticated connection required");
      if (!this.available) throw new RpcFault("storage_unavailable", "lineage admission requires retained authority", true);
      parseEpisodeLineage(metadata);
    }
    if (!previous) await atomicJson(join(this.bindings, key + ".json"), binding);
    if (this.available) {
      try {
        const result = await this.write(binding, context);
        if (result.created) this.wakeDrain();
        return result;
      }
      catch (error) {
        if (!storageUnavailable(error)) {
          if (!previous) await rm(join(this.bindings, key + ".json"));
          throw error;
        }
        this.available = false;
      }
    }
    if (metadata) throw new RpcFault("storage_unavailable", "lineage admission is never spooled without parent authority", true);
    await this.installation.assertOwned();
    const existing = await this.pendingEntry(key);
    if (existing) { this.validated(existing); return this.spooled(binding, existing.sequence); }
    const sequence = await this.spool.append({ origin: originKey(params), revision: key,
      predecessor: params.expected_previous_revision_key, body: binding, incarnation: binding.incarnation });
    // The current producer API fsyncs journal/markers; the runtime owns and
    // syncs their already-created directory before any durable acceptance.
    await syncDirectory(this.spoolRoot);
    await this.installation.assertOwned();
    if (sequence < 1 || (await this.spool.status()).quarantined) throw new RpcFault("spool_corrupt", "spool did not publish a verifiable durable entry");
    this.wakeDrain();
    return this.spooled(binding, sequence);
  }
  private wakeDrain(): void {
    this.drainRequested = true;
    if (this.available && !this.drainStopped) this.scheduleDrain("spool");
  }
  private wakeEmbedding(): void {
    if (!this.capabilities.embeddings) return; // Unconfigured: the worker is never scheduled and never reports.
    this.embedding.requested = true;
    if (this.available && !this.drainStopped) this.scheduleDrain("embedding");
  }
  /** One bounded outbox batch; called only by the daemon's serial owner, never from a second writer. */
  async embeddingTurn(): Promise<EmbeddingTurn> {
    if (this.drainStopped || !this.available) return "stalled";
    if (!this.embedding.requested) return "idle";
    try {
      await this.installation.assertOwned();
      const rows = await this.read<{ epoch: number }>("MATCH (m:Meta {key:'meta'}) RETURN m.writer_epoch AS epoch");
      if (rows[0]?.epoch !== this.epoch) throw new RpcFault("ownership_lost", "database writer epoch changed");
      const batch = await runtimeTimed("engine.drainEmbeddingOutbox", () => this.engine.drainEmbeddingOutbox(100));
      this.embedding.drained_total += batch.drained;
      if ("reason" in batch) { this.embedding.requested = false; return "idle"; }
      this.embedding.quarantined_total += batch.quarantined;
      // A deferred entry is a provider-side failure that stays in the outbox; it
      // is retried on the next wake, never in a loop of its own.
      this.embedding.last_error = batch.deferred ? `${batch.deferred} outbox entries deferred: ${batch.deferral_reason}` : null;
      if (batch.drained > 0) return "more";
      this.embedding.requested = false;
      return "idle";
    } catch (error) {
      if (storageUnavailable(error)) { this.available = false; return "stalled"; } // Recovery re-schedules the pending wake.
      this.embedding.last_error = String(error).slice(0, 512);
      this.embedding.requested = false;
      throw error;
    }
  }
  private wakeExtraction(): void {
    if (!this.extraction) return;
    this.extractionRequested = true;
    if (this.available && !this.drainStopped) this.scheduleDrain("extraction");
  }
  /** One bounded scheduler turn: database work only, provider calls stay in tracked in-flight pipelines. */
  async extractionTurn(): Promise<ExtractionTurn | "stalled"> {
    if (this.drainStopped || !this.available || !this.extraction) return "stalled";
    if (!this.extractionRequested) return this.extraction.inFlightCount ? "waiting" : "idle";
    // Consume the request before working: a pipeline settling mid-turn re-arms the lane and that wake must survive.
    this.extractionRequested = false;
    try {
      await this.installation.assertOwned();
      const rows = await this.read<{ epoch: number }>("MATCH (m:Meta {key:'meta'}) RETURN m.writer_epoch AS epoch");
      if (rows[0]?.epoch !== this.epoch) throw new RpcFault("ownership_lost", "database writer epoch changed");
      const outcome = await runtimeTimed("engine.extractionTurn", () => this.extraction!.turn());
      if (outcome === "more") this.extractionRequested = true;
      return outcome;
    } catch (error) {
      if (storageUnavailable(error)) { this.available = false; this.extractionRequested = true; return "stalled"; } // Recovery re-schedules the pending wake.
      this.extraction.recordError(error);
      throw error;
    }
  }
  private workers(pendingOutbox: number | null): RpcWorkersStatus {
    const { drained_total, quarantined_total, last_error } = this.embedding;
    return { embedding: { pending: pendingOutbox, drained_total, quarantined_total, last_error }, extraction: this.extraction?.status() ?? { state: "unconfigured" } };
  }
  /** Called only by the daemon's serial owner, never from a second writer. */
  async drainTurn(): Promise<boolean> {
    if (this.drainStopped || !this.available) {
      await this.drainJob?.return(); this.drainJob = undefined;
      return false;
    }
    try {
      await this.installation.assertOwned();
      const rows = await this.read<{ epoch: number }>("MATCH (m:Meta {key:'meta'}) RETURN m.writer_epoch AS epoch");
      if (rows[0]?.epoch !== this.epoch) throw new RpcFault("ownership_lost", "database writer epoch changed");
      if (!this.drainJob) {
        if (!this.drainRequested) return false;
        this.drainRequested = false;
        this.drainJob = this.drain();
      }
      if ((await this.drainJob.next()).done) this.drainJob = undefined;
      return !!this.drainJob || this.drainRequested;
    } catch (error) {
      await this.drainJob?.return(); this.drainJob = undefined;
      if (!storageUnavailable(error)) throw error;
      this.available = false;
      this.drainRequested = true; // Explicit recovery will wake a fresh cohort.
      return false;
    }
  }
  /** The active bounded turn finishes; no continuation may write after stop. */
  cancelDrain(): void { this.drainStopped = true; this.drainRequested = false; }
  /** Follow only uncommitted, executable dependencies; retain no chain-sized set. */
  private async *unresolvedPredecessor(entry: SpoolEntry, cohort: number): AsyncGenerator<void, SpoolEntry | null, void> {
    if (!entry.predecessor) return null;
    const rows = await this.read<{ found: number }>("MATCH (e:Episode {revision_key:$key}) RETURN count(e) AS found", { key: entry.predecessor });
    yield;
    if (rows[0]?.found) return null;
    const predecessor = yield* this.findPending(entry.predecessor, cohort);
    if (!predecessor || this.quarantined.has(predecessor.sequence)) return null;
    const reason = this.blocked.get(predecessor.sequence);
    if (reason && reason !== "dependency_cycle") return null;
    this.validated(predecessor);
    return predecessor;
  }
  private async *dependencyReason(entry: SpoolEntry, cohort: number): AsyncGenerator<void, "missing_predecessor" | "dependency_cycle", void> {
    // Floyd traversal distinguishes a cycle member from a tail entering it.
    // DB/missing/invalid/stale termini stop the chain. Each lookup is paged;
    // this trades repeated journal scans for constant chain memory.
    let slow: SpoolEntry | null = entry, fast: SpoolEntry | null = entry;
    do {
      slow = yield* this.unresolvedPredecessor(slow, cohort);
      fast = yield* this.unresolvedPredecessor(fast, cohort);
      if (fast) fast = yield* this.unresolvedPredecessor(fast, cohort);
      if (!slow || !fast) return "missing_predecessor";
    } while (slow.revision !== fast.revision);
    let start: SpoolEntry | null = entry;
    while (start.revision !== slow.revision) {
      start = yield* this.unresolvedPredecessor(start, cohort);
      slow = yield* this.unresolvedPredecessor(slow, cohort);
      if (!start || !slow) return "missing_predecessor";
    }
    return start.revision === entry.revision ? "dependency_cycle" : "missing_predecessor";
  }
  private async *drain(): AsyncGenerator<void, void, void> {
    this.blocked.clear(); this.quarantined.clear();
    const spool = await this.spool.status();
    if (spool.quarantined) return;
    const cohort = spool.nextSequence - 1;
    yield;
    // Page/status/complete still scan O(N) inside core. Yields bound runtime
    // entries and DB operations per turn, not the latency of those core calls.
    const runtime = this;
    let progress = true;
    while (progress) {
      progress = false;
      yield* this.eachPendingPage(cohort, async function* (entry) {
        const self = runtime;
        let binding: Binding;
        try { binding = self.validated(entry); }
        catch (error) {
          const reason = fault(error).code;
          if (reason !== "spool_corrupt" && reason !== "incarnation_mismatch") throw error;
          self.quarantined.set(entry.sequence, reason); return;
        }
        try {
          const existing = await self.committed(binding);
          yield;
          if (!existing && entry.predecessor) {
            const predecessor = await self.read<{ found: number }>("MATCH (e:Episode {revision_key:$key}) RETURN count(e) AS found", { key: entry.predecessor });
            yield;
            // Retained suffix visibility is not evidence that a predecessor
            // still needs execution. Check the DB before deferring to it.
            if (!predecessor[0]?.found) {
              if (!(yield* self.findPending(entry.predecessor, cohort))) self.blocked.set(entry.sequence, "missing_predecessor");
              return;
            }
          }
          const before = await self.spool.status();
          yield;
          const result = existing ?? await self.write(binding);
          yield;
          await self.installation.assertOwned();
          await self.spool.complete(entry.sequence);
          await syncDirectory(self.spoolRoot);
          yield;
          const after = await self.spool.status();
          // Completing an already-committed suffix behind a blocked head is
          // idempotent, not progress. Preserve advancement earlier in the pass.
          progress = progress || result.created || after.pending < before.pending;
          self.blocked.delete(entry.sequence);
        } catch (error) {
          const reason = fault(error).code;
          if (reason === "revision_conflict" || reason === "stale_revision") self.blocked.set(entry.sequence, reason);
          else throw error;
        }
      });
    }
    yield* this.eachPendingPage(cohort, async function* (entry) {
      if (runtime.quarantined.has(entry.sequence) || runtime.blocked.has(entry.sequence)) return;
      const committed = await runtime.committed(runtime.validated(entry));
      yield;
      if (!committed) runtime.blocked.set(entry.sequence, yield* runtime.dependencyReason(entry, cohort));
    });
  }
  async ingestStatus(identity: RpcIngestStatusParams): Promise<RpcIngestStatusResult> {
    if (identity.data_incarnation !== this.installation.incarnation) throw new RpcFault("incarnation_mismatch", "delivery belongs to another incarnation");
    await this.refresh();
    const binding = await this.binding(identity.revision_key);
    if (!binding || binding.body_digest !== identity.body_digest) return { state: "unknown", ...identity };
    if (this.available) {
      const committed = await this.committed(binding);
      if (committed) return committed;
    }
    const spoolStatus = await this.spool.status();
    if (spoolStatus.quarantined) return { state: "quarantined", ...identity, reason: "spool_corrupt" };
    const entry = await this.pendingEntry(identity.revision_key);
    if (!entry) return { state: "unknown", ...identity };
    const quarantined = this.quarantined.get(entry.sequence);
    if (quarantined) return { state: "quarantined", ...identity, reason: quarantined };
    const reason = this.blocked.get(entry.sequence);
    if (reason) return { ...this.spooled(binding, entry.sequence), state: "blocked", expected_previous_revision_key: entry.predecessor, reason };
    this.validated(entry);
    return this.spooled(binding, entry.sequence);
  }
  async status(pending: number, stopping: boolean): Promise<RpcStatusResult> {
    await this.refresh();
    const spool = await this.spool.status();
    let bytes = 0;
    try { bytes = (await stat(join(this.spoolRoot, "spool.journal"))).size; }
    catch (error) { if (!hasCode(error, "ENOENT")) throw error; }
    let outbox: number | null = null;
    if (this.available) {
      try { outbox = (await runtimeTimed("engine.status", () => this.engine.status())).pendingOutbox; }
      catch (error) { if (!storageUnavailable(error)) throw error; this.available = false; }
    }
    return { version: 1, state: stopping ? "stopping" : this.available && !spool.quarantined && this.quarantined.size === 0 ? "ready" : "degraded",
      storage: this.available ? "available" : "unavailable", data_incarnation: this.installation.incarnation,
      fs_epoch: this.installation.epoch, queue: { pending, capacity: RPC_LIMITS.queued_requests },
      spool: { pending: spool.pending, blocked: this.blocked.size, quarantined: spool.quarantined ? 1 : this.quarantined.size, bytes },
      outbox_pending: outbox, capabilities: this.capabilities, workers: this.workers(outbox) };
  }
  async backup(context: InstallationContext, destination: string, operationId: string) {
    await this.installation.assertOwned();
    if (!context.client_binding) throw new RpcFault("unauthenticated", "authenticated connection custody is required");
    if (!destination || !operationId) throw new RpcFault("invalid_params", "backup destination and operation identity are required");
    if (!this.authorityAdapter) throw new RpcFault("backup_adapter_unavailable", "offline backup adapter is not installed");
    const fenced = await this.authorityAdapter.revokeWriters();
    const authority = await this.authorityAdapter.authoritySnapshot(fenced.epoch);
    const objectRoot = join(this.installation.root, "objects");
    const objects = await objectInventory(objectRoot);
    const config = Buffer.from(JSON.stringify({ uri: process.env["ANAMNESIS_NEO4J_URI"] ?? "", user: process.env["ANAMNESIS_NEO4J_USER"] ?? "neo4j", database: process.env["ANAMNESIS_NEO4J_DATABASE"] ?? "neo4j" }));
    const manifest = manifestTemplate(operationId, fenced.cutoff, authority, objects, createHash("sha256").update(config).digest("hex"));
    const cached: TrustedAuthorityAdapter = {
      revokeWriters: async () => fenced,
      authoritySnapshot: async () => authority,
      dumpOffline: this.authorityAdapter.dumpOffline.bind(this.authorityAdapter),
      materializeMembers: this.authorityAdapter.materializeMembers.bind(this.authorityAdapter),
      startAndReady: this.authorityAdapter.startAndReady.bind(this.authorityAdapter),
      stop: this.authorityAdapter.stop.bind(this.authorityAdapter),
      restoreOffline: this.authorityAdapter.restoreOffline.bind(this.authorityAdapter),
      rebindSource: this.authorityAdapter.rebindSource.bind(this.authorityAdapter),
      verifyPhysicalLinks: this.authorityAdapter.verifyPhysicalLinks.bind(this.authorityAdapter),
      quarantine: this.authorityAdapter.quarantine.bind(this.authorityAdapter),
    };
    this.backupOperations.set(operationId, { state: "running" });
    try { await backupOwned({ root: this.installation.root, destination, operationId, compatibility: { schema_versions: ["anamnesis.storage/1"], neo4j_versions: [manifest.compatibility.neo4j_version], neo4j_image_digests: [manifest.compatibility.neo4j_image_digest], episode_digest_version_ceiling: 2 }, manifest, objectRoot }, cached); this.backupOperations.set(operationId, { state: "complete" }); return { state: "complete", operation_id: operationId }; }
    catch (error) { this.backupOperations.set(operationId, { state: "failed", error: String(error) }); throw error; }
  }
  async restore(context: InstallationContext, archive: string, operationId: string) {
    await this.installation.assertOwned();
    if (!context.client_binding) throw new RpcFault("unauthenticated", "authenticated connection custody is required");
    if (!this.authorityAdapter) throw new RpcFault("restore_adapter_unavailable", "offline restore adapter is not installed");
    const liveRoot = this.installation.root;
    const stagingRoot = `${liveRoot}.restore-staging.${operationId}`;
    const rollbackRoot = `${liveRoot}.restore-rollback.${operationId}`;
    const compatibility = { schema_versions: ["anamnesis.storage/1"], neo4j_versions: ["5.26.30"], neo4j_image_digests: ["sha256:037cf5756f0135cbfd66b739b6df7c7c4bb100f9ce11602f6f9538e17e02c74d"], episode_digest_version_ceiling: 2 as const };
    this.restoreOperations.set(operationId, { state: "running" });
    try { const result = await restoreOwned({ archive, liveRoot, stagingRoot, rollbackRoot, operationId, compatibility, expectedSourceId: this.installation.incarnation }, this.authorityAdapter); this.restoreOperations.set(operationId, { state: "complete" }); return { state: "complete", operation_id: operationId, manifest: result.manifest }; }
    catch (error) { this.restoreOperations.set(operationId, { state: "failed", error: String(error) }); throw error; }
  }
  async backupStatus(operation_id: string) {
    const state = this.backupOperations.get(operation_id);
    if (!state && !this.authorityAdapter) return { state: "unknown" as const, operation_id, reason: "adapter_unavailable" as const };
    return state ? { state: state.state, operation_id, ...(state.error ? { error: state.error } : {}) } : { state: "unknown" as const, operation_id, reason: "not_found" as const };
  }
  async restoreStatus(operation_id: string) {
    const state = this.restoreOperations.get(operation_id);
    if (!state && !this.authorityAdapter) return { state: "unknown" as const, operation_id, reason: "adapter_unavailable" as const };
    return state ? { state: state.state, operation_id, ...(state.error ? { error: state.error } : {}) } : { state: "unknown" as const, operation_id, reason: "not_found" as const };
  }
  private async requireStorage(): Promise<void> {
    await this.refresh();
    if (!this.available) throw new RpcFault("storage_unavailable", "database unavailable", true);
  }
  async graphEnvelope(params: { seed_ids: string[]; T?: number }, context: InstallationContext) {
    await this.installation.assertOwned();
    await this.requireStorage();
    return this.engine.graphEnvelope(params.seed_ids, params.T === undefined ? {} : { T: params.T }, context);
  }

  async admitDream(params: RpcDreamAdmitParams, context: InstallationContext) { await this.requireStorage(); return this.engine.store.admitDream(params, context); }
  async dreamStatus(id: string, context: InstallationContext) { await this.requireStorage(); return this.engine.store.dreamStatus(id, context); }
  async leaseDream(params: RpcDreamLeaseParams, context: InstallationContext) { await this.requireStorage(); return this.engine.store.leaseDream(params, context); }
  async expireDream(params: RpcDreamExpireParams, context: InstallationContext) { await this.requireStorage(); return this.engine.store.expireDream(params, context); }
  async executeDream(params: RpcDreamExecuteParams, context: InstallationContext) { await this.requireStorage(); return this.engine.store.executeDream(params, context); }

  async createExtractionPipeline(params: CreateExtractionPipeline, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.createExtractionPipeline(params,context);
  }
  async runExtractionPipeline(params: RunExtractionPipeline, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.runExtractionPipeline(params,context);
  }
  async extractionPipelineStatus(id: string, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.store.readExtractionPipeline(id,context);
  }

  async recall(params: RpcRecallParams, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.recallHybrid(params, context);
  }
  async recordRecallTransport(input: RecallTransportInput, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.recordRecallTransport(input, context);
  }
  async exposeRecall(recallId: string, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.exposeRecall(recallId, context);
  }
  async recoverEmbedding(params: RpcEmbeddingRecoverParams, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.recoverEmbedding(params, context);
  }
  async embeddingStatus(operationId: string, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.embeddingStatus(operationId, context);
  }
  async commit(params: CommitReceiptInput, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.commitReceipt(params, context);
  }
  async setPolicy(params: RpcPolicySetParams, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.setPolicy(params, context);
  }
  async revokePolicy(params: RpcPolicyRevokeParams, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.revokePolicy(params, context);
  }
  async verifyHitCache() {
    await this.requireStorage();
    const result = await this.engine.verifyHitCache();
    if (result.issues.length > 1024) throw new RpcFault("resource_exhausted", "hit-cache verification exceeds the response limit");
    return result;
  }
  async rebuildHitCache() {
    await this.requireStorage();
    return this.engine.rebuildHitCache();
  }
  async close(): Promise<void> {
    this.cancelDrain();
    await this.drainJob?.return(); this.drainJob = undefined;
    await this.extraction?.close(); // In-flight pipelines settle before the writer goes away.
    await this.uploads.close(); await this.engine.close(); await this.reader.close();
  }
}
