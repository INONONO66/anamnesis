import { expect, test } from "bun:test";
import { v5 as uuidv5 } from "uuid";
import {
  identityEvidenceDigest, identityJudgeInput, identityPeople, nameKeys, planIdentity,
  type IdentityAccount, type IdentityDecision,
} from "./identity.ts";

const account = (key: string, fields: Partial<IdentityAccount> = {}): IdentityAccount => ({
  key, source: "kakao", actor: key, names: [], owner: false, bot: false, episodes: 1, sessions: [], directory: null, ...fields,
});
const session = (session_key: string, first_utc: string, last_utc: string) => ({ session_key, first_utc, last_utc });
let sequence = 0;
const decision = (a: string, b: string, fields: Partial<IdentityDecision>): IdentityDecision => ({
  id: `d${++sequence}`, a, b, verdict: "same", confidence: 0.9, authority: "judge", reason: "r", model: "m",
  evidence_digest: "", decided_at: sequence, ...fields,
});
const groupsOf = (accounts: IdentityAccount[], decisions: IdentityDecision[] = []) => planIdentity(accounts, decisions).groups;

test("a bracketed handle and the name outside it are keys of their own; one code point is not a key", () => {
  expect(nameKeys("민웅기 (minpeter)")).toEqual(["minpeter", "민웅기", "민웅기minpeter"]);
  expect(nameKeys("INO (서희원)")).toEqual(["ino", "ino서희원", "서희원"]);
  expect(nameKeys("Q")).toEqual([]);
});

test("same-named ids that talk in one conversation stay apart without a judge call", () => {
  const a = account("a", { names: ["Q Kim"], sessions: [session("room", "2026-01-01T00:00:00.000Z", "2026-03-01T00:00:00.000Z")] });
  const b = account("b", { names: ["Q Kim"], sessions: [session("room", "2026-02-01T00:00:00.000Z", "2026-04-01T00:00:00.000Z")] });
  const plan = planIdentity([a, b], [decision("a", "b", { verdict: "same", confidence: 0.99 })]);
  expect(plan.groups).toEqual([["a"], ["b"]]);
  expect(plan.pending).toEqual([]);
  expect(plan.rules).toBe(1);
});

test("a judge merge needs 0.85; below it, or unsure, accounts stay apart", () => {
  const a = account("a", { names: ["Q Kim"], sessions: [session("r1", "2026-01-01T00:00:00.000Z", "2026-01-02T00:00:00.000Z")] });
  const b = account("b", { names: ["Q Kim"], sessions: [session("r2", "2026-01-01T00:00:00.000Z", "2026-01-02T00:00:00.000Z")] });
  expect(planIdentity([a, b], []).pending.map(pair => [pair.a, pair.b, pair.keys])).toEqual([["a", "b", ["name:qkim"]]]);
  expect(groupsOf([a, b], [decision("a", "b", { confidence: 0.84 })])).toEqual([["a"], ["b"]]);
  expect(groupsOf([a, b], [decision("a", "b", { verdict: "unsure" })])).toEqual([["a"], ["b"]]);
  expect(groupsOf([a, b], [decision("a", "b", { confidence: 0.85 })])).toEqual([["a", "b"]]);
});

test("an unsure verdict is judged again only when the evidence changed", () => {
  const a = account("a", { names: ["Q Kim"] }), b = account("b", { names: ["Q Kim"] });
  const digest = identityEvidenceDigest(a, b);
  expect(planIdentity([a, b], [decision("a", "b", { verdict: "unsure", evidence_digest: digest })]).pending).toEqual([]);
  expect(planIdentity([a, b], [decision("a", "b", { verdict: "unsure", evidence_digest: "old" })]).pending).toHaveLength(1);
  expect(planIdentity([a, b], [decision("a", "b", { verdict: "same", evidence_digest: "old" })]).pending).toEqual([]);
});

test("owner accounts join without a shared name; bots never pair", () => {
  const kakao = account("k", { owner: true, names: ["서희원"] }), agent = account("u", { source: "omo", actor: "user", owner: true });
  const slack = account("s", { source: "slack", owner: true, names: ["INO (서희원)"] });
  const bot = account("x", { source: "slack", bot: true, names: ["서희원"] });
  expect(groupsOf([kakao, agent, slack, bot])).toEqual([["k", "s", "u"], ["x"]]);
});

