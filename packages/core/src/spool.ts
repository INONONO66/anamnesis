import { createHash } from "node:crypto";
import { mkdir, open, rename, stat } from "node:fs/promises";
import { join, resolve } from "node:path";
import { StringDecoder } from "node:string_decoder";

export interface SpoolRecord { origin: string; revision: string; predecessor: string | null; body: unknown; incarnation: string; }
export interface SpoolEntry extends SpoolRecord { sequence: number; }
export interface SpoolOptions { maxBytes?: number; maxFrameBytes?: number; }
/** maxBytes counts encoded journal frames (4-byte length + JSON body + 32-byte digest). */
export interface SpoolPageRequest { cursor?: string; limit?: number; maxBytes?: number; }
export interface SpoolPage { entries: SpoolEntry[]; nextCursor: string | null; }
export type SpoolStatus = { pending: number; nextSequence: number; quarantined: boolean };

const stable = (value: unknown, ancestors = new Set<object>()): string => {
  if (value === null || typeof value === "string" || typeof value === "boolean") return JSON.stringify(value);
  if (typeof value === "number" && Number.isFinite(value)) return JSON.stringify(value);
  if (typeof value !== "object" || ancestors.has(value)) throw new Error("spool requires finite JSON values");
  const array = Array.isArray(value);
  if (!array && Object.getPrototypeOf(value) !== Object.prototype && Object.getPrototypeOf(value) !== null) throw new Error("spool requires plain JSON objects");
  const keys = Reflect.ownKeys(value).filter(key => !array || key !== "length");
  for (const key of keys) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (typeof key !== "string" || !descriptor?.enumerable || !("value" in descriptor)) throw new Error("spool requires JSON data properties");
  }
  ancestors.add(value);
  try {
    if (array) {
      if (keys.length !== value.length || !Array.from({ length: value.length }, (_, i) => Object.hasOwn(value, i)).every(Boolean)) throw new Error("spool requires dense JSON arrays");
      return `[${value.map(item => stable(item, ancestors)).join(",")}]`;
    }
    return `{${Object.entries(value).sort(([a], [b]) => a.localeCompare(b)).map(([k, v]) => `${JSON.stringify(k)}:${stable(v, ancestors)}`).join(",")}}`;
  } finally { ancestors.delete(value); }
};
const checksum = (bytes: Uint8Array): Buffer => createHash("sha256").update(bytes).digest();
function assertRecord(value: unknown): asserts value is SpoolRecord {
  if (!value || typeof value !== "object" || !("origin" in value) || typeof value.origin !== "string" ||
    !("revision" in value) || typeof value.revision !== "string" || !("incarnation" in value) || typeof value.incarnation !== "string" ||
    !("predecessor" in value) || (value.predecessor !== null && typeof value.predecessor !== "string") || !Object.hasOwn(value, "body")) throw new Error("invalid spool record");
}
type Completion = { present: boolean; version: number; frontier: number; maximum: number; contains: boolean; advanced: number; payloadField?: number; arrayField?: number };
const emptyCompletion = (): Completion => ({ present: false, version: 2, frontier: 0, maximum: 0, contains: false, advanced: 0 });
type SpoolState = SpoolPage & { boundary: number; count: number; completion: Completion; quarantined: boolean };
const emptyState = (quarantined = false): SpoolState => ({ boundary: 0, count: 0, entries: [], nextCursor: null, completion: emptyCompletion(), quarantined });
type PageCursor = { boundary: number; count: number; index: number; proof: string; identity: string };
class CorruptSpool extends Error {}

/** Streaming JSON for the existing double-encoded .done envelope, not a new authority.
 * No sequence array or payload string is materialized. Chunks are <=64 KiB;
 * individual scalar/key tokens are <=64 Ki characters, ignored nesting <=64.
 */
