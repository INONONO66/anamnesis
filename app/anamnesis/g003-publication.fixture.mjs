// Test-only gates delegate to real runtime, Store and Node sockets.
import { foreground } from './daemon.ts';
import { Runtime } from './runtime.ts';
import { Socket } from 'node:net';
import { RpcByteBudget } from './wire.ts';
const emit = value => process.send?.(value);
let runtime, cork = false, hold = false, partial = false, failAudit = false, deny, budget;
const sockets = new Set(), callbacks = [];
const init = Runtime.prototype.init;
Runtime.prototype.init = async function () { runtime = this; return init.call(this); };
const recall = Runtime.prototype.recall;
Runtime.prototype.recall = async function (...args) {
  const result = await recall.apply(this, args);
  const durable = await this.engine.getReceipt(result.recall_id);
  if (!durable) throw Error('receipt missing before attempted Socket.write');
  emit({ event: 'receipt-ready', result, durable }); return result;
};
if (Runtime.prototype.recordRecallTransport) {
  const audit = Runtime.prototype.recordRecallTransport;
  Runtime.prototype.recordRecallTransport = async function (...args) {
    if (failAudit) { failAudit = false; throw Error('injected transport audit failure'); }
    const result = await audit.apply(this, args); emit({ event: 'transport-recorded', input: args[0], result }); return result;
  };
}
if (Runtime.prototype.exposeRecall) {
  const expose = Runtime.prototype.exposeRecall;
  Runtime.prototype.exposeRecall = async function (...args) {
    if (deny) { const policy = deny; deny = undefined; await this.setPolicy(policy, args[1]); }
    const result = await expose.apply(this, args); emit({ event: 'exposure-recorded', recall_id: args[0], result }); return result;
  };
}
for (const name of ['reserve', 'output', 'release']) {
  const original = RpcByteBudget.prototype[name];
  RpcByteBudget.prototype[name] = function (...args) { budget = this; return original.apply(this, args); };
}
const write = Socket.prototype.write;
Socket.prototype.write = function (...args) {
  const bytes = args[0];
  if (!Buffer.isBuffer(bytes) || bytes.length < 4 || bytes.readUInt32BE() !== bytes.length - 4) return write.apply(this, args);
  const body = JSON.parse(bytes.subarray(4));
  if (body.method !== 'recall' || !body.result?.recall_id) return write.apply(this, args);
  sockets.add(this);
  this.once('close', () => { sockets.delete(this); emit({ event: 'publication-closed', recall_id: body.result.recall_id }); });
  if (cork) { this._writableState.highWaterMark = 1; this.cork(); }
  const callback = args.at(-1);
  args[args.length - 1] = error => {
    emit({ event: 'write-callback', recall_id: body.result.recall_id, error: error?.code ?? null });
    if (hold) callbacks.push(() => callback(error)); else callback(error);
  };
  if (partial) {
    partial = false;
    const complete = args.at(-1);
    return write.call(this, bytes.subarray(0,16), error => {
      this.destroy(); complete(error ?? Object.assign(Error('injected partial frame failure'), { code: 'EPIPE' }));
      emit({ event: 'partial-output', recall_id: body.result.recall_id, bytes: 16 });
    });
  }
  const accepted = write.apply(this, args);
  emit({ event: 'publication-queued', recall_id: body.result.recall_id, accepted, bytes: bytes.length, writableLength: this.writableLength, corked: this.writableCorked });
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
    if (message.name === 'fail-audit') failAudit = true;
    if (message.name === 'deny-exposure') deny = message.policy;
    if (message.name === 'expose') await runtime.exposeRecall(message.recall_id, message.context);
    if (message.name === 'duplicate') {
      await runtime.recordRecallTransport(message.input, message.context);
      await runtime.exposeRecall(message.input.recall_id, message.context);
    }
    emit({ event: 'command', name: message.name, pools: budget ? { ...budget.used } : null });
  } catch (error) { emit({ event: 'command', name: message.name, error: String(error) }); }
});
process.channel?.unref();
await foreground();
