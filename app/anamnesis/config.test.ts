import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { syncBuiltinESMExports } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { atomicJson } from "./config.ts";

class FilesystemFault extends Error {
  readonly name = "FilesystemFault";
  readonly code: string;
  readonly syscall: string;
  constructor(code: string, syscall: string) {
    super(`owned atomicJson fault: ${syscall}`);
    this.code = code;
    this.syscall = syscall;
  }
}

const cases = [
  { fault: "open", code: "EACCES", events: ["open", "cleanup"] },
  { fault: "open-created", code: "ENOSPC", events: ["open", "cleanup"] },
  { fault: "serialize", code: "EINVAL", events: ["open", "close", "cleanup"] },
  { fault: "write", code: "ENOSPC", events: ["open", "write", "close", "cleanup"] },
  { fault: "sync", code: "EIO", events: ["open", "write", "sync", "close", "cleanup"] },
  { fault: "close", code: "EIO", events: ["open", "write", "sync", "close", "cleanup"] },
  { fault: "rename", code: "EIO", events: ["open", "write", "sync", "close", "rename", "cleanup"] },
  { fault: "directory-open", code: "EIO", events: ["open", "write", "sync", "close", "rename", "directory-open", "cleanup"] },
  { fault: "directory-sync", code: "EIO", events: ["open", "write", "sync", "close", "rename", "directory-open", "directory-sync", "directory-close", "cleanup"] },
  { fault: "directory-close", code: "EIO", events: ["open", "write", "sync", "close", "rename", "directory-open", "directory-sync", "directory-close", "cleanup"] },
  { fault: "cleanup", code: "EIO", events: ["open", "write", "sync", "close", "rename", "directory-open", "directory-sync", "directory-close", "cleanup"] },
  { fault: "success", code: "", events: ["open", "write", "sync", "close", "rename", "directory-open", "directory-sync", "directory-close", "cleanup"] },
  { fault: "collision", code: "EEXIST", events: ["open"] },
] as const;

// Node's test runner isolates this file in its own process. Hooks delegate all
// unrelated paths, and only wrap real handles belonging to this test's directory.
for (const scenario of cases) {
  test(`atomicJson preserves publication and temp ownership when ${scenario.fault}`, { timeout: 10_000 }, async t => {
    // Given: an owned directory, an existing destination, and an unrelated temp.
    const root = await fs.mkdtemp(join(tmpdir(), "ana-atomic-json-"));
    const path = join(root, "state.json"), unrelated = join(root, "state.json.unrelated.tmp");
    const before = '{"version":1}\n', after = '{"version":2}\n';
    const original = { open: fs.open, rename: fs.rename, rm: fs.rm };
    const injected = new FilesystemFault(scenario.code, scenario.fault);
    const events: string[] = [], handles: Awaited<ReturnType<typeof fs.open>>[] = [];
    let temporary: string | undefined, injections = 0;
    const fail = (stage: string) => {
      if (stage === scenario.fault) { injections++; throw injected; }
    };
    try {
      await fs.writeFile(path, before, { mode: 0o600 });
      await fs.writeFile(unrelated, "retain unrelated temp", { mode: 0o600 });
      t.mock.method(fs, "open", async (...args: Parameters<typeof fs.open>) => {
        const [target, flags, mode] = args;
        if (target === root && flags === "r") {
          events.push("directory-open"); fail("directory-open");
          const file = await original.open(...args);
          handles.push(file);
          const sync = file.sync.bind(file), close = file.close.bind(file);
          file.sync = async () => { events.push("directory-sync"); fail("directory-sync"); await sync(); };
          file.close = async () => { events.push("directory-close"); await close(); fail("directory-close"); };
          return file;
        }
        if (typeof target !== "string" || dirname(target) !== root || flags !== "wx") return original.open(...args);
        assert.match(target, new RegExp(`^${path.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\.[0-9a-f-]{36}\\.tmp$`));
        assert.equal(mode, 0o600);
        temporary = target;
        events.push("open"); fail("open");
        if (scenario.fault === "collision") await fs.writeFile(target, "retain existing temp", { mode: 0o600 });
        const file = await original.open(...args);
        handles.push(file);
        if (scenario.fault === "open-created") {
          // Model an open that creates its pathname before reporting an I/O
          // failure. No handle is returned to the caller; the shim closes it.
          await file.close(); fail("open-created");
        }
        const write = file.writeFile.bind(file), sync = file.sync.bind(file), close = file.close.bind(file);
        file.writeFile = async (...writeArgs: Parameters<typeof file.writeFile>) => {
          events.push("write");
          if (scenario.fault === "write") { await write('{"version":'); fail("write"); }
          await write(...writeArgs);
        };
        file.sync = async () => { events.push("sync"); fail("sync"); await sync(); };
        file.close = async () => { events.push("close"); await close(); fail("close"); };
        return file;
      });
      t.mock.method(fs, "rename", async (...args: Parameters<typeof fs.rename>) => {
        if (args[0] === temporary && args[1] === path) {
          events.push("rename");
          assert.equal(await fs.readFile(path, "utf8"), before);
          assert.equal(await fs.readFile(args[0], "utf8"), after);
          assert.equal((await fs.stat(args[0])).mode & 0o777, 0o600);
          fail("rename");
        }
        await original.rename(...args);
      });
      t.mock.method(fs, "rm", async (...args: Parameters<typeof fs.rm>) => {
        if (args[0] === temporary) { events.push("cleanup"); fail("cleanup"); }
        await original.rm(...args);
      });
      syncBuiltinESMExports();

      // When: exactly one production atomic write, with at most one I/O fault.
      const value = scenario.fault === "serialize" ? { toJSON() { fail("serialize"); } } : { version: 2 };
      const operation = atomicJson(path, value);
      if (scenario.fault === "success") await operation;
      else if (scenario.fault === "collision") await assert.rejects(operation, { code: "EEXIST", syscall: "open" });
      else await assert.rejects(operation, error => {
        assert.equal(error, injected); // Original typed error, not a replacement.
        return true;
      });

      // Then: check the exact acquired pathname before fixture teardown, not a
      // directory-wide sweep or an assertion satisfied by our own cleanup.
      assert.equal(typeof temporary, "string");
      assert.ok(temporary);
      if (scenario.fault === "collision") assert.equal(await fs.readFile(temporary, "utf8"), "retain existing temp");
      else await assert.rejects(fs.lstat(temporary), { code: "ENOENT" });
      const published = events.includes("directory-open");
      assert.equal(await fs.readFile(path, "utf8"), published ? after : before);
      assert.equal(await fs.readFile(unrelated, "utf8"), "retain unrelated temp");
      assert.deepEqual(events, scenario.events);
      assert.equal(injections, scenario.fault === "success" || scenario.fault === "collision" ? 0 : 1);
      for (const file of handles) assert.equal(file.fd, -1);
      t.diagnostic(JSON.stringify({ fault: scenario.fault, temporary, absent: scenario.fault !== "collision", published, events }));
    } finally {
      t.mock.restoreAll();
      syncBuiltinESMExports();
      await original.rm(root, { recursive: true, force: true });
    }
  });
}
