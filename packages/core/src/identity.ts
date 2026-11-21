import { createHash } from "node:crypto";
import { v5 as uuidv5 } from "uuid";
import { z } from "zod";

const IDENTITY_MERGE_CONFIDENCE = 0.85;
const IDENTITY_SPLIT_CONFIDENCE = 0.5;
const IDENTITY_MAX_KEY_ACCOUNTS = 12;

type IdentityVerdict = "same" | "different" | "unsure";

interface IdentitySession { readonly session_key: string; readonly first_utc: string; readonly last_utc: string }
interface IdentityDirectory { readonly names: readonly string[]; readonly handles: readonly string[]; readonly emails: readonly string[] }
export interface IdentityAccount {
  readonly key: string;
  readonly source: string;
  readonly actor: string;
  readonly names: readonly string[];
  readonly owner: boolean;
  readonly bot: boolean;
  readonly episodes: number;
  readonly sessions: readonly IdentitySession[];
  readonly directory: IdentityDirectory | null;
}
export interface IdentityDecision {
  readonly id: string;
  readonly a: string;
  readonly b: string;
  readonly verdict: IdentityVerdict;
  readonly confidence: number;
  readonly authority: "judge" | "operator";
  readonly reason: string;
  readonly model: string | null;
  readonly evidence_digest: string;
  readonly decided_at: number;
}
export interface IdentityPair { readonly a: string; readonly b: string; readonly keys: readonly string[] }
export interface IdentityPendingPair extends IdentityPair { readonly digest: string }
export interface IdentityPerson {
  readonly id: string;
  readonly name: string;
  readonly owner: boolean;
  readonly members: readonly string[];
  readonly aliases: readonly string[];
  readonly sources: readonly string[];
}

const sha256 = (value: string) => createHash("sha256").update(value).digest("hex");
const byCodePoint = (x: string, y: string) => (x < y ? -1 : x > y ? 1 : 0);
const pairId = (a: string, b: string) => `${a}:${b}`;
const SEPARATORS = /[\s\p{P}\p{S}]+/gu;
const BRACKETED = /[([{\uFF08\uFF3B\uFF5B\u3010\u300C\u300E\u3008\u300A]([^)\]}\uFF09\uFF3D\uFF5D\u3011\u300D\u300F\u3009\u300B]*)[)\]}\uFF09\uFF3D\uFF5D\u3011\u300D\u300F\u3009\u300B]/gu;
const fold = (value: string) => value.normalize("NFKC").toLowerCase().replace(SEPARATORS, "");

export function nameKeys(name: string): string[] {
  const parts = [name, name.replace(BRACKETED, " "), ...[...name.matchAll(BRACKETED)].map(match => match[1] ?? "")];
  return [...new Set(parts.map(fold).filter(key => [...key].length >= 2))].sort(byCodePoint);
}

function blockingKeys(account: IdentityAccount): string[] {
  const names = [...account.names, ...(account.directory?.names ?? []), ...(account.directory?.handles ?? [])];
  const keys = new Set(names.flatMap(name => nameKeys(name)).map(key => `name:${key}`));
  for (const email of account.directory?.emails ?? []) keys.add(`email:${email.trim().toLowerCase()}`);
  return [...keys].sort(byCodePoint);
}

function identityCandidates(accounts: readonly IdentityAccount[]): { pairs: IdentityPair[]; skipped_keys: number } {
  const byKey = new Map<string, string[]>();
  for (const account of accounts) {
    if (account.bot) continue;
    for (const key of blockingKeys(account)) {
      const members = byKey.get(key);
      if (members) members.push(account.key); else byKey.set(key, [account.key]);
    }
  }
  const owners = accounts.filter(account => account.owner && !account.bot).map(account => account.key);
  if (owners.length > 1) byKey.set("owner", owners);
  const pairs = new Map<string, { a: string; b: string; keys: Set<string> }>();
  let skipped = 0;
  for (const [key, members] of byKey) {
    if (members.length < 2) continue;
    if (key !== "owner" && members.length > IDENTITY_MAX_KEY_ACCOUNTS) { skipped++; continue; }
    const sorted = [...new Set(members)].sort(byCodePoint);
    for (let i = 0; i < sorted.length; i++) for (let j = i + 1; j < sorted.length; j++) {
      const a = sorted[i]!, b = sorted[j]!, id = pairId(a, b);
      const pair = pairs.get(id) ?? { a, b, keys: new Set<string>() };
      pair.keys.add(key);
      pairs.set(id, pair);
    }
  }
  return {
    pairs: [...pairs.values()].map(({ a, b, keys }) => ({ a, b, keys: [...keys].sort(byCodePoint) }))
      .sort((x, y) => byCodePoint(x.a, y.a) || byCodePoint(x.b, y.b)),
    skipped_keys: skipped,
  };
}

