import neo4j, { type ManagedTransaction } from "neo4j-driver";
import { z } from "zod";
import {
  isEpisodeSchema, IdentityAccountRef, RpcIdentityDirectoryParams,
  type RpcIdentityDirectoryParams as DirectoryInput, RpcPeopleListParams, type RpcPeopleShowParams,
} from "@anamnesis/protocol";
import type { IdentityAccount, IdentityDecision, IdentityPerson } from "../identity.ts";
import { tupleHash } from "./digest.ts";
import type { StoreCore } from "./core.ts";
import type { InstallationContext } from "./policy.ts";
import { ReceiptError } from "./receipts.ts";
import { IdentityPeopleStore } from "./identity-people-store.ts";

export interface StoredIdentityAccount extends IdentityAccount {
  readonly sample_ids: readonly string[];
  readonly session_count: number;
}
type AccountFold = {
  key: string; source: string; actor: string; names: string[]; mine: boolean; episodes: number; tool: number;
  first: string[]; last: string[];
};
type SessionFold = { account_key: string; session_key: string; session: string; episodes: number; first: string | null; last: string | null };

const NAME_BYTES = 256;
const encoder = new TextEncoder();
const accountKey = (source: string, actor: string) => tupleHash([source, actor]);
const earlier = (a: string | null, b: string | null) => (a === null ? b : b === null || a <= b ? a : b);
const later = (a: string | null, b: string | null) => (a === null ? b : b === null || a >= b ? a : b);

function speakerName(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const name = value.normalize("NFC").trim();
  return name && encoder.encode(name).byteLength <= NAME_BYTES ? name : null;
}

async function stateTx(tx: ManagedTransaction) {
  const rows = await tx.run<{ covered: number; generation: number; built: string }>(
    `MERGE (s:IdentityState {key:'identity'}) ON CREATE SET s.covered_ingest_seq=0,s.generation=0,s.built_digest=''
     RETURN s.covered_ingest_seq AS covered,s.generation AS generation,s.built_digest AS built`);
  return rows.records[0]!.toObject();
}

export class IdentityStore {
  private readonly people: IdentityPeopleStore;

  constructor(private readonly core: StoreCore) {
    this.people = new IdentityPeopleStore(core);
  }

  async scanPage(limit: number): Promise<{ scanned: number; advanced: boolean; covered: number; live: number }> {
    return this.core.withWriteTx(async tx => {
      const state = await stateTx(tx);
      const live = (await tx.run<{ live: number }>(`MATCH (m:Meta {key:'meta'}) RETURN m.ingest_seq AS live`)).records[0]?.get("live") ?? 0;
      if (state.covered >= live) return { scanned: 0, advanced: false, covered: state.covered, live };
      const page = await tx.run<{ id: string; schema: string; source: string; actor: string | null; session: string; session_key: string;
        role: string | null; time: string | null; properties: string | null; chars: number; seq: number }>(
        `MATCH (e:Episode) USING INDEX e:Episode(ingest_seq) WHERE e.ingest_seq > $after AND e.ingest_seq <= $live
         RETURN e.id AS id,e.schema AS schema,e.origin_source AS source,e.origin_actor AS actor,e.origin_session AS session,
           e.session_key AS session_key,e.origin_role AS role,e.time_utc AS time,e.properties AS properties,size(e.content) AS chars,e.ingest_seq AS seq
         ORDER BY e.ingest_seq LIMIT $limit`, { after: state.covered, live, limit: neo4j.int(limit) });
      const accounts = new Map<string, AccountFold>(), sessions = new Map<string, SessionFold>();
      let scanned = 0;
      for (const record of page.records) {
        const row = record.toObject();
        if (!isEpisodeSchema(row.schema) || row.actor === null) continue;
        scanned++;
        const key = accountKey(row.source, row.actor);
        const account = accounts.get(key) ?? { key, source: row.source, actor: row.actor, names: [], mine: false, episodes: 0, tool: 0, first: [], last: [] };
        const properties = (row.properties ? JSON.parse(row.properties) : {}) as Record<string, unknown>;
        const name = speakerName(properties["speaker_name"]);
        if (name !== null && !account.names.includes(name)) account.names.push(name);
        account.mine ||= properties["mine"] === true;
        account.episodes++;
        if (row.role === "tool") account.tool++;
        if (row.chars > 5) {
          if (account.first.length < 4) account.first.push(row.id);
          account.last = [...account.last, row.id].slice(-4);
        }
        accounts.set(key, account);
        const id = `${key}:${row.session_key}`;
        const session = sessions.get(id) ?? { account_key: key, session_key: row.session_key, session: row.session, episodes: 0, first: null, last: null };
        session.episodes++; session.first = earlier(session.first, row.time); session.last = later(session.last, row.time);
        sessions.set(id, session);
      }
      await tx.run(
        `UNWIND $rows AS row
         MERGE (a:Account {key:row.key})
         ON CREATE SET a.source=row.source,a.actor=row.actor,a.names=[],a.mine=false,a.episodes=0,a.tool_episodes=0,a.sample_first=[],a.sample_last=[]
         SET a.names=(a.names + [n IN row.names WHERE NOT n IN a.names])[0..32],a.mine=a.mine OR row.mine,
           a.episodes=a.episodes + row.episodes,a.tool_episodes=a.tool_episodes + row.tool,
           a.sample_first=(a.sample_first + row.first)[0..4],a.sample_last=(a.sample_last + row.last)[-4..]`,
        { rows: [...accounts.values()] });
      await tx.run(
        `UNWIND $rows AS row
         MERGE (s:AccountSession {account_key:row.account_key,session_key:row.session_key})
         ON CREATE SET s.session=row.session,s.episodes=0
         SET s.episodes=s.episodes + row.episodes,
           s.first_utc=CASE WHEN row.first IS NULL OR (s.first_utc IS NOT NULL AND s.first_utc <= row.first) THEN s.first_utc ELSE row.first END,
           s.last_utc=CASE WHEN row.last IS NULL OR (s.last_utc IS NOT NULL AND s.last_utc >= row.last) THEN s.last_utc ELSE row.last END`,
        { rows: [...sessions.values()] });
      const covered = page.records.length < limit ? live : page.records.at(-1)!.get("seq");
      await tx.run(`MATCH (s:IdentityState {key:'identity'}) SET s.covered_ingest_seq=$covered`, { covered });
      return { scanned, advanced: true, covered, live };
    });
  }

