import neo4j, { type Node, type Record as Neo4jRecord, type RecordShape, type Relationship } from "neo4j-driver";
import { MemoryElement } from "@anamnesis/protocol";
import { HistoricalElement } from "../legacy-format.ts";
import { z } from "zod";
import { type InstallationContext } from "./policy.ts";

export type ElementProperties = Record<string, string | number | null>;
type LinkProperties = Record<string, string | number>;
export type ElementNode = Node<number, ElementProperties>;
export type LinkRelationship = Relationship<number, LinkProperties>;
type QueryParameter =
  | string
  | number
  | boolean
  | null
  | Buffer
  | string[]
  | ReturnType<typeof neo4j.int>;
export type QueryParameters = Record<string, QueryParameter>;

export interface ElementWriteOptions {
  payload?: Uint8Array;
  payloadMediaType?: string;
  sourceRevision?: string;
  expectedPreviousRevisionKey?: string | null;
  previous?: string;
  admission?: { metadata: unknown; context: InstallationContext };
}


export function recordsToObjects<Row extends RecordShape>(
  records: Neo4jRecord<Row>[],
): Row[] {
  return records.map((record) => record.toObject());
}

export function nodeProps(node: ElementNode): ElementProperties {
  return node.properties;
}

export function relProps(rel: LinkRelationship): LinkProperties {
  return rel.properties;
}

export const StoredHash = z.string().regex(/^[0-9a-f]{64}$/).nullable();
const HistoricalStoredElement = HistoricalElement.extend({ id: z.uuidv7() });

/** Frozen post167/pre194 structural read: semantic eligibility is reported
 * separately, not used to reject intact historical bytes. No serving bypass. */
export function decodeHistoricalElement(p: ElementProperties): MemoryElement {
  return HistoricalStoredElement.parse({
    id: p["id"], schema: p["schema"], content: p["content"], mass: p["mass"],
    properties: typeof p["properties"] === "string" ? JSON.parse(p["properties"]) : undefined,
    ...(p["time_value"] != null || p["time_precision"] != null
      ? { time: { value: p["time_value"], precision: p["time_precision"] } } : {}),
    origin: { source: p["origin_source"], session: p["origin_session"], actor: p["origin_actor"], record: p["origin_record"] },
  });
}

export function toElement(p: ElementProperties): MemoryElement {
  return MemoryElement.parse({
    id: p["id"],
    schema: p["schema"],
    ...(p["time_value"] ? { time: { value: p["time_value"], precision: p["time_precision"] } } : {}),
    content: p["content"],
    origin: {
      source: p["origin_source"],
      session: p["origin_session"],
      actor: p["origin_actor"],
      record: p["origin_record"],
    },
    mass: p["mass"],
    properties: {
      ...JSON.parse((p["properties"] as string) ?? "{}"),
      ...(p["payload_hash"] ? { payload_hash: p["payload_hash"] } : {}),
    },
  });
}
