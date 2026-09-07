# Final memory design: decided defaults and their evidence

This report gathers what the memory-design research thread settled before
review. It states what this PR decides, what evidence each decision rests on,
which earlier claims were refuted, and what remains unqualified. No memory
engine ships here: extraction, adjudication, and embedding remain target
design, the PR carries documents plus offline evaluation scripts, and nothing
below certifies production behavior. Review may still change the text, so
treat this as the current state of the thread rather than its final word.

The normative text is `docs/00-10`, above all
[`docs/10-decision-log.md`](../10-decision-log.md) D48-D51 and the storage,
pipeline, and recall sections those entries name. Where this report and the
normative documents disagree, the documents win; where this report and a cited
experiment disagree, the narrower experimental statement wins.

## Scope and audience

Read this if you need to know which defaults are now settled and how far the
supporting evidence actually reaches. Four research reports carry the
measurements: [adjudication conformance](adjudication-conformance.md),
[retrieval fusion](retrieval-fusion.md), the
[extraction spotcheck](extraction-spotcheck.md), and the
[embedding runtime probe](embedding-runtime.md).

Every measurement this thread produced is synthetic or development-scale, and
none of it is a deployment metric. Two kinds of number appear below and must
not be pooled. New measurements are the four probes run for this design; they
ran against development endpoints and archived development inputs. Secondary
historical observations are operational counts a predecessor system reported
in its own private write-up, read at commit `dd37c53`: the 54% invalidated
state and the 6.7% versus 0.6% invalidation arms cited in the evidence table,
plus the repair and benchmark figures the refuted-claims section lists so it
can rule them out. None of those was replayed or independently labeled here,
and none is a current anamnesis deployment or quality metric. The two in the
evidence table motivate conservatism; they qualify nothing.

One scope note, so no reader over-reads the caveats further down. Producing
this research did involve live model calls to the two adjudication and
extraction endpoints on an existing development server, private archived
vectors derived from source material, and delegated agent work. What did not
happen is any production graph write and any deployment test. No new
experiment read or wrote a production system, and no production corpus or
deployment supplied a new number in this report.

## Final defaults

Four decisions, D48 through D51, are recorded in
[`docs/10-decision-log.md`](../10-decision-log.md). They preserve D42-D47 and
close only the gaps that blocked implementation.

| # | Default | Reversibility |
|---|---|---|
| D48 | Facts carry source-language content by default. English content is a whole extraction generation, not a rewrite. Recall keeps the caller's verbatim query and global original-Episode candidates. | Configuration and replacement extraction generation |
| D49 | Echo lineage is bounded and materialized: at most four parent receipts, 16 corroboration roots, depth 8. Every occurrence survives. Grouping and conflict predicates are pairwise and local. Episode digest identity gains a server-owned version discriminator that applies only to new revisions. | Per-generation fields, no global identity claim |
| D50 | Adjudication runs shadow-first. `claude-opus-5` with thinking disabled is the development default for the frozen conformance prompt; `gpt-5.5` at reasoning `none` is the comparator. Operator repair is append-only. | New proposal or new profile, no deletion |
| D51 | Separate `embedding_model_id`, `vector_index_id`, and `embedding_profile_id` fingerprints pin one profile. A permanent failure blocks contiguous coverage until an authenticated retry, skip, or cancel. | New model/profile build, old profile stays a rollback target |

Concretely, the settled positions are these.

**Language.** `content_language` is required, immutable, and identity-bearing;
`mul` covers materially multilingual prose and `und` covers nonlinguistic
input. Quotes, spans, names, paths, URLs, identifiers, and code stay
source-exact. Model-generated translations never enter `Entity.normalized_name`
or aliases. English projections from the spotcheck are nonauthoritative
development artifacts and are neither persisted nor indexed until a later
decision pins their schema and digest
([10-decision-log D48](../10-decision-log.md), [01-storage §1](../01-storage.md)).

