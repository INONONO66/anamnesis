import { once } from "node:events";
import { DurableSpool } from "../../packages/core/src/spool.ts";
import { foreground } from "../../app/anamnesis/daemon.ts";
import { Frames, decodeRequest } from "../../app/anamnesis/wire.ts";

// This import and foreground's Runtime import share one real bundled class.
// No Engine/write/DB result is replaced. The parent subscribes before `start`.
async function main(): Promise<void> {
  if (!process.send) throw new Error("fixture requires IPC");
  const commands = new (await import("node:events")).EventEmitter();
  process.on("message", message => {
    if (message && typeof message === "object" && "command" in message) {
      commands.emit(String(message.command));
    }
  });
  process.on("disconnect", () => process.exit(1)); // Never complete on parent loss.
  const started = once(commands, "start", { signal: AbortSignal.timeout(30_000) });
  process.send({ event: "fixture-ready" });
  await started;
  if (process.argv.includes("--drop-replay")) {
    // Deliberately broken replay, fixture-only. Real page validation still runs;
    // the real second entry is hidden from runtime, never marked complete.
    const page = DurableSpool.prototype.page;
    DurableSpool.prototype.page = async function (request) {
      const result = await page.call(this, request);
      const entries = result.entries.filter(entry => entry.sequence !== 2);
      if (entries.length !== result.entries.length) process.send!({ event: "mutation-dropped", sequence: 2 });
      return { ...result, entries };
    };
  } else {
    const complete = DurableSpool.prototype.complete;
    let parked = false;
    // Observe complete request frames only AFTER the real Frames.push has
    // synchronously handed them to the daemon's admission callback. The mirror
    // handles fragmentation without accessing/replacing that private callback.
    const push = Frames.prototype.push;
    const observers = new WeakMap<Frames, Frames>();
    Frames.prototype.push = function (bytes) {
      push.call(this, bytes);
      let observer = observers.get(this);
      if (!observer) {
        observer = new Frames(body => {
          const request = decodeRequest(body);
          if (parked && request.method === "remember") process.send!({ event: "request-received", request });
          return true;
        });
        observers.set(this, observer);
      }
      push.call(observer, bytes);
    };
    DurableSpool.prototype.complete = async function (sequence: number): Promise<void> {
      if (!parked) {
        parked = true;
        const released = once(commands, "release", { signal: AbortSignal.timeout(60_000) });
        process.send!({ event: "complete-parked", sequence });
        await released; // Only an explicit release can forward completion.
      }
      return complete.call(this, sequence);
    };
  }
  await foreground();
  process.channel?.unref(); // IPC must not keep graceful shutdown alive.
}
main().catch(error => { console.error(error); process.exitCode = 1; process.disconnect?.(); });
