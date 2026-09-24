import { describe, expect, test } from "bun:test";
import { z } from "zod";
import {
  RPC_VERSION,
  RPC_LIMITS,
  RPC_FUTURE_METHODS,
  RPC_METHODS,
  RpcMethod,
  RpcCapabilities,
  RpcRequest,
  RpcResponse,
  RpcRememberParams,
  RpcOutputBudget,
} from "./index.ts";
import { protocolJsonSchemas } from "../scripts/export-schemas.ts";
import { readFile } from "node:fs/promises";

const hash = "a".repeat(64);
const uuid = "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e0f";
const episode = {
  schema: "anamnesis.original-message/1",
  content: "A valid Episode 🙂",
  time: { value: "2026-09-09T12:00:00Z", precision: "second" },
  origin: { source: "test", session: "session", actor: "user", record: "record" },
};
const remember = {
  episode,
  source_revision: "revision-1",
  expected_previous_revision_key: null,
};
function request(method: string, params: unknown) {
  return { jsonrpc: "2.0", id: 1, method, params };
}
const meta = { structure_revision: null, policy_revision: null, server_time: 1 };
function response(method: string, result: unknown) {
  return { jsonrpc: "2.0", id: 1, method, result, ...meta };
}

describe("strict versioned RPC requests", () => {
  test("admits all ingest-phase methods and normalizes Episode defaults", () => {
    const inputs = [
      request("hello", { token: "installation-token", client: "harness", commit_mode: "receipt", version: RPC_VERSION }),
      request("status", {}),
      request("shutdown", {}),
      request("object.begin", { sha256: hash, size: 0, media_type: "application/octet-stream" }),
      request("object.chunk", { upload_id: uuid, seq: 0, bytes_b64: "YQ==" }),
      request("object.commit", { upload_id: uuid }),
      request("remember", remember),
      request("ingest.status", { revision_key: hash, body_digest: hash, data_incarnation: uuid }),
    ];
    for (const input of inputs) expect(RpcRequest.safeParse(input).success).toBe(true);
    expect(RpcRememberParams.parse(remember).episode.properties).toEqual({});
    expect(RpcRememberParams.parse(remember).episode.mass).toBe(0.5);
  });

  test("requires hello version and installation token, not a client principal", () => {
    const hello = { token: "installation-token", client: "harness", commit_mode: "auto", version: RPC_VERSION };
    for (const invalid of [
      { ...hello, token: "" },
      { ...hello, version: RPC_VERSION + 1 },
      { ...hello, version: undefined },
      { ...hello, principal: "trusted-client" },
      { ...hello, commit_mode: "implicit" },
    ]) expect(RpcRequest.safeParse(request("hello", invalid)).success).toBe(false);
  });

  test("rejects notifications, batches, unknown fields at every envelope level", () => {
    const status = request("status", {});
    for (const invalid of [
      [status], { ...status, id: undefined }, { ...status, id: null },
      { ...status, id: 1.5 }, { ...status, id: Number.MAX_SAFE_INTEGER + 1 },
      { ...status, jsonrpc: "1.0" }, { ...status, extra: true },
      request("status", { extra: true }), request("shutdown", { force: true }),
      request("remember", { ...remember, extra: true }),
      request("remember", { ...remember, episode: { ...episode, id: uuid } }),
      request("remember", { ...remember, episode: { ...episode, origin: { ...episode.origin, extra: true } } }),
    ]) expect(RpcRequest.safeParse(invalid).success).toBe(false);
  });

  test("keeps the v1 method registry aligned with request and capability surfaces", () => {
    for (const method of ["commit", "hit-cache.verify", "hit-cache.rebuild"])
      expect(RpcMethod.safeParse(method).success).toBe(true);
    expect(RPC_METHODS).toContain("commit");
    expect(RPC_METHODS).toContain("hit-cache.verify");
    expect(RPC_METHODS).toContain("hit-cache.rebuild");
    expect(RPC_FUTURE_METHODS).not.toContain("commit");
    expect(RpcCapabilities.parse({
      methods: [...RPC_METHODS], recall: false, commit: true, policy: false,
      extraction: false, embeddings: false, writer_fence: "database",
    }).methods).toEqual([...RPC_METHODS]);
  });

  test("reserves future methods without admitting fake implementations", () => {
    for (const method of [...RPC_FUTURE_METHODS, "invented.method"])
      expect(RpcRequest.safeParse(request(method, {})).success).toBe(false);
  });

  test("requires explicit revision CAS and original Episode semantics", () => {
    for (const invalid of [
      { ...remember, source_revision: undefined },
      { ...remember, source_revision: "" },
      { ...remember, expected_previous_revision_key: undefined },
      { ...remember, expected_previous_revision_key: "../bad" },
      { ...remember, episode: { ...episode, time: undefined } },
      { ...remember, episode: { ...episode, schema: "anamnesis.claim/1" } },
      { ...remember, episode: { ...episode, schema: "anamnesis.policy/1" } },
      { ...remember, episode: { ...episode, schema: "anamnesis.future/1" } },
      { ...remember, payload_hash: hash.toUpperCase() },
    ]) expect(RpcRememberParams.safeParse(invalid).success).toBe(false);
    expect(RpcRememberParams.safeParse({ ...remember, expected_previous_revision_key: hash, payload_hash: hash }).success).toBe(true);
    expect(RpcRememberParams.safeParse({ ...remember, episode: { ...episode, schema: "anamnesis.original-document/1" } }).success).toBe(true);
  });

  test("bounds content by UTF-8 bytes and rejects malformed Unicode recursively", () => {
    const atLimit = { ...remember, episode: { ...episode, content: "é".repeat(RPC_LIMITS.content_bytes / 2) } };
    expect(RpcRememberParams.safeParse(atLimit).success).toBe(true);
    expect(RpcRememberParams.safeParse({ ...atLimit, episode: { ...atLimit.episode, content: atLimit.episode.content + "a" } }).success).toBe(false);
    for (const bad of ["\ud800", "\udc00", "a\ud800z"]) {
      for (const params of [
        { ...remember, source_revision: bad },
        { ...remember, episode: { ...episode, content: bad } },
        { ...remember, episode: { ...episode, properties: { nested: [bad] } } },
        { ...remember, episode: { ...episode, properties: { [bad]: "value" } } },
        { ...remember, episode: { ...episode, origin: { ...episode.origin, actor: bad } } },
      ]) expect(RpcRememberParams.safeParse(params).success).toBe(false);
    }
    for (const value of [Infinity, -Infinity, NaN])
      expect(RpcRememberParams.safeParse({ ...remember, episode: { ...episode, properties: { value } } }).success).toBe(false);
  });

  test("bounds total encoded frame and properties", () => {
    expect(RpcRequest.safeParse(request("remember", {
      ...remember, episode: { ...episode, properties: { huge: "x".repeat(RPC_LIMITS.frame_bytes) } },
    })).success).toBe(false);
  });

  test("bounds object sizes and canonical nonempty base64 chunks", () => {
    for (const size of [-1, 0.1, Infinity, NaN, Number.MAX_SAFE_INTEGER + 1, RPC_LIMITS.object_bytes + 1])
      expect(RpcRequest.safeParse(request("object.begin", { sha256: hash, size, media_type: "text/plain" })).success).toBe(false);
    expect(RpcRequest.safeParse(request("object.begin", { sha256: hash, size: RPC_LIMITS.object_bytes, media_type: "text/plain" })).success).toBe(true);
    const chunk = { upload_id: uuid, seq: 0, bytes_b64: Buffer.alloc(RPC_LIMITS.chunk_bytes).toString("base64") };
    expect(RpcRequest.safeParse(request("object.chunk", chunk)).success).toBe(true);
    for (const bytes_b64 of ["", "a", "YQ", "YQ===", "YR==", "YWJ=", "YQ==\n", "____", Buffer.alloc(RPC_LIMITS.chunk_bytes + 1).toString("base64")])
      expect(RpcRequest.safeParse(request("object.chunk", { ...chunk, bytes_b64 })).success).toBe(false);
    for (const seq of [-1, 0.1, Infinity, Number.MAX_SAFE_INTEGER + 1])
      expect(RpcRequest.safeParse(request("object.chunk", { ...chunk, seq })).success).toBe(false);
  });

  test("future output budgets reject unsafe limits and invalid tokenizer combinations", () => {
    for (const unit of ["utf8_bytes", "unicode_scalars"])
      expect(RpcOutputBudget.safeParse({ unit, limit: 0 }).success).toBe(true);
    expect(RpcOutputBudget.safeParse({ unit: "tokens", limit: 10, tokenizer_id: "fixture-v1@sha256:" + hash }).success).toBe(true);
    for (const invalid of [
      { unit: "tokens", limit: 10 }, { unit: "bytes", limit: 1 },
      { unit: "utf8_bytes", limit: 1, tokenizer_id: hash },
      { unit: "utf8_bytes", limit: 1, extra: true },
      ...[-1, 0.1, Infinity, NaN, Number.MAX_SAFE_INTEGER + 1].map((limit) => ({ unit: "unicode_scalars", limit })),
    ]) expect(RpcOutputBudget.safeParse(invalid).success).toBe(false);
  });
});

