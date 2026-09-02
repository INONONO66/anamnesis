import { afterAll, describe, expect, test } from "bun:test";
import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { RememberInput } from "./engine.ts";
import { EpisodeJournal, journaledRemember } from "./journal.ts";
import type { PutResult } from "./store.ts";

const directories: string[] = [];

async function temporaryDirectory(): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), "anamnesis-journal-"));
  directories.push(directory);
  return directory;
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