**Evidence admission.** A present `evidence_quote` must be a byte-for-byte
contiguous substring of the immutable Episode content, 1..8,192 UTF-8 bytes;
the worker derives the canonical `[start,end)` span from that unique
occurrence. Nonliteral quotes reject as `evidence_mismatch`, never repaired.
Absent evidence is legal only through an explicit
`evidence_kind = no_single_locus`, and automatic writes start with that
permission set to false ([02-daemon-and-pipelines §5](../02-daemon-and-pipelines.md)).

**Echo lineage, and what it does to Episode digests.** Each new Episode carries
an adapter-supplied `origin_role`, and an assistant turn that received recall
context must name 1..4 parent receipts. Lineage lands as a retained
`EchoLineage` row in the Episode transaction, so replay never depends on an
expiring receipt. Because the canonical digest body now includes `origin_role`
and `lineage_digest`, digest identity is versioned rather than redefined. The
server owns the version: an Episode stored without the discriminator is
permanently version 1 and keeps its frozen pre-D49 body and serializer, while
every revision accepted after the change stores `episode_digest_version: 2`
and the RFC-8785 body. A stored row's version wins over whatever a retry or a
replayed journal record proposes, so an old row verifies as it always did, is
never reserialized, and never gains lineage in place. That rule is what keeps
CREATE-only originals and deterministic retry intact without a migration. The
PR defines the discriminator; it ships neither compatibility code nor any data
migration ([10-decision-log D49](../10-decision-log.md),
[01-storage §1](../01-storage.md),
[02-daemon-and-pipelines §3](../02-daemon-and-pipelines.md)).

**Adjudication.** Every call appends an `AdjudicationAttempt` row, so transport
and parse failures never vanish from a denominator. A valid output creates an
immutable `AdjudicationProposal` in `SHADOW` state. Only an authenticated
`adjudication.review` moves it to `ACCEPTED` or `REJECTED`, and only an
`ACCEPTED` proposal consumed once by its named generation can write.
Fact-to-Fact `INVALIDATES` is never automatic
([10-decision-log D50](../10-decision-log.md),
[02-daemon-and-pipelines §5.2](../02-daemon-and-pipelines.md)).

**Operator correction.** Corrections append; they never delete. A repair that
restores a wrongly invalidated Fact A appends a CREATE-only
`anamnesis.operator-adjudication/1` Episode, then a replacement A-prime in A's
generation that copies A's meaning, time, bounded authorities, confidence, and
roots without any boost. The bad edge stays. Recall labels the result
`operator_corrected` and must not render it as user speech
([03-time §5](../03-time.md), [05-recall §6](../05-recall.md)).

**Embedding.** One immutable profile identity, a per-entry state machine with
`BLOCKED` as a first-class state, three retry attempts per cycle with fixed
`[1000, 10000]` ms delays, and authenticated `embedding.retry` / `skip` /
`cancel`. No silent skip, no truncation, no synthesized zero vector. `ONLINE`
means queryable, not approved
([10-decision-log D51](../10-decision-log.md), [01-storage §4](../01-storage.md)).

The pinned baseline artifact:

```text
repository: Qwen/Qwen3-Embedding-0.6B-GGUF
revision:   370f27d7550e0def9b39c1f16d3fbaa13aa67728
file:       Qwen3-Embedding-0.6B-Q8_0.gguf
sha256:     06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439
size:       639,150,592 bytes
license:    Apache-2.0
```

That revision, checksum, and size were resolved directly against the official
Hugging Face API at
<https://huggingface.co/api/models/Qwen/Qwen3-Embedding-0.6B-GGUF/revision/370f27d7550e0def9b39c1f16d3fbaa13aa67728?blobs=true>
and match both the pilot record and the [runtime probe](embedding-runtime.md).
The f16 file in the same revision is
`421a27e58d165478cc7acb984a688c2aa41404968b0203e7cd743ece44c54340`; Q8_0/FP16
retrieval parity is unmeasured, so FP16 necessarily gets a different model and
profile ID. The derived fingerprints are
`embedding_model_id 711660f8...96e13e9`,
`vector_index_id 0dfe3d3a...bef81a9`, and
`embedding_profile_id 16d404a7...795405b`, each SHA-256 over the canonical
RFC-8785 object whose fields are enumerated in
[01-storage §4, the embedding stream](../01-storage.md).

## Evidence table

