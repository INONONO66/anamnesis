const at = "2026-04-05T06:07:08Z";
const event = (value: object) => JSON.stringify(value);

export const sessionFixture = [
  event({ type: "session", id: "native-session" }),
  event({ type: "message", id: "m-user", timestamp: at, message: { role: "user", content: [
    { type: "text", text: "first" }, { type: "thinking", text: "reasoning" },
    { type: "toolCall", name: "inspect" }, { type: "text", text: "second" },
  ] } }),
  event({ type: "message", id: "m-tool-only", timestamp: at, message: { role: "toolResult", content: [{ type: "toolResult", text: "hidden" }] } }),
  event({ type: "message", id: "m-tool-text", timestamp: at, message: { role: "toolResult", content: [{ type: "text", text: "tool output" }] } }),
  event({ type: "message", id: "m-system", timestamp: at, message: { role: "system", content: [{ type: "text", text: "system context" }] } }),
  event({ type: "message", id: "m-developer", timestamp: at, message: { role: "developer", content: [{ type: "text", text: "developer context" }] } }),
  event({ type: "compaction", id: "c-1", timestamp: at, summary: "compact summary" }),
  event({ type: "custom", id: "x-1", timestamp: at, payload: { detail: "event data" } }),
].join("\n") + "\n";