class MetadataJson {
  private chunk = ""; private index = 0;
  constructor(private readonly source: AsyncIterator<string>) {}
  async peek(): Promise<string> {
    while (this.index === this.chunk.length) {
      const next = await this.source.next();
      if (next.done) return "";
      this.chunk = next.value; this.index = 0;
    }
    return this.chunk[this.index]!;
  }
  async take(): Promise<string> { const value = await this.peek(); if (value) this.index++; return value; }
  async space(): Promise<void> { while (/^[\t\n\r ]$/.test(await this.peek())) this.index++; }
  async expect(value: string): Promise<void> {
    await this.space(); if (await this.take() !== value) throw new CorruptSpool("invalid completion JSON");
  }
  async end(): Promise<void> { await this.space(); if (await this.peek()) throw new CorruptSpool("trailing completion JSON"); }
  async *string(): AsyncGenerator<string> {
    await this.expect('"');
    let text = "";
    while (true) {
      let char = await this.take();
      if (!char || char.charCodeAt(0) < 32) throw new CorruptSpool("invalid completion string");
      if (char === '"') { if (text) yield text; return; }
      if (char === "\\") {
        const escape = await this.take();
        let encoded = "\\" + escape;
        if (escape === "u") for (let i = 0; i < 4; i++) encoded += await this.take();
        try { char = JSON.parse('"' + encoded + '"'); }
        catch { throw new CorruptSpool("invalid completion escape"); }
      }
      text += char;
      // Hash UTF-8 only on complete surrogate pairs, including escaped pairs.
      const last = text.charCodeAt(text.length - 1);
      if (text.length >= 4096) {
        const carry = last >= 0xd800 && last <= 0xdbff ? text.slice(-1) : "";
        yield carry ? text.slice(0, -1) : text; text = carry;
      }
    }
  }
  async smallString(cap = 64 * 1024): Promise<string> {
    let text = "";
    for await (const chunk of this.string()) {
      text += chunk;
      if (text.length > cap) throw new CorruptSpool("oversized completion token");
    }
    return text;
  }
  async atom(): Promise<unknown> {
    await this.space(); let text = "";
    while (true) {
      const char = await this.peek();
      if (!char || /^[\t\n\r ,\]}]$/.test(char)) break;
      text += await this.take();
      if (text.length > 64 * 1024) throw new CorruptSpool("oversized completion token");
    }
    try { return JSON.parse(text); } catch { throw new CorruptSpool("invalid completion scalar"); }
  }
  async number(): Promise<number> {
    await this.space();
    if (!/^[-0-9]$/.test(await this.peek())) { await this.skip(); return NaN; }
    const value = await this.atom(); return typeof value === "number" ? value : NaN;
  }
  async object(field: (key: string) => Promise<void>): Promise<void> {
    await this.expect("{"); await this.space();
    if (await this.peek() === "}") { await this.take(); return; }
    while (true) {
      const key = await this.smallString(); await this.expect(":"); await field(key); await this.space();
      const next = await this.take(); if (next === "}") return;
      if (next !== ",") throw new CorruptSpool("invalid completion object");
    }
  }
  async array(item: () => Promise<void>): Promise<void> {
    await this.expect("["); await this.space();
    if (await this.peek() === "]") { await this.take(); return; }
    while (true) {
      await item(); await this.space();
      const next = await this.take(); if (next === "]") return;
      if (next !== ",") throw new CorruptSpool("invalid completion array");
    }
  }
  async skip(depth = 0): Promise<void> {
    if (depth > 64) throw new CorruptSpool("completion nesting limit exceeded");
    await this.space();
    switch (await this.peek()) {
      case "{": await this.object(async () => this.skip(depth + 1)); break;
      case "[": await this.array(async () => this.skip(depth + 1)); break;
      case '"': for await (const _ of this.string()) { /* bounded discard */ } break;
      default: await this.atom();
    }
  }
}

type SequenceSummary = { first: number; maximum: number; prefix: number; contains: boolean; advanced: number; valid: boolean; field: number };
async function completionPayload(json: MetadataJson, target: number, output?: { selected: Completion; emit: (value: number) => Promise<void> }): Promise<Completion> {
  let version = NaN, frontier = NaN, legacy: SequenceSummary | undefined, current: SequenceSummary | undefined, field = 0;
  await json.object(async key => {
    if (key === "version") version = await json.number();
    else if (key === "frontier") frontier = await json.number();
    else if (key === "sequences" || key === "completed") {
      field++;
      const summary: SequenceSummary = { first: 0, maximum: 0, prefix: 0, contains: false, advanced: target, valid: true, field };
      await json.space();
      if (await json.peek() !== "[") { await json.skip(); summary.valid = false; }
      else await json.array(async () => {
        const value = await json.number();
        if (!Number.isSafeInteger(value) || value <= summary.maximum) summary.valid = false;
        if (!summary.first) summary.first = value;
        summary.maximum = value;
        if (value === summary.prefix + 1) summary.prefix++;
        if (value === target) summary.contains = true;
        if (value === summary.advanced + 1) summary.advanced++;
        if (output?.selected.arrayField === field) await output.emit(value);
      });
      if (key === "sequences") legacy = summary; else current = summary;
    } else await json.skip();
  });
  const sequences = version === 1 ? legacy : current;
  if (version === 1) frontier = sequences?.prefix ?? NaN;
  if ((version !== 1 && version !== 2) || !sequences?.valid || !Number.isSafeInteger(frontier) || frontier < 0 ||
    (version === 2 && sequences.first !== 0 && sequences.first <= frontier + 1)) throw new CorruptSpool("invalid completion metadata");
  return { present: true, version, frontier, maximum: Math.max(frontier, sequences.maximum), contains: sequences.contains,
    advanced: sequences.advanced, arrayField: sequences.field };
}

