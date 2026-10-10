const line = (record: object): string => JSON.stringify(record);

export const claudeFixture = [
  line({ type: "session", sessionId: "resumed", cwd: "/work", gitBranch: "main" }),
  line({ type: "user", uuid: "user-1", timestamp: 1000, content: [
    { type: "tool_result", content: "ignored" }, { type: "text", text: "kept beside result" },
  ] }),
  line({ type: "assistant", uuid: "tool-only", timestamp: 2000, message: {
    role: "assistant", content: [{ type: "tool_use", id: "call-1" }],
  } }),
  line({ type: "assistant", uuid: "thinking", timestamp: 3000, message: {
    role: "assistant", content: [{ type: "thinking", text: "reason" }, { type: "text", text: "answer" }],
  } }),
  line({ type: "summary", uuid: "summary", timestamp: 4000, summary: "conversation recap" }),
  line({ type: "system", uuid: "system", timestamp: 5000, content: "system event" }),
  line({ type: "file-history-snapshot", uuid: "snapshot", timestamp: 6000, snapshot: "metadata" }),
  line({ type: "user", uuid: "role-mismatch", timestamp: 7000, message: {
    role: "assistant", content: [{ type: "text", text: "strict rejected" }],
  } }),
].join("\n") + "\n";
