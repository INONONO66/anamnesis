import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import { classifyClaudeTranscript, createClaudeRawParser } from "./clauderaw.ts";

const bytes = (value: object) => Buffer.from(JSON.stringify(value));
test("shared parser retains exact raw fallback bytes/nonempty indexes and session headers", () => {
  const parse = createClaudeRawParser({ path: "transcripts/old.jsonl" }, true);
  expect(parse(Buffer.alloc(0))).toEqual([]);
  expect(parse(bytes({ type: "session", sessionId: "renamed" }))).toEqual([]);
  const raw = Buffer.from(' {"type":"user", "timestamp":1000,"content":"hello"}\r');
  const [episode] = parse(raw);
  expect(episode!.input.origin).toEqual({ source: "claude-code", session: "renamed", actor: "user", record: `fallback:${bytesToHex(blake3(raw))}:1` });
  expect(episode!.input.source_revision).toBe(createHash("sha256").update("1970-01-01T00:00:01.000Z\nhello").digest("hex"));
  expect(episode!.input.properties).toEqual({ canonical_kind: "agent_message", kind: "message" });
});
test("strict runtime admission rejects invalid records without changing legacy skips", () => {
  const strict = createClaudeRawParser({ path: "a.jsonl" }, true);
  const legacy = createClaudeRawParser({ path: "a.jsonl" });
  for (const raw of ["{torn", "[]", "null"]) {
    expect(() => strict(Buffer.from(raw))).toThrow("source_invalid_record");
    expect(legacy(Buffer.from(raw))).toEqual([]);
  }
  expect(() => strict(Buffer.from([0xff]))).toThrow();
  expect(legacy(Buffer.from([0xff]))).toEqual([]);
});
test("classification admits nested roots and delegated workflows, excluding plumbing paths", () => {
  for (const root of ["projects", "transcripts", "pre-compact-session-histories"]) expect(classifyClaudeTranscript(`snapshot/home/.claude/${root}/x/a.jsonl`)).toBe("main");
  expect(classifyClaudeTranscript("a.jsonl")).toBe("main");
  expect(classifyClaudeTranscript("projects/a/subagents/workflows/wf_a/agent-child.jsonl")).toEqual({ agentId: "child", transcriptKind: "workflow" });
  for (const path of ["jobs/timeline.jsonl", "projects/a/subagents/journal.jsonl", "projects/._a.jsonl"]) expect(classifyClaudeTranscript(path)).toBeUndefined();
});
test("delegation context survives compaction and exact payload/masking stays native", () => {
  const parse = createClaudeRawParser({ path: "agent-child.jsonl", sidechain: { agentId: "child", transcriptKind: "subagent" } }, true);
  parse(bytes({ type: "session", sessionId: "parent", cwd: "/work", gitBranch: "main" }));
  const text = "use xoxb-1234567890abcdef " + "z".repeat(5000);
  const [episode] = parse(bytes({ type: "system", subtype: "compact_boundary", uuid: "summary", timestamp: 1000, content: text }));
  expect(episode!.input.origin).toEqual({ source: "claude-code", session: "child", actor: "unknown", record: "summary" });
  expect(episode!.input.properties).toEqual({ kind: "compaction", cwd: "/work", git_branch: "main", is_sidechain: "true", agent_id: "child", transcript_kind: "subagent", parent_session_id: "parent" });
  expect(episode!.redactions).toBe(1);
  expect(Buffer.from(episode!.input.payload!).toString()).toBe(text.replace("xoxb-1234567890abcdef", "[REDACTED]"));
  expect(episode!.input.payload_media_type).toBe("text/plain");
  expect(episode!.input.source_revision).toBe(createHash("sha256").update("1970-01-01T00:00:01.000Z\n" + text).digest("hex"));
});
