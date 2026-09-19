import { TextDecoder } from "node:util";
import { RPC_LIMITS, RPC_METHODS, RpcErrorCode, RpcErrorResponse, RpcRequest, RpcResponse, type RpcErrorCode as ErrorCode } from "../../packages/protocol/src/rpc.ts";

export class RpcFault extends Error {
  constructor(readonly code: ErrorCode, message: string, readonly retryable = false) { super(message); }
}
export function storageUnavailable(error: unknown): boolean {
  if (!(error instanceof Error) || !("code" in error)) return false;
  return ["ServiceUnavailable", "SessionExpired", "Neo.TransientError.General.DatabaseUnavailable"].includes(String(error.code));
}
export function fault(error: unknown): RpcFault {
  if (error instanceof RpcFault) return error;
  if (error instanceof Error && ["stale_writer_epoch", "ownership_lost"].includes(error.message)) return new RpcFault("ownership_lost", "writer ownership was lost");
  if (storageUnavailable(error)) return new RpcFault("storage_unavailable", "database unavailable", true);
  if (error instanceof Error && /^dream_[a-z_]+$/.test(error.message) && RpcErrorCode.safeParse(error.message).success) return new RpcFault(error.message as ErrorCode, error.message);
  if (error instanceof Error && "code" in error) {
    const parsed = RpcErrorCode.safeParse(error.code);
    if (parsed.success) return new RpcFault(parsed.data, error.message.slice(0, 512));
    if (["ENOSPC", "EDQUOT"].includes(String(error.code))) return new RpcFault("resource_exhausted", "filesystem quota exhausted");
  }
  if (error instanceof Error && error.message.includes("spool quota")) return new RpcFault("resource_exhausted", "spool quota exhausted");
  return new RpcFault("internal_error", "internal runtime error");
}
export function envelope() {
  return { jsonrpc: "2.0" as const, structure_revision: null, policy_revision: null, server_time: Date.now() };
}
export function errorResponse(id: unknown, failure: RpcFault) {
  const parsedId = RpcErrorResponse.shape.id.safeParse(id);
  const codes = { parse_error: -32700, invalid_request: -32600, invalid_params: -32602, unsupported_method: -32601, internal_error: -32603 } as const;
  const code = failure.code in codes ? codes[failure.code as keyof typeof codes] : -32000;
  return RpcErrorResponse.parse({ ...envelope(), id: parsedId.success ? parsedId.data : null,
    error: { code, message: failure.message, data: { code: failure.code, retryable: failure.retryable } } });
}
export function encode(value: unknown): Buffer {
  const body = Buffer.from(JSON.stringify(RpcResponse.parse(value)));
  if (body.length > RPC_LIMITS.frame_bytes) throw new RpcFault("resource_exhausted", "response exceeds frame limit");
  const frame = Buffer.allocUnsafe(body.length + 4);
  frame.writeUInt32BE(body.length);
  body.copy(frame, 4);
  return frame;
}
export function decodeRequest(bytes: Buffer): RpcRequest {
  let value: unknown;
  try { value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)); }
  catch { throw new RpcFault("parse_error", "frame is not valid UTF-8 JSON"); }
  const result = RpcRequest.safeParse(value);
  if (result.success) return result.data;
  if (value && typeof value === "object" && "method" in value) {
    if (typeof value.method === "string" && !RPC_METHODS.some(method => method === value.method)) throw new RpcFault("unsupported_method", "method is not implemented by this ingest-only runtime");
    if (value.method === "hello" && "params" in value && value.params && typeof value.params === "object" && "version" in value.params && value.params.version !== 1) throw new RpcFault("unsupported_version", "unsupported RPC version");
    throw new RpcFault("invalid_params", "request parameters do not match the RPC contract");
  }
  throw new RpcFault("invalid_request", "invalid RPC request envelope");
}

// Implementation defaults, not wire-contract constants or a latency SLA.
const FRAME = RPC_LIMITS.frame_bytes + 4;
const SMALL = 4096 + 4;
export const RPC_BYTE_LIMITS = {
  frame: FRAME, small: SMALL, error: 4096,
  connection: { general: 16 * (FRAME + SMALL), control: 2 * (FRAME + SMALL), ingress: SMALL + 4096 },
  global: { general: 256 * (FRAME + SMALL), control: 8 * (FRAME + SMALL), ingress: RPC_LIMITS.connections * (SMALL + 4096) },
} as const;
type Pool = "general" | "control" | "ingress";
export interface ByteAccount { used: Record<Pool, number>; }
export interface ByteReservation {
  account: ByteAccount; pool: Pool; input: number; allowance: number; actual: number; live: boolean;
}
/** Encoded bytes, not JS heap size. Output allowance is replaced by actual
 * output, never added to it. Input remains charged through write completion. */