function copresent(a: IdentityAccount, b: IdentityAccount): boolean {
  const intervals = new Map(a.sessions.map(session => [session.session_key, session]));
  return b.sessions.some(session => {
    const other = intervals.get(session.session_key);
    return other !== undefined && other.first_utc <= session.last_utc && session.first_utc <= other.last_utc;
  });
}

function identityRule(pair: IdentityPair, a: IdentityAccount, b: IdentityAccount): { verdict: "same" | "different"; confidence: number } | null {
  if (copresent(a, b)) return { verdict: "different", confidence: 1 };
  if (a.owner && b.owner) return { verdict: "same", confidence: 1 };
  if (pair.keys.some(key => key.startsWith("email:"))) return { verdict: "same", confidence: 0.99 };
  return null;
}

export function identityEvidenceDigest(a: IdentityAccount, b: IdentityAccount): string {
  const side = (account: IdentityAccount) => ({
    source: account.source, actor: account.actor, names: [...account.names].sort(byCodePoint),
    sessions: account.sessions.map(session => session.session_key).sort(byCodePoint),
    directory: account.directory && {
      names: [...account.directory.names].sort(byCodePoint), handles: [...account.directory.handles].sort(byCodePoint),
      emails: [...account.directory.emails].sort(byCodePoint),
    },
  });
  return sha256(JSON.stringify([side(a), side(b)]));
}

function latestDecisions(decisions: readonly IdentityDecision[], authority: IdentityDecision["authority"]): Map<string, IdentityDecision> {
  const latest = new Map<string, IdentityDecision>();
  for (const decision of decisions) {
    if (decision.authority !== authority) continue;
    const id = pairId(decision.a, decision.b), prior = latest.get(id);
    if (!prior || prior.decided_at < decision.decided_at || (prior.decided_at === decision.decided_at && prior.id < decision.id)) latest.set(id, decision);
  }
  return latest;
}

type Edge = { a: string; b: string; rank: number; confidence: number };

function constrainedGroups(accounts: readonly IdentityAccount[], edges: Edge[], cannot: ReadonlySet<string>): string[][] {
  const root = new Map(accounts.map(account => [account.key, account.key]));
  const members = new Map(accounts.map(account => [account.key, new Set([account.key])]));
  const blocked = new Map(accounts.map(account => [account.key, new Set<string>()]));
  for (const id of cannot) {
    const [a, b] = id.split(":") as [string, string];
    blocked.get(a)?.add(b); blocked.get(b)?.add(a);
  }
  const find = (key: string): string => {
    let current = key;
    while (root.get(current) !== current) current = root.get(current)!;
    root.set(key, current);
    return current;
  };
  edges.sort((x, y) => x.rank - y.rank || y.confidence - x.confidence || byCodePoint(x.a, y.a) || byCodePoint(x.b, y.b));
  for (const edge of edges) {
    const ra = find(edge.a), rb = find(edge.b);
    if (ra === rb || [...members.get(rb)!].some(key => blocked.get(ra)!.has(key))) continue;
    const [keep, drop] = ra < rb ? [ra, rb] : [rb, ra];
    for (const key of members.get(drop)!) members.get(keep)!.add(key);
    for (const key of blocked.get(drop)!) blocked.get(keep)!.add(key);
    root.set(drop, keep); members.delete(drop); blocked.delete(drop);
  }
  return [...members.values()].map(group => [...group].sort(byCodePoint)).sort((x, y) => byCodePoint(x[0]!, y[0]!));
}

type PairOutcome = { edge?: Edge; cannot?: boolean; pending?: IdentityPendingPair; rule?: boolean };

function pairOutcome(pair: IdentityPair, a: IdentityAccount, b: IdentityAccount, manual: IdentityDecision | undefined, judged: IdentityDecision | undefined): PairOutcome {
  const edge = (rank: number, confidence: number): Edge => ({ a: pair.a, b: pair.b, rank, confidence });
  if (manual) return manual.verdict === "same" ? { edge: edge(0, 1) } : { cannot: true };
  const rule = identityRule(pair, a, b);
  if (rule) return rule.verdict === "same" ? { rule: true, edge: edge(1, rule.confidence) } : { rule: true, cannot: true };
  const digest = identityEvidenceDigest(a, b);
  const outcome: PairOutcome = !judged || (judged.verdict === "unsure" && judged.evidence_digest !== digest) ? { pending: { ...pair, digest } } : {};
  if (judged?.verdict === "same" && judged.confidence >= IDENTITY_MERGE_CONFIDENCE) outcome.edge = edge(2, judged.confidence);
  if (judged?.verdict === "different" && judged.confidence >= IDENTITY_SPLIT_CONFIDENCE) outcome.cannot = true;
  return outcome;
}

