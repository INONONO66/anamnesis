// Private Node fixture: real daemon queue, RPC, filesystem and object store.
// Only the monotone clock and the exact owned upload FileHandle are hooked.
import { foreground } from './daemon.ts';
import { Runtime } from './runtime.ts';
const emit = value => process.send?.(value);
let now = 0, next = 0, uploads, active = 0, maxActive = 0, armed, release;
const timers = new Map();
const clock = {
  now: () => now,
  setTimeout(callback, delay) { const id = ++next; timers.set(id, { callback, at: now + delay }); return id; },
  clearTimeout(id) { timers.delete(id); },
};
function snapshot() {
  return { now, active, maxActive, timers: timers.size,
    survivingCount: uploads?.survivingCount, survivingBytes: uploads?.survivingBytes,
    uploads: uploads ? [...uploads.uploads].map(([id, u]) => ({ id, received: u.received, actual: u.actual, declared: u.expected.size,
      next: u.next, failed: u.failed, invalid: u.invalid, digest: u.hash.copy().digest('hex') })) : [] };
}
const init = Runtime.prototype.init;
Runtime.prototype.init = async function () {
  uploads = this.uploads; uploads.clock = clock;
  await init.call(this);
  // expire can also be called within begin; observe only the queued callback as
  // a mutation turn, not that nested call, when measuring serial concurrency.
  const enqueue = uploads.lifecycle.enqueue;
  uploads.lifecycle.enqueue = job => enqueue(async () => {
    active++; maxActive = Math.max(maxActive, active);
    try { await job(); }
    finally { active--; emit({ event: 'expiry-finished', ...snapshot() }); }
  });
  for (const method of ['begin', 'chunk', 'commit', 'disconnect', 'close']) {
    const original = uploads[method].bind(uploads);
    uploads[method] = async (...args) => {
      active++; maxActive = Math.max(maxActive, active);
      let restore;
      try {
        if (method === 'chunk' && armed) {
          const mode = armed; armed = undefined;
          const upload = uploads.uploads.get(args[1]), file = upload.file;
          const write = file.write, sync = file.sync;
          file.write = async function (bytes, offset, length, position) {
            if (mode === 'park') {
              const gate = new Promise(resolve => { release = resolve; });
              emit({ event: 'parked', id: args[1], ...snapshot() });
              await gate;
              return write.call(this, bytes, offset, length, position);
            }
            const written = await write.call(this, bytes, offset, Math.min(2, length), position);
            emit({ event: 'prefix-written', id: args[1], bytes: written.bytesWritten });
            if (mode === 'short') return written;
            if (mode === 'zero') return { bytesWritten: 0, buffer: bytes };
            throw Object.assign(Error('owned upload ENOSPC'), { code: 'ENOSPC' });
          };
          if (mode === 'rollback-failure') file.sync = async () => { throw Object.assign(Error('owned rollback fsync failure'), { code: 'EIO' }); };
          restore = () => { file.write = write; file.sync = sync; };
        }
        return await original(...args);
      } finally {
        restore?.(); active--;
        emit({ event: method + '-finished', ...snapshot() });
      }
    };
  }
};
process.on('message', message => {
  if (message.command === 'advance') {
    if (message.now < now) throw Error('clock moved backwards');
    now = message.now;
    for (const [id, timer] of timers) if (timer.at <= now) { timers.delete(id); timer.callback(); }
  } else if (message.command === 'arm') armed = message.mode;
  else if (message.command === 'release') { release?.(); release = undefined; }
  emit({ event: 'command', command: message.command, ...snapshot() });
});
process.channel?.unref();
await foreground();