| Claim that carries weight | Evidence | What the evidence supports |
|---|---|---|
| Original query plus original Episodes is at least as good as translated fusion on the development set | [retrieval-fusion.md Results](retrieval-fusion.md), commit `4bbbd7e` | Hit@1 20/20 original-query union versus 19/20 two-query RRF, MRR 1.000 versus 0.975, on 20 queries |
| English-only retrieval is not justified | [retrieval-fusion.md](retrieval-fusion.md) | English Facts with English queries reached 19/20 Hit@1, MRR 0.967; no arm beat the original-query baseline |
| Opus is the development adjudication default on the frozen prompt | [adjudication-conformance.md Results](adjudication-conformance.md) | 36/36 core exact versus GPT 31/36; 0 false-invalidating verdicts for both; 0 failures of any kind |
| Missed invalidations are the failure mode worth separating | [adjudication-conformance.md, Where the answers diverged](adjudication-conformance.md) | GPT missed 3 required invalidations and 1 unresolved conflict; `unresolved_invalidated` is 0 for both models |
| Exact-quote validation catches real extractor corruption | [extraction-spotcheck.md](extraction-spotcheck.md) | Opus collapsed distinct Latin/Cyrillic identifiers in 1 of 11 claims and changed its quote; GPT emitted 12 claims with no literal-quote error |
| Confidence cannot gate admission | [extraction-spotcheck.md](extraction-spotcheck.md) | Opus assigned 0.95 to its corrupted claim; GPT assigned 1.0 to everything |
| Generated name renderings must not become aliases | [extraction-spotcheck.md](extraction-spotcheck.md) | English projections invented `Jihyun` and `(Sato)` |
| One oversized request was rejected, not truncated, on the pinned server | [embedding-runtime.md](embedding-runtime.md) | 5,002-token request returned HTTP 400 `exceed_context_size_error`, no vector |
| Embedding output shape is as pinned | [embedding-runtime.md](embedding-runtime.md) | 1024 dimensions, all values finite, `/health` 200 |
| CPU latency is seconds at kilotoken scale | [embedding-runtime.md](embedding-runtime.md) | Single samples: 269 ms at 129 tokens, 789 ms at 513, 3,553 ms at 2,049 |
| The model pin is reproducible | Hugging Face revision API, above | Repository, revision, Q8_0 SHA-256, size, license all resolve |
| A false-invalidation failure mode really existed in the predecessor | Secondary historical observation: predecessor incident report, repair job, and oracle-50 A/B, read at commit `dd37c53` of the private predecessor repository (not publicly resolvable) | The predecessor reported a 54% invalidated state, a four-instrument case with 3 erroneous superseded markers, and oracle-50 arms at 6.7% versus 0.6% invalidation. Reported operational counts from another system, not measured here and not an anamnesis metric |

External literature is cited for framing only. None of it scores anamnesis.

| Source | Pinned URL | What it supports here |
|---|---|---|
| ECon (EMNLP 2024) | <https://aclanthology.org/2024.emnlp-main.447/> | Conflict *detection* and conflict *resolution* are separable tasks, which is why D50 keeps unresolved conflicts as CONTRASTS instead of electing a winner |
| XOR QA (NAACL 2021) | <https://aclanthology.org/2021.naacl-main.46/> | Cross-lingual open-retrieval QA is a distinct setting from monolingual retrieval, so an English-only Fact corpus is a real restriction, not a normalization |
| MIRACL (TACL 2023) | <https://aclanthology.org/2023.tacl-1.63/> | Same-language monolingual retrieval across many languages is its own evaluation regime; the D48 source-language default sits in it |
| AVeriTeC (arXiv 2305.13117) | <https://arxiv.org/abs/2305.13117> | Evidence-sufficiency and evidence-backed verification are established evaluation concerns, relevant to a future fidelity gate |
| LongMemEval (arXiv 2410.10813) | <https://arxiv.org/abs/2410.10813> | Long-term memory retrieval benchmarking exists as a separate line; the predecessor's LongMemEval-S figure belongs to it, not to anamnesis |

ECon, XOR QA, MIRACL, and AVeriTeC were fetched directly with HTTP 200 at the
URLs above while this design was being reconciled. These citations are
qualitative. No numeric anchor from any of them is used,
because none was verified for this design.

