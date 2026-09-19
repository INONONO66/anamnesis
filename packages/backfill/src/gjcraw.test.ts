import { strict as assert } from "node:assert";
import { test } from "bun:test";
import { createGjcRawParser } from "./gjcraw.ts";

test("GJC raw parser inherits session, joins text, and excludes tools/scratch", () => {
  const parse = createGjcRawParser();
  assert.deepEqual(parse(JSON.stringify({ type: "session", id: "sess-1" })), []);
  const [episode] = parse(JSON.stringify({
    type: "message", id: "evt-1", timestamp: "2026-01-01T00:00:00Z",
    message: { role: "user", content: [
      { type: "text", text: "hello" }, { type: "thinking", text: "secret scratch" },
      { type: "text", text: "world" }, { type: "toolCall", text: "ignored" },
    ] },
  }));
  assert.equal(episode?.input.origin.source, "gjc");
  assert.equal(episode?.input.origin.session, "sess-1");
  assert.equal(episode?.input.origin.record, "evt-1");
  assert.equal(episode?.input.content, "hello\nworld");
  assert.equal(episode?.input.properties?.canonical_kind, "agent_message");
  assert.deepEqual(parse(JSON.stringify({
    type: "message", id: "tool-1", timestamp: "2026-01-01T00:00:01Z",
    message: { role: "toolResult", content: [{ type: "text", text: "ignored" }] },
  })), []);
});

test("GJC compaction preserves native identity and summary", () => {
  const parse = createGjcRawParser();
  parse(JSON.stringify({ type: "session", id: "sess-2" }));
  const [episode] = parse(JSON.stringify({ type: "compaction", id: "compact-1", timestamp: "2026-01-01T00:00:00Z", summary: "prior context" }));
  assert.equal(episode?.input.origin.session, "sess-2");
  assert.equal(episode?.input.origin.record, "compact-1");
  assert.equal(episode?.input.content, "prior context");
  assert.equal(episode?.input.properties?.canonical_kind, "compaction");
});
