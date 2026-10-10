const stamp = "2026-03-09T15:36:16.000Z";
const line = (type: string, payload: object): string => JSON.stringify({ timestamp: stamp, type, payload });

export const codexFixture = [
  line("session_meta", { id: "codex-session", cwd: "/work" }),
  line("response_item", { type: "message", role: "user", id: "user-1", content: [
    { type: "function_call_output", output: "ignored" }, { type: "input_text", text: "kept beside result" },
  ] }),
  line("response_item", { type: "message", role: "assistant", content: [
    { type: "function_call", arguments: "{}" },
  ] }),
  line("response_item", { type: "message", role: "assistant", content: [
    { type: "reasoning", text: "think" }, { type: "output_text", text: "answer" },
  ] }),
  line("response_item", { type: "reasoning", summary: [{ text: "internal reason" }] }),
  line("response_item", { type: "function_call", name: "shell", arguments: "{}" }),
  line("response_item", { type: "function_call_output", output: "tool result" }),
  line("event_msg", { type: "token_count", total: 12 }),
  line("response_item", { type: "message", role: "developer", content: [{ type: "input_text", text: "harness" }] }),
  line("response_item", { type: "context_compacted" }),
  line("turn_context", { model: "gpt-test" }),
].join("\n") + "\n";
