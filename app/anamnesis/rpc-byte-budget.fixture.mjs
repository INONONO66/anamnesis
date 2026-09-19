// Private instrumentation delegates to actual allocation, parser, runtime and
// socket writes. IPC gates hold exact execution/write-completion boundaries.
import { foreground } from './daemon.ts';
import { Runtime } from './runtime.ts';
import * as wire from './wire.ts';
import { Socket } from 'node:net';
const emit = value => process.send?.(value);
let armed = false, release, active = 0, maxActive = 0, allocations = 0, allocated = 0;
let parserId = 0, socketId = 0, currentParser, holdWrites = false, corkWrites = false, budget;
const parsers = new WeakMap(), sockets = new Set(), completions = [];
const accountParsers = new WeakMap(), reservationIds = new WeakMap();
let reservationId = 0;
const init = Runtime.prototype.init;
Runtime.prototype.init = async function () {
  await init.call(this);
  const commit = this.uploads.commit.bind(this.uploads);
  this.uploads.commit = async (...args) => {
    active++; maxActive = Math.max(maxActive, active);
    try {
      if (armed) {
        armed = false;
        const gate = new Promise(resolve => { release = resolve; });
        emit({ event: 'parked', active, maxActive }); await gate;
      }
      return await commit(...args);
    } finally { active--; emit({ event: 'mutation-finished', active, maxActive }); }
  };
};
for (const name of ['remember', 'drainTurn']) {
  const original = Runtime.prototype[name];
  Runtime.prototype[name] = async function (...args) {
    active++; maxActive = Math.max(maxActive, active);
    try { return await original.apply(this, args); }
    finally { active--; emit({ event: 'mutation-finished', method: name, active, maxActive }); }
  };
}
const allocate = Buffer.allocUnsafe;
Buffer.allocUnsafe = function (size) {
  const result = allocate(size);
  if (currentParser !== undefined && size === 1024 * 1024) {
    allocations++; allocated += size; emit({ event: 'body-allocation', parser: currentParser, size, allocations, allocated });
  }
  return result;
};
const push = wire.Frames.prototype.push;
wire.Frames.prototype.push = function (bytes) {
  if (!parsers.has(this)) parsers.set(this, ++parserId);
  currentParser = parsers.get(this);
  try { return push.call(this, bytes); }
  finally { emit({ event: 'input', parser: currentParser, bytes: bytes.length, allocations, allocated }); currentParser = undefined; }
};
// Optional only for baseline compatibility: RED asserts actual allocation,
// never the presence of new accounting symbols.
if (wire.RpcByteBudget) {
  for (const name of ['reserve', 'release', 'output', 'control']) {
    const original = wire.RpcByteBudget.prototype[name];
    wire.RpcByteBudget.prototype[name] = function (...args) {
      budget = this;
      const result = original.apply(this, args);
      const reservation = name === 'reserve' ? result : args[0];
      const account = name === 'reserve' ? args[0] : reservation.account;
      if (currentParser !== undefined) accountParsers.set(account, currentParser);
      if (reservation && !reservationIds.has(reservation)) reservationIds.set(reservation, ++reservationId);
      // The error-output reserve happens outside Frames.push. Retain the peer's
      // parser identity on its account and follow the exact reservation to release.
      emit({ event: 'accounting', operation: name, parser: accountParsers.get(account), pools: { ...this.used }, accounts: [...this.accounts].map(a => ({ ...a.used })),
        reservation: reservation ? { id: reservationIds.get(reservation), input: reservation.input, allowance: reservation.allowance, actual: reservation.actual, pool: reservation.pool, live: reservation.live } : null });
      return result;
    };
  }
}
const write = Socket.prototype.write;
Socket.prototype.write = function (...args) {
  if (this.remoteAddress === undefined && Buffer.isBuffer(args[0]) && args[0].length >= 4 && args[0].readUInt32BE() === args[0].length - 4) {
    if (!sockets.has(this)) {
      const id = ++socketId;
      this.once('close', () => { sockets.delete(this); emit({ event: 'socket-closed', id, pools: budget ? { ...budget.used } : null }); });
    }
    sockets.add(this);
    if (corkWrites && !this.writableCorked) {
      // A real corked Socket with a small high-water mark forces write(false)
      // deterministically; the actual callback runs only after uncork/destroy.
      this._writableState.highWaterMark = 1; this.cork();
    }
    const callback = args.at(-1);
    if (typeof callback === 'function') {
      const body = JSON.parse(args[0].subarray(4));
      args[args.length - 1] = error => {
        emit({ event: 'write-ready', id: body.id, bytes: args[0].length, error: error?.code ?? null });
        if (holdWrites) completions.push(() => callback(error)); else callback(error);
      };
    }
  }
  const result = write.apply(this, args);
  if (sockets.has(this)) emit({ event: 'write-queued', bytes: args[0].length, writableLength: this.writableLength, corked: this.writableCorked, accepted: result });
  return result;
};
process.on('message', message => {
  if (message === 'arm') armed = true;
  else if (message === 'release') { release?.(); release = undefined; }
  else if (message === 'hold-writes') holdWrites = true;
  else if (message === 'cork-writes') corkWrites = true;
  else if (message === 'uncork-writes') { corkWrites = false; for (const socket of sockets) socket.uncork(); }
  else if (message === 'finish-writes') { holdWrites = false; for (const finish of completions.splice(0)) finish(); }
  else if (message === 'fail-writes') { holdWrites = false; for (const socket of sockets) socket.destroy(); for (const finish of completions.splice(0)) finish(); }
  emit({ event: 'command', command: message, allocations, allocated, maxActive, pools: budget ? { ...budget.used } : null });
});
process.channel?.unref();
await foreground();