  async loadAccounts(owners: ReadonlySet<string>): Promise<StoredIdentityAccount[]> {
    return this.core.withReadTx(async tx => {
      const rows = await tx.run(
        `MATCH (a:Account)
         OPTIONAL MATCH (d:DirectoryEntry {source:a.source,actor:a.actor})
         CALL (a) {
           OPTIONAL MATCH (s:AccountSession {account_key:a.key})
           RETURN count(s) AS session_count,
             [x IN collect(s) WHERE x.first_utc IS NOT NULL | {session_key:x.session_key,first_utc:x.first_utc,last_utc:x.last_utc}] AS sessions
         }
         RETURN a{.*} AS a,d{.names,.handles,.emails} AS d,sessions,session_count ORDER BY a.key`);
      return rows.records.map(record => {
        const a = record.get("a"), d = record.get("d");
        return {
          key: a.key, source: a.source, actor: a.actor, names: a.names, episodes: a.episodes,
          owner: a.mine || a.actor === "user" || owners.has(`${a.source}:${a.actor}`),
          bot: a.actor === "assistant" || a.tool_episodes * 2 > a.episodes,
          sessions: record.get("sessions"), session_count: record.get("session_count"),
          directory: d === null ? null : { names: d.names ?? [], handles: d.handles ?? [], emails: d.emails ?? [] },
          sample_ids: [...new Set([...a.sample_first, ...a.sample_last])],
        };
      });
    });
  }

  async loadDecisions(): Promise<IdentityDecision[]> {
    return this.core.withReadTx(async tx => (await tx.run(
      `MATCH (d:IdentityDecision) RETURN d{.*} AS d ORDER BY d.decided_at,d.id`)).records.map(record => record.get("d") as IdentityDecision));
  }

  async writeDecisions(decisions: readonly IdentityDecision[]): Promise<void> {
    if (decisions.length === 0) return;
    await this.core.withWriteTx(tx => tx.run(`UNWIND $rows AS row CREATE (d:IdentityDecision) SET d=row`, { rows: decisions }));
  }

  async accountsExist(refs: readonly IdentityAccountRef[]): Promise<string[] | null> {
    const keys = refs.map(ref => accountKey(ref.source, ref.actor));
    const found = await this.core.withReadTx(tx => tx.run(`MATCH (a:Account) WHERE a.key IN $keys RETURN count(a) AS n`, { keys }));
    return found.records[0]?.get("n") === new Set(keys).size ? keys : null;
  }

  async judgeSamples(ids: { a: readonly string[]; b: readonly string[] }, context: InstallationContext): Promise<{ a: string[]; b: string[] }> {
    return this.core.extractionTx(context, async (tx, policy) => {
      const texts = async (list: readonly string[]) => {
        const allowed: string[] = [];
        for (const id of list) {
          try { await this.core.authorizeEpisodesTx(tx, [id], policy); }
          catch (error) {
            if (error instanceof ReceiptError && (error.code === "policy_denied" || error.code === "invalid_selection")) continue;
            throw error;
          }
          allowed.push(id);
        }
        const rows = await tx.run<{ content: string }>(
          `UNWIND $ids AS id MATCH (e:Element:Episode {id:id}) RETURN e.content AS content`, { ids: allowed });
        return rows.records.map(record => record.get("content"));
      };
      return { a: await texts(ids.a), b: await texts(ids.b) };
    });
  }

