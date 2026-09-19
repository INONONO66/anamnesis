import { foreground } from "./daemon.ts";
foreground().catch(error => { console.error(String(error)); process.exitCode = 1; });
