import { expect, jest, test } from "bun:test";
import { randomUUID } from "node:crypto";
import { once } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { RPC_LIMITS, RPC_METHODS, type RpcCapabilities } from "@anamnesis/protocol";
import { RpcClient } from "./client.ts";
import type { TimingSink } from "./timing.ts";
import { Frames, encode } from "./wire.ts";

type TimingEvent = Parameters<TimingSink>[0];
// A literal rather than runtime.ts's constant: importing runtime.ts would load the Neo4j-backed graph into a pure test.
const capabilities: RpcCapabilities = { methods: [...RPC_METHODS], recall: true, commit: false, policy: true, extraction: false, embeddings: false, writer_fence: "database" };
const remember = { episode: { schema: "anamnesis.original-message/1", content: "slow", time: { value: "2026-09-09T00:00:00Z", precision: "second" }, origin: { source: "test", session: "s", actor: "a", record: "r" }, mass: 1 }, source_revision: "v1", expected_previous_revision_key: null } as const;

test("a reply that never arrives rejects at the deadline as UNKNOWN and closes the transport", async () => {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-deadline-"));
  // The server answers hello and swallows everything after it.
  const server = createServer(socket => {
    const frames = new Frames(bytes => {
      const request = JSON.parse(bytes.toString("utf8"));
      if (request.method === "hello") socket.write(encode({ jsonrpc: "2.0", id: request.id, method: "hello", structure_revision: null, policy_revision: null, server_time: 1, result: { version: 1, principal: "installation", commit_mode: "receipt", data_incarnation: randomUUID(), fs_epoch: randomUUID(), capabilities, limits: { frame_bytes: RPC_LIMITS.frame_bytes, chunk_bytes: RPC_LIMITS.chunk_bytes, object_bytes: RPC_LIMITS.object_bytes, content_bytes: RPC_LIMITS.content_bytes } } }));
      return true;
    });
    socket.on("data", (bytes: Buffer) => frames.push(bytes));
  });
  const events: TimingEvent[] = [];
  let client: RpcClient | undefined;
  try {
    const listening = once(server, "listening", { signal: AbortSignal.timeout(3000) });
    server.listen(join(root, "socket")); await listening;
    client = await RpcClient.connect(join(root, "socket"), "token", "receipt", event => events.push(event));
    jest.useFakeTimers();
    const outcome = client.request("remember", remember).then(() => null, (error: Error & { code: string; retryable: boolean }) => error);
    jest.advanceTimersByTime(120_000);
    jest.useRealTimers();
    const error = await outcome;
    expect([error?.code, error?.retryable, error?.message]).toEqual(["outcome_unknown", false, "RPC connection closed; delivery outcome is unknown"]);
    expect(error?.cause).toEqual(new Error("RPC deadline exceeded; delivery outcome is unknown"));
    const deadline = events.filter(event => event.event === "deadline");
    expect(deadline).toHaveLength(1);
    expect(deadline[0]).toMatchObject({ layer: "client", event: "deadline", id: 2, method: "remember", deadlineMs: 120_000 });
    expect(events.map(event => event.event)).toContain("failed");
    await expect(client.request("remember", remember)).rejects.toThrow("RPC connection is closed");
  } finally {
    jest.useRealTimers();
    await client?.close();
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    await rm(root, { recursive: true, force: true });
  }
});