## Measured results

### Adjudication conformance, 36 synthetic cases

Both models judged the same 36 cases, six per verdict, twelve per language,
under prompt SHA-256
`94a74ca2825e17d09188ae0af97c915c7a79a64a45ac998e2f584d8878c3275b` and
fixtures SHA-256
`5c3db78a88f524eabac1f29ed3c5535a2e15f5bf78e3ac9a99ce47a9060afc2d`
([adjudication-conformance.md Setup](adjudication-conformance.md)).

| Measure | gpt-5.5 (`none`) | claude-opus-5 (thinking disabled) |
|---|---:|---:|
| Attempted / scored | 36 / 36 | 36 / 36 |
| Core exact | 31 | 36 |
| Full exact | 31 | 34 |
| False invalidating verdict | 0 | 0 |
| Missed required invalidation | 3 | 0 |
| Unresolved case missed | 1 | 0 |
| Unresolved case resolved to a winner | 0 | 0 |
| Failures of any kind | 0 | 0 |

Opus's two non-full rows are evidence-set supersets, not wrong verdicts: on
`chg-en-301` and `unr-en-501` it cited a second relevant snippet where gold
lists one. GPT's three `CHANGE` misses share one shape, a world change filed
as an addition. Latency p50 was 3,307 ms for GPT and 3,396 ms for Opus over 36
samples on a shared network path.

Caveats that travel with these counts: the fixtures are synthetic
specification probes that over-sample hard boundaries; two gold labels
(`new-en-004` verdict, `unr-en-501` evidence span) are disclosed as arguable;
one run, one setting per model, no interval belongs on any count. Captured
rows replay byte-identically with no key and no network, and 61 runner tests
pass with 1,352 assertions in this checkout
([adjudication-conformance.md Reproducing, Limitations](adjudication-conformance.md)).

### Offline retrieval fusion, 20 queries

The fusion probe reuses archived Qwen3-Embedding-0.6B Q8_0 vectors from the
eight-Episode, twenty-query development pilot and makes no model calls. Equal
weights and RRF `k=60` were fixed before execution
([retrieval-fusion.md](retrieval-fusion.md)).

| Candidate corpus | Query | Hit@1 | Hit@3 | MRR |
|---|---|---:|---:|---:|
| Original Episodes | Original | 20/20 | 20/20 | 1.000 |
| English Facts | English | 19/20 | 20/20 | 0.967 |
| Original + English Facts | Original | 20/20 | 20/20 | 1.000 |
| Original + English Facts | English | 19/20 | 20/20 | 0.967 |
| Original + English Facts | Two-query RRF | 19/20 | 20/20 | 0.975 |

Only q20, asking for four continuously collected agent sources, missed top-1:
third with the English query, second with fusion, first with the original
query. Adding translation and equal-weight fusion did not beat the
original-query baseline here.

This is a source-informed development set, not held-out data. Seven
substantive target Episodes, human translations, no realistic distractor
population, saturated Hit@3, one embedding model. Extraction variability means
corpus-language differences are not a translation-only effect. The result
supports a reversible default; it establishes no statistical superiority
([retrieval-fusion.md Interpretation](retrieval-fusion.md)).

### Extraction spotcheck, six synthetic Episodes per model

Twelve requests, all HTTP 200 and parseable, every confidence finite in
`[0,1]`. GPT returned 12 claims with no literal-quote error; Opus returned 11
with one. Both preserved intention versus completion, self-correction, and
conflicting attributed reports, and both emitted zero claims for
metadata-only input. Opus collapsed distinct Latin/Cyrillic collector
identifiers in one claim and changed the accompanying quote
([extraction-spotcheck.md](extraction-spotcheck.md)).

These are case counts on six deliberately selected cases. They are not error
rates, not a model ranking, and not writer admission. The adjudication screen
favoring Opus and this probe exposing an Opus extraction fidelity failure are
exactly why extraction and adjudication are selected separately
([extraction-spotcheck.md Decision relevance](extraction-spotcheck.md)).

### Embedding runtime probe

