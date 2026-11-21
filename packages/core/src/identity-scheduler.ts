import { v7 as uuidv7 } from "uuid";
import {
  IdentityJudgeOutput, identityJudgeInput, identityPeople, identityPeopleDigest, planIdentity,
  type IdentityDecision, type IdentityJudgeInput, type IdentityPendingPair,
} from "./identity.ts";
import type { IdentityStore, StoredIdentityAccount } from "./store/identity-store.ts";
import type { InstallationContext } from "./store/policy.ts";

export type IdentityTurn = "more" | "waiting" | "idle";
export interface IdentitySchedulerOptions {
  readonly provider?: { readonly model: string; readonly judgeIdentity?: ((input: IdentityJudgeInput) => Promise<unknown>) | undefined } | undefined;
  readonly context: InstallationContext;
  readonly owners?: readonly string[] | undefined;
  readonly wake: () => void;
  readonly clock?: () => number;
  readonly pageSize?: number;
}
const RETRY_BASE_MS = 60_000, RETRY_MAX_MS = 3_600_000;

export class IdentityScheduler {
  private readonly settled: IdentityDecision[] = [];
  private readonly failures = new Map<string, { count: number; until: number }>();
  private readonly owners: ReadonlySet<string>;
  private readonly clock: () => number;
  private inFlight: Promise<void> | null = null;
  private planNeeded = true;
  private lastError: string | null = null;
  private last = { pending: 0, rules: 0, skipped_keys: 0, owners: 0, bots: 0 };

  constructor(private readonly store: IdentityStore, private readonly options: IdentitySchedulerOptions) {
    this.owners = new Set(options.owners ?? []);
    this.clock = options.clock ?? Date.now;
  }

  get inFlightCount(): number { return this.inFlight ? 1 : 0; }

  invalidate(): void { this.planNeeded = true; }

  async settle(): Promise<void> { await this.inFlight; }

  status() {
    return { ...this.last, in_flight: this.inFlightCount, last_error: this.lastError,
      judge: this.options.provider?.judgeIdentity ? "configured" as const : "unconfigured" as const };
  }

  async turn(): Promise<IdentityTurn> {
    if (this.settled.length > 0) {
      await this.store.writeDecisions(this.settled.splice(0));
      this.planNeeded = true;
      return "more";
    }
    const scan = await this.store.scanPage(this.options.pageSize ?? 500);
    if (scan.advanced) {
      if (scan.scanned > 0) this.planNeeded = true;
      return "more";
    }
    if (!this.planNeeded) return this.inFlight ? "waiting" : "idle";
    this.planNeeded = false;
    const { accounts, pending } = await this.build();
    const next = this.next(pending);
    if (next) await this.start(next, accounts);
    return this.settled.length > 0 ? "more" : this.inFlight ? "waiting" : "idle";
  }

  async rebuild() {
    const { accounts, built, generation, persons } = await this.build();
    return { generation, persons, accounts: accounts.length, built };
  }

  private async build() {
    const accounts = await this.store.loadAccounts(this.owners);
    const plan = planIdentity(accounts, await this.store.loadDecisions());
    const people = identityPeople(plan.groups, accounts), digest = identityPeopleDigest(people);
    this.last = { pending: plan.pending.length, rules: plan.rules, skipped_keys: plan.skipped_keys,
      owners: accounts.filter(account => account.owner).length, bots: accounts.filter(account => account.bot).length };
    if (digest === await this.store.builtDigest()) {
      return { accounts, pending: plan.pending, built: false, generation: (await this.store.status()).generation, persons: people.length };
    }
    const result = await this.store.buildGeneration(people, digest);
    return { accounts, pending: plan.pending, built: true, ...result };
  }

  private next(pending: readonly IdentityPendingPair[]): IdentityPendingPair | undefined {
    if (!this.options.provider?.judgeIdentity || this.inFlight) return undefined;
    const now = this.clock();
    return pending.find(pair => (this.failures.get(`${pair.a}:${pair.b}`)?.until ?? 0) <= now);
  }

  private async start(pair: IdentityPendingPair, accounts: readonly StoredIdentityAccount[]): Promise<void> {
    const byKey = new Map(accounts.map(account => [account.key, account]));
    const a = byKey.get(pair.a)!, b = byKey.get(pair.b)!, provider = this.options.provider!;
    const samples = await this.store.judgeSamples({ a: a.sample_ids, b: b.sample_ids }, this.options.context);
    const input = identityJudgeInput(pair, a, b, samples, { a: a.session_count, b: b.session_count });
    const id = `${pair.a}:${pair.b}`;
    this.inFlight = (async () => {
      try {
        const output = IdentityJudgeOutput.parse(await provider.judgeIdentity!(input));
        this.settled.push({ id: uuidv7(), a: pair.a, b: pair.b, verdict: output.verdict, confidence: output.confidence, authority: "judge",
          reason: output.reason.slice(0, 512) || "no reason given", model: provider.model, evidence_digest: pair.digest, decided_at: this.clock() });
        this.failures.delete(id);
        this.lastError = null;
      } catch (error) {
        const count = (this.failures.get(id)?.count ?? 0) + 1, delay = Math.min(RETRY_BASE_MS * 2 ** (count - 1), RETRY_MAX_MS);
        this.failures.set(id, { count, until: this.clock() + delay });
        this.lastError = String(error).slice(0, 512);
        setTimeout(() => { this.planNeeded = true; this.options.wake(); }, delay).unref();
      } finally {
        this.inFlight = null;
        this.planNeeded = true;
        this.options.wake();
      }
    })();
  }
}
