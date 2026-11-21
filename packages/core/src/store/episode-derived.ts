import neo4j, { type ManagedTransaction } from "neo4j-driver";
import { v5 as uuidv5 } from "uuid";
import { z } from "zod";
import {
  type MemoryElement, RpcSpacesImportParams, RpcSpacesListParams, RpcSpacesListResult,
  RpcEpisodeRebuildParams, type RpcEpisodeRebuildResult,
} from "@anamnesis/protocol";
import { tupleHash } from "./digest.ts";
import type { StoreCore } from "./core.ts";

type EpisodeEvidence = {
  readonly id: string;
  readonly source: string;
  readonly session: string;
  readonly properties: MemoryElement["properties"];
};
type Address = { readonly source: string; readonly session: string; readonly record: string };

/** The evidence stays untouched. Neither cwd nor content participates. */
function roomOf(episode: EpisodeEvidence): string {
  const room = episode.properties["room"];
  return typeof room === "string" && room.length > 0 ? room : episode.session;
}

function replyAddresses(episode: EpisodeEvidence): Address[] {
  const addresses: Address[] = [];
  const reply = episode.properties["reply"];
  if (typeof reply === "string") {
    const colon = reply.indexOf(":"), hash = reply.indexOf("#", colon + 1);
    if (colon > 0 && hash > colon + 1 && hash < reply.length - 1) {
      addresses.push({ source: reply.slice(0, colon), session: reply.slice(colon + 1, hash), record: reply.slice(hash + 1) });
    }
  }
  const thread = episode.properties["thread"], room = roomOf(episode);
  if (episode.source === "slack" && typeof thread === "string" && thread.length > 0 && episode.session === `${room}/${thread}`) {
    addresses.push({ source: episode.source, session: room, record: thread });
  }
  return [...new Map(addresses.map(address => [JSON.stringify(address), address])).values()];
}

/** The admission and rebuild writers share this exact assignment lookup. */
async function labelEpisodesTx(tx: ManagedTransaction, episodes: readonly EpisodeEvidence[]): Promise<number> {
  const result = await tx.run<{ updated: number }>(
    `UNWIND $rows AS row
     MATCH (e:Element:Episode {id:row.id})
     OPTIONAL MATCH (r:RoomAssignment {source:row.source,room:row.room})
     WITH e,row,coalesce(r.space,'unknown:' + row.source) AS space
     WHERE e.room IS NULL OR e.room <> row.room OR e.space IS NULL OR e.space <> space
     SET e.room=row.room,e.space=space RETURN count(e) AS updated`,
    { rows: episodes.map(episode => ({ id: episode.id, source: episode.source, room: roomOf(episode) })) });
  return result.records[0]?.get("updated") ?? 0;
}

async function replyEpisodesTx(tx: ManagedTransaction, episodes: readonly EpisodeEvidence[]) {
  const candidates = episodes.flatMap(episode => replyAddresses(episode).map(address => ({ from: episode.id, ...address })));
  const resolved = await tx.run<{ from: string; to: string | null }>(
    `UNWIND $rows AS row
     CALL (row) {
       OPTIONAL MATCH (target:Episode)
       USING INDEX target:Episode(origin_source,origin_session,origin_record,ingest_seq)
       WHERE target.origin_source=row.source AND target.origin_session=row.session
         AND target.origin_record=row.record AND target.ingest_seq IS NOT NULL AND target.id <> row.from
       RETURN target.id AS id ORDER BY target.ingest_seq DESC LIMIT 1
     }
     RETURN row.from AS from,id AS to`, { rows: candidates });
  const links = resolved.records.flatMap(record => {
    const to = record.get("to"), from = record.get("from");
    if (to === null) return [];
    const key = tupleHash([from, to, "REPLIES_TO"]);
    return [{ from, to, key, id: uuidv5(key, uuidv5.URL) }];
  });
  const removed = await tx.run(
    `UNWIND $ids AS id MATCH (e:Element:Episode {id:id})-[l:REPLIES_TO]->()
     WHERE NOT l.idem_key IN $keys DELETE l`,
    { ids: episodes.map(episode => episode.id), keys: links.map(link => link.key) });
  const created = await tx.run(
    `UNWIND $links AS link
     MATCH (a:Element:Episode {id:link.from}),(b:Element:Episode {id:link.to})
     MERGE (a)-[r:REPLIES_TO {idem_key:link.key}]->(b)
     ON CREATE SET r.id=link.id,r.content='This episode replies to that episode',r.weight=1.0`,
    { links });
  return {
    created: created.summary.counters.updates().relationshipsCreated,
    removed: removed.summary.counters.updates().relationshipsDeleted,
    missing: resolved.records.filter(record => record.get("to") === null).length,
  };
}

