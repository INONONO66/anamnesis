import { expect, test } from "bun:test";
import { mkdtemp, readFile, readdir, rm, stat } from "node:fs/promises";
import { join } from "node:path";
import { timingHash, timingLog } from "../../app/anamnesis/timing.ts";

const rows = async (path: string) => (await readFile(path, "utf8")).trim().split("\n").map(line => JSON.parse(line));

test("timing records have monotonic sequence, bounded rotation and no arbitrary payload fields", async () => {
  const root = await mkdtemp("/tmp/g006-timing-");
  const path = join(root, "timing.jsonl");
  try {
    const emit = timingLog(path, 1024);
    for (let index = 0; index < 20; index++) emit({ layer: "client", event: "request", method: "remember", id: index, deadlineMs: 120_000 });
    expect((await readdir(root)).sort()).toEqual(["timing.jsonl", "timing.jsonl.1"]);
    for (const file of [path + ".1", path]) expect((await stat(file)).size).toBeLessThanOrEqual(1024);
    const retained = [...await rows(path + ".1"), ...await rows(path)];
    expect(retained.at(-1).eventSequence).toBe(20);
    expect(retained.length).toBeLessThan(20);
    for (let index = 1; index < retained.length; index++) {
      expect(retained[index].eventSequence).toBe(retained[index - 1].eventSequence + 1);
      expect(retained[index].monotonicMs).toBeGreaterThanOrEqual(retained[index - 1].monotonicMs);
    }
    const privatePath = join(root, "private.jsonl");
    const privateEmit = timingLog(privatePath);
    privateEmit(Object.assign({ layer: "daemon" as const, event: "request_received", method: "status" as const, id: "private-client-id" }, { params: { token: "private-token", content: "private-content" } }));
    const text = await readFile(privatePath, "utf8");
    for (const secret of ["private-client-id", "private-token", "private-content", "params"]) expect(text).not.toContain(secret);
    expect((await rows(privatePath))[0]).toMatchObject({ id: timingHash("private-client-id"), at: expect.any(String), monotonicMs: expect.any(Number), eventSequence: 1 });
  } finally { await rm(root, { recursive: true, force: true }); }
});
