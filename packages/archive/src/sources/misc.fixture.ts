const timestamp = 1782794590501;
export const miscTime = timestamp;
const line = (record: object) => JSON.stringify(record);

export const asideFixture = [
  line({ role: "user", timestamp, content: "plain prompt" }),
  line({ role: "toolResult", timestamp, content: [{ type: "toolResult", text: "omitted" }] }),
  line({ role: "toolResult", timestamp, content: [{ type: "text", text: "tool output" }] }),
  line({ role: "system", timestamp, content: "system context" }),
  line({ role: "assistant", timestamp, content: [{ type: "text", text: "answer" }, { type: "thinking", text: "reason" }] }),
  line({ type: "compaction", role: "system", timestamp, summary: "compacted context" }),
].join("\n") + "\n";

export const antigravityFixture = [
  line({ type: "USER_INPUT", step_index: 1, created_at: new Date(timestamp).toISOString(), content: "<USER_REQUEST>\nrequest\n</USER_REQUEST>\nmetadata" }),
  line({ type: "PLANNER_RESPONSE", step_index: 2, created_at: new Date(timestamp).toISOString(), content: "answer" }),
  line({ type: "RUN_COMMAND", step_index: 3, created_at: new Date(timestamp).toISOString(), content: "tool report" }),
  line({ type: "SYSTEM_MESSAGE", step_index: 4, created_at: new Date(timestamp).toISOString(), content: "system context" }),
  line({ type: "compaction", step_index: 5, created_at: new Date(timestamp).toISOString(), content: "summary" }),
].join("\n") + "\n";

export const opencodeFixture = [
  line({ input: "prompt", parts: [{ type: "text", text: "attachment" }, { type: "toolResult", text: "ignored" }] }),
  line({ input: "another" }),
].join("\n") + "\n";