describe("strict RPC responses", () => {
  test("validates handshake, lifecycle and object results without fabricated capabilities", () => {
    const capabilities = {
      methods: ["hello", "status", "shutdown", "object.begin", "object.chunk", "object.commit", "remember", "ingest.status"],
      recall: false,
      commit: true,
      policy: false,
      extraction: false,
      embeddings: false,
      writer_fence: "local_only",
    };
    const hello = {
      version: RPC_VERSION,
      principal: "installation",
      commit_mode: "receipt",
      data_incarnation: uuid,
      fs_epoch: uuid,
      capabilities,
      limits: {
        frame_bytes: RPC_LIMITS.frame_bytes,
        chunk_bytes: RPC_LIMITS.chunk_bytes,
        object_bytes: RPC_LIMITS.object_bytes,
        content_bytes: RPC_LIMITS.content_bytes,
      },
    };
    const status = {
      version: RPC_VERSION,
      state: "degraded",
      storage: "unavailable",
      data_incarnation: uuid,
      fs_epoch: uuid,
      queue: { pending: 0, capacity: RPC_LIMITS.queued_requests },
      spool: { pending: 1, blocked: 0, quarantined: 0, bytes: 128 },
      outbox_pending: null,
      capabilities,
      workers: { embedding: { pending: null, drained_total: 0, quarantined_total: 0, last_error: null }, extraction: { state: "unconfigured" } },
    };
    const object = { hash, size: RPC_LIMITS.object_bytes, media_type: "text/plain" };
    const valid = [
      response("hello", hello),
      response("hello", { ...hello, capabilities: { ...capabilities, extraction: true } }),
      response("status", { ...status, capabilities: { ...capabilities, extraction: true } }),
      response("status", status),
      response("shutdown", { state: "stopping" }),
      response("object.begin", { state: "uploading", upload_id: uuid, next_seq: 0, chunk_bytes_max: RPC_LIMITS.chunk_bytes }),
      response("object.begin", { state: "committed", object }),
      response("object.chunk", { upload_id: uuid, next_seq: 1 }),
      response("object.commit", object),
    ];
    for (const value of valid) {
      expect(RpcResponse.safeParse(value).success).toBe(true);
    }
    const invalid = [
      response("hello", { ...hello, principal: "trusted-client" }),
      response("hello", { ...hello, capabilities: { ...capabilities, extraction: "true" } }),
      response("hello", { ...hello, capabilities: { ...capabilities, methods: ["hello", "hello"] } }),
      response("status", { ...status, queue: { pending: RPC_LIMITS.queued_requests + 1, capacity: RPC_LIMITS.queued_requests } }),
      response("shutdown", { state: "stopped" }),
      response("object.commit", { ...object, size: RPC_LIMITS.object_bytes + 1 }),
      response("object.chunk", { upload_id: uuid, next_seq: 0 }),
      response("recall", {}),
    ];
    for (const value of invalid) {
      expect(RpcResponse.safeParse(value).success).toBe(false);
    }
  });

  test("separates durable acceptance from authoritative commit and UNKNOWN", () => {
    const identity = { revision_key: hash, body_digest: hash, data_incarnation: uuid };
    const committed = { state: "committed", ...identity, id: uuid, created: true, ingest_seq: 1 };
    const spooled = { state: "spooled", ...identity, fs_epoch: uuid, spool_seq: 1 };
    const unknown = { state: "unknown", ...identity };
    expect(RpcResponse.safeParse({ ...response("remember", committed), structure_revision: 1 }).success).toBe(true);
    expect(RpcResponse.safeParse(response("remember", spooled)).success).toBe(true);
    expect(RpcResponse.safeParse(response("ingest.status", unknown)).success).toBe(true);
    expect(RpcResponse.safeParse(response("remember", unknown)).success).toBe(false);
    expect(RpcResponse.safeParse(response("remember", { ...spooled, created: true })).success).toBe(false);
    expect(RpcResponse.safeParse(response("remember", { ...spooled, id: uuid })).success).toBe(false);
    expect(RpcResponse.safeParse(response("ingest.status", { ...unknown, id: uuid })).success).toBe(false);
  });

  test("closed machine errors cannot masquerade as success", () => {
    const error = { jsonrpc: "2.0", id: null, error: { code: -32601, message: "Not implemented", data: { code: "unsupported_method", retryable: false } }, ...meta };
    expect(RpcResponse.safeParse(error).success).toBe(true);
    expect(RpcResponse.safeParse({ ...error, result: {} }).success).toBe(false);
    expect(RpcResponse.safeParse({ ...error, error: { ...error.error, data: { code: "invented", retryable: false } } }).success).toBe(false);
    expect(RpcResponse.safeParse({ ...error, server_time: Infinity }).success).toBe(false);
    expect(RpcResponse.safeParse({ ...error, structure_revision: Number.MAX_SAFE_INTEGER + 1 }).success).toBe(false);
  });

  test("exports machine-readable JSON schemas without erasing strictness", () => {
    const schema = z.toJSONSchema(RpcRequest, { target: "draft-2020-12", io: "input" });
    const validate = z.fromJSONSchema(schema);
    expect(validate.safeParse(request("status", {})).success).toBe(true);
    expect(validate.safeParse(request("status", { extra: true })).success).toBe(false);
    expect(validate.safeParse(request("remember", remember)).success).toBe(true);
  });
});

test("shipped JSON schemas match the schema exporter", async () => {
  for (const [name, schema] of Object.entries(protocolJsonSchemas())) {
    const shipped = JSON.parse(await readFile(
      new URL(`../schemas/${name}.schema.json`, import.meta.url),
      "utf8",
    ));
    expect(shipped).toEqual(schema);
  }
});
