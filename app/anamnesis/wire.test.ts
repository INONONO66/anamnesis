import { describe, expect, test, spyOn } from "bun:test";
import { FrameFault, Frames, RpcByteBudget, RpcFault, RPC_BYTE_LIMITS, decodeRequest, encode, errorResponse, fault, storageUnavailable, type ByteAccount } from "./wire.ts";
import { RPC_LIMITS } from "../../packages/protocol/src/rpc.ts";

const account = (): ByteAccount => ({ used: { general: 0, control: 0, ingress: 0 } });
const header = (size: number) => { const bytes = Buffer.alloc(4); bytes.writeUInt32BE(size); return bytes; };

describe("Frames optional reservation ownership", () => {
  test("standalone fragmented maximum frame and coalesced next frame are preserved", () => {
    const accepted: number[] = [];
    const frames = new Frames(body => { accepted.push(body.length); return true; });
    const h = header(RPC_LIMITS.frame_bytes);
    frames.push(h.subarray(0, 1)); frames.push(h.subarray(1));
    frames.push(Buffer.alloc(RPC_LIMITS.frame_bytes - 1));
    frames.push(Buffer.concat([Buffer.alloc(1), header(1), Buffer.alloc(1)]));
    expect(accepted).toEqual([RPC_LIMITS.frame_bytes, 1]);
  });
  test("maximum +1 and zero lengths reject before admission or allocation", () => {
    for (const size of [0, RPC_LIMITS.frame_bytes + 1]) {
      let admissions = 0;
      const frames = new Frames(() => true, () => { admissions++; return () => {}; });
      const h = header(size), allocate = spyOn(Buffer, "allocUnsafe");
      try { expect(() => frames.push(h)).toThrow("invalid frame length"); expect(allocate).not.toHaveBeenCalled(); }
      finally { allocate.mockRestore(); }
      expect(admissions).toBe(0);
    }
  });
  test("admission denial allocates no body; partial dispose releases once", () => {
    const denied = new Frames(() => true, () => { throw Error("denied"); });
    const h = header(10), allocate = spyOn(Buffer, "allocUnsafe");
    try { expect(() => denied.push(h)).toThrow("denied"); expect(allocate).not.toHaveBeenCalled(); }
    finally { allocate.mockRestore(); }
    let released = 0;
    const partial = new Frames(() => true, bytes => { expect(bytes).toBe(14); return () => { released++; }; });
    partial.push(h); partial.push(Buffer.alloc(3)); partial.dispose(); partial.dispose();
    expect(released).toBe(1);
  });
  test("allocation failure releases the admitted reservation", () => {
    let released = 0;
    const frames = new Frames(() => true, () => () => { released++; });
    const h = header(10), allocate = spyOn(Buffer, "allocUnsafe").mockImplementation(() => { throw Error("allocation failure"); });
    try { expect(() => frames.push(h)).toThrow("allocation failure"); }
    finally { allocate.mockRestore(); }
    frames.dispose(); expect(released).toBe(1);
  });
  test("accept ownership transfers on normal return; throwing accept releases once", () => {
    for (const result of [true, false]) {
      let released = 0;
      const frames = new Frames(() => result, () => () => { released++; });
      frames.push(Buffer.concat([header(1), Buffer.alloc(1)])); frames.dispose();
      expect(released).toBe(0);
    }
    let released = 0;
    const frames = new Frames(() => { throw Error("decode"); }, () => () => { released++; });
    expect(() => frames.push(Buffer.concat([header(1), Buffer.alloc(1)]))).toThrow("decode"); frames.dispose();
    expect(released).toBe(1);
  });
});

describe("encoded byte reservations", () => {
  test("exact connection/global boundaries and one byte excess", () => {
    const budget = new RpcByteBudget(), peers = Array.from({ length: 16 }, account), held = [];
    for (const peer of peers) {
      for (let i = 0; i < 8; i++) held.push(budget.reserve(peer, RPC_BYTE_LIMITS.frame, false)!);
      expect(budget.reserve(peer, RPC_BYTE_LIMITS.frame, false)).toBeUndefined();
    }
    const peer = account();
    const remaining = RPC_BYTE_LIMITS.global.general - budget.used.general - RPC_BYTE_LIMITS.frame;
    expect(budget.reserve(peer, remaining + 1, false)).toBeUndefined();
    held.push(budget.reserve(peer, remaining, false)!);
    expect(budget.used.general).toBe(RPC_BYTE_LIMITS.global.general);
    expect(budget.reserve(account(), 1, false)).toBeUndefined();
    for (const reservation of held) { budget.release(reservation); budget.release(reservation); }
    expect(budget.used).toEqual({ general: 0, control: 0, ingress: 0 }); expect(budget.accounts.size).toBe(0);
  });
  test("small ingress boundary requires authentication and permits only one pending ingress per peer", () => {
    const budget = new RpcByteBudget(), peer = account();
    for (let i = 0; i < 16; i++) expect(budget.reserve(peer, RPC_BYTE_LIMITS.small, false)).toBeDefined();
    expect(peer.used.general).toBe(RPC_BYTE_LIMITS.connection.general);
    expect(budget.reserve(peer, 1, false)).toBeUndefined();
    expect(budget.reserve(peer, RPC_BYTE_LIMITS.small + 1, true)).toBeUndefined();
    const ingress = budget.reserve(peer, RPC_BYTE_LIMITS.small, true)!;
    expect(ingress.pool).toBe("ingress"); expect(peer.used.ingress).toBe(RPC_BYTE_LIMITS.connection.ingress);
    expect(budget.reserve(peer, 1, true)).toBeUndefined();
    expect(budget.control(ingress)).toBe(true);
    expect(peer.used.ingress).toBe(0); expect(peer.used.control).toBe(RPC_BYTE_LIMITS.frame + RPC_BYTE_LIMITS.small);
    const next = budget.reserve(peer, RPC_BYTE_LIMITS.small, true)!;
    expect(budget.control(next)).toBe(true);
    const excess = budget.reserve(peer, 1, true)!;
    expect(budget.control(excess)).toBe(false);
    budget.release(excess); expect(peer.used.ingress).toBe(0);
  });
  test("output actual replaces maximum, cannot exceed it, and callback release is idempotent", () => {
    const budget = new RpcByteBudget(), peer = account();
    const reservation = budget.reserve(peer, 100, false)!;
    expect(budget.used.general).toBe(100 + RPC_BYTE_LIMITS.frame);
    expect(() => budget.output(reservation, RPC_BYTE_LIMITS.frame + 1)).toThrow("reserved output");
    expect(budget.used.general).toBe(100 + RPC_BYTE_LIMITS.frame);
    budget.output(reservation, 200);
    expect(reservation.allowance).toBe(0); expect(reservation.actual).toBe(200);
    expect(budget.used.general).toBe(300);
    expect(() => budget.output(reservation, 100)).toThrow("reserved output");
    budget.release(reservation); budget.release(reservation); expect(budget.used.general).toBe(0);
  });
});