On one CPU-only development host running llama.cpp `0.4.0-dev` build 10819,
commit `6a1a922d269908a29cbd4b49c27e6a8e7fd10fae`, four threads, context
4,096, last-token pooling: a short input returned a 1024-dimensional
all-finite vector; a synthetic 5,002-token input returned HTTP 400
`exceed_context_size_error` with no vector. Single wall-time samples were
269 ms at 129 tokens, 789 ms at 513, and 3,553 ms at 2,049. The probe server
was terminated and its runtime files removed
([embedding-runtime.md](embedding-runtime.md)).

One rejected request on one configuration. Boundary behavior near 4,096,
batch and array requests, client-side truncation, worker behavior, sustained
throughput, thread scaling, and contention are all unmeasured, and an earlier
6-113 hour corpus projection that had been extrapolated from single samples is
withdrawn ([embedding-runtime.md](embedding-runtime.md)).

## Dissent and refuted claims

Review of the earlier drafts rejected or narrowed the claims below. These stay
refuted; the design must not be justified by any of them.

**The 95.3% figure is not precision.** `97,163 / 101,914` restored edges is the
fraction the same GPT-5.5 repair process chose to restore, not an
independently measured precision. The primary calls those cases
"spot-checked" and supplies no sample, labels, agreement, or error bound. Do
not write 95.3% precision anywhere.

**The 119-case set is pseudo-labeled and circular.** `sample_cases()` labels
every GPT-5.5 repair `restore` as not superseded, and labels a `keep` as
superseded when a regex matches the same model's own free-text reason. No SHA,
URL, run, or state transition is independently inspected. The tested
`gpt-5.5:none` arm is therefore scored against labels it produced itself, so
both classes carry self-agreement, not accuracy. The source log and per-case
manifest are absent from the repository; the checked-in result is
aggregate-only. Balanced accuracy 0.875, "reasoning hurts accuracy", and
"other models miss real supersessions" are not citable as adjudication
accuracy against gold.

**The 119-case denominator leaks.** `gpt-5.6-terra:low` has `n=112` where other
rows have 119, because request exceptions return without an attempted or error
row and `parse_fail` is computed from completed rows only. Seven failures are
silently missing from that comparison. That's the reason D50 mandates one
terminal `AdjudicationAttempt` row per call
([10-decision-log D50](../10-decision-log.md)).

**Three predecessor signals are not independent.** The incident report, the
repair job, and the 119-case benchmark reuse the same graph, the same GPT-5.5
repair decisions, and the same prompt or readjudication log. They form one
evidence chain and cannot elect a production judge.

**Oracle-50 does not measure a prompt effect.** It defines a model-routing
change on the same `resolve_extracted_edge` path, and ADR-114 records that the
empty small-model setting hit every `ModelSize.small` call site. The +28% Fact
yield and the invalidation-rate delta are not resolver-only causal estimates.

**The model pin was never broken.** The claim that the GGUF revision was
dangling checked the base Transformers repository and an unrelated GitHub
repository. The exact revision and Q8_0 checksum resolve in the official
GGUF repository. Only the repository slug was missing from the pilot text.

**Cutover does not gate on captured watermarks alone.** Captured watermarks
delimit backlog enqueue; post-capture writes enter the dual tail. At
activation, under the write-queue barrier, coverage must reach the *current*
`Meta.ingest_seq` and each active generation's *current* cursor
([01-storage §4](../01-storage.md)).

**"No validator mechanically detects the Opus quote error" was wrong.** The
pilot runner did check exact quote substrings and counted the mismatch. The
true, narrower statement is that a boundary-valid span proves literal
provenance only, never that claim text or every material detail is faithful.

**"The design has no original-language path" was wrong.** `docs/05-recall.md`
§2 already puts global original Episodes and active-generation Facts into both
vector and BM25 candidate sources, and original Episodes are immutable. What
was genuinely missing is an explicit translated-query policy, which D48 leaves
unadmitted ([05-recall §2](../05-recall.md)).

**Predecessor benchmark numbers belong to another line.** LoCoMo 0.776 and
LongMemEval-S 0.938 are retrieval Recall@20 figures from the separate Bayesian
ACT-R `origin/dev` line, not anamnesis2 answer accuracy, and commit `0ae1fe5`
is not on `origin/anamnesis2`.

