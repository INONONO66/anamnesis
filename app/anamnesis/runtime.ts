import { randomUUID } from "node:crypto";
import { opendir, readdir, rm } from "node:fs/promises";
import { join } from "node:path";
import neo4j, { type Driver, type RecordShape } from "neo4j-driver";
import { Engine, envConfig, type EngineOptions } from "@anamnesis/core";
import { JevGate, OpenAiChatExtractionProvider } from "@anamnesis/core";
import { ExtractionScheduler, type ExtractionScope, type ExtractionTurn } from "@anamnesis/core";
import { EmptyClaimJudgeProvider, PacedExtractionProvider } from "@anamnesis/core";
import { OpenAiEmbeddingProvider } from "@anamnesis/core";
import { elementDigest, verifyLineageRetry, verifyEpisodeLineage } from "@anamnesis/core";
import { ExtractionAuditError, parseEpisodeLineage } from "@anamnesis/protocol";
import type { CreateExtractionPipeline, RunExtractionPipeline } from "@anamnesis/protocol";
import type { InstallationContext } from "@anamnesis/core";
import type { RpcPolicySetParams, RpcPolicyRevokeParams, RpcRecallParams, RpcProjectionParams, RpcReportOutcomeParams, RpcEmbeddingRecoverParams, RpcEmbeddingRequeueParams } from "@anamnesis/protocol";
import { RPC_LIMITS, RPC_METHODS, RpcRememberParams, type RpcCapabilities, type RpcCommittedResult, type RpcRememberBatchResult, type RpcExtractionPacing, type RpcStatusResult, type RpcWorkersStatus } from "@anamnesis/protocol";
import { EXTRACTION_PACING_DEFAULTS, hasCode, identityOwners, loadProviderConfig, syncDirectory, type ExtractionPacingConfig, type Installation } from "./config.ts";
import { Uploads, type UploadLifecycle } from "./objects.ts";
import { loadTokenizers } from "./tokenizer.ts";
import { daemonTiming, runtimeTimed, timingHash } from "./timing.ts";
import { fault, RpcFault, storageUnavailable } from "./wire.ts";
import { RECOVERY_PROBE_MS, RecoveryProbe, nodeTimers } from "./recovery-probe.ts";
import type { TrustedAuthorityAdapter } from "./backup-restore-orchestrator.ts";
import { backupOwned, restoreOwned } from "./backup-restore-orchestrator.ts";
import { fencedAdapter, manifestTemplate, objectInventory } from "./runtime-authority.ts";
import { logEvent } from "./log.ts";
import type { RpcSpacesImportParams, RpcSpacesListParams, RpcEpisodeRebuildParams } from "@anamnesis/protocol";
import type { RpcIdentityCorrectParams, RpcIdentityDirectoryParams, RpcIdentityRebuildParams, RpcPeopleListParams, RpcPeopleShowParams } from "@anamnesis/protocol";
import { IdentityRuntime } from "./identity-runtime.ts";
import { canonical, sha256 } from "./archive-manifest.ts";

function digest(params: RpcRememberParams): string { return sha256(canonical({ digest_version: 1, params })); }

export const capabilities: RpcCapabilities = { methods: [...RPC_METHODS], recall: true, policy: true, extraction: false, embeddings: false, projection: true, writer_fence: "database" };
type StoredEpisode = {
  id: string; schema: string; time_value: string; time_precision: string;
  content: string; mass: number; properties: string;
  origin_source: string; origin_session: string; origin_actor: string; origin_record: string;
  source_revision: string; previous_revision_key?: string; payload_hash?: string;
  ingest_seq: number; digest: string; digest_format?: string;
  episode_digest_version?: number; origin_role?: string; lineage_digest?: string;
};
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
export type BackgroundLane = "recovery" | "embedding" | "extraction" | "identity";
/** Outcome of one embedding turn. "stalled" keeps the wake pending for the next storage recovery; "waiting": a batch is
 * embedding off the serial slot and its end wakes the lane again. */
export type EmbeddingTurn = "more" | "idle" | "stalled" | "waiting";
/** Facts per embedding batch: one discovery scan inside the serial turn, then provider calls and commits outside it. */
const EMBEDDING_BATCH = 32;