/** Cache writes never touch originals, topology, conducting coverage or serving revisions. */
export class EpisodeDerivedStore {
  constructor(private readonly core: StoreCore) {}

  async admitTx(tx: ManagedTransaction, episode: MemoryElement): Promise<void> {
    const evidence = [{ id: episode.id, source: episode.origin.source, session: episode.origin.session, properties: episode.properties }];
    await labelEpisodesTx(tx, evidence);
    // Admission runs on the serial writer: an Episode that answers nothing costs no reply statements.
    if (evidence.some(row => replyAddresses(row).length > 0)) await replyEpisodesTx(tx, evidence);
  }

  async importAssignments(input: RpcSpacesImportParams) {
    const { assignments } = RpcSpacesImportParams.parse(input);
    // Last line for a room wins, deterministically, including removal of its optional name.
    const rows = [...new Map(assignments.map(row => [JSON.stringify([row.source, row.room]), { ...row, name: row.name ?? null }])).values()];
    return this.core.withWriteTx(async tx => {
      const result = await tx.run<{ changed: number }>(
        `UNWIND $rows AS row
         MERGE (r:RoomAssignment {source:row.source,room:row.room})
         WITH r,row WHERE r.space IS NULL OR r.space <> row.space OR coalesce(r.name,'') <> coalesce(row.name,'')
         SET r.space=row.space,r.name=row.name RETURN count(r) AS changed`, { rows });
      return { imported: rows.length, changed: result.records[0]?.get("changed") ?? 0 };
    });
  }

  async listSpaces(input: RpcSpacesListParams) {
    const { after, limit } = RpcSpacesListParams.parse(input);
    return this.core.withReadTx(async tx => {
      const result = await tx.run(
        `MATCH (e:Episode) USING INDEX e:Episode(space) WHERE e.space > $after
         WITH e.space AS space,count(e) AS episodes,count(DISTINCT [e.origin_source,e.room]) AS rooms,
           min(e.time_utc) AS first,max(e.time_utc) AS last
         RETURN space,episodes,rooms,first,last ORDER BY space LIMIT $limit`,
        { after: after ?? "", limit: neo4j.int(limit + 1) });
      const spaces = result.records.slice(0, limit).map(record => record.toObject());
      return RpcSpacesListResult.parse({ spaces, next: result.records.length > limit ? spaces.at(-1)?.["space"] : null });
    });
  }

  /** A committed page is its checkpoint. Replaying a page is safe; callers retain
   * through for a finite pass while admission continues beyond that high-water mark. */
  async rebuild(input: RpcEpisodeRebuildParams, kind: "labels" | "replies"): Promise<RpcEpisodeRebuildResult> {
    const params = RpcEpisodeRebuildParams.parse(input);
    return this.core.withWriteTx(async tx => {
      const high = await tx.run<{ through: number }>(`MATCH (m:Meta {key:'meta'}) RETURN m.ingest_seq AS through`);
      const through = Math.min(params.through ?? Number.MAX_SAFE_INTEGER, high.records[0]?.get("through") ?? 0);
      const scope = params.room
        ? "USING INDEX e:Episode(origin_source,room,ingest_seq) WHERE e.origin_source=$source AND e.room=$room AND"
        : "USING INDEX e:Episode(ingest_seq) WHERE";
      const page = await tx.run<{ id: string; source: string; session: string; properties: string; seq: number }>(
        `MATCH (e:Episode) ${scope} e.ingest_seq > $after AND e.ingest_seq <= $through
         RETURN e.id AS id,e.origin_source AS source,e.origin_session AS session,e.properties AS properties,e.ingest_seq AS seq
         ORDER BY e.ingest_seq LIMIT $limit`,
        { after: params.after, through, limit: neo4j.int(params.limit), source: params.room?.source ?? null, room: params.room?.room ?? null });
      const episodes = page.records.map(record => {
        const row = record.toObject();
        return { ...row, properties: z.record(z.string(), z.json()).parse(JSON.parse(row.properties)) };
      });
      const updated = kind === "labels" ? await labelEpisodesTx(tx, episodes) : 0;
      const replies = kind === "replies" ? await replyEpisodesTx(tx, episodes) : { created: 0, removed: 0, missing: 0 };
      const after = episodes.at(-1)?.seq ?? through;
      return { after, through, done: episodes.length < params.limit || after >= through, scanned: episodes.length, updated, ...replies };
    });
  }
}
