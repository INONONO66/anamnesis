# Adjudication conformance screen (D46)

Two models judged the same 36 synthetic adjudication cases under the same
prompt and the same output schema. The question was narrow: does the model
apply the written rules of [02-daemon-and-pipelines](../02-daemon-and-pipelines.md)
§5 and §5.1, [03-time](../03-time.md) §5, and the D46 decision in
[10-decision-log](../10-decision-log.md)? Nothing here is a statistical
estimate, a production error rate, or a deployment certification.

The fixtures are synthetic specification probes written to exercise the
verdict boundaries. They are not human-labeled production data, so a score
on this set says how a model reads the spec, not how it would behave on real
episodes.

## Setup

- 36 cases, six per verdict (`NEW`, `DUPLICATE_OCCURRENCE`, `ELABORATION`,
  `CHANGE`, `CORRECTION`, `UNRESOLVED_CONTRADICTION`), twelve per language
  (English, Korean, Japanese).
- `gpt-5.5` on the Responses endpoint, reasoning effort `none`, streaming,
  `store: false`.
- `claude-opus-5` on the Messages endpoint, thinking disabled, streaming.
- Concurrency 2 per model, 120 s timeout, no retries. One case yields exactly
  one terminal row, whether it scored or failed.
- Both models ran concurrently to limit time-of-day ordering effects. This was
  not randomized or blinded.

The payload handed to the model strips every gold field and every
label-bearing identifier. Case, episode and claim IDs encode the answer twice
over, once in their stem (`new-`, `chg-`, `unr-`) and once in the numeric
block their ordinal falls in (`3xx` for change, `5xx` for unresolved), so
neither reaches the model. Candidate IDs are opaque hashes of the form
`c_d87e44dd7434`: the label stem and the numeric ordinal are both gone, and
nothing in the ID orders or groups the candidates. Candidate and snippet IDs
survive in that form because the model has to cite them back, and
`checkReferences` verifies every cited ID against the case's own ID sets.

Frozen inputs, verified before inference:

```text
fixtures sha256  5c3db78a88f524eabac1f29ed3c5535a2e15f5bf78e3ac9a99ce47a9060afc2d
prompt   sha256  94a74ca2825e17d09188ae0af97c915c7a79a64a45ac998e2f584d8878c3275b
```

Both hashes recompute from `scripts/research/adjudication-cases.json` and
`scripts/research/adjudication-prompt.md` in this checkout.

## Results

Every number below was recomputed from the captured `rows.jsonl` files by
importing the runner's exported `aggregate()` and re-reducing the rows. The
recomputation agrees with the aggregates written at run time.

| Measure | gpt-5.5 | claude-opus-5 |
|---|---:|---:|
| Attempted / scored | 36 / 36 | 36 / 36 |
| Core exact (verdict + target set + normative time basis) | 31 | 36 |
| Full exact (core + gold evidence set + time basis) | 31 | 34 |
| False invalidating verdict | 0 | 0 |
| Missed required invalidation | 3 | 0 |
| Target-set mismatches, total | 3 | 0 |
| Wrong target given a correct verdict | 0 | 0 |
| Wrong change/correction mode or normative basis | 0 | 0 |
| Unresolved case missed (no conflict written) | 1 | 0 |
| Unresolved case resolved to a winner | 0 | 0 |
| Failures of any kind | 0 | 0 |

Per label, as `n / verdict / target / evidence / core / full`:

```text
                          gpt-5.5              claude-opus-5
NEW                       6 / 5 / 5 / 6 / 5 / 5   6 / 6 / 6 / 6 / 6 / 6
DUPLICATE_OCCURRENCE      6 / 6 / 6 / 6 / 6 / 6   6 / 6 / 6 / 6 / 6 / 6
ELABORATION               6 / 6 / 6 / 6 / 6 / 6   6 / 6 / 6 / 6 / 6 / 6
CHANGE                    6 / 3 / 5 / 6 / 3 / 3   6 / 6 / 6 / 5 / 6 / 5
CORRECTION                6 / 6 / 6 / 6 / 6 / 6   6 / 6 / 6 / 6 / 6 / 6
UNRESOLVED_CONTRADICTION  6 / 5 / 5 / 6 / 5 / 5   6 / 6 / 6 / 5 / 6 / 5
```

Failure counters are zero across all six kinds for both models: no transport
error, no non-2xx status, no parse or schema violation, no unknown target ID
and no unknown evidence ID. Requested and resolved model IDs agree
(`gpt-5.5`, `claude-opus-5`).

### Latency and tokens

Nearest-rank percentiles over the 36 per-case wall-clock latencies, and token
totals summed over the run:

