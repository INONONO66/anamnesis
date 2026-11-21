import { randomUUID } from "node:crypto";
import { IdentityScheduler, type Engine, type ExtractionProvider, type IdentityDecision, type IdentityTurn } from "@anamnesis/core";
import type {
  RpcIdentityCorrectParams, RpcIdentityDirectoryParams, RpcIdentityRebuildParams, RpcIdentityStatusResult,
  RpcPeopleListParams, RpcPeopleShowParams,
} from "@anamnesis/protocol";
import { RpcFault } from "./wire.ts";

interface IdentityRuntimeOptions {
  readonly engine: Engine;
  readonly provider: ExtractionProvider | undefined;
  readonly owners: readonly string[];
  readonly requireStorage: () => Promise<void>;
  readonly schedule: () => void;
}

export class IdentityRuntime {
  private readonly scheduler: IdentityScheduler;
  private requested = false;

  constructor(private readonly options: IdentityRuntimeOptions) {
    this.scheduler = new IdentityScheduler(options.engine.store.identity, {
      provider: options.provider, owners: options.owners,
      context: Object.freeze({ principal: "installation", commit_mode: "auto", client_binding: randomUUID() }),
      wake: () => this.wake(),
    });
  }

  wake(): void {
    this.requested = true;
    this.options.schedule();
  }

  async turn(): Promise<IdentityTurn> {
    if (!this.requested) return this.scheduler.inFlightCount ? "waiting" : "idle";
    this.requested = false;
    const outcome = await this.scheduler.turn();
    if (outcome === "more") this.requested = true;
    return outcome;
  }

  async status(): Promise<RpcIdentityStatusResult> {
    await this.options.requireStorage();
    const stored = await this.options.engine.store.identity.status(), lane = this.scheduler.status();
    return {
      covered_ingest_seq: stored.covered_ingest_seq, live_ingest_seq: stored.live_ingest_seq, generation: stored.generation,
      accounts: stored.accounts, owners: lane.owners, bots: lane.bots, persons: stored.persons,
      decisions: { rule: lane.rules, ...stored.decisions }, pending: lane.pending, skipped_keys: lane.skipped_keys,
      judge: lane.judge, in_flight: lane.in_flight, last_error: lane.last_error,
    };
  }

  async rebuild(params: RpcIdentityRebuildParams) {
    await this.options.requireStorage();
    if (!params.accounts) return this.scheduler.rebuild();
    await this.options.engine.store.identity.resetAccounts();
    this.scheduler.invalidate(); this.wake();
    return { generation: (await this.options.engine.store.identity.status()).generation, persons: 0, accounts: 0, built: false };
  }

  async correct(params: RpcIdentityCorrectParams) {
    await this.options.requireStorage();
    const keys = await this.options.engine.store.identity.accountsExist([params.a, params.b]);
    if (!keys) throw new RpcFault("invalid_selection", "unknown identity account");
    const [a, b] = [...keys].sort() as [string, string];
    const decision: IdentityDecision = { id: randomUUID(), a, b, verdict: params.verdict, confidence: 1, authority: "operator",
      reason: params.reason ?? "operator correction", model: null, evidence_digest: "", decided_at: Date.now() };
    await this.options.engine.store.identity.writeDecisions([decision]);
    this.scheduler.invalidate(); this.wake();
    return { decision_id: decision.id };
  }

  async directory(params: RpcIdentityDirectoryParams) {
    await this.options.requireStorage();
    const result = await this.options.engine.store.identity.importDirectory(params);
    this.scheduler.invalidate(); this.wake();
    return result;
  }

  async list(params: RpcPeopleListParams) {
    await this.options.requireStorage();
    return this.options.engine.store.identity.listPeople(params);
  }

  async show(params: RpcPeopleShowParams) {
    await this.options.requireStorage();
    return this.options.engine.store.identity.showPerson(params);
  }
}
