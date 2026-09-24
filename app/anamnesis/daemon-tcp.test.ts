// Real Node daemon over TCP with a bearer frame; no database is required because
// hello/status answer offline. Every wait is an event with a bounded deadline.
import { afterAll, beforeAll, expect, test } from "bun:test";
import { execFile, spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import { once } from "node:events";
import { chmod, mkdtemp, readdir, rm, stat, writeFile } from "node:fs/promises";
import { connect, createServer, type AddressInfo } from "node:net";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { RpcClient } from "./client.ts";
import { Frames } from "./wire.ts";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const TOKEN = "installation-token-fixture";
const BEARER = `listen-bearer-${randomUUID()}`;
const HOST = "127.0.0.1";
const hello = { token: TOKEN, client: "tcp-test", commit_mode: "receipt", version: 1 };
const deadline = (ms = 15_000) => AbortSignal.timeout(ms);
const frame = (value: unknown) => {
  const body = Buffer.from(JSON.stringify(value));
  const bytes = Buffer.allocUnsafe(4 + body.length);
  bytes.writeUInt32BE(body.length); body.copy(bytes, 4);
  return bytes;
};

let build: string, bundle: string;
beforeAll(async () => {
  build = await mkdtemp("/tmp/ana-tcp-bundle-");
  bundle = join(build, "main.mjs");
  await promisify(execFile)(process.execPath, ["build", "app/anamnesis/main.ts", "--target=node", "--outfile", bundle], { cwd: ROOT });
});
afterAll(async () => { await rm(build, { recursive: true, force: true }); });

interface Listening { event: "listening"; socket: string; tcp?: { host: string; port: number }; data_incarnation: string; }
interface Daemon { root: string; child: ChildProcess; output(): string; listening: Promise<Listening>; exit: Promise<[number | null, NodeJS.Signals | null]>; }
type Configure = (root: string) => Promise<NodeJS.ProcessEnv>;

async function fixture(configure: Configure, run: (daemon: Daemon) => Promise<void>): Promise<void> {
  const root = await mkdtemp("/tmp/ana-tcp-");
  let child: ChildProcess | undefined;
  try {
    const env: NodeJS.ProcessEnv = { ...process.env };
    for (const name of Object.keys(env)) if (/^ANAMNESIS_(LLM_|EMBEDDING_|EXTRACTION_|LISTEN)/.test(name)) delete env[name];
    Object.assign(env, { ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: TOKEN, ANAMNESIS_NEO4J_PASSWORD: "unused-offline", ANAMNESIS_NEO4J_URI: "bolt://127.0.0.1:1" }, await configure(root));
    child = spawn("node", [bundle], { env, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "", stderr = "";
    child.stderr!.on("data", (bytes: Buffer) => { stderr += bytes; });
    const exit = once(child, "exit") as Promise<[number | null, NodeJS.Signals | null]>;
    const listening = new Promise<Listening>((resolve, reject) => {
      createInterface({ input: child!.stdout! }).on("line", line => {
        stdout += line + "\n";
        const value = JSON.parse(line);
        if (value.event === "listening") resolve(value);
      });
      exit.then(([code]) => reject(new Error(`daemon exited ${code} before listening: ${stderr}`)));
    });
    listening.catch(() => {}); // Startup-failure cases observe exit instead.
    await run({ root, child, listening, exit, output: () => stdout + stderr });
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) {
      const exited = once(child, "exit", { signal: deadline() });
      child.kill("SIGKILL");
      await exited;
    }
    await rm(root, { recursive: true, force: true });
  }
}

const tokenFile = (mode: number, bearer = BEARER): Configure => async root => {
  const path = join(root, "listen-token");
  await writeFile(path, bearer + "\n", { mode });
  await chmod(path, mode);
  return { ANAMNESIS_LISTEN: `${HOST}:0`, ANAMNESIS_LISTEN_TOKEN_FILE: path };
};

/** Raw framed peer: decodes replies with the shared parser, waits on socket events only. */
function rawPeer(port: number) {
  const socket = connect({ host: HOST, port });
  const replies: unknown[] = [];
  const waiters: Array<{ resolve(value: unknown): void; reject(error: Error): void }> = [];
  const errors: Error[] = [];
  const frames = new Frames(body => {
    const value: unknown = JSON.parse(body.toString("utf8"));
    const waiter = waiters.shift();
    if (waiter) waiter.resolve(value); else replies.push(value);
    return true;
  });
  socket.on("data", (bytes: Buffer) => frames.push(bytes));
  socket.on("error", error => errors.push(error));
  socket.on("close", () => { for (const waiter of waiters.splice(0)) waiter.reject(new Error("connection closed before a reply")); });
  const closed = once(socket, "close", { signal: deadline() });
  return {
    socket, replies, errors, closed,
    next: () => new Promise<unknown>((resolve, reject) => {
      if (replies.length) return resolve(replies.shift());
      const signal = deadline(5000);
      signal.addEventListener("abort", () => reject(signal.reason), { once: true });
      waiters.push({ resolve, reject });
    }),
  };
}

test("TCP peer presenting the bearer completes hello and status while UDS is unchanged", async () => {
  await fixture(tokenFile(0o600), async daemon => {
    const ready = await daemon.listening;
    expect(ready.tcp?.host).toBe(HOST);
    const port = ready.tcp!.port;
    expect(port).toBeGreaterThan(0);
    const remote = await RpcClient.connect({ host: HOST, port, token: BEARER }, TOKEN);
    const local = await RpcClient.connect(join(daemon.root, "anamnesis.sock"), TOKEN);
    try {
      const status = await remote.request("status", {});
      expect(status.version).toBe(1);
      expect(status.storage).toBe("unavailable");
      expect(status.data_incarnation).toBe(ready.data_incarnation);
      expect((await local.request("status", {})).data_incarnation).toBe(status.data_incarnation);
      expect((await stat(join(daemon.root, "anamnesis.sock"))).mode & 0o777).toBe(0o600);
      const exited = once(daemon.child, "exit", { signal: deadline() });
      expect((await local.request("shutdown", {})).state).toBe("stopping");
      expect((await exited)[0]).toBe(0);
    } finally { await remote.close(); await local.close(); }
    expect(daemon.output()).not.toContain(BEARER);
  });
}, 60_000);

test("ops CLI in ANAMNESIS_RPC_TCP mode reaches a remote daemon and refuses a non-0600 token file", async () => {
  await fixture(tokenFile(0o600), async daemon => {
    const { host, port } = (await daemon.listening).tcp!;
    const clientRoot = await mkdtemp("/tmp/ana-tcp-client-"); // no socket or token here: only TCP can succeed
    try {
      const bearerFile = join(clientRoot, "bearer"), installationFile = join(clientRoot, "installation");
      await writeFile(bearerFile, BEARER + "\n", { mode: 0o600 }); await chmod(bearerFile, 0o600);
      await writeFile(installationFile, TOKEN + "\n", { mode: 0o600 }); await chmod(installationFile, 0o600);
      const env = { ...process.env, ANAMNESIS_RUNTIME_ROOT: clientRoot, ANAMNESIS_RPC_TCP: `${host}:${port}`, ANAMNESIS_RPC_TCP_TOKEN_FILE: bearerFile, ANAMNESIS_RUNTIME_TOKEN_FILE: installationFile };
      const ops = (extra: NodeJS.ProcessEnv = {}) => promisify(execFile)(process.execPath, ["app/anamnesis/ops.ts", "status"], { cwd: ROOT, env: { ...env, ...extra } }).then(r => ({ code: 0, ...r }), (error: { code?: number; stdout?: string; stderr?: string }) => ({ code: error.code ?? 1, stdout: error.stdout ?? "", stderr: error.stderr ?? "" }));

      const ok = await ops();
      expect(ok.code).toBe(0);
      const status = JSON.parse(ok.stdout.trim().split("\n").at(-1)!);
      expect(status.version).toBe(1);
      expect(status.storage).toBe("unavailable");

      await chmod(installationFile, 0o644);
      const refused = await ops();
      expect(refused.code).not.toBe(0);
      expect(refused.stderr).toContain("ANAMNESIS_RUNTIME_TOKEN_FILE");
      expect(refused.stdout + refused.stderr).not.toContain(TOKEN);
      expect(refused.stdout + refused.stderr).not.toContain(BEARER);

      const malformed = await ops({ ANAMNESIS_RPC_TCP: host });
      expect(malformed.code).not.toBe(0);
      expect(malformed.stderr).toContain("ANAMNESIS_RPC_TCP must be host:port");
    } finally { await rm(clientRoot, { recursive: true, force: true }); }
    expect(daemon.output()).not.toContain(BEARER);
  });
}, 60_000);

test("wrong or missing bearer is refused with unauthorized and the TCP connection is closed", async () => {
  await fixture(tokenFile(0o600), async daemon => {
    const port = (await daemon.listening).tcp!.port;
    const wrong = `not-the-bearer-${randomUUID()}`;
    await expect(RpcClient.connect({ host: HOST, port, token: wrong }, TOKEN)).rejects.toMatchObject({ code: "unauthorized" });

    // A wrong bearer pipelined with hello: one fault, no reply to hello, then close.
    const mismatched = rawPeer(port);
    mismatched.socket.write(Buffer.concat([frame({ auth: { bearer: wrong } }), frame({ jsonrpc: "2.0", id: 1, method: "hello", params: hello })]));
    const fault = await mismatched.next() as { id: unknown; error: { message: string; data: { code: string } } };
    expect(fault.id).toBeNull();
    expect(fault.error.data.code).toBe("unauthorized");
    expect(fault.error.message).not.toContain(wrong);
    expect(fault.error.message).not.toContain(BEARER);
    await mismatched.closed;
    expect(mismatched.replies).toEqual([]);

    // No auth frame at all: an RPC request is refused before decoding.
    const missing = rawPeer(port);
    missing.socket.write(frame({ jsonrpc: "2.0", id: 1, method: "status", params: {} }));
    expect((await missing.next() as { error: { data: { code: string } } }).error.data.code).toBe("unauthorized");
    await missing.closed;

    // The bearer requirement is TCP-only; the socket keeps its hello-only contract.
    const local = await RpcClient.connect(join(daemon.root, "anamnesis.sock"), TOKEN);
    try { expect((await local.request("status", {})).version).toBe(1); } finally { await local.close(); }
    expect(daemon.output()).not.toContain(BEARER);
    expect(daemon.output()).not.toContain(wrong);
  });
}, 60_000);

test("ANAMNESIS_LISTEN startup fails closed without a private token file and never echoes the token", async () => {
  const cases: Array<[string, Configure, string[]]> = [
    ["mode 0644", tokenFile(0o644), ["listen-token"]],
    ["absent file", async root => ({ ANAMNESIS_LISTEN: `${HOST}:0`, ANAMNESIS_LISTEN_TOKEN_FILE: join(root, "absent-token") }), []],
    ["unset variable", async () => ({ ANAMNESIS_LISTEN: `${HOST}:0` }), []],
  ];
  for (const [name, configure, expectedEntries] of cases) {
    await fixture(configure, async daemon => {
      const outcome = await Promise.race([
        daemon.exit.then(([code]) => ({ code })),
        daemon.listening.then(() => ({ code: "listening" as const })),
      ]);
      expect({ name, ...outcome }).toEqual({ name, code: 1 });
      expect(daemon.output()).toContain("ANAMNESIS_LISTEN_TOKEN_FILE");
      expect(daemon.output()).not.toContain(BEARER);
      // Configuration is refused before the runtime root is claimed.
      expect(await readdir(daemon.root)).toEqual(expectedEntries);
    });
  }
}, 60_000);

test("a TCP bind failure after the socket is up exits instead of serving UDS-only", async () => {
  const occupant = createServer();
  const occupied = once(occupant, "listening", { signal: deadline() });
  occupant.listen({ host: HOST, port: 0 });
  await occupied;
  const { port } = occupant.address() as AddressInfo;
  try {
    await fixture(async root => {
      const path = join(root, "listen-token");
      await writeFile(path, BEARER + "\n", { mode: 0o600 });
      await chmod(path, 0o600);
      return { ANAMNESIS_LISTEN: `${HOST}:${port}`, ANAMNESIS_LISTEN_TOKEN_FILE: path };
    }, async daemon => {
      const outcome = await Promise.race([
        daemon.exit.then(([code]) => ({ code })),
        daemon.listening.then(() => ({ code: "listening" as const })),
      ]);
      expect(outcome).toEqual({ code: 1 });
      expect(daemon.output()).toContain("EADDRINUSE");
      expect(daemon.output()).not.toContain(BEARER);
      await expect(stat(join(daemon.root, "owner"))).rejects.toMatchObject({ code: "ENOENT" }); // lease released
    });
  } finally { await new Promise<void>((resolve, reject) => occupant.close(error => error ? reject(error) : resolve())); }
}, 60_000);