**AVeriTeC is not unknown, and it is not ours.** Its official arXiv and NeurIPS
pages agree on identity and dataset size. It is legitimate evidence-sufficiency
literature and it certifies nothing about anamnesis spans, conflicts, or
writes.

One more prohibition, stated plainly because it is easy to slip: 36/36 is not a
production error bound, 12/0 and 11/1 are not model error rates, generated name
renderings are not verified aliases, Q8_0 is not FP16-equivalent, `ONLINE` is
not quality approval, and no target or estimate in the design is a deployed
metric.

## Limitations

What is decided is the contract text. What is not qualified is nearly every
quantitative property of the unshipped pipelines.

- **No human gold anywhere.** No independent annotators were obtained. The 36
  cases are specification conformance; the 119 cases are pseudo-label
  agreement. They must not be pooled, and neither yields a production error
  bound.
- **No production graph writes, no deployment test.** Nothing in this work
  wrote to a production graph or exercised a deployment, and no production
  corpus supplied a new number. The new measurements did come from live model
  calls to an existing development server, from private archived vectors
  derived from source material that stays outside git, and from delegated
  agent work. Every new number is synthetic or development-scale. The only
  exceptions to "development-scale" are the predecessor's reported operational
  counts in the evidence table, which are secondary historical observations
  from a private report, not measurements taken here and not deployment or
  quality metrics for anamnesis.
- **Failure denominators are only now enforced.** The predecessor comparison
  lost seven rows to silent exception paths. The attempt-row rule in D50 fixes
  that going forward; it does not retroactively repair the old comparison.
- **Retrieval evidence is tiny and saturated.** Twenty queries, seven
  substantive targets, human translations, no realistic distractors, Hit@3
  saturated at 20/20 in every arm. Episode rank was measured, not answer or
  detail correctness ([retrieval-fusion.md](retrieval-fusion.md)).
- **Extraction fidelity is unverified.** Exact-substring checks prove literal
  provenance only. Some passing quotes omit the subject or context the claim
  depends on, so a valid quote does not establish that every claim detail is
  supported ([extraction-spotcheck.md](extraction-spotcheck.md)).
- **Embedding behavior is one host, one configuration.** Boundary cases at
  4,096 tokens, batching, throughput, thread scaling, contention, corpus
  duration, index quality, and Q8_0/FP16 parity are all unmeasured.
- **The pipelines do not exist.** Extraction, policy propagation, receipts,
  utility, vector search, PPR, and conflict bundles are unshipped; current code
  ships originals plus fulltext recall
  ([09-roadmap](../09-roadmap.md)). The digest version discriminator is in the
  same state: the contract is written, the compatibility path is a future
  milestone, and no migration is planned or needed.

Five qualification packages, M1 through M5, are defined for the day a
capability is actually pursued: multilingual retrieval and detail coverage,
quote and source fidelity, independent adjudication gold with causal ablation,
CPU context and throughput, and Q8_0/FP16 plus index cutover. Their triggers
and acceptance shape live in
[07-gds-validation §7](../07-gds-validation.md) and
[09-roadmap](../09-roadmap.md). They are trigger-based, not queued work, and
none is a prerequisite for this PR. Until the relevant package runs,
unattended extraction writes, unattended `INVALIDATES`, and production vector
activation stay off.

## Landing order

This PR ships documents plus the offline evaluation scripts that produced the
numbers above. It ships no memory-engine runtime. Below is the order in which
the text lands; each step depends on the one before it for vocabulary.

1. **Decision log.** Append D48-D51 after D47 in
   [`docs/10-decision-log.md`](../10-decision-log.md) and update the
   introduction's range to D43-D51. D42-D47 history is not rewritten.
2. **Overview and terms.** Amend invariant 2 in
   [`docs/00-overview.md`](../00-overview.md) with the new regeneration
   authorities, add the no-silent-echo and no-silent-vector-skip rule after
   invariant 9, and define `fact_language_policy`, corroboration root,
   adjudication proposal, and embedding profile.
