import { afterAll, beforeAll, expect, test } from "bun:test";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import type { MemoryElement } from "@anamnesis/protocol";
import { Engine } from "./engine.ts";
import { IdentityScheduler } from "./identity-scheduler.ts";
import type { IdentityJudgeInput } from "./identity.ts";

const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("Isolated test database credentials required");
const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
const root = await mkdtemp(join(tmpdir(), "identity-"));
const engine = new Engine({ uri, password, objectsRoot: root });
const context = { principal: "installation", client_binding: uuidv7() } as const;
let minute = 0;
const at = () => new Date(Date.UTC(2026, 8, 1) + ++minute * 60_000).toISOString().replace(".000", "");
const remember = (source: string, session: string, actor: string, content: string, properties: MemoryElement["properties"] = {}, role: "user" | "assistant" | "tool" = "user") =>
  engine.remember({ content, time: { value: at(), precision: "second" }, origin: { source, session, actor, record: randomUUID() }, properties },
    { metadata: { origin_role: role, lineage_mode: "direct", parent_recall_ids: [] }, context });

beforeAll(async () => { await engine.init(); await engine.claimWriterEpoch(); }, 120000);
afterAll(async () => { await engine.close(); await driver.close(); await rm(root, { recursive: true }); });

test("accounts group by owner rules, co-presence and the judge; operator decisions and rebuilds keep them", async () => {
  const tag = randomUUID().slice(0, 8), kakao = `kakao-${tag}`, slack = `slack-${tag}`, omo = `omo-${tag}`;
  const [lo, q100, q101, q102] = ["208", "q100", "q101", "q102"].map(actor => `${actor}-${tag}`) as [string, string, string, string];
  const store = engine.store.identity;
  await remember(kakao, "room1", lo, `I will take the demo ${tag}`, { speaker_name: "서희원", mine: true });
  await remember(kakao, "room1", q100, `Q-main first words ${tag}`, { speaker_name: "Q Kim" });
  await remember(kakao, "room1", q102, `Q-other first words ${tag}`, { speaker_name: "Q Kim" });
  await remember(kakao, "room1", q100, `Q-main again here ${tag}`, { speaker_name: "Q Kim" });
  await remember(kakao, "room1", q102, `Q-other again here ${tag}`, { speaker_name: "Q Kim" });
  await remember(kakao, "room2", q101, `Q-main in room two ${tag}`, { speaker_name: "Q Kim" });
  const secret = await remember(kakao, "room2", q101, `secret-sample ${tag}`, { speaker_name: "Q Kim" });
  await remember(slack, "general", "U1", `ino on slack ${tag}`, { speaker_name: "INO (서희원)" });
  await remember(slack, "general", "B1", `deploy finished ${tag}`, { speaker_name: "서희원" }, "tool");
  await remember(omo, "session", "user", `fix the loader ${tag}`);
  await remember(omo, "session", "assistant", `I fixed the loader ${tag}`, {}, "assistant");
  await engine.setPolicy({ policy_id: uuidv7(), selector: { episode_id: secret.id }, scope: "content" }, context);

  const calls: IdentityJudgeInput[] = [];
  const judgeIdentity = async (input: IdentityJudgeInput) => {
    calls.push(input);
    const main = (side: IdentityJudgeInput["a"]) => side.samples.some(sample => sample.includes("Q-main"));
    return main(input.a) && main(input.b)
      ? { verdict: "same", confidence: 0.95, reason: "same writing" } : { verdict: "different", confidence: 0.8, reason: "different writing" };
  };
  const scheduler = new IdentityScheduler(store, { provider: { model: "fake-judge", judgeIdentity }, context, owners: [`${slack}:U1`], wake: () => {} });
  const drain = async () => {
    for (;;) {
      const outcome = await scheduler.turn();
      if (outcome === "idle") return;
      if (outcome === "waiting") await scheduler.settle();
    }
  };
  const members = async (source: string, actor: string) =>
    (await store.showPerson({ account: { source, actor } })).accounts.map(account => `${account.source}:${account.actor}`).sort();

  await drain();

  const ours = calls.filter(call => JSON.stringify(call).includes(tag));
  expect(ours).toHaveLength(2);
  for (const call of ours) {
    const wire = JSON.stringify(call);
    for (const hidden of [q100, q101, q102, "secret-sample"]) expect(wire).not.toContain(hidden);
  }
  expect(await members(kakao, q100)).toEqual([`${kakao}:${q100}`, `${kakao}:${q101}`].sort());
  expect(await members(kakao, q102)).toEqual([`${kakao}:${q102}`]);
  const owner = await store.showPerson({ account: { source: kakao, actor: lo } });
  expect(owner.person?.owner).toBe(true);
  expect(owner.accounts.map(account => `${account.source}:${account.actor}`)).toEqual(expect.arrayContaining([`${kakao}:${lo}`, `${slack}:U1`, `${omo}:user`]));
  expect(await members(slack, "B1")).toEqual([`${slack}:B1`]);
  expect(await members(omo, "assistant")).toEqual([`${omo}:assistant`]);
  const judged = await store.showPerson({ account: { source: kakao, actor: q101 } });
  expect(judged.decisions.map(d => [d.authority, d.verdict])).toEqual(expect.arrayContaining([["judge", "same"], ["judge", "different"]]));
  expect((await store.listPeople({ query: "q kim", limit: 200 })).people.some(person => person.id === judged.person?.id)).toBe(true);

  const before = await store.showPerson({ account: { source: kakao, actor: q100 } });
  const [a, b] = [before.accounts.find(x => x.actor === q100)!, before.accounts.find(x => x.actor === q101)!];
  const keys = await store.accountsExist([{ source: a.source, actor: a.actor }, { source: b.source, actor: b.actor }]);
  const [ka, kb] = [...keys!].sort() as [string, string];
  await store.writeDecisions([{ id: uuidv7(), a: ka, b: kb, verdict: "different", confidence: 1, authority: "operator",
    reason: "operator correction", model: null, evidence_digest: "", decided_at: Date.now() }]);
  scheduler.invalidate();
  await drain();
  const split = await store.showPerson({ account: { source: kakao, actor: q100 } });
  expect(split.accounts.map(account => account.actor)).toEqual([q100]);
  const generations = await driver.executeQuery("MATCH (p:Person) RETURN collect(DISTINCT p.generation) AS g");
  expect(generations.records[0]!.get("g")).toHaveLength(1);

  await store.resetAccounts();
  scheduler.invalidate();
  await drain();
  expect(await members(kakao, q100)).toEqual([`${kakao}:${q100}`]);
  expect((await store.showPerson({ account: { source: kakao, actor: q100 } })).person?.id).toBe(split.person!.id);
  expect(calls.filter(call => JSON.stringify(call).includes(tag))).toHaveLength(2);
}, 120000);