export function planIdentity(accounts: readonly IdentityAccount[], decisions: readonly IdentityDecision[]) {
  const byKey = new Map(accounts.map(account => [account.key, account]));
  const { pairs, skipped_keys } = identityCandidates(accounts);
  const operator = latestDecisions(decisions, "operator"), judge = latestDecisions(decisions, "judge");
  const candidates = new Map(pairs.map(pair => [pairId(pair.a, pair.b), pair]));
  for (const decision of operator.values()) {
    const id = pairId(decision.a, decision.b);
    if (!candidates.has(id) && byKey.has(decision.a) && byKey.has(decision.b)) candidates.set(id, { a: decision.a, b: decision.b, keys: [] });
  }
  const cannot = new Set<string>(), edges: Edge[] = [], pending: IdentityPendingPair[] = [];
  let rules = 0;
  for (const [id, pair] of candidates) {
    const outcome = pairOutcome(pair, byKey.get(pair.a)!, byKey.get(pair.b)!, operator.get(id), judge.get(id));
    if (outcome.rule) rules++;
    if (outcome.edge) edges.push(outcome.edge);
    if (outcome.cannot) cannot.add(id);
    if (outcome.pending) pending.push(outcome.pending);
  }
  return { groups: constrainedGroups(accounts, edges, cannot), pending, rules, skipped_keys };
}

export function identityPeople(groups: readonly (readonly string[])[], accounts: readonly IdentityAccount[]): IdentityPerson[] {
  const byKey = new Map(accounts.map(account => [account.key, account]));
  return groups.map(group => {
    const members = group.map(key => byKey.get(key)!), weights = new Map<string, number>();
    for (const member of members) {
      for (const name of new Set([...member.names, ...(member.directory?.names ?? [])])) weights.set(name, (weights.get(name) ?? 0) + Math.max(member.episodes, 1));
    }
    const ranked = [...weights].sort((x, y) => y[1] - x[1] || byCodePoint(x[0], y[0])).map(([name]) => name);
    return {
      id: uuidv5(group[0]!, uuidv5.URL), name: ranked[0] ?? members[0]!.actor, owner: members.some(member => member.owner),
      members: [...group], aliases: ranked.slice(0, 8), sources: [...new Set(members.map(member => member.source))].sort(byCodePoint),
    };
  });
}

export function identityPeopleDigest(people: readonly IdentityPerson[]): string {
  return sha256(JSON.stringify(people));
}

interface IdentityJudgeAccount {
  readonly source: string;
  readonly names: readonly string[];
  readonly owner: boolean;
  readonly episodes: number;
  readonly sessions: number;
  readonly first: string | null;
  readonly last: string | null;
  readonly samples: readonly string[];
}
export interface IdentityJudgeInput {
  readonly task: "judge_identity";
  readonly a: IdentityJudgeAccount;
  readonly b: IdentityJudgeAccount;
  readonly shared: { readonly session: boolean; readonly email: boolean };
}
export const IdentityJudgeOutput = z.object({
  verdict: z.enum(["same", "different", "unsure"]), confidence: z.number().min(0).max(1), reason: z.string().max(2048),
});
export const identityJudgeSchema = z.toJSONSchema(z.strictObject({
  verdict: z.enum(["same", "different", "unsure"]), confidence: z.number(), reason: z.string(),
}), { target: "draft-2020-12" });

export function identityJudgeInput(pair: IdentityPair, a: IdentityAccount, b: IdentityAccount, samples: { a: readonly string[]; b: readonly string[] }, sessionCounts: { a: number; b: number }): IdentityJudgeInput {
  const side = (account: IdentityAccount, texts: readonly string[], sessions: number): IdentityJudgeAccount => ({
    source: account.source, names: account.names.slice(0, 5), owner: account.owner, episodes: account.episodes, sessions,
    first: account.sessions.reduce<string | null>((min, s) => (min === null || s.first_utc < min ? s.first_utc : min), null)?.slice(0, 10) ?? null,
    last: account.sessions.reduce<string | null>((max, s) => (max === null || s.last_utc > max ? s.last_utc : max), null)?.slice(0, 10) ?? null,
    samples: texts.map(text => text.slice(0, 280)),
  });
  const keys = new Set(a.sessions.map(session => session.session_key));
  return {
    task: "judge_identity",
    a: side(a, samples.a, sessionCounts.a), b: side(b, samples.b, sessionCounts.b),
    shared: { session: b.sessions.some(session => keys.has(session.session_key)), email: pair.keys.some(key => key.startsWith("email:")) },
  };
}