| Measure | gpt-5.5 | claude-opus-5 |
|---|---:|---:|
| p50 latency (ms) | 3307 | 3396 |
| p90 latency (ms) | 3906 | 3962 |
| p95 latency (ms) | 4034 | 4039 |
| Input tokens, provider cache fields included | 59328 | 98997 |
| Output tokens | 3312 | 4882 |

The Opus input figure is almost entirely cache accounting: 72 plain input
tokens, 79274 cache-creation tokens and 19651 cache-read tokens. GPT reports
59328 with no cache fields. Different tokenizers and different cache
semantics make these two columns non-comparable as cost. No price claim and
no throughput generalization follows from a 72-call sample.

## Where the answers diverged

### GPT missed three state transitions

All three GPT `CHANGE` misses are the same shape: a sentence describes a
world change, and the model files it as an addition instead.

- `chg-en-301`, "I moved to Busan last month" answered `NEW`.
- `chg-en-304`, Ana leaving the platform team and joining the search team,
  answered `ELABORATION` with the correct target.
- `chg-ja-306`, the production cluster relocating to the Osaka region,
  answered `ELABORATION` with the correct target.

The runner writes no graph at all; it scores verdicts against gold and stops.
Spoken hypothetically, in a pipeline that acted on these verdicts, none of
the three would create an `INVALIDATES` edge, so the prior fact would stay
valid alongside a contradicting one. That's the failure mode this screen
cares about most, and it is the reason `missed_invalidation` is reported
separately from raw verdict accuracy.

### One missed conflict, not a forced winner

On `unr-ko-505` GPT answered `NEW` where the gold label is
`UNRESOLVED_CONTRADICTION`. The case reports what a third party said about a
contract having ended, conflicting with an existing attributed report. GPT
treated it as a fresh fact rather than preserving the conflict.

The distinction matters, again hypothetically, since nothing here touches a
graph. A non-invalidating verdict on an unresolved case still creates a Fact
occurrence for the new claim; what it creates no record of is the conflict,
because no `INVALIDATES` edge and no contradiction bundle follows from it.
Electing a winner would go the other way and invalidate one side against D46.
Only the first happened: `unresolved_missed` is 1 and `unresolved_invalidated`
is 0 for GPT, both zero for Opus.

The aggregates captured at run time reported this under a single
`unresolved_resolved` counter, which conflated the two outcomes. That name
was corrected afterwards; the runner now exports
`unresolved_missed` and `unresolved_invalidated` separately, and splits
`wrong_target` into `wrong_target_given_verdict` and
`target_mismatch_total`. This changed output labels only. No prediction, no
gold label and no fixture was touched, and the recomputation above runs the
corrected `aggregate()` over the original frozen rows.

### NEW versus ELABORATION on new-en-004

GPT's fifth mismatch overall, and its only mismatch on a `NEW`-labeled case,
is `new-en-004`: "I'm thinking of moving the archive to object storage next
quarter." Gold says `NEW`, GPT said `ELABORATION`. The
boundary is genuinely arguable, since an intention about existing storage
does add information about a thing already known. The label stays as frozen
and the ambiguity is disclosed instead. Retuning gold against observed model
answers would make every count in this report unfalsifiable. The runner
carries the same caveat in its `INTERPRETATION_LIMITS` export, so it travels
with the aggregate rather than living only in prose.

### Opus cited extra relevant evidence

Opus got the core structure right on all 36 cases. Its two non-full rows are
both evidence-set supersets, not wrong verdicts:

- `chg-en-301`: correct `CHANGE` and correct target, evidence `["s1","s2"]`
  where gold is `["s1"]`. The extra snippet is "The new place has better
  light."
- `unr-en-501`: correct `UNRESOLVED_CONTRADICTION` and correct target,
  evidence `["s1","s2"]` where gold is `["s1"]`. The extra snippet is "I have
  not checked the audit log myself," which is squarely relevant to why the
  claim is uncertain.

`evidence_exact` is deliberately set equality against a minimal gold span,
which is stricter than asking whether the cited evidence is valid and
sufficient. So 34/36 full-exact is not two hallucinations; it's two answers
that cited more context than the minimal set. Gold was not changed here
either.

## API contract note

The Messages endpoint rejected the initial request with HTTP 400. Removing
only `reason.maxLength` still returned 400. The equivalent nullable encoding,

```json
{"anyOf": [{"type": "string", "enum": ["proposed", "target"]}, {"type": "null"}]}
```

returned 200, a clean `message_stop` with `stop_reason=end_turn`, and a valid
verdict, with every other field identical to the failed maxLength-free
request. That encoding is what both endpoints now use.