test("the latest operator decision overrides rules and the judge", () => {
  const a = account("a", { names: ["Q Kim"], sessions: [session("room", "2026-01-01T00:00:00.000Z", "2026-03-01T00:00:00.000Z")] });
  const b = account("b", { names: ["Q Kim"], sessions: [session("room", "2026-02-01T00:00:00.000Z", "2026-04-01T00:00:00.000Z")] });
  expect(groupsOf([a, b], [decision("a", "b", { authority: "operator", verdict: "same", confidence: 1 })])).toEqual([["a", "b"]]);
  const c = account("c", { names: ["Lee"] }), d = account("d", { names: ["Lee"] });
  expect(groupsOf([c, d], [decision("c", "d", { confidence: 0.99 }), decision("c", "d", { authority: "operator", verdict: "different" })]))
    .toEqual([["c"], ["d"]]);
  expect(groupsOf([c, d], [decision("c", "d", { authority: "operator", verdict: "different" }), decision("c", "d", { authority: "operator", verdict: "same" })]))
    .toEqual([["c", "d"]]);
});

test("a stronger edge wins when merges would join two accounts that must stay apart", () => {
  const [a, b, c] = ["a", "b", "c"].map(key => account(key, { names: ["Park"] })) as [IdentityAccount, IdentityAccount, IdentityAccount];
  const groups = groupsOf([a, b, c], [
    decision("a", "b", { confidence: 0.9 }), decision("b", "c", { confidence: 0.95 }),
    decision("a", "c", { authority: "operator", verdict: "different" }),
  ]);
  expect(groups).toEqual([["a"], ["b", "c"]]);
});

test("a name held by more than twelve accounts is too generic to pair on", () => {
  const accounts = Array.from({ length: 13 }, (_, i) => account(`a${String(i).padStart(2, "0")}`, { names: ["사용자"] }));
  const plan = planIdentity(accounts, []);
  expect(plan.skipped_keys).toBe(1);
  expect(plan.pending).toEqual([]);
});

test("a shared directory email merges by rule", () => {
  const directory = (email: string) => ({ names: [], handles: [], emails: [email] });
  const slack = account("s", { source: "slack", directory: directory("Ino@Vooy.com ") });
  const discord = account("d", { source: "discord", directory: directory("ino@vooy.com") });
  expect(groupsOf([slack, discord])).toEqual([["d", "s"]]);
});

test("a person's id is stable on its smallest member and its name is the most used one", () => {
  const people = identityPeople([["a", "b"]], [
    account("a", { names: ["Q"], episodes: 3 }), account("b", { names: ["Q Kim", "Q"], episodes: 10, owner: true }),
  ]);
  expect(people).toEqual([{ id: uuidv5("a", uuidv5.URL), name: "Q", owner: true, members: ["a", "b"], aliases: ["Q", "Q Kim"], sources: ["kakao"] }]);
});

test("judge input carries no ids or emails and truncated samples", () => {
  const a = account("a", { actor: "U123", names: ["Q Kim"], directory: { names: [], handles: [], emails: ["q@x.com"] },
    sessions: [session("room", "2026-01-05T10:00:00.000Z", "2026-02-01T00:00:00.000Z")] });
  const b = account("b", { actor: "U456", names: ["Q Kim"], directory: { names: [], handles: [], emails: ["q@x.com"] }, sessions: [] });
  const input = identityJudgeInput({ a: "a", b: "b", keys: ["email:q@x.com", "name:qkim"] }, a, b, { a: ["x".repeat(400)], b: [] }, { a: 1, b: 0 });
  const wire = JSON.stringify(input);
  for (const secret of ["U123", "U456", "q@x.com"]) expect(wire).not.toContain(secret);
  expect(input.shared).toEqual({ session: false, email: true });
  expect(input.a.samples[0]).toHaveLength(280);
  expect([input.a.first, input.a.last]).toEqual(["2026-01-05", "2026-02-01"]);
});
