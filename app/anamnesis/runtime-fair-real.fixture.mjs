// Surface-only hooks: every wrapped method delegates to the actual method.
// IPC parks one completion to admit control at an exact durable boundary.
import { foreground } from './daemon.ts';
import { Runtime } from './runtime.ts';
import { Frames } from './wire.ts';
let armed = false, release;
process.on('message', message => {
  if (message === 'arm') { armed = true; process.send?.({ event: 'armed' }); }
  if (message === 'release') release?.();
});
process.channel?.unref();
const init = Runtime.prototype.init;
Runtime.prototype.init = async function () {
  const complete = this.spool.complete.bind(this.spool);
  this.spool.complete = async sequence => {
    await complete(sequence);
    if (armed) {
      armed = false;
      const released = new Promise(resolve => { release = resolve; });
      process.send?.({ event: 'completion_parked', sequence });
      await released;
    }
  };
  await init.call(this);
};
const push = Frames.prototype.push;
Frames.prototype.push = function (bytes) {
  if (!this.observed) {
    this.observed = true; const accept = this.accept;
    this.accept = body => { const result = accept(body); process.send?.({ event: 'admitted', method: JSON.parse(body).method }); return result; };
  }
  push.call(this, bytes);
};
await foreground();
