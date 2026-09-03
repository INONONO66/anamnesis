import { afterAll, describe, expect, test } from "bun:test";
import { createHash, randomUUID } from "node:crypto";
import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import neo4j from "neo4j-driver";
import { Engine, type RememberInput } from "./engine.ts";
import { EpisodeJournal, journaledRemember } from "./journal.ts";
import type { PutResult } from "./store.ts";

const directories: string[] = [];

async function temporaryDirectory(): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), "anamnesis-journal-"));
  directories.push(directory);
  return directory;
}

const TEST_DB = {
  uri: "bolt://127.0.0.1:7688",
  user: "neo4j",
  password: "anamnesis-test",
};

function sha256(value: string | Uint8Array): string {
  return createHash("sha256").update(value).digest("hex");
}

function revisionKey(input: RememberInput): string {
  const originKey = sha256(
    JSON.stringify([
      input.origin.source,
      input.origin.session,
      input.origin.actor,
      input.origin.record,
    ]),
  );
  return sha256(JSON.stringify([originKey, input.source_revision ?? input.origin.record]));
}

function episode(record: string, content = `Journal fixture ${record}`): RememberInput {
  return {
    time: { value: "2026-09-02T10:00:00Z", precision: "second" },
    content,
    origin: {
      source: "journal-test",
      session: "durable-deploy",
      actor: "test",
      record,
    },
  };
}

class RecordingEngine {
  readonly inputs: RememberInput[] = [];
  shouldFail = false;

  async remember(input: RememberInput): Promise<PutResult> {
    this.inputs.push(input);
    if (this.shouldFail) throw new Error("ingestion failed");
    return { id: `recorded-${this.inputs.length}`, created: true };
  }
}

afterAll(async () => {
  await Promise.all(directories.map((directory) => rm(directory, { recursive: true })));
});

describe("EpisodeJournal", () => {
  test("appends validated JSONL and preserves payload bytes", async () => {
    const directory = await temporaryDirectory();
    const journal = new EpisodeJournal(
      directory,
      () => new Date("2026-09-02T12:34:56.000Z"),
    );

    await journal.append({ ...episode("happy"), payload: Uint8Array.from([0, 127, 255]) });

    const text = await readFile(join(directory, "journal-2026-09.jsonl"), "utf8");
    expect(text.endsWith("\n")).toBe(true);
    expect(JSON.parse(text)).toEqual({
      recordedAt: "2026-09-02T12:34:56.000Z",
      element: {
        ...episode("happy"),
        schema: "anamnesis.original-message/1",
        mass: 0.5,
        properties: {},
        payload: [0, 127, 255],
      },
    });

    const engine = new RecordingEngine();
    expect(await journal.replay(engine)).toBe(1);
    expect(engine.inputs[0]?.payload).toEqual(Uint8Array.from([0, 127, 255]));
  });

  test("replays journaled remembers idempotently through revision_key and restores payload bytes", async () => {
    const journal = new EpisodeJournal(await temporaryDirectory());
    const objectsRoot = await temporaryDirectory();
    const engine = new Engine({ ...TEST_DB, objectsRoot });
    const payload = Uint8Array.from([0, 127, 255]);
    const input: RememberInput = {
      ...episode(`revision-replay-${randomUUID()}`),
      source_revision: "source-revision-1",
      payload,
      payload_media_type: "application/octet-stream",
    };

    await engine.init();
    try {
      const first = await journaledRemember(journal, engine, input);
      expect(first.created).toBe(true);
      expect(await journal.replay(engine)).toBe(1);
      expect(await journal.replay(engine)).toBe(1);

      const payloadHash = sha256(payload);
      expect(await engine.store.getPayload(payloadHash)).toEqual(payload);

      const driver = neo4j.driver(
        TEST_DB.uri,
        neo4j.auth.basic(TEST_DB.user, TEST_DB.password),
        { disableLosslessIntegers: true },
      );
      const session = driver.session();
      try {
        const result = await session.run<{
          ids: string[];
          payloadHashes: string[];
        }>(
          `MATCH (e:Element:Episode { revision_key: $revisionKey })
           OPTIONAL MATCH (e)-[:HAS_PAYLOAD]->(p:Payload)
           RETURN collect(e.id) AS ids, collect(p.hash) AS payloadHashes`,
          { revisionKey: revisionKey(input) },
        );
        expect(result.records).toHaveLength(1);
        const record = result.records[0]!;
        expect(record.get("ids")).toEqual([first.id]);
        expect(record.get("payloadHashes")).toEqual([payloadHash]);
      } finally {
        await session.close();
        await driver.close();
      }
    } finally {
      await engine.close();
    }
  });

  test("journal-first ingestion leaves a replayable entry on engine failure", async () => {
    const directory = await temporaryDirectory();
    const journal = new EpisodeJournal(directory);
    const engine = new RecordingEngine();
    engine.shouldFail = true;

    await expect(journaledRemember(journal, engine, episode("failed-ingest"))).rejects.toThrow(
      "ingestion failed",
    );
    expect(await readdir(directory)).toHaveLength(1);

    engine.shouldFail = false;
    expect(await journal.replay(engine)).toBe(1);
    expect(engine.inputs.at(-1)?.origin.record).toBe("failed-ingest");
  });

  test("rejects malformed input before creating the journal directory", async () => {
    const root = await temporaryDirectory();
    const directory = join(root, "not-created");
    const journal = new EpisodeJournal(directory);

    await expect(journal.append({ content: "missing origin and time" })).rejects.toThrow();
    expect(await readdir(root)).toEqual([]);
  });

  test("rolls journal files by UTC month and replays them chronologically", async () => {
    const directory = await temporaryDirectory();
    const dates = [
      new Date("2026-10-01T00:00:00Z"),
      new Date("2026-09-30T23:59:59Z"),
    ];
    const journal = new EpisodeJournal(directory, () => dates.shift()!);
    await journal.append(episode("october"));
    await journal.append(episode("september"));

    expect((await readdir(directory)).sort()).toEqual([
      "journal-2026-09.jsonl",
      "journal-2026-10.jsonl",
    ]);
    const engine = new RecordingEngine();
    expect(await journal.replay(engine)).toBe(2);
    expect(engine.inputs.map((input) => input.origin.record)).toEqual([
      "september",
      "october",
    ]);
  });

  test("honors an aborted replay signal", async () => {
    const directory = await temporaryDirectory();
    const journal = new EpisodeJournal(directory);
    await journal.append(episode("abort"));
    const controller = new AbortController();
    controller.abort();

    await expect(
      journal.replay(new RecordingEngine(), { signal: controller.signal }),
    ).rejects.toThrow();
  });
});
