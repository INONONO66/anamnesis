import neo4j from "neo4j-driver";
import { z } from "zod";
import {
  RpcPeopleListParams, RpcPeopleListResult, RpcPeopleShowParams, RpcPeopleShowResult,
} from "@anamnesis/protocol";
import type { IdentityDecision } from "../identity.ts";
import { tupleHash } from "./digest.ts";
import type { StoreCore } from "./core.ts";

const accountKey = (source: string, actor: string) => tupleHash([source, actor]);

export class IdentityPeopleStore {
  constructor(private readonly core: StoreCore) {}

  async list(input: z.input<typeof RpcPeopleListParams>) {
    const { after, limit, query } = RpcPeopleListParams.parse(input);
    return this.core.withReadTx(async tx => {
      const generation = (await tx.run(`MATCH (s:IdentityState {key:'identity'}) RETURN s.generation AS g`)).records[0]?.get("g") ?? 0;
      const rows = await tx.run(
        `MATCH (p:Person) WHERE p.generation = $g AND p.id > $after
           AND ($q IS NULL OR toLower(p.name) CONTAINS $q OR any(n IN p.aliases WHERE toLower(n) CONTAINS $q))
         WITH p ORDER BY p.id LIMIT $limit
         CALL (p) { MATCH (a:Account)-[:OF_PERSON]->(p) RETURN count(a) AS accounts,sum(a.episodes) AS episodes }
         RETURN p{.id,.name,.owner,.sources,.aliases} AS p,accounts,episodes ORDER BY p.id`,
        { g: generation, after: after ?? "", q: query?.toLowerCase() ?? null, limit: neo4j.int(limit + 1) });
      const people = rows.records.slice(0, limit).map(record => ({ ...record.get("p"), accounts: record.get("accounts"), episodes: record.get("episodes") }));
      return RpcPeopleListResult.parse({ people, next: rows.records.length > limit ? people.at(-1)?.id ?? null : null });
    });
  }

  async show(input: RpcPeopleShowParams) {
    const params = RpcPeopleShowParams.parse(input);
    return this.core.withReadTx(async tx => {
      const generation = (await tx.run(`MATCH (s:IdentityState {key:'identity'}) RETURN s.generation AS g`)).records[0]?.get("g") ?? 0;
      const person = (await tx.run(
        "person_id" in params
          ? `MATCH (p:Person {generation:$g,id:$id}) RETURN p{.*} AS p`
          : `MATCH (:Account {key:$key})-[:OF_PERSON]->(p:Person {generation:$g}) RETURN p{.*} AS p`,
        { g: generation, id: "person_id" in params ? params.person_id : null,
          key: "account" in params ? accountKey(params.account.source, params.account.actor) : null })).records[0]?.get("p") ?? null;
      const accounts = await tx.run(
        `MATCH (a:Account) WHERE ($pid IS NOT NULL AND EXISTS { (a)-[:OF_PERSON]->(:Person {generation:$g,id:$pid}) }) OR ($pid IS NULL AND a.key = $key)
         CALL (a) { OPTIONAL MATCH (s:AccountSession {account_key:a.key}) RETURN count(s) AS sessions,min(s.first_utc) AS first,max(s.last_utc) AS last }
         RETURN a{.key,.source,.actor,.names,.mine,.episodes,.tool_episodes} AS a,sessions,first,last ORDER BY a.episodes DESC,a.key LIMIT 256`,
        { g: generation, pid: person?.id ?? null, key: "account" in params ? accountKey(params.account.source, params.account.actor) : null });
      const members = accounts.records.map(record => record.get("a"));
      const keys = members.map(member => member.key as string);
      const decisions = (await tx.run(
        `MATCH (d:IdentityDecision) WHERE d.a IN $keys OR d.b IN $keys RETURN d{.*} AS d ORDER BY d.decided_at DESC,d.id LIMIT 64`, { keys }))
        .records.map(record => record.get("d") as IdentityDecision);
      const refs = new Map((await tx.run(
        `MATCH (a:Account) WHERE a.key IN $keys RETURN a.key AS key,a.source AS source,a.actor AS actor`,
        { keys: [...new Set(decisions.flatMap(d => [d.a, d.b]))] })).records.map(record => [record.get("key"), { source: record.get("source"), actor: record.get("actor") }]));
      return RpcPeopleShowResult.parse({
        person: person && { id: person.id, name: person.name, owner: person.owner, sources: person.sources, aliases: person.aliases,
          accounts: members.length, episodes: members.reduce((sum, member) => sum + member.episodes, 0) },
        accounts: accounts.records.map(record => {
          const a = record.get("a");
          return { source: a.source, actor: a.actor, names: a.names, owner: a.mine || a.actor === "user", bot: a.actor === "assistant" || a.tool_episodes * 2 > a.episodes,
            episodes: a.episodes, sessions: record.get("sessions"), first: record.get("first"), last: record.get("last") };
        }),
        decisions: decisions.filter(d => refs.has(d.a) && refs.has(d.b)).map(d => ({ id: d.id, a: refs.get(d.a), b: refs.get(d.b), verdict: d.verdict,
          confidence: d.confidence, authority: d.authority, reason: d.reason, decided_at: d.decided_at })),
      });
    });
  }
}
