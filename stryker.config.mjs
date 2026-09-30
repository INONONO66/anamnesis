// Mutation gate. The sandbox lives outside the project tree so bun's workspace
// resolution and the docker-backed suites are not disturbed by Stryker's copies.
export default {
  mutate: ["app/anamnesis/**/*.ts", "packages/*/src/**/*.ts", "!**/*.test.ts", "!**/*.fixture.ts"],
  testRunner: "command",
  commandRunner: { command: "bun test $(cat /tmp/pure-tests-cut.txt) app/anamnesis/recovery-probe.test.ts packages/core/src/schema-migrations.test.ts" },
  tempDirName: "/tmp/anamnesis-stryker",
  reporters: ["progress", "clear-text"],
  thresholds: { high: 100, low: 100, break: 100 },
};
