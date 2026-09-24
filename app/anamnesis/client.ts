import { connect, type Socket } from "node:net";
import { once } from "node:events";
import { RPC_LIMITS, RpcRequest, RpcResponse, type RpcMethod, type RpcRequestInput, type RpcSuccessResponse } from "../../packages/protocol/src/rpc.ts";
import { Frames } from "./wire.ts";
import { timingHash, type TimingSink } from "./timing.ts";

/** TCP endpoint of a daemon started with ANAMNESIS_LISTEN; token is that listener's bearer. */
export interface RpcTcpTarget { host: string; port: number; token: string; }
function frame(value: unknown): Buffer {
  const body = Buffer.from(JSON.stringify(value));
  const bytes = Buffer.allocUnsafe(4 + body.length);
  bytes.writeUInt32BE(body.length); body.copy(bytes, 4);
  return bytes;
}
type Params<M extends RpcMethod> = Extract<RpcRequestInput, { method: M }>["params"];
type Result<M extends RpcMethod> = Extract<RpcSuccessResponse, { method: M }>["result"];
interface Pending { resolve(value: unknown): void; reject(error: Error): void; timer: ReturnType<typeof setTimeout>; method: RpcMethod; started: number; }
export class RpcClient {
  private id = 0;
  private readonly pending = new Map<number, Pending>();
  private constructor(private readonly socket: Socket, private readonly timing?: TimingSink) {
    const frames = new Frames(body => {
      const response = RpcResponse.parse(JSON.parse(body.toString("utf8")));
      const pending = typeof response.id === "number" ? this.pending.get(response.id) : undefined;
      if (!pending) {
        // A refused TCP bearer precedes every request, so nothing was admitted.
        if (response.id === null && "error" in response && response.error.data.code === "unauthorized") {
          this.fail(Object.assign(new Error(response.error.message), { code: response.error.data.code, retryable: false }), true);
          socket.destroy(); return false;
        }
        throw new Error("response has no matching request");
      }
      this.timing?.({ layer: "client", event: "response", id: response.id!, method: pending.method, elapsedMs: performance.now() - pending.started });
      this.pending.delete(response.id as number);
      clearTimeout(pending.timer);
      if ("error" in response) pending.reject(Object.assign(new Error(response.error.message), { code: response.error.data.code, retryable: response.error.data.retryable }));
      else if (response.method !== pending.method) pending.reject(new Error("response method mismatch"));
      else pending.resolve(response.result);
      return true;
    });
    socket.on("data", (bytes: Buffer) => {
      this.timing?.({ layer: "client", event: "socket_data", bytes: bytes.length });
      try { frames.push(bytes); } catch (error) { this.fail(error instanceof Error ? error : new Error(String(error))); socket.destroy(); }
    });
    socket.on("error", error => this.fail(error));
    socket.on("close", () => this.fail(new Error("RPC connection closed")));
  }
  static async connect(target: string | RpcTcpTarget, token: string, mode: "auto" | "receipt" = "receipt", timing?: TimingSink): Promise<RpcClient> {
    const socket = typeof target === "string" ? connect(target) : connect({ host: target.host, port: target.port });
    const client = new RpcClient(socket, timing);
    try {
      await once(socket, "connect", { signal: AbortSignal.timeout(5000) });
      if (typeof target !== "string") { socket.setNoDelay(true); socket.write(frame({ auth: { bearer: target.token } })); }
      await client.request("hello", { token, client: "node-client", commit_mode: mode, version: 1 });
      return client;
    } catch (error) { socket.destroy(); throw error; }
  }
  private fail(error: Error, known = false): void {
    // A closed transport after an admitted request leaves its effect unknown;
    // callers must resolve it with ingest.status rather than retrying blindly.
    const unknown = known ? error : Object.assign(new Error("RPC connection closed; delivery outcome is unknown", { cause: error }), { code: "outcome_unknown", retryable: false });
    this.timing?.({ layer: "client", event: "transport_terminal", hash: timingHash(String(error)) });
    for (const [id, pending] of this.pending) {
      this.timing?.({ layer: "client", event: "failed", id, method: pending.method, elapsedMs: performance.now() - pending.started });
      clearTimeout(pending.timer); pending.reject(unknown);
    }
    this.pending.clear();
  }
  request<M extends RpcMethod>(method: M, params: Params<M>): Promise<Result<M>> {
    if (this.socket.destroyed) return Promise.reject(new Error("RPC connection is closed"));
    if (this.pending.size >= RPC_LIMITS.queued_requests_per_connection) return Promise.reject(new Error("client request limit reached"));
    const id = ++this.id;
    const bytes = frame(RpcRequest.parse({ jsonrpc: "2.0", id, method, params }));
    const started = performance.now();
    return new Promise<unknown>((resolve, reject) => {
      const timer = setTimeout(() => {
        // Lost replies have UNKNOWN outcome; do not synthesize success or retry.
        this.timing?.({ layer: "client", event: "deadline", id, method, elapsedMs: performance.now() - started, deadlineMs: 120_000 });
        this.fail(new Error("RPC deadline exceeded; delivery outcome is unknown"));
        this.socket.destroy();
      }, 120_000);
      this.pending.set(id, { method, resolve, reject, timer, started });
      this.timing?.({ layer: "client", event: "request", id, method, deadlineMs: 120_000, bytes: bytes.length });
      if (this.timing) this.socket.write(bytes, error => this.timing!({ layer: "client", event: error ? "write_failed" : "write_complete", id, method, elapsedMs: performance.now() - started }));
      else this.socket.write(bytes);
    }) as Promise<Result<M>>;
  }
  async close(): Promise<void> {
    if (this.socket.destroyed) return;
    const closed = once(this.socket, "close", { signal: AbortSignal.timeout(5000) });
    this.socket.end();
    await closed;
  }
}
