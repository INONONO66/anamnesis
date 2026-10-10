import { streamAgentLogFile } from "@anamnesis/backfill";
import { expect, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { agentlogFixture } from "./agentlog.fixture.ts";
import { agentlogAdapter } from "./agentlog.ts";

test("agentlog matches backfill identity time and revision while retaining non-recallable and oversized events", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-agentlog-"));
  try {
    const path = join(root, "export.jsonl");
    await writeFile(path, `${agentlogFixture.map((event) => JSON.stringify(event)).join("\n")}\n`);
    await writeFile(join(root, "._sidecar.jsonl"), "");
    expect(await Array.fromAsync(agentlogAdapter.files(root))).toEqual([path]);
    const parsed = await agentlogAdapter.parse(path, await readFile(path));
    const records = parsed.flatMap((session) => session.records);
    expect(parsed.map((session) => session.sessionId)).toEqual(["session-1", "session-2"]);
    expect(records.map((record) => [record.line, record.role, record.kind, record.canonical_kind])).toEqual([
      [1, "assistant", "message", "message"],
      [2, "tool", "tool_result", "system-event"],
      [3, "operator", "summary", "summary"],
    ]);
    expect(records[1]?.text).toBe("");
    expect(records[2]?.text).toBe(agentlogFixture[2].text);
    const guarded = streamAgentLogFile(path);
    await guarded.next();
    await guarded.next();
    await expect(guarded.next()).rejects.toThrow("source_record_too_large");
    const episodes = [];
    for await (const { episode } of streamAgentLogFile(path, (await readFile(path)).length)) {
      if (episode) episodes.push(episode.input);
    }
    expect(episodes).toHaveLength(2);
    for (const episode of episodes) {
      const record = records.find((item) => item.origin.record === episode.origin.record);
      expect(record?.origin).toEqual(episode.origin);
      expect(record?.occurred_at).toBe(episode.time?.value);
      expect(record?.revision).toBe(episode.source_revision);
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
