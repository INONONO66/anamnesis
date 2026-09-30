/** One line per event on stdout (info) or stderr (error): { ts, level, event, ...fields }. Operators grep the event
 * name; ts is ISO-8601 so lines from the supervisor and the daemon interleave in order. */
export type LogLevel = "info" | "error";
export function logEvent(level: LogLevel, event: string, fields: Record<string, unknown> = {}): void {
  const line = JSON.stringify({ ts: new Date().toISOString(), level, event, ...fields });
  if (level === "error") console.error(line); else console.log(line);
}
