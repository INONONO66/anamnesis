import { expect, test } from "bun:test";
import { OwnedNeo4jAdapter, type OwnedNeo4jAdapterOptions } from "./owned-neo4j-adapter.ts";
import type { ArchiveManifest } from "./archive-manifest.ts";

const CONTAINER = "anamnesis-qa-neo4j";
const OWNER = "qa-owner";
const OWNER_LABEL = "anamnesis.qa.owner";
const INSPECT = ["inspect", "--format", "{{json .}}", CONTAINER];

type Arg = string | number | ArchiveManifest | undefined;

/** Each delegate rejects with its own name: Promise<never> satisfies every declared return type without inventing result shapes. */
function harness(ownerLabel: string) {
  const calls: { name: string; args: Arg[] }[] = [];
  const execArgs: string[][] = [];
  const delegate = (name: string) => async (...args: Arg[]): Promise<never> => {
    calls.push({ name, args });
    throw new Error(`delegated:${name}`);
  };
  const options: OwnedNeo4jAdapterOptions = {
    container: CONTAINER,
    owner: OWNER,
    exec: async args => {
      execArgs.push(args);
      return { stdout: JSON.stringify({ Config: { Labels: { [OWNER_LABEL]: ownerLabel } }, State: { Running: true } }) };
    },
    authority: {
      revokeWriters: delegate("revokeWriters"),
      authoritySnapshot: delegate("authoritySnapshot"),
      restoredAuthoritySnapshot: delegate("restoredAuthoritySnapshot"),
      materializeMembers: delegate("materializeMembers"),
      startAndReady: delegate("startAndReady"),
      rebindSource: delegate("rebindSource"),
      verifyPhysicalLinks: delegate("verifyPhysicalLinks"),
      quarantine: delegate("quarantine"),
    },
    lifecycle: { stop: delegate("stop") },
  };
  return { adapter: new OwnedNeo4jAdapter(options), calls, execArgs };
}

const manifest = { format: "anamnesis.archive/1" } as ArchiveManifest;

test("authority and lifecycle methods pass their arguments straight through", async () => {
  const { adapter, calls, execArgs } = harness(OWNER);
  const expected: [string, () => Promise<object | void>, Arg[]][] = [
    ["revokeWriters", () => adapter.revokeWriters(), []],
    ["authoritySnapshot", () => adapter.authoritySnapshot("7"), ["7"]],
    ["restoredAuthoritySnapshot", () => adapter.restoredAuthoritySnapshot(), []],
    ["materializeMembers", () => adapter.materializeMembers("/root", manifest), ["/root", manifest]],
    ["startAndReady", () => adapter.startAndReady("/root", "7"), ["/root", "7"]],
    ["rebindSource", () => adapter.rebindSource("src-1"), ["src-1"]],
    ["verifyPhysicalLinks", () => adapter.verifyPhysicalLinks("/root"), ["/root"]],
    ["quarantine", () => adapter.quarantine("/root"), ["/root"]],
    ["stop", () => adapter.stop(), []],
  ];
  for (const [name, call] of expected) await expect(call()).rejects.toThrow(`delegated:${name}`);
  expect(calls).toEqual(expected.map(([name, , args]) => ({ name, args })));
  expect(execArgs).toEqual([]);
});

test("dumpOffline refuses a container that is not labelled with the owner before touching docker", async () => {
  const { adapter, execArgs } = harness("someone-else");
  await expect(adapter.dumpOffline("/tmp/never-written.dump", "7")).rejects.toThrow("owned_container_required");
  expect(execArgs).toEqual([INSPECT]);
});

test("dumpOffline refuses a non-numeric writer epoch after ownership passes", async () => {
  const { adapter, execArgs } = harness(OWNER);
  await expect(adapter.dumpOffline("/tmp/never-written.dump", "epoch-7")).rejects.toThrow("invalid_writer_epoch");
  expect(execArgs).toEqual([INSPECT]);
});
