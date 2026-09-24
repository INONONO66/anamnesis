import { createHash, timingSafeEqual, randomUUID } from "node:crypto";
import { chmod, rm } from "node:fs/promises";
import { createServer, type AddressInfo, type Socket } from "node:net";
import { once } from "node:events";
import { RPC_LIMITS, RpcTcpAuth, type RpcRequest } from "../../packages/protocol/src/rpc.ts";
import { acquireInstallation, loadListenConfig, runtimeRoot, socketPath } from "./config.ts";
import { daemonTiming, timingContext, timingHash } from "./timing.ts";
import { Runtime } from "./runtime.ts";
import type { InstallationContext, RecallTransportInput } from "../../packages/core/src/store.ts";
import { Frames, RpcByteBudget, RpcFault, decodeRequest, encode, envelope, errorResponse, fault, type ByteAccount, type ByteReservation } from "./wire.ts";

/** authorized: transport admission (always for the socket, bearer frame for TCP); authenticated: hello. */
interface Connection { socket: Socket; authorized: boolean; authenticated: boolean; context?: InstallationContext; pending: number; closed: boolean; bytes: ByteAccount; cancel: Set<() => void>; writes: Set<() => void>; }
export async function foreground(): Promise<void> {
  const listen = await loadListenConfig(); // Refused before the root is claimed; never logged.
  const bearer = listen && createHash("sha256").update(listen.token).digest();
  const installation = await acquireInstallation(runtimeRoot());
  const path = socketPath(installation.root);
  let runtime: Runtime | undefined;
  let stopping = false;
  let ownershipLost = false;
  let pending = 0, connectionSequence = 0;
  daemonTiming?.({ layer: "daemon", event: "starting" });
  const budget = new RpcByteBudget();
  let serial: Promise<void> = Promise.resolve();
  const requests: Array<() => Promise<void>> = [];
  let running = false, started = false, drainReady = false, embeddingReady = false, preferDrain = false, embeddingNext = false;
  const backgroundReady = () => (drainReady || embeddingReady) && !stopping;
  const lostOwnership = () => {
    ownershipLost = true; stopping = true;
    runtime!.cancelDrain();
    setImmediate(() => { void stop().catch(logError); });
  };
  /** One spool-drain turn. Woken by remember/recovery through the runtime's scheduler. */
  const drainTurn = async () => {
    drainReady = false;
    try {
      daemonTiming?.({ layer: "daemon", event: "drain_start" });
      drainReady = await runtime!.drainTurn();
      daemonTiming?.({ layer: "daemon", event: "drain_complete" });
      if (!drainReady) console.log(JSON.stringify({ event: "drain_settled" }));
    } catch (error) {
      logError(error);
      if (fault(error).code === "ownership_lost") lostOwnership();
    }
  };
  /** One embedding outbox batch. Part B hooks the extraction scheduler as a third lane beside this one. */
  const embeddingTurn = async () => {
    embeddingReady = false;
    try {
      daemonTiming?.({ layer: "daemon", event: "embedding_start" });
      const outcome = await runtime!.embeddingTurn();
      daemonTiming?.({ layer: "daemon", event: "embedding_complete" });
      embeddingReady = outcome === "more";
      // Busy -> nothing left. A stalled worker (storage gone) is not idle; recovery wakes it.
      if (outcome === "idle") console.log(JSON.stringify({ event: "workers_idle" }));
    } catch (error) {
      logError(error);
      if (fault(error).code === "ownership_lost") lostOwnership();
    }
  };
  // Background has its own coalesced admission slot. Alternate dispatch lanes
  // so neither a saturated foreground nor a large drain cohort can starve one.
  // Inside the slot the spool and embedding lanes alternate as well, so a long
  // spool cohort cannot starve vectors, nor a deep outbox the spool.
  const kick = () => {
    if (running || !started) return;
    running = true;
    serial = (async () => {
      try {
        while (requests.length || backgroundReady()) {
          // Let real socket admissions arrive between turns, not just promises.
          await new Promise<void>(resolve => setImmediate(resolve));
          if (backgroundReady() && (preferDrain || !requests.length)) {
            preferDrain = false;
            const embedding = embeddingReady && (!drainReady || embeddingNext);
            embeddingNext = !embedding;
            if (embedding) await embeddingTurn(); else await drainTurn();
          } else {
            const request = requests.shift();
            if (request) { preferDrain = true; await request(); }
          }
        }
      } finally { running = false; }
    })();
  };
  const enqueue = (request: () => Promise<void>) => {
    const queued = async () => { try { await request(); } catch (error) { logError(error); } };
    requests.push(queued);
    kick();
    return () => { const index = requests.indexOf(queued); if (index !== -1) requests.splice(index, 1); };
  };
  const connections = new Set<Connection>();
  let shutdown: Promise<void> | undefined;
  const logError = (error: unknown) => {
    daemonTiming?.({ layer: "daemon", event: "error", hash: timingHash(String(error)) });
    console.error(JSON.stringify({ event: "error", error: String(error) }));
  };
  // Unfinished local publications, not receipts or client consumption. Policy
  // cancels these before acknowledging; it never waits for a non-reading peer.
  const publications = new Map<() => void, Socket>();
  const cancelPublications = async () => {
    await Promise.all([...new Set(publications.values())].map(socket => new Promise<void>(resolve => {
      socket.once("close", resolve); socket.destroy();
    })));
  };
  const auditPublication = (recall_id: string, state: RecallTransportInput["state"], context: InstallationContext) => {
    enqueue(async () => {
      let stage = "transport";
      try {
        await installation.assertOwned();
        await runtime!.recordRecallTransport({ recall_id, state }, context);
        if (state === "local_complete" && context.commit_mode === "auto") {
          stage = "exposure";
          await runtime!.exposeRecall(recall_id, context);
        }
      } catch (error) {
        // Bytes cannot be unsent. Never emit a second RPC reply or feedback.
        console.error(JSON.stringify({ event: "recall_publication_error", recall_id, stage, code: fault(error).code, error: String(error) }));
      }
    });
  };
  const send = (connection: Connection, value: unknown, reservation: ByteReservation,
    released = () => budget.release(reservation), recallId?: string, policyProtected = false, timing?: (event: string) => void) => {
    let finished = false;
    const disconnected = () => complete("delivery_unknown");
    const complete = (state: RecallTransportInput["state"]) => {
      if (finished) return;
      timing?.(state);
      finished = true; connection.writes.delete(disconnected); publications.delete(disconnected); released();
      if (recallId) auditPublication(recallId, state, connection.context!);
    };
    if (connection.socket.destroyed) { disconnected(); return; }
    try {
      const bytes = encode(value);
      budget.output(reservation, bytes.length);
      if (connection.socket.writableLength + bytes.length > RPC_LIMITS.frame_bytes * 2) {
        connection.socket.destroy(); disconnected(); return;
      }
      // Accounting and transport outcome are distinct. Only a successful local
      // callback permits exposure; close/partial output always remains unknown.
      connection.writes.add(disconnected);
      if (recallId || policyProtected) publications.set(disconnected, connection.socket);
      timing?.("write_start");
      connection.socket.write(bytes, error => {
        complete(error || connection.socket.destroyed ? "delivery_unknown" : "local_complete");
        if (error) connection.socket.destroy();
      });
    } catch (error) { disconnected(); connection.socket.destroy(); logError(error); }
  };
  const accept = (transport: "uds" | "tcp") => (socket: Socket) => {
    if (stopping || connections.size >= RPC_LIMITS.connections) { socket.destroy(); return; }
    const connection: Connection = { socket, authorized: transport === "uds", authenticated: false, pending: 0, closed: false,
      bytes: { used: { general: 0, control: 0, ingress: 0 } }, cancel: new Set(), writes: new Set() };
    connections.add(connection);
    const connectionId = ++connectionSequence;
    if (transport === "tcp") socket.setNoDelay(true);
    daemonTiming?.({ layer: "daemon", event: "connection", connection: connectionId });
    // Operational idle deadline; tests synchronize on socket/process events.
    socket.setTimeout(30_000, () => { daemonTiming?.({ layer: "daemon", event: "socket_idle_deadline", connection: connectionId }); socket.destroy(); });
    socket.on("error", error => {
      daemonTiming?.({ layer: "daemon", event: "socket_error", connection: connectionId, hash: timingHash(String(error)) });
      if (!("code" in error && ["ECONNRESET", "EPIPE"].includes(String(error.code)))) logError(error);
    });
    socket.on("close", () => {
      daemonTiming?.({ layer: "daemon", event: "socket_close", connection: connectionId });
      connection.closed = true;
      frames.dispose();
      for (const cancel of connection.cancel) cancel();
      for (const complete of connection.writes) complete();
      enqueue(async () => {
        await runtime?.uploads.disconnect(connection);
        connections.delete(connection);
      });
    });
    let receiving: ByteReservation | undefined;
    let failed = false;
    const frames = new Frames(bytes => {
      const reservation = receiving!; receiving = undefined;
      if (!connection.authorized) {
        // The first TCP frame must be the bearer. Anything else, malformed or
        // mismatched, is one uniform unauthorized fault; the peer learns nothing.
        let admitted = false;
        try { admitted = timingSafeEqual(createHash("sha256").update(RpcTcpAuth.parse(JSON.parse(bytes.toString("utf8"))).auth.bearer).digest(), bearer!); }
        catch { admitted = false; }
        if (admitted) { connection.authorized = true; budget.release(reservation); return true; }
        failed = true;
        daemonTiming?.({ layer: "daemon", event: "unauthorized", connection: connectionId });
        console.error(JSON.stringify({ event: "unauthorized", connection: connectionId }));
        send(connection, errorResponse(null, new RpcFault("unauthorized", "bearer authorization is required on this listener")), reservation);
        socket.end();
        return false;
      }
      if (stopping) { send(connection, errorResponse(null, new RpcFault("shutting_down", "runtime is stopping")), reservation); return false; }
      let request: RpcRequest;
      try { request = decodeRequest(bytes); }
      catch (error) { send(connection, errorResponse(null, fault(error)), reservation); return true; }
      const receivedAt = performance.now();
      const context = { method: request.method, id: request.id, connection: connectionId };
      const timing = daemonTiming && ["status", "remember", "ingest.status"].includes(request.method)
        ? (event: string) => daemonTiming?.({ layer: "daemon", event, ...context, elapsedMs: performance.now() - receivedAt }) : undefined;
      timing?.("request_received");
      // Eight global/two connection slots remain available to authenticated
      // control. Pre-hello status/shutdown cannot spend the reserved headroom.
      const control = connection.authenticated && ["status", "ingest.status", "shutdown"].includes(request.method);
      if ((reservation.pool === "ingress" && (!control || !budget.control(reservation))) ||
          pending >= RPC_LIMITS.queued_requests - (control ? 0 : 8) ||
          connection.pending >= RPC_LIMITS.queued_requests_per_connection - (control ? 0 : 2)) {
        send(connection, errorResponse(request.id, new RpcFault("resource_exhausted", "serial request reservation is full", true)), reservation, undefined, undefined, false, timing);
        return true;
      }
      pending++; connection.pending++;
      let reserved = true;
      const release = () => { if (reserved) { reserved = false; pending--; connection.pending--; budget.release(reservation); } };
      const cancel = () => { timing?.("queue_cancelled"); remove(); connection.cancel.delete(cancel); release(); };
      const remove = enqueue(async () => {
        timing?.("dispatch_start");
        connection.cancel.delete(cancel);
        let sent = false;
        try {
          if (connection.closed) return;
          if (ownershipLost) throw new RpcFault("ownership_lost", "runtime writer ownership was lost");
          await installation.assertOwned();
          const result = await (timing ? timingContext.run(context, () => dispatch(connection, request)) : dispatch(connection, request));
          timing?.("dispatch_complete");
          const policyRevision = (request.method === "policy.set" || request.method === "policy.revoke")
            ? (result as { policy_revision: number }).policy_revision : request.method === "recall"
              ? (result as { diagnostics: { policy_revision: number } }).diagnostics.policy_revision : request.method === "graph.envelope"
                ? (result as { pin: { policy_revision: number } }).pin.policy_revision : null;
          const recallId = request.method === "recall" ? (result as { recall_id: string | null }).recall_id : null;
          send(connection, { ...envelope(), policy_revision: policyRevision, id: request.id, method: request.method, result }, reservation, release, recallId ?? undefined, request.method.startsWith('extraction.audit.'), timing); sent = true;
        } catch (error) {
          timing?.("dispatch_failed");
          const failure = fault(error);
          send(connection, errorResponse(request.id, failure), reservation, release, undefined, false, timing); sent = true;
          if (failure.code === "internal_error") logError(error);
          if (failure.code === "ownership_lost") { ownershipLost = true; stopping = true; setImmediate(() => { void stop().catch(logError); }); }
        } finally { if (!sent) release(); }
      });
      connection.cancel.add(cancel);
      return true;
    }, encodedBytes => {
      const reservation = budget.reserve(connection.bytes, encodedBytes, connection.authenticated);
      if (!reservation) throw new RpcFault("resource_exhausted", "encoded frame byte budget is full", true);
      receiving = reservation;
      return () => { receiving = undefined; budget.release(reservation); };
    });
    socket.on("data", (data: Buffer) => {
      if (failed) return;
      try { frames.push(data); }
      catch (error) {
        failed = true; frames.dispose();
        // Invalid lengths have no body reservation. A best-effort error must
        // acquire ordinary bytes too; saturation closes without uncharged output.
        const reservation = budget.reserve(connection.bytes, 0, false);
        if (reservation) { send(connection, errorResponse(null, fault(error)), reservation); socket.end(); }
        else socket.destroy();
      }
    });
  };
  const server = createServer(accept("uds"));
  const tcpServer = listen ? createServer(accept("tcp")) : undefined;
  const listeners = tcpServer ? [server, tcpServer] : [server];
  async function dispatch(connection: Connection, request: RpcRequest): Promise<unknown> {
    if (!runtime) throw new RpcFault("storage_unavailable", "runtime is starting", true);
    if (request.method === "hello") {
      if (connection.authenticated) throw new RpcFault("already_authenticated", "hello already completed");
      const supplied = Buffer.from(request.params.token);
      const expected = Buffer.from(installation.token);
      if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) throw new RpcFault("authentication_failed", "invalid installation token");
      connection.authenticated = true;
      connection.context = Object.freeze({ principal: "installation", commit_mode: request.params.commit_mode, client_binding: randomUUID() });
      return { version: 1, principal: "installation", commit_mode: request.params.commit_mode,
        data_incarnation: installation.incarnation, fs_epoch: installation.epoch, capabilities: runtime.capabilities,
        limits: { frame_bytes: RPC_LIMITS.frame_bytes, chunk_bytes: RPC_LIMITS.chunk_bytes, object_bytes: RPC_LIMITS.object_bytes, content_bytes: RPC_LIMITS.content_bytes } };
    }
    if (!connection.authenticated) throw new RpcFault("unauthenticated", "hello authentication is required");
    switch (request.method) {
      case "status": return runtime.status(pending, stopping);
      case "remember": return runtime.remember(request.params, connection.context!);
      case "ingest.status": return runtime.ingestStatus(request.params);
      case "object.begin": return runtime.uploads.begin(connection, { hash: request.params.sha256, size: request.params.size, media_type: request.params.media_type });
      case "object.chunk": return runtime.uploads.chunk(connection, request.params.upload_id, request.params.seq, Buffer.from(request.params.bytes_b64, "base64"));
      case "object.commit": return runtime.uploads.commit(connection, request.params.upload_id);
      case "recall": return runtime.recall(request.params, connection.context!);
      case "extraction.audit.create": return runtime.createExtractionPipeline(request.params,connection.context!);
      case "extraction.audit.run": return runtime.runExtractionPipeline(request.params,connection.context!);
      case "extraction.audit.status": return runtime.extractionPipelineStatus(request.params.pipeline_id,connection.context!);
      case "dream.admit": return runtime.admitDream(request.params, connection.context!);
      case "dream.status": return runtime.dreamStatus(request.params.job_id, connection.context!);
      case "dream.lease": return runtime.leaseDream(request.params, connection.context!);
      case "dream.expire": return runtime.expireDream(request.params, connection.context!);
      case "dream.execute": return runtime.executeDream(request.params, connection.context!);
      case "graph.envelope": return runtime.graphEnvelope(request.params as { seed_ids: string[]; T?: number }, connection.context!);
      case "embedding.recover": return runtime.recoverEmbedding(request.params, connection.context!);
      case "embedding.status": return runtime.embeddingStatus(request.params.operation_id, connection.context!);
      case "backup": return runtime.backup(connection.context!, request.params.destination, request.params.operation_id);
      case "restore": return runtime.restore(connection.context!, request.params.archive, request.params.operation_id);
      case "backup.status": return runtime.backupStatus(request.params.operation_id);
      case "restore.status": return runtime.restoreStatus(request.params.operation_id);
      case "commit":
        if (connection.context!.commit_mode !== "receipt") throw new RpcFault("commit_mode_mismatch", "commit requires receipt-mode authentication");
        return runtime.commit(request.params, connection.context!);
      case "policy.set":
        await cancelPublications();
        return runtime.setPolicy(request.params, connection.context!);
      case "policy.revoke":
        await cancelPublications();
        return runtime.revokePolicy(request.params, connection.context!);
      case "hit-cache.verify": return runtime.verifyHitCache();
      case "hit-cache.rebuild": return runtime.rebuildHitCache();
      case "shutdown":
        stopping = true;
        setImmediate(() => { void stop().catch(logError); });
        return { state: "stopping" };
    }
  }
  const stop = (): Promise<void> => shutdown ??= (async () => {
    stopping = true;
    drainReady = false; embeddingReady = false;
    runtime?.cancelDrain();
    const closed = Promise.all(listeners.map(listener => new Promise<void>((resolve, reject) => listener.close(error => error ? reject(error) : resolve()))));
    const deadline = setTimeout(() => {
      logError(new Error("graceful shutdown deadline exceeded"));
      for (const connection of connections) connection.socket.destroy();
      process.exitCode = 1;
    }, 120_000);
    try {
      await serial;
      await cancelPublications();
      for (const connection of connections) connection.socket.destroySoon();
      await closed;
      await serial; // disconnect cleanup is queued by the exact socket close event.
      await runtime?.close();
      await installation.assertOwned();
      await rm(path, { force: true });
      await installation.release();
      daemonTiming?.({ layer: "daemon", event: "stopped" });
      console.log(JSON.stringify({ event: "stopped" }));
    } finally {
      clearTimeout(deadline);
      process.off("SIGINT", signalStop);
      process.off("SIGTERM", signalStop);
    }
  })();
  const signalStop = () => { void stop().catch(error => { logError(error); process.exitCode = 1; }); };
  try {
    runtime = await Runtime.create(installation, lane => { if (lane === "embedding") embeddingReady = true; else drainReady = true; kick(); }, {
      enqueue: job => { if (!stopping) enqueue(async () => { if (!stopping) { await installation.assertOwned(); await job(); } }); },
    });
    await runtime.init();
    await rm(path, { force: true }); // Only after acquiring the exclusive root owner.
    const listening = once(server, "listening");
    server.listen(path);
    await listening;
    await chmod(path, 0o600);
    let tcp: { host: string; port: number } | undefined;
    if (tcpServer && listen) {
      const tcpListening = once(tcpServer, "listening");
      tcpServer.listen({ host: listen.host, port: listen.port });
      await tcpListening;
      const address = tcpServer.address() as AddressInfo; // Port 0 binds an ephemeral port; report the real one.
      tcp = { host: address.address, port: address.port };
    }
    process.on("SIGINT", signalStop);
    process.on("SIGTERM", signalStop);
    daemonTiming?.({ layer: "daemon", event: "listening" });
    console.log(JSON.stringify({ event: "listening", socket: path, ...(tcp ? { tcp } : {}), data_incarnation: installation.incarnation, fs_epoch: installation.epoch }));
    started = true; kick();
  } catch (error) {
    for (const listener of listeners) if (listener.listening) listener.close(); // A failed second bind must not keep the process alive.
    await runtime?.close();
    await installation.release();
    throw error;
  }
}
