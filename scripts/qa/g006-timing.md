# G006 timing evidence

The continuous driver enables diagnostic-only traces for its owned daemon.
No RPC deadline, retry/resend rule, receipt, or qualification is changed.

- `continuous-timing.jsonl`: driver call sequence/traffic sequence; client RPC
  method/id, request, local write callback, parsed response, deadline and failure.
- `continuous-daemon-timing.jsonl`: connection, request admission, serial dispatch,
  runtime Neo4j read/session-close and engine-operation spans, reply write callback,
  drain turns, socket events and daemon lifecycle.
- `continuous-endpoint-timing.jsonl`: owned Docker/Neo4j process, readiness,
  connectivity and output-observation timestamps from the outer runner.

Each trace retains two segments of at most 8 MiB each: read `.jsonl.1` before
`.jsonl`. Events are not sampled; older segments are overwritten. `eventSequence`
exposes retained sequence ranges. Existing lifecycle receipts and raw logs remain
unchanged apart from added timing/sequence fields on continuous lifecycle events.
New traces whitelist metadata; string RPC ids, query text, errors and observed
output are SHA-256 hashes, never request/response bodies or credentials.

Use method/id to join client and daemon events (daemon connection disambiguates
ids). Driver calls are sequential. `monotonicMs` and `elapsedMs` are process-local;
`at` is wall time for cross-process correlation, not duration measurement. Endpoint
`output_observed` timestamps are runner observation times, not server execution
times; the existing Docker log retains Neo4j's own timestamps.

The first unmatched boundary narrows the stall: client request to daemon admission,
admission to dispatch, runtime read/engine operation, or daemon local completion
to client response. A local write callback does not prove peer consumption.
`transport_terminal` also occurs on normal close; per-request `failed` records
identify outstanding UNKNOWN deliveries. Deadline elapsed time exposes timer
overshoot without extending the 120-second RPC deadline.

These traces localize the last observed layer, not an unobserved root cause.
Engine spans include their internal DB work; a DB call without a completion alone
cannot distinguish server delay from Bolt transport loss. Host suspension can
pause multiple observers, and rotation can remove older boundaries. Keep UNKNOWN
when the retained evidence cannot distinguish these cases; never resend a write
based on timing evidence.
