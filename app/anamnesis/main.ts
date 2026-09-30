import { logEvent } from "./log.ts";
import { foreground } from "./daemon.ts";
foreground().catch(error => { logEvent("error", "daemon_failed", { error: String(error) }); process.exitCode = 1; });
