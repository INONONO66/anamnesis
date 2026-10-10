export const slackFixture = [
  { ts: "100.25", user: "U1", text: "ordinary message" },
  { ts: "101.5", user: "B1", subtype: "bot_message", text: "bot output" },
  { ts: "102.0", user: "U2", subtype: "channel_join", text: "joined" },
  { ts: "103.0", user: "U3", text: "edited message", edited: { ts: "104.0" }, thread_ts: "100.25" },
] as const;