export class Runtime {
  readonly capabilities: RpcCapabilities;
  /** Load asynchronous provider assets once, before accepting RPC traffic. */
  static async create(installation: Installation, scheduleBackground: (lane: BackgroundLane) => void = () => {}, uploadLifecycle: UploadLifecycle = {}, authorityOptions: RuntimeAuthorityOptions = {}): Promise<Runtime> {
    const providers: EngineOptions = {};
    let gate: JevGate | undefined;
    let gateThreshold = 0.2;
    let scope: ExtractionScope | undefined;
    let pacing = EXTRACTION_PACING_DEFAULTS;
    let extractionEnabled = true;
    if (process.env["ANAMNESIS_LLM_BASE_URL"] !== undefined || process.env["ANAMNESIS_EMBEDDING_BASE_URL"] !== undefined ||
      process.env["ANAMNESIS_EXTRACTION_LANE"] !== undefined ||
      process.env["ANAMNESIS_EXTRACTION_GATE"] === "jev") {
      const config = await loadProviderConfig();
      extractionEnabled = config.extractionLane === "on";
      pacing = config.extractionPacing;
      gateThreshold = config.gate.kind === "jev" ? config.gate.threshold : 0.2;
      scope = config.scope;
      if (config.llm.baseUrl !== undefined) {
        if (!config.llm.apiKey) throw new Error("ANAMNESIS_LLM_API_KEY_FILE required");
        providers.extractionProvider = new OpenAiChatExtractionProvider({
          ...config.llm, baseUrl: config.llm.baseUrl, apiKey: config.llm.apiKey, systemPrompt: config.systemPrompt,
          relationPrompt: config.relationPrompt, identityPrompt: config.identityPrompt, timeoutMs: 30000,
        });
      }
      if (config.gate.kind === "jev" && providers.extractionProvider) gate = await JevGate.create({ keyFile: config.gate.keyFile });
      if (config.embedding) {
        const { model, dimensions, baseUrl } = config.embedding;
        // Configuration identity only; an alias does not attest immutable server weights.
        const profile = { model, dimensions, model_incarnation: sha256(JSON.stringify([baseUrl, model, dimensions])),
          document_prefix: "", query_prefix: "", max_input_bytes: 65536, norm: "unit_l2" as const, norm_tolerance: 0.01 };
        providers.embeddingProvider = new OpenAiEmbeddingProvider({ ...config.embedding, profile, timeoutMs: 30000 });
      }
    }
    return new Runtime(installation, scheduleBackground, uploadLifecycle, authorityOptions, providers, pacing, gate, gateThreshold, scope, extractionEnabled);
  }
  readonly uploads: Uploads;
  private readonly engine: Engine;
  private readonly reader: Driver;
  private readonly database: string;
  private readonly authorityAdapter: TrustedAuthorityAdapter | undefined;
  private readonly backupOperations = new Map<string, { state: "running" | "complete" | "failed"; error?: string }>();
  private readonly restoreOperations = new Map<string, { state: "running" | "complete" | "failed"; error?: string }>();
  private epoch: number | undefined;
  private initialized = false;
  private available = false;
  private backgroundStopped = false;
  private readonly embedding = { requested: false, drained_total: 0, quarantined_total: 0, last_error: null as string | null };
  /** The batch embedding off the serial slot, and a failure it hands back to the next turn so the lane reports it. */
  private embeddingBatch: Promise<void> | undefined;
  private embeddingFailure: unknown;
  /** Present only with an extraction provider; the lane is otherwise never scheduled and reports unconfigured. */
  private readonly extraction: ExtractionScheduler | undefined;
  /** Wraps the configured provider so every model call (scheduler and audit RPCs alike) is metered; present with `extraction`. */
  private readonly pacer: PacedExtractionProvider | undefined;
  private extractionRequested = false;
  private readonly identityRuntime: IdentityRuntime;
  /** While storage is unavailable, the probe schedules a storage check on the daemon's serial queue. */
  private readonly recovery = new RecoveryProbe(() => { if (!this.backgroundStopped) this.scheduleBackground("recovery"); }, RECOVERY_PROBE_MS, nodeTimers);
  constructor(readonly installation: Installation, private readonly scheduleBackground: (lane: BackgroundLane) => void = () => {}, uploadLifecycle: UploadLifecycle = {}, authorityOptions: RuntimeAuthorityOptions = {}, providers: EngineOptions = {}, private readonly pacing: ExtractionPacingConfig = EXTRACTION_PACING_DEFAULTS, private readonly gate?: JevGate, private readonly gateThreshold = 0.2, private readonly scope?: ExtractionScope, private readonly extractionEnabled = true) {
    this.authorityAdapter = authorityOptions.authorityAdapter;
    const unpaced = { ...envConfig(), ...providers, tokenizers: loadTokenizers(), objectsRoot: join(installation.root, "objects") };
    this.pacer = unpaced.extractionProvider && new PacedExtractionProvider(unpaced.extractionProvider, { minIntervalMs: pacing.minIntervalMs, jitterFraction: pacing.jitterFraction });
    const config = this.pacer ? { ...unpaced, extractionProvider: new EmptyClaimJudgeProvider(this.pacer) } : unpaced;
    this.capabilities = { ...capabilities, extraction: extractionEnabled && !!config.extractionProvider, embeddings: !!config.embeddingProvider };
    // Same bounded deadlines as the reader: with defaults (30 s connect, 60 s acquisition) a background lane meeting a
    // dead Bolt endpoint held the serial dispatch slot past the 30 s socket idle deadline and dropped queued RPCs.
    const writer = neo4j.driver(config.uri, neo4j.auth.basic(config.user, config.password), {
      disableLosslessIntegers: true, connectionTimeout: 1000, connectionAcquisitionTimeout: 1500, maxTransactionRetryTime: 0,
    });
    // Attempt/quarantine state is an operational ledger on disk, never graph rows; Neo4j holds only memory.
    this.engine = new Engine({ ...config, driver: writer, embeddingLedgerPath: join(installation.root, "embedding-state.json"),
      extractionJournalPath: join(installation.root, "extraction-state.json"),
      materializationStatePath: join(installation.root, "materialization-state.json"), audit: (event, fields) => logEvent("info", event, fields) });
    this.extraction = extractionEnabled && config.extractionProvider ? new ExtractionScheduler(this.engine, { provider: config.extractionProvider, gate: this.gate,
      gateThreshold: this.gate ? this.gateThreshold : undefined, scope: this.scope, maxInFlight: pacing.maxInFlight,
      context: Object.freeze({ principal: "installation", client_binding: randomUUID() }),
      // A settled pipeline may have materialized Facts, and Facts are the embedding lane's input (D56): wake both lanes.
      read: (query, params) => this.read(query, params), wake: () => { this.wakeExtraction(); this.wakeEmbedding(); } }) : undefined;
    this.identityRuntime = new IdentityRuntime({ engine: this.engine, provider: config.extractionProvider, owners: identityOwners(),
      requireStorage: () => this.requireStorage(),
      schedule: () => { if (this.available && !this.backgroundStopped) this.scheduleBackground("identity"); } });
    this.database = config.database ?? "neo4j";
    this.reader = neo4j.driver(config.uri, neo4j.auth.basic(config.user, config.password), {
      disableLosslessIntegers: true, connectionTimeout: 1000, connectionAcquisitionTimeout: 1500, maxTransactionRetryTime: 0,
    });
    this.uploads = new Uploads(config.objectsRoot, join(installation.root, "uploads"), uploadLifecycle);
  }
  async init(): Promise<void> {
    const spool = join(this.installation.root, "spool");
    const directory = await opendir(spool).catch(error => { if (!hasCode(error, "ENOENT")) throw error; });
    if (directory) {
      try {
        if (await directory.read()) throw new Error(`legacy spool is not empty: ${spool}`);
      } finally { await directory.close(); }
    }
    const deliveries = join(this.installation.root, "deliveries");
    const leftovers = await readdir(deliveries, { withFileTypes: true }).catch(error => {
      if (!hasCode(error, "ENOENT")) throw error;
      return [];
    });
    let removed = 0;
    for (const entry of leftovers) {
      if (!entry.isFile() || !/^[a-f0-9]{64}\.json$/.test(entry.name)) continue;
      await rm(join(deliveries, entry.name));
      removed++;
    }
    if (removed) {
      await syncDirectory(deliveries);
      logEvent("info", "legacy_deliveries_cleaned", { path: deliveries, removed });
    }
    await this.uploads.init();
    await syncDirectory(this.installation.root);
    await this.refresh();
  }
  private async read<Row extends RecordShape>(query: string, params: Record<string, unknown> = {}): Promise<Row[]> {
    const session = this.reader.session({ database: this.database, defaultAccessMode: neo4j.session.READ });
    try { return (await runtimeTimed("neo4j.read", () => session.run<Row>(query, params, { timeout: 5000 }), daemonTiming ? timingHash(query) : undefined)).records.map(record => record.toObject()); }
    finally { await runtimeTimed("neo4j.session.close", () => session.close()); }
  }
  /** Reconnect runs on demand (status/remember/startup) and, while unavailable, on the recovery probe's bounded timer.
   * Given a revision key it also returns that revision's stored Episode row, read in the same round trip as the
   * writer-epoch check; nothing is returned while storage is unavailable. */
  async refresh(revision?: string): Promise<StoredEpisode | undefined> {
    await this.installation.assertOwned();
    try {
      // Once initialized, the writer-epoch read below doubles as the connectivity probe.
      if (!this.initialized) { await this.read("RETURN 1 AS connected"); await runtimeTimed("engine.init", () => this.engine.init()); this.initialized = true; }
      if (this.epoch === undefined) this.epoch = await runtimeTimed("engine.claimWriterEpoch", () => this.engine.claimWriterEpoch());
      const rows = await this.read<{ epoch: number; e: StoredEpisode | null }>("MATCH (m:Meta {key:'meta'}) OPTIONAL MATCH (e:Element:Episode {revision_key:$revision}) RETURN m.writer_epoch AS epoch, properties(e) AS e", { revision: revision ?? null });
      if (rows[0]?.epoch !== this.epoch) throw new RpcFault("ownership_lost", "database writer epoch changed");
      const recovered = !this.available;
      this.available = true;
      this.recovery.disarm();
      if (recovered) { this.wakeEmbedding(); this.wakeExtraction(); this.identityRuntime.wake(); }
      return rows[0]?.e ?? undefined;
    } catch (error) {
      if (!storageUnavailable(error)) throw error;
      this.markUnavailable();
      return undefined;
    }
  }
  /** Every outage observation goes through here so the recovery probe is armed regardless of which lane or RPC saw it first. */
  private markUnavailable(): void {
    this.available = false;
    this.recovery.arm();
  }
  /** The remember params a stored row implies: its digest version checked and a v2 row's retained lineage verified
   * against its stored digests, and against the delivery's own lineage when one is given. */
  private storedParams(row: StoredEpisode, delivered: RpcRememberParams): RpcRememberParams {
    if (row.episode_digest_version !== undefined && row.episode_digest_version !== 2)
      throw new RpcFault("unsupported_digest_version", "stored Episode version is unsupported");
    if (row.episode_digest_version !== 2 && row.digest_format !== undefined && row.digest_format !== "rfc8785-v1")
      throw new RpcFault("unsupported_digest_version", "stored digest format is unsupported");
    const lineage = row.episode_digest_version === 2 ? verifyEpisodeLineage(row.id, row.lineage_digest ?? null, row) : undefined;
    if (lineage) verifyLineageRetry(lineageMetadata(delivered), row.origin_role ?? null, lineage);
    return RpcRememberParams.parse({ episode: {
      schema: row.schema, time: { value: row.time_value, precision: row.time_precision },
      content: row.content, mass: row.mass, properties: JSON.parse(row.properties),
      origin: { source: row.origin_source, session: row.origin_session, actor: row.origin_actor, record: row.origin_record },
    }, source_revision: row.source_revision, expected_previous_revision_key: row.previous_revision_key ?? null,
    ...(row.payload_hash ? { payload_hash: row.payload_hash } : {}),
    ...(lineage ? { origin_role: row.origin_role, lineage_mode: lineage.lineage_mode, parent_recall_ids: lineage.parent_recall_ids } : {}) });
  }
  /** A stored row answers a delivery only when it verifies the complete envelope and its own core digest. */
  private rememberExisting(row: StoredEpisode, delivered: RpcRememberParams, identity: Pick<RpcCommittedResult, "revision_key" | "body_digest" | "data_incarnation">): RpcCommittedResult {
    const stored = this.storedParams(row, delivered);
    const candidate = row.episode_digest_version === 2
      ? { ...delivered, ...parseEpisodeLineage(lineageMetadata(delivered)) } : compatibilityParams(delivered);
    if (digest(stored) !== digest(candidate)) throw new RpcFault("revision_conflict", "stored revision has a different delivery body");
    // Verify the core digest as well as the complete RPC envelope (which also
    // binds mass and origin). A revision-only match can never yield success.
    const coreDigest = elementDigest(candidate.episode, { payloadHash: candidate.payload_hash ?? null,
      previousRevisionKey: candidate.expected_previous_revision_key, format: row.digest_format ?? null,
      episodeDigestVersion: row.episode_digest_version ?? null, originRole: row.origin_role ?? null, lineageDigest: row.lineage_digest ?? null });
    if (row.digest !== coreDigest) throw new RpcFault("revision_conflict", "stored digest does not verify");
    return { state: "committed", ...identity, id: row.id, created: false, ingest_seq: row.ingest_seq };
  }
  /** `remember` for each delivery in order, in one serial turn. A non-retryable refusal answers its delivery alone and the
   * batch goes on; any other failure ends the batch and fails the request, so no later delivery is admitted before an
   * earlier one. Deliveries admitted before it stay admitted and answer as stored when sent again. */
  async rememberBatch(deliveries: RpcRememberParams[], context?: InstallationContext): Promise<RpcRememberBatchResult> {
    const results: RpcRememberBatchResult["results"] = [];
    for (const params of deliveries) {
      try { results.push(await this.remember(params, context)); }
      catch (error) {
        const failure = fault(error);
        if (failure.retryable || failure.code === "ownership_lost" || failure.code === "internal_error") throw error;
        results.push({ state: "refused", code: failure.code, message: failure.message });
      }
    }
    return { results };
  }
  async remember(params: RpcRememberParams, context?: InstallationContext): Promise<RpcCommittedResult> {
    const origin = params.episode.origin;
    const originKey = sha256(JSON.stringify([origin.source, origin.session, origin.actor, origin.record]));
    const key = sha256(JSON.stringify([originKey, params.source_revision]));
    const identity = { revision_key: key, body_digest: digest(params), data_incarnation: this.installation.incarnation };
    // One read checks both the writer epoch and whether the store holds this revision.
    const stored = await this.refresh(key);
    if (stored) return this.rememberExisting(stored, params, identity);
    if (!this.available) throw new RpcFault("storage_unavailable", "database unavailable", true);

    let payload: { payload: Uint8Array<ArrayBuffer>; payload_media_type: string } | undefined;
    if (params.payload_hash) {
      const metadata = await this.uploads.metadata(params.payload_hash);
      if (!metadata) throw new RpcFault("object_not_found", "remember references an uncommitted object");
      payload = { payload: new Uint8Array(await this.uploads.store.get(params.payload_hash)), payload_media_type: metadata.media_type };
    }
    await this.installation.assertOwned();
    const metadata = lineageMetadata(params);
    let lineage: { metadata: typeof metadata; context: InstallationContext } | undefined;
    if (metadata) {
      if (!context?.client_binding) throw new RpcFault("lineage_binding_mismatch", "authenticated connection required");
      parseEpisodeLineage(metadata);
      lineage = { metadata, context };
    }
    try {
      const result = await runtimeTimed("engine.remember", () => this.engine.remember({ ...params.episode, source_revision: params.source_revision,
        expected_previous_revision_key: params.expected_previous_revision_key, ...payload }, lineage));
      if (result.created && result.ingest_seq !== undefined) {
        this.wakeEmbedding(); this.wakeExtraction(); this.identityRuntime.wake();
        return { state: "committed", ...identity, id: result.id, created: true, ingest_seq: result.ingest_seq };
      }
      const row = (await this.read<{ e: StoredEpisode }>("MATCH (e:Element:Episode {revision_key:$key}) RETURN properties(e) AS e", { key }))[0]?.e;
      if (!row || row.id !== result.id) throw new RpcFault("internal_error", "database did not expose the committed delivery");
      return this.rememberExisting(row, params, identity);
    } catch (error) {
      if (storageUnavailable(error)) this.markUnavailable();
      throw error;
    }
  }
  private wakeEmbedding(): void {
    if (!this.capabilities.embeddings) return; // Unconfigured: the worker is never scheduled and never reports.
    this.embedding.requested = true;
    if (this.available && !this.backgroundStopped) this.scheduleBackground("embedding");
  }
  /** Called only by the daemon's serial owner, never from a second writer. The owner alternates a background turn with
   * a queued request, so the turn's length bounds RPC latency during a backlog: the turn only finds the next batch of
   * Facts, and their provider calls run outside it (a turn that awaited the provider held `status` past the client
   * deadline). A failed batch is rethrown by the next turn, so the daemon logs it and handles ownership loss. */
  async embeddingTurn(): Promise<EmbeddingTurn> {
    if (this.embeddingFailure !== undefined) { const failure = this.embeddingFailure; this.embeddingFailure = undefined; throw failure; }
    if (this.backgroundStopped || !this.available) return "stalled";
    if (this.embeddingBatch) return "waiting";
    if (!this.embedding.requested) return "idle";
    // Consume the request before working: a remember committing mid-turn re-arms the lane, and that wake must survive
    // this turn's outcome. Clearing the flag at the end instead dropped such a wake once turns embedded one entry
    // each: the lane reported idle with the new entry still queued, and nothing retried it until the next commit.
    this.embedding.requested = false;
    try {
      await this.installation.assertOwned();
      const rows = await this.read<{ epoch: number }>("MATCH (m:Meta {key:'meta'}) RETURN m.writer_epoch AS epoch");
      if (rows[0]?.epoch !== this.epoch) throw new RpcFault("ownership_lost", "database writer epoch changed");
      const due = await runtimeTimed("engine.dueEmbeddings", () => this.engine.dueEmbeddings(EMBEDDING_BATCH));
      if (due === null) return "idle";
      if (due.length === 0) { this.embedding.last_error = null; return this.embedding.requested ? "more" : "idle"; }
      this.embeddingBatch = this.embedBatch(due);
      return "waiting";
    } catch (error) {
      if (storageUnavailable(error)) { this.markUnavailable(); this.embedding.requested = true; return "stalled"; } // Recovery re-schedules the pending wake.
      this.embedding.last_error = String(error).slice(0, 512);
      throw error;
    }
  }
  private wakeExtraction(): void {
    if (!this.extraction) return;
    this.extractionRequested = true;
    if (this.available && !this.backgroundStopped) this.scheduleBackground("extraction");
  }
  /** One bounded scheduler turn: database work only, provider calls stay in tracked in-flight pipelines. */
  async extractionTurn(): Promise<ExtractionTurn | "stalled"> {
    if (this.backgroundStopped || !this.available || !this.extraction) return "stalled";
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
      if (storageUnavailable(error)) { this.markUnavailable(); this.extractionRequested = true; return "stalled"; } // Recovery re-schedules the pending wake.
      this.extraction.recordError(error);
      throw error;
    }
  }
  async identityTurn(): Promise<import("@anamnesis/core").IdentityTurn | "stalled"> {
    if (this.backgroundStopped || !this.available) return "stalled";
    try {
      await this.installation.assertOwned();
      const rows = await this.read<{ epoch: number }>("MATCH (m:Meta {key:'meta'}) RETURN m.writer_epoch AS epoch");
      if (rows[0]?.epoch !== this.epoch) throw new RpcFault("ownership_lost", "database writer epoch changed");
      return await runtimeTimed("engine.identityTurn", () => this.identityRuntime.turn());
    } catch (error) {
      if (storageUnavailable(error)) { this.markUnavailable(); this.identityRuntime.wake(); return "stalled"; }
      throw error;
    }
  }
  private workers(pendingOutbox: number | null): RpcWorkersStatus {
    const { drained_total, quarantined_total, last_error } = this.embedding;
    const extraction = this.extraction && this.pacer ? { ...this.extraction.status(), pacing: this.pacingStatus(this.pacer) } : { state: "unconfigured" as const };
    return { embedding: { pending: pendingOutbox, drained_total, quarantined_total, last_error }, extraction };
  }
  private pacingStatus(pacer: PacedExtractionProvider): RpcExtractionPacing {
    const { calls_total, waited_total_ms } = pacer.stats();
    const { maxInFlight: max_in_flight, minIntervalMs: min_interval_ms, jitterFraction: jitter_fraction } = this.pacing;
    return { max_in_flight, min_interval_ms, jitter_fraction, calls_total, waited_total_ms };
  }
  /** A deferred Fact is a provider-side failure recorded in the embedding ledger with a retry_after in the future, so
   * the rescan after a batch moves on to the other Facts lacking a vector and the deferred one is retried on a later
   * wake, never in a loop of its own: the lane only rests once a rescan finds nothing due. A stop ends the batch
   * between Facts, so no continuation writes after it. */
  private async embedBatch(ids: string[]): Promise<void> {
    try {
      let deferred = 0, reason: string | null = null;
      for (const id of ids) {
        if (this.backgroundStopped) break;
        const step = await runtimeTimed("engine.embedOutbox", () => this.engine.embedOutbox([id]));
        this.embedding.drained_total += step.drained;
        this.embedding.quarantined_total += step.quarantined;
        deferred += step.deferred; reason = step.deferral_reason ?? reason;
      }
      this.embedding.last_error = deferred ? `${deferred} embeddings deferred: ${reason}` : null;
      this.embeddingBatch = undefined;
      this.wakeEmbedding();
    } catch (error) {
      this.embeddingBatch = undefined;
      if (storageUnavailable(error)) { this.markUnavailable(); this.embedding.requested = true; return; } // Recovery re-schedules the pending wake.
      this.embedding.last_error = String(error).slice(0, 512);
      this.embeddingFailure = error;
      if (!this.backgroundStopped) this.scheduleBackground("embedding");
    }
  }
  /** The active turn finishes; no worker continuation may write after stop. */
  stopBackground(): void { this.backgroundStopped = true; this.recovery.stop(); }
  async status(pending: number, stopping: boolean): Promise<RpcStatusResult> {
    await this.refresh();
    let outbox: number | null = null;
    if (this.available) {
      try { outbox = (await runtimeTimed("engine.status", () => this.engine.status())).pendingOutbox; }
      catch (error) { if (!storageUnavailable(error)) throw error; this.markUnavailable(); }
    }
    return { version: 2, state: stopping ? "stopping" : this.available ? "ready" : "degraded",
      storage: this.available ? "available" : "unavailable", data_incarnation: this.installation.incarnation,
      fs_epoch: this.installation.epoch, queue: { pending, capacity: RPC_LIMITS.queued_requests },
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
    const manifest = manifestTemplate(operationId, fenced.cutoff, authority, objects, sha256(config));
    const cached = fencedAdapter(this.authorityAdapter, fenced, authority);
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


  async createExtractionPipeline(params: CreateExtractionPipeline, context: InstallationContext) {
    await this.requireStorage();
    if (!this.extractionEnabled) throw new ExtractionAuditError("extraction_not_configured");
    return this.engine.createExtractionPipeline(params,context);
  }
  async runExtractionPipeline(params: RunExtractionPipeline, context: InstallationContext) {
    await this.requireStorage();
    if (!this.extractionEnabled) throw new ExtractionAuditError("extraction_not_configured");
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
  async projection(params: RpcProjectionParams, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.projection(params, context);
  }
  async reportOutcome(params: RpcReportOutcomeParams, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.reportOutcome(params, context);
  }
  async recoverEmbedding(params: RpcEmbeddingRecoverParams, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.recoverEmbedding(params, context);
  }
  async embeddingStatus(operationId: string, context: InstallationContext) {
    await this.requireStorage();
    return this.engine.embeddingStatus(operationId, context);
  }
  /** Operator requeue: quarantined ledger entries are dropped so their Facts are found again by the missing-vector
   * scan, and the lane wakes at once, which also retries any deferred Fact whose backoff elapsed. */
  async requeueQuarantinedEmbeddings(params: RpcEmbeddingRequeueParams, context: InstallationContext) {
    await this.requireStorage();
    const result = await this.engine.requeueQuarantinedEmbeddings(params, context);
    this.wakeEmbedding();
    return result;
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
  async importRoomAssignments(params: RpcSpacesImportParams) {
    await this.requireStorage();
    return this.engine.store.importRoomAssignments(params);
  }
  async listSpaces(params: RpcSpacesListParams) {
    await this.requireStorage();
    return this.engine.store.listSpaces(params);
  }
  async rebuildEpisodeDerived(params: RpcEpisodeRebuildParams, kind: "labels" | "replies") {
    await this.requireStorage();
    return this.engine.store.rebuildEpisodeDerived(params, kind);
  }
  identityStatus() { return this.identityRuntime.status(); }
  rebuildIdentity(params: RpcIdentityRebuildParams) { return this.identityRuntime.rebuild(params); }
  correctIdentity(params: RpcIdentityCorrectParams) { return this.identityRuntime.correct(params); }
  importIdentityDirectory(params: RpcIdentityDirectoryParams) { return this.identityRuntime.directory(params); }
  listPeople(params: RpcPeopleListParams) { return this.identityRuntime.list(params); }
  showPerson(params: RpcPeopleShowParams) { return this.identityRuntime.show(params); }
  async close(): Promise<void> {
    this.stopBackground();
    await this.extraction?.close(); // In-flight pipelines settle before the writer goes away.
    await this.embeddingBatch;
    await this.uploads.close(); await this.engine.close(); await this.reader.close();
  }
}