test("a shared directory email joins accounts across sources by rule", async () => {
  const tag = randomUUID().slice(0, 8), store = engine.store.identity;
  await remember(`slack-${tag}`, "c", "U9", `hello ${tag}`, { speaker_name: `Dana ${tag}` });
  await remember(`discord-${tag}`, "c", "D9", `hi ${tag}`, { speaker_name: `dana_${tag}` });
  const scheduler = new IdentityScheduler(store, { context, wake: () => {} });
  while ((await scheduler.turn()) === "more");
  expect((await store.showPerson({ account: { source: `slack-${tag}`, actor: "U9" } })).accounts).toHaveLength(1);
  expect(await store.importDirectory({ entries: [
    { source: `slack-${tag}`, actor: "U9", names: [], handles: [], emails: [`dana-${tag}@example.com`] },
    { source: `discord-${tag}`, actor: "D9", names: [], handles: [], emails: [`Dana-${tag}@Example.com `] },
  ] })).toEqual({ imported: 2, changed: 2 });
  scheduler.invalidate();
  while ((await scheduler.turn()) === "more");
  const person = await store.showPerson({ account: { source: `discord-${tag}`, actor: "D9" } });
  expect(person.accounts.map(account => account.source).sort()).toEqual([`discord-${tag}`, `slack-${tag}`]);
  expect(scheduler.status()).toMatchObject({ judge: "unconfigured", in_flight: 0 });
}, 120000);