export class DurableSpool {
  private readonly journal: string; private readonly done: string; private readonly durable: string;
  private readonly maxBytes: number; private readonly maxFrameBytes: number;
  private queue: Promise<void> = Promise.resolve();
  constructor(private readonly root: string, options: SpoolOptions = {}) {
    this.journal = join(root, "spool.journal"); this.done = join(root, "spool.done"); this.durable = join(root, "spool.durable");
    this.maxBytes = options.maxBytes ?? 1024 * 1024 * 1024; this.maxFrameBytes = options.maxFrameBytes ?? 1024 * 1024;
  }
  async append(record: SpoolRecord): Promise<number> {
    // Snapshot and reject non-JSON input before recovery can mutate any file.
    const body = Buffer.from(stable(record)); assertRecord(JSON.parse(body.toString()));
    if (body.length + 36 > this.maxFrameBytes) throw new Error("spool quota exceeded");
    const frame = Buffer.concat([Buffer.alloc(4), body, checksum(body)]); frame.writeUInt32BE(body.length, 0);
    return this.serial(async () => {
      const state = await this.load();
      if (state.quarantined) throw new Error("spool is quarantined");
      if (state.boundary + frame.length > this.maxBytes || !Number.isSafeInteger(state.count + 1)) throw new Error("spool quota exceeded");
      const marker = String(state.boundary + frame.length);
      // The existing proof domain includes the decimal boundary, so extending it
      // requires a second bounded scan, not a copy of the old journal in memory.
      const proof = createHash("sha256").update(`spool.durable.v1\0${marker}\0`);
      if (state.boundary) {
        const previous = await open(this.journal, "r");
        try {
          for (let offset = 0; offset < state.boundary; offset += 64 * 1024) {
            proof.update(await this.readAt(previous, offset, Math.min(64 * 1024, state.boundary - offset)));
          }
        } finally { await previous.close(); }
      }
      proof.update(frame);
      await mkdir(this.root, { recursive: true, mode: 0o700 });
      const handle = await open(this.journal, "a", 0o600);
      try { await this.writeAll(handle, frame); await handle.sync(); } finally { await handle.close(); }
      // Keep the public numeric marker, but never trust it without its independently
      // synced prefix proof. Interrupted publication fails closed on a mismatch.
      await this.publish(this.durable + ".sha256", proof.digest("hex"));
      await this.publish(this.durable, marker);
      return state.count + 1;
    });
  }
  /** Replay includes completed suffix entries until every preceding entry is complete. */
  async pending(pageSize = 100): Promise<SpoolEntry[]> {
    // Legacy caller-selected output size is retained; use page() for hard output caps.
    return this.serial(async () => {
      let limit = Number.isNaN(pageSize) ? 0 : Math.max(0, Math.trunc(pageSize));
      if (pageSize < 0) {
        // Preserve Array.slice's legacy negative-end semantics without collecting
        // the journal first. Only this compatibility case needs a counting pass.
        const state = await this.load();
        const end = Math.trunc(state.completion.frontier + pageSize);
        limit = Math.max(0, (end < 0 ? Math.max(0, state.count + end) : Math.min(end, state.count)) - state.completion.frontier);
      }
      return (await this.load(limit)).entries;
    });
  }
  /**
   * Live memory: one configured maximum frame, the capped page, and fixed-size
   * metadata chunks/tokens. Every operation still verifies the full durable
   * prefix and completion metadata (O(journal + metadata bytes) work).
   * Cursors retain their original replay view across append/complete and reopen.
   */
  async page(request: SpoolPageRequest = {}): Promise<SpoolPage> {
    return this.serial(async () => {
      const limit = this.pageLimit(request.limit ?? 100);
      const maxBytes = this.pageBytes(request.maxBytes ?? 4 * 1024 * 1024);
      if (await this.isQuarantined()) throw new Error("spool is quarantined");
      const cursor = request.cursor === undefined ? null : this.decodePageCursor(request.cursor);
      try { const { entries, nextCursor } = await this.scan(cursor, limit, maxBytes); return { entries, nextCursor }; }
      catch (error) {
        if (!(error instanceof CorruptSpool)) throw error;
        await this.quarantine();
        throw new Error("spool is quarantined", { cause: error });
      }
    });
  }
  async complete(sequence: number): Promise<void> {
    return this.serial(async () => {
      const state = await this.load(0, sequence);
      if (state.quarantined) throw new Error("spool is quarantined");
      if (!Number.isSafeInteger(sequence) || sequence < 1 || sequence > state.count) throw new Error("sequence is not pending");
      if (sequence <= state.completion.frontier || state.completion.contains) return;
      await this.publishCompletion(sequence, state.completion);
    });
  }
  async status(): Promise<SpoolStatus> {
    return this.serial(async () => { const state = await this.load(); return { pending: state.count - state.completion.frontier, nextSequence: state.count + 1, quarantined: state.quarantined }; });
  }
  private serial<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.queue.then(operation);
    // Only release the queue on failure; the caller still receives the rejection.
    this.queue = result.then(() => {}, () => {});
    return result;
  }
  private async load(limit = 0, sequence = 0): Promise<SpoolState> {
    if (await this.isQuarantined()) return emptyState(true);
    try { return await this.scan(null, limit, Infinity, sequence); }
    catch (error) {
      if (!(error instanceof CorruptSpool)) throw error;
      return this.quarantine();
    }
  }
  private async publish(path: string, contents: string): Promise<void> {
    const handle = await open(path + ".tmp", "w", 0o600);
    try { await this.writeAll(handle, Buffer.from(contents)); await handle.sync(); } finally { await handle.close(); }
    await this.publishRename(path);
  }
  private async publishRename(path: string): Promise<void> {
    await rename(path + ".tmp", path);
    const directory = await open(this.root, "r");
    try { await directory.sync(); } finally { await directory.close(); }
  }
  private async quarantine(): Promise<SpoolState> {
    await this.publish(join(this.root, "spool.quarantine"), "protected corruption\n"); return emptyState(true);
  }
  private async isQuarantined(): Promise<boolean> { try { await stat(join(this.root, "spool.quarantine")); return true; } catch (e) { if (this.isMissing(e)) return false; throw e; } }
  private async openOptional(path: string): Promise<Awaited<ReturnType<typeof open>> | null> {
    try { return await open(path, "r"); }
    catch (error) { if (this.isMissing(error)) return null; throw error; }
  }
  private async readMarker(path: string, cap: number): Promise<Buffer | null> {
    const handle = await this.openOptional(path);
    if (!handle) return null;
    try {
      const { size } = await handle.stat();
      if (size > cap) throw new CorruptSpool("oversized admission metadata");
      return await this.readAt(handle, 0, size);
    } finally { await handle.close(); }
  }
  private async writeAll(handle: Awaited<ReturnType<typeof open>>, bytes: Buffer): Promise<void> {
    let offset = 0;
    while (offset < bytes.length) {
      const { bytesWritten } = await handle.write(bytes, offset, Math.min(bytes.length - offset, 64 * 1024), null);
      if (!bytesWritten) throw new Error("spool write made no progress");
      offset += bytesWritten;
    }
  }
  private pageLimit(value: number): number { if (!Number.isSafeInteger(value) || value <= 0 || value > 10_000) throw new Error("invalid spool page limit"); return value; }
  private pageBytes(value: number): number { if (!Number.isSafeInteger(value) || value <= 0 || value > 16 * 1024 * 1024) throw new Error("invalid spool page byte budget"); return value; }
  private encodePageCursor(value: PageCursor): string {
    const payload = JSON.stringify({ version: 2, ...value });
    return Buffer.from(JSON.stringify({ payload, checksum: checksum(Buffer.from(payload)).toString("hex") })).toString("base64url");
  }
  private decodePageCursor(cursor: string): PageCursor {
    try {
      if (typeof cursor !== "string" || cursor.length > 4096 || !/^[A-Za-z0-9_-]+$/.test(cursor)) throw new Error();
      const envelope = JSON.parse(Buffer.from(cursor, "base64url").toString());
      if (!envelope || typeof envelope.payload !== "string" || envelope.checksum !== checksum(Buffer.from(envelope.payload)).toString("hex")) throw new Error();
      const value = JSON.parse(envelope.payload);
      if (!value || value.version !== 2 || !Number.isSafeInteger(value.boundary) || value.boundary <= 0 ||
        !Number.isSafeInteger(value.count) || value.count <= 0 || !Number.isSafeInteger(value.index) || value.index < 0 || value.index >= value.count ||
        typeof value.proof !== "string" || !/^[a-f0-9]{64}$/.test(value.proof) ||
        typeof value.identity !== "string" || !/^[a-f0-9]{64}$/.test(value.identity)) throw new Error();
      return { boundary: value.boundary, count: value.count, index: value.index, proof: value.proof, identity: value.identity };
    } catch { throw new Error("invalid spool page cursor"); }
  }
  private async scan(cursor: PageCursor | null, limit: number, maxBytes: number, sequence = 0): Promise<SpoolState> {
    const markerBytes = await this.readMarker(this.durable, 16);
    const proofBytes = await this.readMarker(this.durable + ".sha256", 64);
    const completion = await this.readCompletion(sequence);
    const handle = await this.openOptional(this.journal);
    try {
      const info = await handle?.stat();
      if (!info?.size && markerBytes === null && proofBytes === null && !completion.present) {
        if (cursor) throw new Error("stale spool page cursor");
        return emptyState();
      }
      const marker = markerBytes?.toString(); const boundary = Number(marker);
      if (!handle || !info || !marker || !/^[1-9][0-9]*$/.test(marker) || !Number.isSafeInteger(boundary) ||
        boundary > info.size || !proofBytes || !/^[a-f0-9]{64}$/.test(proofBytes.toString())) throw new CorruptSpool("invalid admission");
      // Identity survives ordinary reopen/append, but not root relocation or file replacement.
      const identity = checksum(Buffer.from(JSON.stringify([resolve(this.root), info.dev, info.ino, info.birthtimeMs]))).toString("hex");
      if (cursor && (cursor.identity !== identity || cursor.boundary > boundary)) throw new Error("stale spool page cursor");
      const start = cursor?.index ?? completion.frontier;
      const snapshotBoundary = cursor?.boundary ?? boundary;
      const admissionHash = createHash("sha256").update(`spool.durable.v1\0${marker}\0`);
      const snapshotHash = createHash("sha256").update(`spool.durable.v1\0${snapshotBoundary}\0`);
      const entries: SpoolEntry[] = [];
      let offset = 0, count = 0, snapshotCount = 0, encodedBytes = 0;
      let full = limit === 0, tooSmall = false;
      while (offset < boundary) {
        if (boundary - offset < 4) throw new CorruptSpool("short frame header");
        const header = await this.readAt(handle, offset, 4); const length = header.readUInt32BE(0);
        if (length + 36 > this.maxFrameBytes || offset + length + 36 > boundary) throw new CorruptSpool("invalid frame length");
        const data = await this.readAt(handle, offset + 4, length + 32);
        admissionHash.update(header).update(data);
        if (offset < snapshotBoundary) snapshotHash.update(header).update(data);
        const body = data.subarray(0, length);
        if (!checksum(body).equals(data.subarray(length))) throw new CorruptSpool("frame checksum mismatch");
        let record: SpoolRecord;
        try {
          const parsed: unknown = JSON.parse(body.toString()); assertRecord(parsed);
          if (!Buffer.from(stable(parsed)).equals(body)) throw new Error();
          record = parsed;
        } catch { throw new CorruptSpool("invalid canonical record"); }
        count++;
        if (offset < snapshotBoundary && count > start && !full) {
          if (entries.length === limit || encodedBytes + length + 36 > maxBytes) {
            full = true; tooSmall = entries.length === 0;
          } else { entries.push({ ...record, sequence: count }); encodedBytes += length + 36; }
        }
        offset += length + 36;
        if (offset === snapshotBoundary) snapshotCount = count;
      }
      if (admissionHash.digest("hex") !== proofBytes.toString() || completion.maximum > count) throw new CorruptSpool("invalid protected metadata");
      const proof = snapshotHash.digest("hex");
      if (cursor && (snapshotCount !== cursor.count || proof !== cursor.proof)) throw new Error("stale spool page cursor");
      if (tooSmall) throw new Error("spool page byte budget too small");
      // Preserve recovery ordering: validate every admitted frame and done metadata first.
      if (info.size > boundary) {
        const writable = await open(this.journal, "r+");
        try { await writable.truncate(boundary); await writable.sync(); } finally { await writable.close(); }
      }
      const index = start + entries.length;
      return { boundary, count, completion, quarantined: false, entries,
        nextCursor: limit > 0 && index < snapshotCount ? this.encodePageCursor({ boundary: snapshotBoundary, count: snapshotCount, index, proof, identity }) : null };
    } finally { await handle?.close(); }
  }
  private async readCompletion(sequence = 0, output?: { selected: Completion; emit: (value: number) => Promise<void> }): Promise<Completion> {
    const handle = await this.openOptional(this.done);
    if (!handle) return emptyCompletion();
    try {
      const json = new MetadataJson(this.metadataChunks(handle));
      let result: Completion | undefined, expected: string | undefined, digest: string | undefined, payloadField = 0;
      await json.object(async key => {
        if (key === "payload") {
          payloadField++;
          const hash = createHash("sha256");
          const chunks = json.string();
          async function* payload() { for await (const chunk of chunks) { hash.update(chunk); yield chunk; } }
          const nested = new MetadataJson(payload());
          result = await completionPayload(nested, sequence, output?.selected.payloadField === payloadField ? output : undefined);
          await nested.end();
          result.payloadField = payloadField;
          digest = hash.digest("hex");
        } else if (key === "checksum") expected = await json.smallString(64);
        else await json.skip();
      });
      await json.end();
      if (!result || expected !== digest) throw new CorruptSpool("invalid completion checksum");
      return result;
    } finally { await handle.close(); }
  }
  private async *metadataChunks(handle: Awaited<ReturnType<typeof open>>): AsyncGenerator<string> {
    const buffer = Buffer.allocUnsafe(64 * 1024), decoder = new StringDecoder("utf8");
    while (true) {
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, null);
      if (!bytesRead) break;
      yield decoder.write(buffer.subarray(0, bytesRead));
    }
    yield decoder.end();
  }
  private async publishCompletion(sequence: number, previous: Completion): Promise<void> {
    const frontier = sequence === previous.frontier + 1 ? Math.max(sequence, previous.advanced) : previous.frontier;
    const handle = await open(this.done + ".tmp", "w", 0o600);
    try {
      const hash = createHash("sha256");
      let buffer = '{"payload":"', first = true, inserted = false;
      const flush = async () => { await this.writeAll(handle, Buffer.from(buffer)); buffer = ""; };
      const payload = async (text: string) => {
        hash.update(text);
        buffer += JSON.stringify(text).slice(1, -1);
        if (buffer.length >= 4096) await flush();
      };
      const emit = async (value: number) => {
        if (value <= frontier) return;
        await payload(`${first ? "" : ","}${value}`); first = false;
      };
      await payload('{"completed":[');
      await this.readCompletion(sequence, { selected: previous, emit: async value => {
        if (!inserted && sequence < value) { await emit(sequence); inserted = true; }
        await emit(value);
      } });
      if (!inserted) await emit(sequence);
      await payload(`],"frontier":${frontier},"version":2}`);
      buffer += `","checksum":"${hash.digest("hex")}"}`;
      await flush();
      await handle.sync();
    } finally { await handle.close(); }
    await this.publishRename(this.done);
  }
  private async readAt(handle: Awaited<ReturnType<typeof open>>, position: number, length: number): Promise<Buffer> {
    const buffer = Buffer.allocUnsafe(length); let offset = 0;
    while (offset < length) {
      const result = await handle.read(buffer, offset, Math.min(length - offset, 64 * 1024), position + offset);
      if (!result.bytesRead) throw new CorruptSpool("short frame read");
      offset += result.bytesRead;
    }
    return buffer;
  }
  private isMissing(error: unknown): boolean { return error instanceof Error && "code" in error && error.code === "ENOENT"; }
}
