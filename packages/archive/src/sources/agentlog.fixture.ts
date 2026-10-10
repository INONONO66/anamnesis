export const agentlogFixture = [
  { provider: "codex", partition_id: "session-1", upstream_event_id: "turn-1", occurred_at: 1000, role: "assistant", kind: "message", canonical_kind: "message", text: "first" },
  { provider: "codex", partition_id: "session-1", upstream_event_id: "empty", occurred_at: 1001, role: "tool", kind: "tool_result", canonical_kind: "system-event", text: "" },
  { provider: "codex", partition_id: "session-2", upstream_event_id: "large", occurred_at: 1002, role: "custom", kind: "summary", canonical_kind: "summary", text: "x".repeat(1024 * 1024 + 10) },
] as const;