Two consequences are baked into the runner. The schema still accepts a real
JSON `null` for `effective_time_basis`; no string sentinel stands in for it.
And the 320-character bound on `reason` is stated in the prompt and enforced
client-side by `validateModelOutput` against `REASON_MAX_LENGTH`.

Be precise about why the bound lives client-side. This runner builds its
request body by hand and sends it with a raw `fetch`, so nothing in our
runtime rewrites the schema; whatever we put in the body is what goes on the
wire. We omit `maxLength` deliberately, because the endpoint returned 400
with it present. Vendor documentation describes SDK-side stripping of length
keywords, and that is cited here as documentation only, not as behavior we
observed or rely on in this code path. The local check is what actually
guarantees the bound either way.

This isolates one encoding against one captured request. It does not prove
that union types or null-bearing enums are unsupported in general.

## Reproducing

The captured `rows.jsonl` for both models replays with no API key and no
network access. Replay produced byte-identical rows, checked with `cmp`.

Offline replay from a capture directory:

```sh
bun scripts/research/adjudication-pilot.mjs \
  --fixtures scripts/research/adjudication-cases.json \
  --model gpt-5.5 \
  --replay <capture-dir> \
  --out-dir <out-dir>

bun scripts/research/adjudication-pilot.mjs \
  --fixtures scripts/research/adjudication-cases.json \
  --model claude-opus-5 \
  --replay <capture-dir> \
  --out-dir <out-dir>
```

Recompute the aggregate from an existing rows file, without rerunning
anything:

```js
import { readFileSync } from "node:fs";
import { aggregate } from "./scripts/research/adjudication-pilot.mjs";

const rows = readFileSync("rows.jsonl", "utf8")
  .trim()
  .split("\n")
  .map((line) => JSON.parse(line));

console.log(aggregate({ rows, expected: 36, model: "gpt-5.5" }));
```

Runner tests:

```sh
bun test scripts/research/adjudication-pilot.test.mjs
```

61 tests pass with 0 failures and 1352 assertions in this checkout. Two of
them assert that the corrected aggregate no longer emits the old
`unresolved_resolved` and `wrong_target` keys.

The live run needs `--key-file` pointing at a credential JSON with `bearer`
and `base_url`. Credentials are never persisted: `redactRequest` replaces
`authorization` and `x-api-key` before anything is written or logged, and
captures and outputs are created with mode 0600 in 0700 directories.

### Runner hashes

The preregistration recorded two runner hashes: `56b21b2d...c28615` before the
API preflight amendment, and `06db2928...178b05` for the runner that actually
executed the 36-case screen after the `anyOf` change. Both were verified,
and the second is the one transferred to the server alongside the
unchanged fixtures and prompt.

The metric-naming revision, before subsequent comment-only clarification,
hashed to `678728eb47a18cfbcec799b59a4751f47422b5bf5d81d1d8d3d8b4e2b63a45f7`, which
differs from `06db2928...178b05` because of the post-run metric-naming
correction described above. Fixture and prompt hashes are unchanged from the
frozen run, so the predictions in the rows remain exactly what the executing
runner produced.

The research scripts also pass the repository typecheck, a JavaScript syntax
check and a gitleaks scan.

## Limitations

- 36 synthetic cases, one prompt, one setting per model, one run each. No
  confidence interval belongs on any count here, and no threshold in this
  report gates a deployment.
- Fixtures probe specification boundaries by construction. They over-sample
  hard edges and under-sample ordinary traffic, so the counts don't transfer
  to production distributions.
- The result favors Opus on this prompt and this case set. That is not
  evidence of universal superiority, and it does not license automatic
  invalidation without review.
- Percentiles come from 36 samples over a shared network path with two
  concurrent runs. Treat them as sanity checks on responsiveness, not as
  latency characterization.
- Earlier circular repair results over a 119-case set address a different
  task and must not be pooled with these counts.
- Two gold labels are known to be arguable (`new-en-004` verdict,
  `unr-en-501` evidence span). Discount the affected counts accordingly
  rather than treating 36 as a clean ceiling.

## Finalization disposition (D50)

[D50](../10-decision-log.md) adopts `claude-opus-5` on Messages with thinking
disabled only as the development adjudication default for this frozen prompt;
`gpt-5.5` on Responses with reasoning effort `none` remains the comparator.
Runtime admission remains shadow/no-write and requires an authenticated review
of the exact proposal
([02-daemon-and-pipelines §5.2](../02-daemon-and-pipelines.md)). These
synthetic cases are not human gold, select no extractor or global model
winner, and do not authorize unattended writes.
