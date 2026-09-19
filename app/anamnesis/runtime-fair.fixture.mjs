// Fixture-only DB availability probe and invalid-envelope cohort. No Engine
// write or spool completion is mocked; malformed entries must never reach them.
import { foreground } from './daemon.ts';
import { Runtime } from './runtime.ts';
import { Frames } from './wire.ts';

const proto = Runtime.prototype;
const init = proto['init'];
const validate = proto['validated'];
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
  await init.call(this);
  const page = this.spool.page.bind(this.spool);
  this.spool.page = async (...args) => {
    const result = await page(...args);
    if (online && park) {
      park = false;
      const released = new Promise(resolve => { release = resolve; });
      process.send?.({ event: 'parked' });
      await released;
    }
    return result;
  };
  for (let i = 0; i < 12; i++) await this.spool.append({ origin: `invalid-${i}`, revision: `invalid-${i}`, predecessor: null, body: {}, incarnation: this.installation.incarnation });
};
proto['validated'] = function (...args) { handled++; return validate.apply(this, args); };
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
