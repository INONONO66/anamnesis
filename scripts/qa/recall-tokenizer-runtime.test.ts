import { expect, test } from "bun:test";
import { startProcess } from "./runtime-scenarios.ts";

test("installed exact tokenizer through real Node UDS and owned Neo4j", async () => {
  const child = startProcess("node", ["app/anamnesis/tokenizer.surface.mjs"], {
    deadlineMs: 240000, onOutput: text => process.stdout.write(text),
  });
  const result = await child.done;
  expect(result.timedOut).toBe(false);
  expect(result.code).toBe(0);
}, 250000);
