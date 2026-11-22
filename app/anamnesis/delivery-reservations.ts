import { createHash } from "node:crypto";
import { mkdir, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { RpcRememberParams } from "@anamnesis/protocol";
import { atomicJson, hasCode } from "./config.ts";
import { logEvent } from "./log.ts";
import { RpcFault } from "./wire.ts";

function canonical(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  return `{${Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([key, member]) => `${JSON.stringify(key)}:${canonical(member)}`).join(",")}}`;
}
export const sha = (value: string) => createHash("sha256").update(value).digest("hex");
export const originKey = (params: RpcRememberParams) => { const o = params.episode.origin; return sha(JSON.stringify([o.source, o.session, o.actor, o.record])); };
// Origin field order is explicit, not object-insertion dependent.
export function revisionKey(params: RpcRememberParams): string {
  return sha(JSON.stringify([originKey(params), params.source_revision]));
}
export interface Binding { digest_version: 1; params: RpcRememberParams; body_digest: string; incarnation: string; fs_epoch: string; }
export const Binding: { parse(value: unknown): Binding; safeParse(value: unknown): Binding | null } = {
  parse(value: unknown): Binding {
    if (!value || typeof value !== "object") throw new RpcFault("spool_corrupt", "invalid delivery binding");
    const data = value as Record<string, unknown>;
    if (data["digest_version"] !== 1 || typeof data["body_digest"] !== "string" || !/^[a-f0-9]{64}$/.test(data["body_digest"]) || typeof data["incarnation"] !== "string" || typeof data["fs_epoch"] !== "string") throw new RpcFault("spool_corrupt", "invalid delivery binding");
    return { digest_version: 1, params: RpcRememberParams.parse(data["params"]), body_digest: data["body_digest"], incarnation: data["incarnation"], fs_epoch: data["fs_epoch"] };
  },
  safeParse(value) {
    try { return Binding.parse(value); }
    catch { return null; }
  },
};
export function digest(params: RpcRememberParams): string { return sha(canonical({ digest_version: 1, params })); }

export class DeliveryReservations {
  constructor(readonly root: string) {}
  async init(): Promise<void> {
    await mkdir(this.root, { recursive: true, mode: 0o700 });
  }
  async read(key: string): Promise<Binding | null> {
    try { return Binding.parse(JSON.parse(await readFile(join(this.root, key + ".json"), "utf8"))); }
    catch (error) { if (hasCode(error, "ENOENT")) return null; throw error; }
  }
  async write(key: string, binding: Binding): Promise<void> {
    await atomicJson(join(this.root, key + ".json"), binding);
  }
  async remove(key: string): Promise<void> {
    await rm(join(this.root, key + ".json"));
  }
  async release(key: string): Promise<void> {
    try { await rm(join(this.root, key + ".json"), { force: true }); }
    catch (error) { logEvent("error", "delivery_reservation_cleanup_failed", { revision_key: key, error: String(error) }); }
  }
}
