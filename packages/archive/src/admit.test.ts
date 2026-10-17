import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { RpcRememberParams, RpcRememberResult } from "@anamnesis/protocol";
import { admit, type Rememberer } from "./admit.ts";
import { Manifest } from "./manifest.ts";
import { revisionOf } from "./record.ts";
import type { ArchiveRecord } from "./record.ts";

const hash = (value: string): string => createHash("sha256").update(value).digest("hex");

function record(session: string, n: number): ArchiveRecord {
  const occurred_at = new Date(Date.UTC(2026, 9, 10, 12, 0, n)).toISOString();
  const text = `${session} turn ${n}`;
  return {
    origin: { source: "omo", session, actor: n % 2 === 0 ? "user" : "assistant", record: `${session}-r${n}` },
    seq: n, role: n % 2 === 0 ? "user" : "assistant", occurred_at, time_precision: "second", text,
    kind: "message", canonical_kind: "message", revision: revisionOf(occurred_at, text),
    file: { sha256: hash(session), path: `/fixture/${session}.jsonl`, line: n + 1 },
  };
}

async function archive(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "archive-admit-"));
  const manifest = Manifest.open(root);
  for (const [session, count] of [["s-b", 2], ["s-a", 3]] as const) {
    manifest.insertSession({ sha256: hash(session), source: "omo", sessionId: session, bytes: 1, path: `/fixture/${session}.jsonl`, collectedAt: "2026-10-10T12:00:00.000Z" },
      Array.from({ length: count }, (_, n) => record(session, n)));
  }
  manifest.close();
  return root;
}

function stub(created: boolean, failAfter = Infinity): Rememberer & { calls: RpcRememberParams[] } {
  const calls: RpcRememberParams[] = [];
  return {
    calls,
    async remember(params): Promise<RpcRememberResult> {
      if (calls.length >= failAfter) throw new Error("connection reset");
      calls.push(params);
      return { state: "committed", revision_key: hash(params.source_revision), body_digest: hash(params.episode.content),
        data_incarnation: "0199f000-0000-7000-8000-000000000000", id: "0199f000-0000-7000-8000-000000000001", created, ingest_seq: calls.length };
    },
  };
}

const run = (archiveDir: string, client: Rememberer, stopped = () => false, since?: string) =>
  admit({ archiveDir, source: "omo", client, stopped, ...(since === undefined ? {} : { since }) });
const cursor = async (root: string): Promise<Record<string, unknown>> => JSON.parse(await readFile(join(root, "admit", "omo.cursor.json"), "utf8"));
const ORDER = ["s-a-r0", "s-a-r1", "s-a-r2", "s-b-r0", "s-b-r1"];

test("admit sends one remember per record in session then record order", async () => {
  const root = await archive();
  try {
    const client = stub(true);
    expect(await run(root, client)).toEqual({ source: "omo", records: 5, created: 5, duplicates: 0, failed: 0, skipped: 0, since: null, resumed_from: null });
    expect(client.calls.map((params) => params.episode.origin.record)).toEqual(ORDER);
    const first = client.calls[0]!;
    expect(first.source_revision).toBe(record("s-a", 0).revision);
    expect(first.expected_previous_revision_key).toBeNull();
    expect(first.episode.properties).toEqual({ kind: "message", role: "user" });
    expect(await cursor(root)).toMatchObject({ status: "complete", last: { session_id: "s-b", index: 1 }, created: 5, duplicates: 0 });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("a rerun after a complete cursor starts over and reports the daemon's duplicates", async () => {
  const root = await archive();
  try {
    await run(root, stub(true));
    const client = stub(false);
    expect(await run(root, client)).toEqual({ source: "omo", records: 5, created: 0, duplicates: 5, failed: 0, skipped: 0, since: null, resumed_from: null });
    expect(client.calls.map((params) => params.episode.origin.record)).toEqual(ORDER);
    expect(await cursor(root)).toMatchObject({ status: "complete", created: 0, duplicates: 5 });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("a run that dies after the k-th remember resumes at k+1", async () => {
  const root = await archive();
  try {
    await expect(run(root, stub(true, 2))).rejects.toThrow("connection reset");
    expect(await cursor(root)).toMatchObject({ status: "in_progress", last: { session_id: "s-a", index: 1 }, created: 2, duplicates: 0 });
    const client = stub(true);
    expect(await run(root, client)).toEqual({ source: "omo", records: 3, created: 3, duplicates: 0, failed: 0, skipped: 0, since: null, resumed_from: { session_id: "s-a", index: 1 } });
    expect(client.calls.map((params) => params.episode.origin.record)).toEqual(ORDER.slice(2));
    expect(await cursor(root)).toMatchObject({ status: "complete", last: { session_id: "s-b", index: 1 }, created: 5 });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("a stop request ends the run after the in-flight remember with an in-progress cursor", async () => {
  const root = await archive();
  try {
    const client = stub(true);
    expect(await run(root, client, () => client.calls.length === 1)).toMatchObject({ records: 1, created: 1 });
    expect(await cursor(root)).toMatchObject({ status: "in_progress", last: { session_id: "s-a", index: 0 } });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("a non-retryable daemon refusal counts as failed and the run continues", async () => {
  const root = await archive();
  try {
    const client = stub(true);
    const refusing: Rememberer = {
      remember: (params) => params.episode.origin.record === "s-a-r1"
        ? Promise.reject(Object.assign(new Error("conflict"), { code: "revision_conflict", retryable: false }))
        : client.remember(params),
    };
    expect(await run(root, refusing)).toEqual({ source: "omo", records: 5, created: 4, duplicates: 0, failed: 1, skipped: 0, since: null, resumed_from: null });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("--since skips earlier records without moving the cursor and re-applies the bound on resume", async () => {
  const root = await archive();
  try {
    // s-a-r1 and s-b-r1 occurred exactly at the bound; s-a-r0 and s-b-r0 one second before it.
    const since = record("s-a", 1).occurred_at;
    const full = stub(true);
    expect(await run(root, full, () => false, since)).toEqual({ source: "omo", records: 3, created: 3, duplicates: 0, failed: 0, skipped: 2, since, resumed_from: null });
    expect(full.calls.map((params) => params.episode.origin.record)).toEqual(["s-a-r1", "s-a-r2", "s-b-r1"]);

    const first = stub(true);
    expect(await run(root, first, () => first.calls.length === 1, since)).toMatchObject({ records: 1, created: 1, skipped: 1 });
    expect(await cursor(root)).toMatchObject({ status: "in_progress", last: { session_id: "s-a", index: 1 }, created: 1 });
    const second = stub(true);
    expect(await run(root, second, () => false, since)).toEqual({ source: "omo", records: 2, created: 2, duplicates: 0, failed: 0, skipped: 1, since, resumed_from: { session_id: "s-a", index: 1 } });
    const delivered = [...first.calls, ...second.calls].map((params) => params.episode.origin.record);
    expect(delivered).toEqual(["s-a-r1", "s-a-r2", "s-b-r1"]);
    expect(await cursor(root)).toMatchObject({ status: "complete", last: { session_id: "s-b", index: 1 }, created: 3 });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
