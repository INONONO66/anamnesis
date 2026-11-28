// Test-only gates delegate to real runtime, Store and Node sockets.
import { foreground } from './daemon.ts';
import { Socket } from 'node:net';
import { RpcByteBudget } from './wire.ts';
const emit = value => process.send?.(value);
let cork = false, hold = false, partial = false, budget;
const sockets = new Set(), callbacks = [];
for (const name of ['reserve', 'output', 'release']) {
  const original = RpcByteBudget.prototype[name];
  RpcByteBudget.prototype[name] = function (...args) { budget = this; return original.apply(this, args); };
}
const write = Socket.prototype.write;
Socket.prototype.write = function (...args) {
  const bytes = args[0];
  if (!Buffer.isBuffer(bytes) || bytes.length < 4 || bytes.readUInt32BE() !== bytes.length - 4) return write.apply(this, args);
  const body = JSON.parse(bytes.subarray(4));
  if (body.method !== 'recall' || !body.result?.usage_id) return write.apply(this, args);
  sockets.add(this);
  this.once('close', () => { sockets.delete(this); emit({ event: 'publication-closed', usage_id: body.result.usage_id }); });
  if (cork) { this._writableState.highWaterMark = 1; this.cork(); }
  const callback = args.at(-1);
  args[args.length - 1] = error => {
    emit({ event: 'write-callback', usage_id: body.result.usage_id, error: error?.code ?? null });
    if (hold) callbacks.push(() => callback(error)); else callback(error);
  };
  if (partial) {
    partial = false;
    const complete = args.at(-1);
    return write.call(this, bytes.subarray(0,16), error => {
      this.destroy(); complete(error ?? Object.assign(Error('injected partial frame failure'), { code: 'EPIPE' }));
      emit({ event: 'partial-output', usage_id: body.result.usage_id, bytes: 16 });
    });
  }
  const accepted = write.apply(this, args);
  emit({ event: 'publication-queued', usage_id: body.result.usage_id, accepted, bytes: bytes.length, writableLength: this.writableLength, corked: this.writableCorked });
  return accepted;
};
process.on('message', async message => {
  try {
    if (message.name === 'cork') cork = true;
    if (message.name === 'uncork') { cork = false; for (const socket of sockets) socket.uncork(); }
    if (message.name === 'disconnect') for (const socket of sockets) socket.destroy();
    if (message.name === 'hold') hold = true;
    if (message.name === 'partial') partial = true;
    if (message.name === 'finish') { hold = false; for (const callback of callbacks.splice(0)) callback(); }
    emit({ event: 'command', name: message.name, pools: budget ? { ...budget.used } : null });
  } catch (error) { emit({ event: 'command', name: message.name, error: String(error) }); }
});
process.channel?.unref();
await foreground();