  async importDirectory(input: DirectoryInput): Promise<{ imported: number; changed: number }> {
    const { entries } = RpcIdentityDirectoryParams.parse(input);
    const unique = (values: readonly string[], fold: (value: string) => string) => [...new Set(values.map(fold).filter(Boolean))].sort();
    const rows = [...new Map(entries.map(entry => [JSON.stringify([entry.source, entry.actor]), {
      source: entry.source, actor: entry.actor, names: unique(entry.names, name => name.normalize("NFC").trim()),
      handles: unique(entry.handles, handle => handle.normalize("NFC").trim()), emails: unique(entry.emails, email => email.trim().toLowerCase()),
    }])).values()];
    return this.core.withWriteTx(async tx => {
      const result = await tx.run<{ changed: number }>(
        `UNWIND $rows AS row
         MERGE (d:DirectoryEntry {source:row.source,actor:row.actor})
         WITH d,row WHERE d.names IS NULL OR d.names <> row.names OR d.handles <> row.handles OR d.emails <> row.emails
         SET d.names=row.names,d.handles=row.handles,d.emails=row.emails RETURN count(d) AS changed`, { rows });
      return { imported: rows.length, changed: result.records[0]?.get("changed") ?? 0 };
    });
  }

  async builtDigest(): Promise<string> {
    return this.core.withWriteTx(async tx => (await stateTx(tx)).built);
  }

  async buildGeneration(people: readonly IdentityPerson[], digest: string): Promise<{ generation: number; persons: number }> {
    const generation = await this.core.withWriteTx(async tx => {
      const next = (await stateTx(tx)).generation + 1;
      await tx.run(
        `UNWIND $people AS p
         CREATE (n:Person {id:p.id,generation:$g,name:p.name,owner:p.owner,aliases:p.aliases,sources:p.sources})
         WITH n,p UNWIND p.members AS key
         MATCH (a:Account {key:key}) CREATE (a)-[:OF_PERSON {generation:$g}]->(n)`, { people, g: next });
      await tx.run(`MATCH (s:IdentityState {key:'identity'}) SET s.generation=$g,s.built_digest=$digest`, { g: next, digest });
      return next;
    });
    await this.core.withWriteTx(tx => tx.run(`MATCH (n:Person) WHERE n.generation < $g DETACH DELETE n`, { g: generation }));
    return { generation, persons: people.length };
  }

  async resetAccounts(): Promise<void> {
    for (const label of ["Person", "AccountSession", "Account"]) {
      for (;;) {
        const deleted = await this.core.withWriteTx(tx => tx.run<{ n: number }>(
          `MATCH (n:${label}) WITH n LIMIT 10000 DETACH DELETE n RETURN count(*) AS n`));
        if ((deleted.records[0]?.get("n") ?? 0) === 0) break;
      }
    }
    await this.core.withWriteTx(tx => tx.run(
      `MERGE (s:IdentityState {key:'identity'}) SET s.covered_ingest_seq=0,s.built_digest=''`));
  }

  async status() {
    return this.core.withReadTx(async tx => {
      const row = (await tx.run(
        `OPTIONAL MATCH (s:IdentityState {key:'identity'})
         OPTIONAL MATCH (m:Meta {key:'meta'})
         CALL () { MATCH (a:Account) RETURN count(a) AS accounts }
         CALL (s) { MATCH (p:Person) WHERE p.generation = s.generation RETURN count(p) AS persons }
         CALL () { OPTIONAL MATCH (d:IdentityDecision) RETURN collect(d{.authority,.verdict}) AS decisions }
         RETURN coalesce(s.covered_ingest_seq,0) AS covered,coalesce(m.ingest_seq,0) AS live,coalesce(s.generation,0) AS generation,
           accounts,persons,decisions`)).records[0]!;
      const decisions = row.get("decisions") as { authority: string; verdict: string }[];
      const count = (field: "authority" | "verdict", value: string) => decisions.filter(d => d[field] === value).length;
      return {
        covered_ingest_seq: row.get("covered"), live_ingest_seq: row.get("live"), generation: row.get("generation"),
        accounts: row.get("accounts"), persons: row.get("persons"),
        decisions: { judge: count("authority", "judge"), operator: count("authority", "operator"),
          same: count("verdict", "same"), different: count("verdict", "different"), unsure: count("verdict", "unsure") },
      };
    });
  }

  listPeople(input: z.input<typeof RpcPeopleListParams>) { return this.people.list(input); }
  showPerson(input: RpcPeopleShowParams) { return this.people.show(input); }
}