describe("decodeRequest", () => {
  const frame = (value: unknown) => Buffer.from(JSON.stringify(value));
  test("frame-level refusals echo the request id so a real client can match the reply", () => {
    const refusals: [unknown, FrameFault["code"], string | number | null][] = [
      [{ jsonrpc: "2.0", id: 7, method: "commit", params: {} }, "unsupported_method", 7],
      [{ jsonrpc: "2.0", id: "req-1", method: "hello", params: { version: 2 } }, "unsupported_version", "req-1"],
      [{ jsonrpc: "2.0", id: 9, method: "status", params: { extra: true } }, "invalid_params", 9],
      [{ jsonrpc: "2.0", method: "commit", params: {} }, "unsupported_method", null],
      [{ jsonrpc: "2.0", id: "x".repeat(128), method: "commit", params: {} }, "unsupported_method", "x".repeat(128)],
      [{ jsonrpc: "2.0", id: "x".repeat(129), method: "commit", params: {} }, "unsupported_method", null],
      [{ jsonrpc: "2.0", id: { nested: true }, method: "commit", params: {} }, "unsupported_method", null],
    ];
    for (const [request, code, id] of refusals) {
      let caught: unknown;
      try { decodeRequest(frame(request)); } catch (error) { caught = error; }
      expect(caught).toBeInstanceOf(FrameFault);
      const failure = caught as FrameFault;
      expect(failure.code).toBe(code);
      const response = errorResponse(failure.id, failure);
      expect(response.id).toBe(id);
      expect(encode(response).length).toBeLessThanOrEqual(RPC_BYTE_LIMITS.error); // fits the error-output allowance even at the longest id
    }
  });
  test("envelope and encoding faults carry no id", () => {
    for (const bytes of [Buffer.from([0xff, 0xfe]), frame({ jsonrpc: "2.0", id: 1 }), frame("text")]) {
      let caught: unknown;
      try { decodeRequest(bytes); } catch (error) { caught = error; }
      expect(caught).not.toBeInstanceOf(FrameFault);
      expect(["parse_error", "invalid_request"]).toContain((caught as { code: string }).code);
    }
  });
});

describe("fault", () => {
  const coded = (code: string, message = "driver said no") => Object.assign(new Error(message), { code });
  test("storageUnavailable recognises only Error instances carrying a transient driver code", () => {
    expect(storageUnavailable("ServiceUnavailable")).toBe(false);
    expect(storageUnavailable(new Error("ServiceUnavailable"))).toBe(false);
    expect(storageUnavailable(coded("Neo.ClientError.Statement.SyntaxError"))).toBe(false);
    for (const code of ["ServiceUnavailable", "SessionExpired", "Neo.TransientError.General.DatabaseUnavailable"]) expect(storageUnavailable(coded(code))).toBe(true);
  });
  test("an RpcFault passes through untouched", () => {
    const own = new RpcFault("policy_denied", "no", true);
    expect(fault(own)).toBe(own);
  });
  test("runtime errors map to a protocol code, retryable only for a transient store", () => {
    const cases: [unknown, RpcFault["code"], boolean][] = [
      [new Error("stale_writer_epoch"), "ownership_lost", false],
      [new Error("ownership_lost"), "ownership_lost", false],
      [coded("SessionExpired"), "storage_unavailable", true],
      [coded("invalid_selection"), "invalid_selection", false],
      [coded("ENOSPC"), "resource_exhausted", false],
      [coded("EDQUOT"), "resource_exhausted", false],
      [coded("ENOENT"), "internal_error", false],
      [new Error("spool quota exceeded"), "resource_exhausted", false],
      [new Error("anything else"), "internal_error", false],
      ["not an error", "internal_error", false],
    ];
    for (const [error, code, retryable] of cases) {
      const mapped = fault(error);
      expect([mapped.code, mapped.retryable]).toEqual([code, retryable]);
    }
  });
  test("a message that merely spells a protocol code is not mapped; only the ownership and spool message allowlists are", () => {
    expect([fault(new Error("policy_denied")).code, fault(new Error("storage_unavailable")).code]).toEqual(["internal_error", "internal_error"]);
  });
  test("a protocol-coded error keeps at most 512 characters of its own message; other mappings use fixed text", () => {
    expect(fault(coded("invalid_selection", "x".repeat(600))).message).toBe("x".repeat(512));
    expect(fault(coded("ENOENT", "secret path")).message).not.toContain("secret path");
  });
});
