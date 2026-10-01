// Mutation gate. The sandbox lives outside the project tree so bun's workspace
// resolution and the docker-backed suites are not disturbed by Stryker's copies.
// scripts/gate/mutation.ts owns the run: it passes --mutate for the executed line ranges of one group of
// sources and the command that runs exactly the pure tests loading them.
// Without the gate's environment the only command is a refusal, so a bare `stryker run` fails its initial
// run loudly instead of mutating against some default suite. (knip imports this file, so it must not throw.)
const command = process.env.STRYKER_TEST_COMMAND ?? "echo 'run Stryker through: bun run gate:mutation' >&2; exit 1";
const report = process.env.STRYKER_REPORT ?? "/tmp/anamnesis-stryker/report.json";

export default {
  testRunner: "command",
  commandRunner: { command },
  ignorePatterns: [".omo", "dist"],
  tempDirName: "/tmp/anamnesis-stryker",
  // A run with survivors exits non-zero; its sandbox copy must still go, or every gate run leaves one behind.
  cleanTempDir: "always",
  reporters: ["clear-text", "json"],
  jsonReporter: { fileName: report },
  // Scoped commands finish in a few seconds; a minute of headroom keeps host load from reading as a timeout.
  timeoutMS: 60000,
  thresholds: { high: 100, low: 100, break: 100 },
};
