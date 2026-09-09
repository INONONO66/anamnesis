import { expect, test } from "bun:test";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { collectAgentLog, streamAgentLogFile } from "./agentlog.ts";

const event = { provider: "codex", partition_id: "session", upstream_event_id: "turn", occurred_at: 1000, text: "secret xoxb-1234567890abcdef " + "x".repeat(4200) };
async function fixture(text: string, run: (root: string, path: string) => Promise<void>) {
  const root = await mkdtemp("/tmp/ana-agentlog-stream-");
  const path = root + "/export.jsonl";
  await writeFile(path, text);
  try { await run(root, path); } finally { await rm(root, { recursive: true, force: true }); }
}

test("streaming conversion exactly reuses collector identity, redaction, payload and source context", () => fixture("\r\n" + JSON.stringify({ ...event, text: " " }) + "\n" + JSON.stringify(event) + "\r\n", async (root, path) => {
  const records = [];
  for await (const record of streamAgentLogFile(path)) records.push(record);
  expect(records.map(r => r.line)).toEqual([1, 2, 3]);
  expect(records.slice(0, 2).map(r => r.episode)).toEqual([null, null]);
  expect([records[2]!.episode]).toEqual(await collectAgentLog(root));
}));

test("iterator yields one record without parsing or collecting the malformed remainder", () => fixture(JSON.stringify(event) + "\n{bad}\n", async (_root, path) => {
  const iterator = streamAgentLogFile(path);
  expect((await iterator.next()).value?.line).toBe(1);
  await expect(iterator.next()).rejects.toThrow("source_parse_error");
}));

test("record allocation is bounded before decode, with an explicit limit failure", () => fixture("x".repeat(1025), async (_root, path) => {
  await expect(streamAgentLogFile(path, 1024).next()).rejects.toThrow("source_record_too_large");
}));

test("unterminated final record stays incomplete while legacy collector still accepts it", () => fixture(JSON.stringify(event), async (root, path) => {
  expect(await collectAgentLog(root)).toHaveLength(1);
  await expect(streamAgentLogFile(path).next()).rejects.toThrow("source_partial_final_line");
}));

test("fatal UTF-8 decoding reports a parse failure without logging record secrets", async () => {
  const root = await mkdtemp("/tmp/ana-agentlog-utf8-");
  try {
    const path = root + "/export.jsonl";
    await writeFile(path, Buffer.from([0xff, 10]));
    await expect(streamAgentLogFile(path).next()).rejects.toThrow(`source_parse_error: ${path}:1`);
  } finally { await rm(root, { recursive: true, force: true }); }
});