3. **Storage.** In [`docs/01-storage.md`](../01-storage.md), add the retained
   control records, the language, grouping, scope, root, and echo fields, the
   correction records and maps, and replace the embedding-stream model-ID
   paragraph with the profile identity and lifecycle. Add the
   `episode_digest_version` discriminator with both exact digest bodies, and
   make `verify`, backup/restore, and rebuild dispatch on the stored version
   with no SET on a legacy Episode. Extend indexes, constraints, allowlists,
   backup, and `verify`.
4. **Daemon and pipelines.** In
   [`docs/02-daemon-and-pipelines.md`](../02-daemon-and-pipelines.md), add
   `adjudication.review`, `adjudication.correct`, `embedding.retry`,
   `embedding.skip`, `embedding.cancel`, and the `gen` action `qualify`; rework
   L1/L1b/R2/L4/W1-W5; add the embedding state machines and error classes;
   classify the new methods as control-only. Retry validation and spool replay
   dispatch on the stored row's version, an absent value reads as version 1,
   and a legacy row can never gain lineage in place.
5. **Time and forgetting.** In [`docs/03-time.md`](../03-time.md) add the
   operator adjudication repair after the existing replacement protocol and
   state that lineage and acceptance times never change Fact time. In
   [`docs/04-forgetting.md`](../04-forgetting.md) state that occurrence count,
   echo depth, root count, operator review, and language never boost
   confidence, mass, or utility.
6. **Recall.** In [`docs/05-recall.md`](../05-recall.md) pin the new versions,
   keep the original query and original-Episode channels, insert the group and
   conflict predicates, the unknown-lineage suppression, and the
   `operator_corrected` provenance, and extend the degradation ladder. All D46
   and D44 caps stay exactly as they are.
7. **Envelope, validation, release.** Add the bounded root policy checks to
   [`docs/06-envelope-ppr.md`](../06-envelope-ppr.md), the machine-value CI
   fixtures to [`docs/07-gds-validation.md`](../07-gds-validation.md), the new
   schemas, RPCs, and version axes to
   [`docs/08-repo-and-release.md`](../08-repo-and-release.md), and the
   trigger-gated qualification work to
   [`docs/09-roadmap.md`](../09-roadmap.md).
8. **Research reports.** Append the disposition and cross-reference paragraphs
   to [adjudication-conformance.md](adjudication-conformance.md),
   [extraction-spotcheck.md](extraction-spotcheck.md), and
   [retrieval-fusion.md](retrieval-fusion.md), and publish
   [embedding-runtime.md](embedding-runtime.md). Existing results and
   limitations sections are not rewritten.
9. **Offline evaluation code.** Ship the replayable runners and their tests
   under `scripts/research/`, plus the frozen extraction prompt artifacts under
   `packages/backfill/prompts/`. These are evaluation and prompt-development
   assets, not a memory-engine runtime, and no daemon path calls them.

No runtime memory-engine implementation belongs to this PR, and none is
scheduled here. Nothing in it migrates or rewrites a stored Episode either.
Steps 1 through 4 write down the version-1/version-2 digest discriminator and
the stored-version-wins retry and journal rules; step 7 records the same
no-implementation, no-migration boundary in the release notes and the future
round-trip fixture. The compatibility code itself is a later change.
Fixtures added in step 7 assert machine-consumed values only: hashes, DDL
option objects, bounds, state transitions. They pin no prose.

## What is decided, and what is not

Decided: source-language Facts with an optional English generation; original
query and original Episodes as the recall default; mechanical exact-quote
admission; shadow-first adjudication with Opus as the development judge and GPT
as comparator; append-only operator correction; bounded echo lineage with no
corroboration boost; local grouping and conflict predicates; a prospective
versioned Episode digest that leaves every stored original byte-identical; one
pinned embedding profile with a blocking failure path and authenticated
recovery.

Not qualified: any statement that one language policy retrieves better; any
semantic-fidelity claim for either extractor; any production error bound for
either judge; unattended `INVALIDATES`; embedding throughput, boundary
behavior, index quality, or quantization parity; and any claim that a model is
generally superior. Those wait for their own evidence, or they stay off.
