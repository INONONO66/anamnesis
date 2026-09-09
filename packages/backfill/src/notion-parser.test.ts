import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, rm, utimes, writeFile } from "node:fs/promises";
import { collectNotion, notionEpisode } from "./notion.ts";

const hash = (text: string) => createHash("sha256").update(text).digest("hex");
test("shared Notion conversion retains masked revision, title/path/session, full payload and collector time order", async () => {
  const root = await mkdtemp("/tmp/notion-parser-");
  try {
    await mkdir(root + "/Workspace/nested", { recursive: true });
    const raw = "# Page\n\nsecret: abcdefghijklmnopqrstuvwxyz\n", text = "# Page\n\n[REDACTED]\n";
    const path = root + "/Workspace/nested/Page.md", date = new Date("2026-03-01T00:00:00Z");
    await writeFile(path, raw); await utimes(path, date, date);
    await writeFile(root + "/A.md", "# Later\n"); await utimes(root + "/A.md", new Date(0), new Date("2026-09-01T00:00:00Z"));
    const parsed = notionEpisode(root, path, raw, date);
    expect(parsed).toEqual({ redactions: 1, input: {
      schema: "anamnesis.original-document/1", content: "Page\n\n# Page [REDACTED]",
      origin: { source: "notion", session: "Workspace", actor: "export", record: "Workspace/nested/Page.md" },
      source_revision: hash(text), time: { value: date.toISOString(), precision: "day" },
      payload: new TextEncoder().encode(text), payload_media_type: "text/markdown",
      properties: { title: "Page", path: "Workspace/nested/Page.md" },
    } });
    const collected = await collectNotion(root);
    expect(collected[0]).toEqual(parsed);
    expect(collected.map(e => e.input.origin.record)).toEqual(["Workspace/nested/Page.md", "A.md"]);
    expect(notionEpisode(root, path, raw.replace("abcdefghijklmnopqrstuvwxyz", "zyxwvutsrqponmlkjihgfedcba"), date)).toEqual(parsed);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("collector conversion retains empty and unterminated valid Markdown behavior", () => {
  const date = new Date(0), empty = notionEpisode("/export", "/export/Empty.md", "", date);
  expect(empty.input.content).toBe("Empty"); expect(empty.input.origin.session).toBe("Empty.md");
  expect(empty.input.source_revision).toBe(hash("")); expect(empty.input.payload).toEqual(new Uint8Array());
  const text = "\ufeff# valid Markdown without newline";
  expect(notionEpisode("/export", "/export/A.md", text, date).input.payload).toEqual(new TextEncoder().encode(text));
});
