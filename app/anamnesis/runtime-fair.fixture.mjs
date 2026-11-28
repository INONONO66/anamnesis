// Fixture-only storage probe and bounded embedding turns; socket admission and scheduling stay real.
import { foreground } from './daemon.ts';
import { Runtime } from './runtime.ts';
import { Frames } from './wire.ts';

const proto = Runtime.prototype;
const init = proto['init'];
const status = proto['status'];
let online = false, handled = 0, park = process.env.FAIR_PARK === '1', release;
process.on('message', message => {
  if (message === 'recover') { online = true; process.send?.({ event: 'recovered' }); }
  if (message === 'release') release?.();
});
process.channel?.unref();
proto['init'] = async function () {
  this.read = async (query) => {
    if (!online) throw Object.assign(new Error('fixture offline'), { code: 'ServiceUnavailable' });
    if (query === 'RETURN 1 AS connected') return [{ connected: 1 }];
    if (query.includes('writer_epoch')) return [{ epoch: 1 }];
    throw new Error(`unexpected fixture DB query: ${query}`);
  };
  this.engine.init = async () => {};
  this.engine.claimWriterEpoch = async () => 1;
  this.engine.status = async () => ({ pendingOutbox: 0 });
  this.capabilities.embeddings = true;
  await init.call(this);
};
proto.embeddingTurn = async function () {
  if (park) {
    park = false;
    const released = new Promise(resolve => { release = resolve; });
    process.send?.({ event: 'parked' });
    await released;
  }
  handled++;
  if (handled < 12) return 'more';
  process.send?.({ event: 'fixture-settled' });
  return 'idle';
};
proto['status'] = async function (...args) {
  const value = await status.apply(this, args);
  process.send?.({ event: 'control-return', handled });
  return value;
};
const push = Frames.prototype.push;
Frames.prototype.push = function (bytes) {
  if (!this.observed) {
    this.observed = true; const accept = this.accept;
    this.accept = body => { const value = accept(body); process.send?.({ event: 'admitted', id: JSON.parse(body).id }); return value; };
  }
  push.call(this, bytes);
};
await foreground();