export class RpcByteBudget {
  readonly used = { general: 0, control: 0, ingress: 0 };
  readonly accounts = new Set<ByteAccount>();
  private fits(account: ByteAccount, pool: Pool, bytes: number): boolean {
    return account.used[pool] + bytes <= RPC_BYTE_LIMITS.connection[pool] && this.used[pool] + bytes <= RPC_BYTE_LIMITS.global[pool];
  }
  private charge(account: ByteAccount, pool: Pool, bytes: number): void {
    account.used[pool] += bytes; this.used[pool] += bytes;
    if (Object.values(account.used).some(value => value !== 0)) this.accounts.add(account);
    else this.accounts.delete(account);
  }
  reserve(account: ByteAccount, input: number, authenticated: boolean): ByteReservation | undefined {
    let pool: Pool = "general", allowance = FRAME;
    if (!this.fits(account, pool, input + allowance)) {
      // Method is still unknown: one small ingress per authenticated peer,
      // NOT a control reservation. Unauthenticated traffic never uses it.
      pool = "ingress"; allowance = RPC_BYTE_LIMITS.error;
      if (!authenticated || input > SMALL || account.used.ingress || !this.fits(account, pool, input + allowance)) return;
    }
    this.charge(account, pool, input + allowance);
    return { account, pool, input, allowance, actual: 0, live: true };
  }
  control(reservation: ByteReservation): boolean {
    if (reservation.pool !== "ingress") return true;
    const bytes = reservation.input + FRAME;
    if (!this.fits(reservation.account, "control", bytes)) return false;
    this.charge(reservation.account, reservation.pool, -(reservation.input + reservation.allowance));
    reservation.pool = "control"; reservation.allowance = FRAME;
    this.charge(reservation.account, reservation.pool, bytes);
    return true;
  }
  output(reservation: ByteReservation, bytes: number): void {
    if (!reservation.live || reservation.actual || bytes > reservation.allowance) throw new RpcFault("resource_exhausted", "response exceeds reserved output allowance");
    this.charge(reservation.account, reservation.pool, bytes - reservation.allowance);
    reservation.allowance = 0; reservation.actual = bytes;
  }
  release(reservation: ByteReservation): void {
    if (!reservation.live) return;
    reservation.live = false;
    this.charge(reservation.account, reservation.pool, -(reservation.input + reservation.allowance + reservation.actual));
  }
}

/** Incremental u32-be parser. Admission precedes body allocation. accept takes
 * ownership of the optional reservation on normal return, including false. */
export class Frames {
  private readonly header = Buffer.alloc(4);
  private headerOffset = 0;
  private body: Buffer | undefined;
  private offset = 0;
  private release: (() => void) | undefined;
  constructor(private readonly accept: (body: Buffer) => boolean, private readonly reserve?: (encodedBytes: number) => () => void) {}
  dispose(): void {
    this.body = undefined; this.headerOffset = 0; this.offset = 0;
    this.release?.(); this.release = undefined;
  }
  push(chunk: Buffer): void {
    let cursor = 0;
    while (cursor < chunk.length) {
      if (!this.body) {
        const size = Math.min(4 - this.headerOffset, chunk.length - cursor);
        chunk.copy(this.header, this.headerOffset, cursor, cursor + size);
        this.headerOffset += size;
        cursor += size;
        if (this.headerOffset < 4) continue;
        const length = this.header.readUInt32BE();
        if (!length || length > RPC_LIMITS.frame_bytes) throw new RpcFault("invalid_request", "invalid frame length");
        this.release = this.reserve?.(length + 4);
        try { this.body = Buffer.allocUnsafe(length); }
        catch (error) { this.dispose(); throw error; }
        this.offset = 0;
      }
      const size = Math.min(this.body.length - this.offset, chunk.length - cursor);
      chunk.copy(this.body, this.offset, cursor, cursor + size);
      this.offset += size;
      cursor += size;
      if (this.offset === this.body.length) {
        const body = this.body;
        this.body = undefined;
        this.headerOffset = 0;
        const release = this.release; this.release = undefined;
        try { if (!this.accept(body)) return; }
        catch (error) { release?.(); throw error; }
      }
    }
  }
}
