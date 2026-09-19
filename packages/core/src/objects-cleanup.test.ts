import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs/promises";
import { syncBuiltinESMExports } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spyOn, test } from "bun:test";
import { after } from "node:test";
import { ObjectStore } from "./objects.ts";

class FilesystemFault extends Error {
  readonly name = "FilesystemFault";
  constructor(readonly code: string, readonly syscall: string) { super(`owned ObjectStore fault: ${syscall}`); }
}

const payloadUUID = "00000000-0000-4000-8000-000000000001";
const metadataUUID = "00000000-0000-4000-8000-000000000002";
const reports: unknown[] = [];
const patch = (object: object, method: string, implementation: (...args: any[]) => any) =>
  spyOn(object as any, method).mockImplementation(implementation);
const stages = ["write", "sync", "close", "rename", "directory-sync", "collision", "cleanup", "success"] as const;
const scenarios = [
  ...stages.map(fault => ({ target: "payload", repair: false, fault })),
  ...[false, true].flatMap(repair => [...stages, "serialize", "rename-enoent"].map(fault => ({ target: "metadata", repair, fault }))),
  { target: "payload", repair: false, fault: "competitor" },
];

// These tests run serially and restore every hook. Filesystem hooks match an
// exact fixture-owned path or a real handle opened there; no prototype hooks.
for (const scenario of scenarios) {
  const label = `${scenario.repair ? "repair" : "new"}-${scenario.target}-${scenario.fault}`;
  test(`ObjectStore temp ownership: ${label}`, async () => {
    const root = await fs.mkdtemp(join(tmpdir(), "ana-object-cleanup-"));
    const store = new ObjectStore(root);
    const bytes = Buffer.from(`payload ${label}`);
    const hash = crypto.createHash("sha256").update(bytes).digest("hex");
    const directory = join(root, hash.slice(0, 2));
    const path = join(directory, hash), sidecar = `${path}.json`;
    const payloadTemp = join(directory, `.${hash}.${payloadUUID}.tmp`);
    const metadataTemp = `${sidecar}.${metadataUUID}.tmp`;
    const targetTemp = scenario.target === "payload" ? payloadTemp : metadataTemp;
    const unrelated = join(directory, `.${hash}.unrelated.tmp`);
    const metadata = { hash, size: bytes.length, mediaType: "text/plain" };
    const original = { open: fs.open, rename: fs.rename, unlink: fs.unlink, stringify: JSON.stringify };
    const error = new FilesystemFault(scenario.fault === "rename-enoent" ? "ENOENT" : scenario.fault === "write" ? "ENOSPC" : "EIO", scenario.fault);
    const events: string[] = [];
    const handles: Awaited<ReturnType<typeof fs.open>>[] = [];
    const acquired: string[] = [];
    let injections = 0, uuidCalls = 0, renamed = "", collisionError: unknown;
    let committedPath = "", committedSidecar = "", committedBytes = Buffer.alloc(0), committedJson = "";
    let spies: any[] = [];
    let repairInode: number | undefined;
    let verified = false;
    const fail = (stage: string) => { if (scenario.fault === stage) { injections++; throw error; } };
    try {
      await fs.mkdir(directory, { recursive: true });
      await fs.writeFile(unrelated, "retain unrelated temp");
      committedBytes = Buffer.from(`already committed ${label}`);
      const committed = await store.put(committedBytes, "application/octet-stream");
      committedPath = join(root, committed.hash.slice(0, 2), committed.hash);
      committedSidecar = `${committedPath}.json`;
      committedJson = await fs.readFile(committedSidecar, "utf8");
      if (scenario.repair) {
        await fs.writeFile(path, bytes);
        repairInode = (await fs.stat(path)).ino;
      }
      if (scenario.fault === "collision") await fs.writeFile(targetTemp, "another creator owns this");
      spies = [patch(crypto, "randomUUID", () => {
        uuidCalls++;
        assert.ok(uuidCalls <= (scenario.repair ? 1 : 2), "unexpected publication retry inside put");
        return scenario.repair || uuidCalls === 2 ? metadataUUID : payloadUUID;
      }),
      ];
      spies.push(patch(fs, "open", async (...args: Parameters<typeof fs.open>) => {
        const [target, flags, mode] = args;
        if (target === directory && flags === "r") {
          const phase = renamed;
          events.push(`${phase}:directory-open`);
          const file = await original.open(...args);
          handles.push(file);
          const sync = file.sync.bind(file), close = file.close.bind(file);
          file.sync = async () => {
            events.push(`${phase}:directory-sync`);
            if (phase === scenario.target) fail("directory-sync");
            await sync();
          };
          file.close = async () => { events.push(`${phase}:directory-close`); await close(); };
          return file;
        }
        if (target !== payloadTemp && target !== metadataTemp) return original.open(...args);
        assert.equal(flags, "wx"); assert.equal(mode, 0o600);
        const phase = target === payloadTemp ? "payload" : "metadata";
        events.push(`${phase}:open`);
        let file: Awaited<ReturnType<typeof fs.open>>;
        try { file = await original.open(...args); }
        catch (caught) { collisionError = caught; throw caught; }
        handles.push(file); acquired.push(target);
        const write = file.writeFile.bind(file), sync = file.sync.bind(file), close = file.close.bind(file);
        file.writeFile = async (...writeArgs: Parameters<typeof file.writeFile>) => {
          events.push(`${phase}:write`);
          if (phase === scenario.target && scenario.fault === "write") {
            await write("partial");
            assert.equal(await fs.readFile(target, "utf8"), "partial");
            fail("write");
          }
          await write(...writeArgs);
        };
        file.sync = async () => { events.push(`${phase}:sync`); if (phase === scenario.target) fail("sync"); await sync(); };
        file.close = async () => {
          events.push(`${phase}:close`); await close();
          if (phase === scenario.target) fail("close");
          if (phase === "payload" && scenario.fault === "competitor") {
            // A competing creator commits between our initial and final has().
            await fs.writeFile(path, bytes);
            await fs.writeFile(sidecar, original.stringify(metadata));
          }
        };
        return file;
      }));
      spies.push(patch(JSON, "stringify", (...args: Parameters<typeof JSON.stringify>) => {
        if (args[0]?.hash === hash && args[0]?.mediaType === metadata.mediaType) {
          events.push("metadata:serialize"); fail("serialize");
        }
        return original.stringify(...args);
      }));
      spies.push(patch(fs, "rename", async (...args: Parameters<typeof fs.rename>) => {
        if (args[0] !== payloadTemp && args[0] !== metadataTemp) return original.rename(...args);
        const phase = args[0] === payloadTemp ? "payload" : "metadata";
        assert.equal(args[1], phase === "payload" ? path : sidecar);
        events.push(`${phase}:rename`);
        assert.equal((await fs.stat(args[0])).mode & 0o777, 0o600);
        if (phase === "payload") assert.deepEqual(await fs.readFile(args[0]), bytes);
        else {
          assert.deepEqual(await fs.readFile(path), bytes);
          assert.deepEqual(JSON.parse(await fs.readFile(args[0], "utf8")), metadata);
          assert.equal(await store.has(hash), false, "sidecar is the commit marker");
        }
        if (phase === scenario.target) {
          fail("rename"); fail("rename-enoent");
          // Make cleanup necessary without a second injected error.
          if (scenario.fault === "cleanup") { await fs.mkdir(phase === "payload" ? path : sidecar); }
        }
        await original.rename(...args);
        renamed = phase;
      }));
      spies.push(patch(fs, "unlink", async (...args: Parameters<typeof fs.unlink>) => {
        if (args[0] === payloadTemp || args[0] === metadataTemp) {
          events.push(`${args[0] === payloadTemp ? "payload" : "metadata"}:cleanup`);
          if (args[0] === targetTemp) fail("cleanup");
        }
        await original.unlink(...args);
      }));
      syncBuiltinESMExports();
      const operation = store.put(bytes, metadata.mediaType);
      if (scenario.fault === "success" || scenario.fault === "competitor") assert.deepEqual(await operation, metadata);
      else await assert.rejects(operation, caught => {
        assert.equal(caught, scenario.fault === "collision" ? collisionError : error);
        if (scenario.fault === "collision") assert.equal((caught as NodeJS.ErrnoException).code, "EEXIST");
        else assert.ok(caught instanceof FilesystemFault);
        return true;
      });
      assert.equal(injections, ["success", "competitor", "collision"].includes(scenario.fault) ? 0 : 1);
      // Successful operations must exercise the hooks too, not pass vacuously
      // when a runtime keeps stale namespace bindings for patched builtins.
      const expectedUUIDCalls = scenario.repair || (scenario.target === "payload" && scenario.fault !== "success") ? 1 : 2;
      assert.equal(uuidCalls, expectedUUIDCalls);
      const attempted = scenario.repair ? [metadataTemp] : expectedUUIDCalls === 1 ? [payloadTemp] : [payloadTemp, metadataTemp];
      assert.deepEqual(acquired, attempted.filter(temporary => scenario.fault !== "collision" || temporary !== targetTemp));
      for (const file of handles) assert.equal(file.fd, -1);
      if (scenario.fault === "collision") assert.equal(await fs.readFile(targetTemp, "utf8"), "another creator owns this");
      for (const temporary of acquired) {
        if (temporary === targetTemp && scenario.fault === "cleanup") assert.ok((await fs.stat(temporary)).isFile());
        else await assert.rejects(fs.lstat(temporary), { code: "ENOENT" });
      }
      if (scenario.fault !== "cleanup") {
        const committed = scenario.fault === "success" || scenario.fault === "competitor" || (scenario.target === "metadata" && scenario.fault === "directory-sync");
        assert.equal(await store.has(hash), committed);
        const dataExists = scenario.repair || scenario.target === "metadata" || committed || scenario.fault === "directory-sync";
        if (dataExists) assert.deepEqual(await fs.readFile(path), bytes);
        else await assert.rejects(fs.lstat(path), { code: "ENOENT" });
        if (committed) assert.deepEqual(JSON.parse(await fs.readFile(sidecar, "utf8")), metadata);
        else await assert.rejects(fs.lstat(sidecar), { code: "ENOENT" });
      }
      if (!scenario.repair && events.includes("metadata:open")) {
        assert.ok(events.indexOf("payload:directory-close") < events.indexOf("metadata:open"));
        assert.ok(events.indexOf("payload:rename") < events.indexOf("payload:directory-sync"));
      }
      for (const phase of ["payload", "metadata"]) {
        if (events.includes(`${phase}:rename`)) assert.ok(events.indexOf(`${phase}:close`) < events.indexOf(`${phase}:rename`));
        if (events.includes(`${phase}:close`) && scenario.fault !== "write" && scenario.fault !== "serialize") {
          assert.ok(events.indexOf(`${phase}:sync`) < events.indexOf(`${phase}:close`));
        }
      }
      for (const spy of spies) spy.mockRestore(); syncBuiltinESMExports();
      if (scenario.fault === "cleanup") await fs.rmdir(scenario.target === "payload" ? path : sidecar);
      const beforeRetry = await fs.stat(path).catch(caught => { if (caught.code === "ENOENT") return undefined; throw caught; });
      await store.put(bytes, metadata.mediaType);
      assert.equal(await store.has(hash), true);
      assert.deepEqual(await store.get(hash), new Uint8Array(bytes));
      if (beforeRetry) assert.equal((await fs.stat(path)).ino, beforeRetry.ino, "retry preserves existing data");
      if (repairInode !== undefined) assert.equal((await fs.stat(path)).ino, repairInode);
      const markerBefore = await fs.stat(sidecar);
      await store.put(bytes, "different/type");
      assert.equal((await fs.stat(sidecar)).ino, markerBefore.ino);
      assert.deepEqual(JSON.parse(await fs.readFile(sidecar, "utf8")), metadata);
      verified = true;
    } finally {
      for (const spy of spies) spy.mockRestore(); syncBuiltinESMExports();
      // Evidence is captured BEFORE teardown, including genuine RED leftovers.
      // Unrelated temp and committed object/marker are always verified first.
      try {
        assert.equal(await fs.readFile(unrelated, "utf8"), "retain unrelated temp");
        assert.deepEqual(await fs.readFile(committedPath), committedBytes);
        assert.equal(await fs.readFile(committedSidecar, "utf8"), committedJson);
        const temps = [];
        for (const temporary of [payloadTemp, metadataTemp]) {
          const info = await fs.lstat(temporary).catch(caught => { if (caught.code === "ENOENT") return undefined; throw caught; });
          temps.push({ path: temporary, acquired: acquired.includes(temporary), existsBeforeTeardown: !!info });
        }
        const report = { label, root, verified, uuidCalls, injections, unrelatedVerified: true, committedVerified: true, temps, events, handlesClosed: handles.every(file => file.fd === -1) };
        reports.push(report);
        console.error(original.stringify(report));
      } finally {
        await fs.rm(root, { recursive: true });
        await assert.rejects(fs.lstat(root), { code: "ENOENT" });
      }
    }
  });
}

after(async () => {
  const reportPath = process.env["OBJECTSTORE_CLEANUP_REPORT"];
  if (reportPath) await fs.writeFile(resolve(reportPath), JSON.stringify({ cases: reports, fixtureRootsRemoved: true }, null, 2) + "\n");
});
