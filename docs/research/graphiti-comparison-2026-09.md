# Anamnesis vs Graphiti: real-case comparison (2026-09)

Audience: the anamnesis maintainer. Status: reviewed against the G7 E2E
artifact of 2026-09-24.

Sources. Graphiti claims cite `.omo/research/graphiti-dossier.md` ("dossier
§N"), which describes getzep/graphiti main @ 1980db2. The dossier's own header
says its §7 and §8 may be truncated and need re-verification, so every claim
drawn from those two sections is marked UNVERIFIED here. Anamnesis claims cite
files in this repository. Where docs and code disagree, the code is named as
"today" and the docs as "target", following docs/00-overview.md ("Target, not
runtime status").

Numbers in §9 and §10 come from
`.omo/evidence/auto-pipeline/g7/e2e/e2e-summary.json` and
`.omo/evidence/auto-pipeline/g7/e2e/sample-facts.json` (G7 E2E run 3,
2026-09-24, status `passed`, stage `complete`). Nothing in those sections is
quoted from memory.

## 1. Summary verdict

- Graphiti has the more complete retrieval toolbox today: three search methods
  per component, five rerankers, prebuilt recipes and SearchFilters time
  windows (dossier §5). Anamnesis's shipped recall is Episode bm25 + vector +
  a small PPR over ConductingArc, with Fact bm25 layered on top
  (`packages/core/src/store.ts` L2273-2405); the weighted RRF, mass, utility
  and conflict pipeline in `docs/05-recall.md` is a target.
- Anamnesis is stricter about what an LLM may write. Facts reach the graph
  only through `validateSemanticClaim` and `writeValidatedFactTx`
  (`docs/10-decision-log.md` D52; `packages/protocol/src/semantic-claim.ts`;
  `packages/core/src/store.ts` L2221), and Fact-to-Fact relations reach it
  only through a digest-bound relation verdict that is applied mechanically
  (D53). Graphiti lets `resolve_edge` and `resolve_node` LLM verdicts write
  dedup, merge and invalidation directly (dossier §2, §3, §4).
- Graphiti's bi-temporal edges answer "what did the system believe at time X"
  (dossier §3). Anamnesis has one semantic axis, event time, and calls the
  record-time question a debugging question (`docs/03-time.md` §6;
  `docs/09-roadmap.md` "Not doing"). Different by design on both sides.
- Anamnesis keeps every occurrence as its own Fact and never merges by
  similarity (D46, D49); the only collapse is the relation judge's
  `duplicate` verdict, which stops a paraphrase from being written at all
  (D53). Graphiti merges duplicate nodes and edges into one UUID with an
  LLM-chosen name and summary (dossier §4). Graphiti's graph is smaller and
  easier to read; anamnesis's is auditable back to a byte span.
- Both systems now do contradiction handling unattended, and neither should
  be trusted blind. Graphiti does it with one LLM call per edge and no
  feedback path (dossier §3, §6). Anamnesis does it with one `judge_relations`
  call per claim that has same-Entity candidates, applied as CONTRASTS (both
  kept) or dated non-recursive INVALIDATES (D53). In the G7 run that produced
  12 CONTRASTS, 0 INVALIDATES and 9 refused duplicates over 200 Episodes (§9).
  The adjudicator stays shadow-only (D50).
- Anamnesis has forgetting, adoption reinforcement, outcome utility, receipts
  and policy suppression designed and partly implemented
  (`docs/04-forgetting.md`; `docs/05-recall.md` §10; `store.ts` L2311-2318).
  Graphiti has none of these (dossier §6). This is the largest asymmetry.
- Graphiti is broader operationally: four graph backends, five LLM vendors,
  MCP and REST servers, multi-tenant `group_id` (dossier §1; §7 UNVERIFIED).
  Anamnesis is single-tenant and Neo4j-only; its daemon listens on a UDS and,
  when opted in, on one TCP port behind a bearer token (`docs/deploy.md`;
  `docs/09-roadmap.md` "Not doing" for multi-tenancy).
- Cost: the dossier counts roughly 5 LLM calls per episode for Graphiti with
  per-entity and per-edge fan-out (dossier §2; §8 UNVERIFIED). Anamnesis's
  automatic pipeline is 2 base calls per Episode (claim, judge_claims) plus one
  judge_relations call per validated claim that has same-Entity candidates, so
  an Episode with six candidate-bearing claims costs 8 calls before retries,
  with deterministic Entity resolution
  (`packages/core/src/engine.ts` L176-186; D52; D53;
  `app/anamnesis/prompts/judge-relations.v1.md`).

## 2. Data model

| Aspect | Anamnesis | Graphiti |
|---|---|---|
| Original input | `:Element:Episode`, CREATE-only, event time `time_utc`, `ingested_at` separate, `revision_key` unique per revision (`docs/01-storage.md` §1) | `EpisodicNode {content, source, valid_at, created_at, entity_edges}` (dossier §1, `graphiti_core/nodes.py:EpisodicNode`) |
| Derived statement | `:Element:Fact` node with `content`, `sub_kind`, `modality`, `confidence`, `content_language`, `predicate_text`, `scope`, `speaker_key`, `subject_keys`, echo lineage fields (`docs/01-storage.md` §1; `store.ts` L2237-2245) | Fact text lives on the edge: `EntityEdge` (RELATES_TO) `{name, fact, fact_embedding, valid_at, invalid_at, expired_at, episodes}` (dossier §1, `graphiti_core/edges.py:EntityEdge`) |
| Entity | `:Element:Entity` with `entity_key = sha256(generation, normalized_name, entity_kind)`; no summary, no decay (`docs/01-storage.md` §1; `docs/04-forgetting.md` §3) | `EntityNode {name, name_embedding, summary, attributes, labels}`; summary evolves via LLM merges (dossier §1, §4) |
| Community | `:Element:Community` built by offline Leiden, `HAS_MEMBER` to Entity or Fact, half-members visibility threshold (`docs/01-storage.md` §4; `docs/02-daemon-and-pipelines.md` §7) | `CommunityNode {name, name_embedding, summary}` with `HAS_MEMBER` to Entity (dossier §1) |
| Sequence | `NEXT_EPISODE` is a rebuildable cache ordered `(time_utc, ingest_seq)` inside a session (`docs/02-daemon-and-pipelines.md` §3) | `SagaNode` + `HAS_EPISODE` + `NEXT_EPISODE` with incremental saga summaries (dossier §1) |
| Link roles | Seven closed roles with a from/to lattice enforced at write: `MENTIONS`, `RELATES_TO`, `NEXT_EPISODE`, `HAS_MEMBER`, `DERIVED_FROM`, `INVALIDATES`, `CONTRASTS` (`packages/protocol/src/link.ts`) | Five edge types: RELATES_TO, MENTIONS, HAS_MEMBER, HAS_EPISODE, NEXT_EPISODE (dossier §1) |
| Provenance | `DERIVED_FROM` Fact->Episode carries `span` byte offsets and `evidence_text` (`store.ts` L2246-2248; `docs/01-storage.md` §5) | `EntityEdge.episodes: list[uuid]` and `EpisodicEdge` MENTIONS (dossier §1). No span. |
| Embedding placement | Target: per-model property on Episode, Fact, Entity and `RELATES_TO`; one HNSW index per generation (`docs/01-storage.md` §4). Today: an Episode vector node keyed by `profile_id`, `input_revision`, `input_digest` (`store.ts` L2306-2308) | `EntityNode.name_embedding` (name only), `EntityEdge.fact_embedding`, `CommunityNode.name_embedding` (dossier §1). Episodes have no embedding; episode search is BM25 only (dossier §5) |
| Tenancy | Single tenant; the boundary is the OS user (`docs/02-daemon-and-pipelines.md` §10). Isolation inside one store is by derived `generation` and policy | `group_id` on every node and edge, validated alphanumeric plus `-`/`_` (dossier §1) |

Two differences matter most. First, Graphiti embeds the entity name and the
edge fact, never the episode; anamnesis embeds the Episode text itself and
(target) Fact, Entity and relation content (`docs/01-storage.md` §4). Second,
in Graphiti the natural-language fact is a property of an Entity->Entity
edge, so a claim with zero or one resolved entity has nowhere to live;
anamnesis's Fact is a node and Entities are optional MENTIONS
(`semantic-claim.ts` allows an empty `entities` array and null `subject_keys`).
The G7 sample in §9 shows how often that second point bites: most Facts there
are single-subject statements about GitHub audit logs or a token hub, not
two-entity triples.

## 3. Temporal model

| Aspect | Anamnesis | Graphiti |
|---|---|---|
| Axes | One semantic axis: event time on Episode and Fact. `ingested_at`, Hit `t`, receipt times are operational and never enter `snapshot(T)` (`docs/03-time.md` §1, §6; `docs/00-overview.md` invariant 4) | Two axes on `EntityEdge`: event time `valid_at`/`invalid_at`, assertion time `created_at`/`expired_at` (dossier §3) |
| Fact effective time | "The time the claim took effect." Resolution order explicit > relative against Episode.time > inherited (`docs/03-time.md` §2). Today the prompt asks for absolute dates only and the writer records `time_basis` as `claim` or `episode_fallback` (`app/anamnesis/prompts/extract-claims.v2.md`; `store.ts` L2228-2236) | `valid_at` LLM-extracted or defaulted to episode `valid_at`; relative expressions resolved by a second small-model call `extract_timestamps` against `reference_time` (dossier §2, §3; `graphiti_core/utils/maintenance/edge_operations.py:_extract_edge_timestamps`) |
| Ending a fact | `INVALIDATES` edge, Fact->Fact or Episode->Episode, copying `target_id` and `effective_time_utc`; at most 8 outgoing per Fact (`docs/01-storage.md` §5; `link.ts`). Today the relation judge writes it new->candidate with `effective_time_utc` = the new Fact's time, never onto a Fact that already invalidates one (D53) | `invalid_at` set from text or from contradiction (`= new_edge.valid_at`); `expired_at = utc_now()` marks system discovery (dossier §3) |
| Validity predicate | `valid(f,T)` = visible and some source Episode live and no INVALIDATES/marker with `effective_time_utc <= T`; the invalidator's own validity is not consulted (`docs/03-time.md` §4; D46, D50) | Current facts filter `invalid_at IS NULL AND expired_at IS NULL`; invalidation is "not recursive/transitive by design" (dossier §3) |
| Correction vs change | Two modes with different C.time: change keeps the event's time, correction backdates to B.time; a wrongly invalidated A is restored only by a new A' (`docs/03-time.md` §5) | Not distinguished in the dossier. Invalidated edges are kept, never deleted (dossier §3) |
| Historical query | `snapshot(T)` answers "what the record says the world was like at T", not "what the system knew at T" (`docs/03-time.md` §0, §6) | `SearchFilters(valid_at_before, invalid_at_after, created_at_*)` answers both (dossier §5) |
| Today in code | Recall filters Episodes by `time_utc <= $T` and excludes those with an INVALIDATES edge effective at or before T; Facts filter `time_utc <= $T` only (`store.ts` L2292-2294, L2329) | n/a |

Both agree on non-recursive invalidation. Graphiti's `expired_at` gives an
audit answer anamnesis chose not to store; anamnesis pays for that with
`Episode.ingested_at`, receipts and generation integers (`docs/03-time.md` §6).
The friction point is the correction mode: the dossier shows no backdating
rule, so in Graphiti "the record was wrong" and "the world changed" look the
same in the edge fields. Graphiti is plainly better at point-in-time filters:
anamnesis has no equivalent of `created_at_*` on the recall path.

## 4. Contradiction and duplicate handling

| Aspect | Anamnesis | Graphiti |
|---|---|---|
| Duplicate policy | Never collapse written Facts. Each occurrence is its own immutable Fact; grouping happens at assembly under `anamnesis.duplicate-group/1` and only within one candidate set (D46, D49; `docs/05-recall.md` §6). Today the relation judge may refuse a claim as `duplicate` before it is written; the refusal is recorded as a `duplicate:` operation with `duplicate_of` (D53). G7: 9 refusals over 200 Episodes | Collapse. `resolve_edge` picks a duplicate index; the new edge reuses the existing UUID (dossier §2, §3) |
| Contradiction detection | Today: `judge_relations` (`judge-relations.v1.md`) returns one of `duplicate / invalidates / contrasts / unrelated` per candidate, confidence below 0.6 is read as `unrelated`; the verdict must echo the premise digest and cover exactly the candidate set or it is rejected (D53). Candidates are at most 16 ACTIVE Facts of the writable generation that share a MENTIONS Entity with the new claim; vector neighbours are not consulted because no Fact vector index exists (D53). Target: the L4 judge against up to 128 candidates (`docs/02-daemon-and-pipelines.md` §5 L4) | `resolve_edge` prompt receives EXISTING FACTS (same endpoints), the NEW EDGE and FACT INVALIDATION CANDIDATES from a broader hybrid search; one LLM call per extracted edge decides duplicate and contradiction; `resolve_edge_contradictions` applies the temporal rule (dossier §3; `graphiti_core/prompts/dedupe_edges.py:resolve_edge`) |
| Unattended writes | Relation verdicts are applied mechanically without review: `contrasts` -> CONTRASTS new->candidate, both stay ACTIVE; `invalidates` -> INVALIDATES with the new Fact's time, non-recursive (`chain_refused` otherwise); `duplicate` -> not written (D53). No Fact of a source is written while a verdict is owed. The adjudicator for operator corrections is shadow-only (D50) | Fully unattended inside `add_episode` (dossier §2, §3) |
| Winner selection | CONTRASTS elects no winner by confidence or recency; recall returns up to 4 companions per primary with mandatory warnings (D46; `docs/05-recall.md` §6). Today: `store.ts` L2373-2379 attaches up to 4 CONTRASTS peers. `invalidates` requires the new claim to be at or after the candidate's time; an earlier conflicting claim is `contrasts` (`judge-relations.v1.md`) | Newer `valid_at` wins when the LLM says the pair contradicts (dossier §3) |
| Repair | `adjudication.correct` appends an operator Episode and a replacement Fact; nothing is deleted (D50; `docs/03-time.md` §5) | "Delete and re-ingest corrected episodes" (dossier §6) |
| Where the seam is | `Engine.proposeRetainedClaim` calls `SemanticReviewProvider.resolve` then `.review` around `validateSemanticClaim` (`engine.ts` L101-119); the relation judge gates the write there (D53) | `dedupe_edges.resolve_edge` inside `resolve_extracted_edge` (dossier §2) |

Honest reading. Both systems now emit relations automatically; the shapes
differ. Graphiti's candidate set is wider (same endpoints plus a hybrid search
over all edges), so it can catch a contradiction between two facts that share
no named entity. Anamnesis's candidate set is same-Entity ACTIVE Facts only
(D53), so a paraphrase whose Entities failed to resolve is invisible to the
judge; §9 shows paraphrase Facts from duplicated Episodes that both survive,
which is consistent with that limit, though the artifact does not record
which candidates each judgement saw. Anamnesis's advantages here are that the
verdict is bound to the exact candidate set it saw, that retries are
idempotent, and that a duplicate is refused before it exists rather than
merged after. The D52 position that a wrong relation is worse than none still
holds; it now has a judge behind it, and the judge's own error rate is
unmeasured.

## 5. Entity resolution

| Aspect | Anamnesis | Graphiti |
|---|---|---|
| Mention extraction | Extractor returns `{mention, normalized_name, entity_kind}`; `normalized_name` must be an exact substring of the source (`extract-claims.v2.md`) | `extract_nodes.extract_message/json/text` returns names, types, summaries; chunked when density > 0.15 (dossier §2, §4) |
| Candidate finding | Target: synchronous fulltext top 16 per mention, global top 64 (`docs/02-daemon-and-pipelines.md` §5 R1). Today (D52 path): lookup in the generation's `entity_key` registry, `entity_key = sha256(generation, normalized_name, entity_kind)` (`docs/01-storage.md` §1; `semantic-claim.ts`) | Hybrid: cosine on `name_embedding` (min 0.6) + BM25 on name/summary, RRF merge, optional BFS to depth 3 (dossier §4; `graphiti_core/search/search_utils.py:hybrid_node_search`) |
| Decision | Target: L3 Entity judge LLM (`docs/02-daemon-and-pipelines.md` §5 L3). Today: deterministic `existing -> reuse, new -> create, unresolved -> mention dropped`, supplied by the trusted resolver and checked by the validator (D52; `semantic-claim.ts` `SemanticResolution`) | `dedupe_nodes.resolve_node` LLM call per extracted node returns duplicate index, merged name, merged summary (dossier §4; `graphiti_core/prompts/dedupe_nodes.py`) |
| Merge behavior | No merge. An Entity is immutable; a new generation creates its own copy under a new `entity_key` (`docs/01-storage.md` §1, §4). Names are never transliterated or translated (D48) | Reuse candidate UUID, LLM selects name, append labels, update summary via `summarize_node_pair` (dossier §2, §4; `graphiti_core/utils/maintenance/node_operations.py:resolve_extracted_nodes`) |
| Schema | `entity_kind` is a free lowercase noun; no per-kind attributes (`extract-claims.v2.md`) | Custom Pydantic `entity_types` with typed attributes via `extract_attributes_from_nodes`; `excluded_entity_types` filter (dossier §4) |
| Custody | `MENTIONS` Fact->Entity link plus `EntityWitness {entity_id, generation, policy_revision}` written in the same transaction (`store.ts` L2256-2260); target adds `visible_from_utc` (`docs/03-time.md` §3) | Entity has `created_at` and `group_id` only; no visibility threshold (dossier §1) |
| Known failure | Unresolved mention drops silently from `subject_keys` and disables grouping (D49). No alias table today. Since D53 an unresolved mention also removes the claim from the relation judge's candidate pool | Small models emit invalid JSON; Kuzu FTS bug returns empty candidates; embedding dimension change breaks similarity (dossier §4) |

Graphiti's resolver is better at "Alice Smith" = "Alice S." because it
searches by embedding and lets an LLM merge. Anamnesis's is better at not
inventing that equality: D49 says a pairwise key "asserts no global identity".
The cost on a personal chat corpus: the same person spelled two ways becomes
two Entities. In the G7 sample, "GitHub Enterprise", "GitHub Enterprise
Cloud", "Enterprise Cloud" and "GitHub" all appear as surface forms across
neighbouring Episodes; Graphiti would likely fold most of them, anamnesis
keeps whichever `normalized_name` the extractor returned (§9).

## 6. Retrieval

| Aspect | Anamnesis today (`store.ts` L2273-2405) | Anamnesis target (`docs/05-recall.md`) | Graphiti (dossier §5) |
|---|---|---|---|
| Lexical | Lucene `element_content` on Episodes, limit 64; same index on active-generation Facts, limit 64 | Episode fulltext + `fts_fact_g<active>` (cjk analyzer), 64 each | BM25 on edges, nodes, episodes, communities |
| Vector | `vec_episode_<profile>` cosine, k 256 then 64, Episodes only | Episode + Fact vectors (64) + `RELATES_TO` relationship vectors (16) | cosine on `fact_embedding`, `name_embedding`, community name; min score 0.6 |
| Graph | Local PPR over `ConductingArc` rows touching the seed set, `LIMIT 1024`; `ppr_used` in diagnostics | Envelope <= 2,000 nodes / 20,000 links, CSR PPR alpha 0.85, hub damping, 100 ms deadline (`docs/06-envelope-ppr.md`) | BFS to `max_depth` 3 from seed nodes |
| Session / identity | `session` channel: last 32 Episodes; `identity`: exact Episode ID match | session 32 Episodes + 2 Facts each; identity anchors + profile cache | `episode_mentions` reranker; no session channel |
| Fusion | `normalizedRrf` over channel lists for Episodes; Facts scored by PPR value or mass | RRF weights vector .25, bm25 .25, ppr .30, session .15, identity .05; `score = relevance * max(m, .02)^0.5 * (1 + .25 U)` | RRF `1/(rank+60)`, MMR lambda 0.5, cross-encoder LLM, node distance, episode mentions |
| Rerank | None beyond score sort | None; LLM calls on the recall path are on the not-doing list (`docs/09-roadmap.md`) | Cross-encoder via OpenAI/Gemini reranker clients |
| Recipes | None | None | `COMBINED_HYBRID_SEARCH_RRF/MMR/CROSS_ENCODER`, `EDGE_HYBRID_SEARCH_*`, `NODE_HYBRID_SEARCH_RRF`, `COMMUNITY_HYBRID_SEARCH_RRF` |
| Time filter | `time_utc <= T` on every channel | `visible(T)` in WHERE; `valid(T)` at assembly | `SearchFilters` on valid_at, invalid_at, created_at |
| Output | Bundles with up to 4 CONTRASTS companions, up to 8 supersedes, canonical JSONL `context_text`, exact byte/scalar/token budget, durable receipt | Same plus occurrence grouping, entities block, degradation ladder | Ranked edges/nodes/episodes/communities, `limit` default 10 |
| Derived layer | Facts via bm25 + PPR; Entities and Communities are not results | Facts and Episodes as results; Entities top 8 separately; Communities never returned; Dream (Leiden) builds them offline (`docs/02-daemon-and-pipelines.md` §7; `packages/core/src/dream-leiden-adapter.ts`) | Communities are searchable results with summaries |

Graphiti retrieval is wider and tunable per call, and its reranker variety has
no anamnesis counterpart. Anamnesis retrieval is narrower and carries things
Graphiti has no analogue for: an exact output budget, a receipt that enables
adoption and outcome feedback, mass and utility in the score, and a rule that
a failed channel is dropped whole rather than served partial
(`docs/05-recall.md` §8). Neither source gives a measured quality number;
`docs/00-overview.md` says the envelope truncation cost "is unknown today",
and the dossier marks Graphiti runtime performance UNVERIFIED. The G7 run adds
one observation: in 4 of 5 recall queries no Fact made the top 20, and in the
fifth the first Fact sat at rank 17 behind 16 Episodes (§9). Facts are
outranked by their own source Episodes today.

## 7. Forgetting and feedback

Graphiti: no decay, no reinforcement beyond implicit episode-mention counts,
no usage tracking, no feedback API; correction is delete and re-ingest
(dossier §6).

Anamnesis, as documented and partly implemented:

- Retention `R(t,S) = (1 + (19/81) t/S)^-0.5`, computed at read time from
  `(s, t_last_hit)`; no tick daemon (`docs/04-forgetting.md` §0, §2). Today:
  `store.ts` L2311-2318 reads `HitCache {s, t_last_hit, utility}` and
  multiplies mass by `retention(...)`.
- `m0 = confidence * prior(sub_kind) * prior(modality)` for Facts; Entities do
  not decay (`docs/04-forgetting.md` §1, §3). Today: Fact `mass` is the
  model-reported confidence (`store.ts` L2236; D52).
- Only a confirmed `recall_hit` adoption raises `S`; exposure, re_mention,
  promotion and outcome are audit-only; `S` never decreases
  (`docs/00-overview.md` invariant 6; `docs/04-forgetting.md` §5).
- Outcome utility `U` per Episode enters score as `(1 + beta U)`
  (`docs/04-forgetting.md` §5.1; `docs/05-recall.md` §10). Today:
  `(1 + 0.25 * utility)` (`store.ts` L2316).
- Receipts: append-only `RecallReceipt` written before delivery, TTL
  3,600,000 ms, `commit {recall_id, adopted?, reward?}` exactly-once
  (`docs/05-recall.md` §10). Today: `issueReceiptTx` runs inside recall with
  `expires_at = now + 3600000` (`store.ts` L2399, L2407).
- Policy suppression via `policy.set`/`policy.revoke`; suppresses, never
  erases (`docs/01-storage.md` §3.2; D43). Today: denies are applied in the
  recall WHERE clause and to supersedes text (`store.ts` L2290-2294,
  L2388-2392).
- Echo lineage: an assistant turn that repeats delivered context never counts
  as corroboration (D49). In G7 this is visible as one refused claim
  (`refused_claims: 1`) and as the deviation note that pre-lineage
  `ingest-codex-raw` Episodes yield no Facts (`echo_lineage_unavailable`).

Nothing in the anamnesis sources describes deletion of aged memories;
forgetting is a score multiplier, not a GC policy. Whether the `S` update and
`commit` RPC are fully wired today was not checked in this read (UNVERIFIED).

## 8. Operations

| Aspect | Anamnesis | Graphiti |
|---|---|---|
| Process model | One daemon, only Bolt client, length-prefixed JSON-RPC with a capability token. Listeners: a UDS at `<root>/anamnesis.sock` always, plus an optional TCP listener enabled by `ANAMNESIS_LISTEN` that requires a bearer token read from a token file with mode 0600 (`docs/02-daemon-and-pipelines.md` §0, §2, §10; `docs/deploy.md`; `app/anamnesis/README.md`) | Python library embedded in the caller; optional MCP server and FastAPI REST server (dossier §7, UNVERIFIED) |
| Graph store | Neo4j Community 5.26+ only, `127.0.0.1:7687`, per-install random password (`docs/01-storage.md` §0; `docs/02-daemon-and-pipelines.md` §10) | Neo4j 5.26+, FalkorDB 1.1.2+, Kuzu 0.11.2 (deprecated), Amazon Neptune (dossier §7, UNVERIFIED) |
| Indexes | Unique/range/fulltext/vector per generation; vector options pinned (`vector-2.0`, cosine, 1024, HNSW m 16 / ef 100) (`docs/01-storage.md` §4, §7) | `build_indices_and_constraints`: uuid/group_id, name/group_id, fulltext on name+summary, content, fact (dossier §7, UNVERIFIED) |
| LLM | HTTP extraction provider; default `claude-haiku-4-5` via token-hub, `anthropic_messages` dialect (`app/anamnesis/README.md`; `scripts/qa/e2e-real.ts` L85-86, L139). Adjudication default `claude-opus-5`, shadow mode (D50) | OpenAI default, Anthropic, Gemini, Groq, OpenAI-compatible (dossier §7, UNVERIFIED) |
| Embeddings | Self-hosted Qwen3-Embedding-0.6B on llama.cpp, 1024 dims, L2-normalized, pinned query template; profile ID hashes endpoint/model/dimensions (`docs/01-storage.md` §4; D51; `app/anamnesis/README.md`). G7 `embedding_channel`: `qwen3-embedding-0.6b via llama-server` | OpenAI `text-embedding-3-small` default, Voyage, Ollama (dossier §7, UNVERIFIED) |
| Concurrency | Extraction lane: at most 4 pipelines in flight per writer (`ExtractionScheduler` `maxInFlight` default 4, `packages/core/src/extraction-scheduler.ts`), each pipeline's provider calls sequential; no inter-call pacing in the daemon (`ANAMNESIS_QA_LLM_MIN_INTERVAL_MS` is consumed only by the Engine-driven `scripts/qa/e2e-real.ts` pacer, not by `app/anamnesis`) | `SEMAPHORE_LIMIT=20` default, `semaphore_gather` around parallel LLM/DB work (dossier §2) |
| QA database | Isolated ephemeral Neo4j per harness run, owner-labelled container, never the live `anamnesis-neo4j-1` (`.omo/g7-context.md`; `app/anamnesis/README.md`) | Not described in the dossier |
| Backup / restore | Offline `neo4j-admin database dump` with the pinned image, manifest of `objects/`, staged restore with activation journal (`docs/01-storage.md` §9; `docs/deploy.md`; `app/anamnesis/README.md`) | Not described in the dossier |
| Multi-tenancy | Not doing (`docs/09-roadmap.md`) | `group_id` partition key (dossier §1) |
| Payload egress | Every outbound body <= 256 KiB; remote endpoints opt-in with HTTPS and fixed host (`docs/02-daemon-and-pipelines.md` §10) | Not described in the dossier |

Graphiti wins on breadth here: multiple graph backends and LLM vendors are
real options, not a roadmap item. Anamnesis wins on operational specificity:
pinned index options, an offline backup format and an isolated QA database
are documented and exercised by the E2E harness.

## 9. Real-case run (G7 E2E)

### 9.1 Run header

Source: `.omo/evidence/auto-pipeline/g7/e2e/e2e-summary.json`, run 3,
2026-09-24. Model `claude-haiku-4-5`; embeddings `qwen3-embedding-0.6b via
llama-server`; `workers_deadline_ms` 5,400,000. The artifact's `llm_min_interval_ms: 3000`
field was recorded by the driver but the daemon never reads that variable (no
`app/anamnesis` or `packages/core` code consumes it); the worker lanes ran at
the scheduler's own bound of 4 pipelines in flight with no inter-call pacing.

| Field | Value |
|---|---|
| status / stage | `passed` / `complete` |
| episodes | 200 |
| facts_active | 421 |
| vectors | 200 |
| relation_links | contrasts 12, invalidates 0, duplicates 9, total 21; `fact_links_by_type` {CONTRASTS: 12} |
| refused_claims | 1 |
| recall | 5/5 queries answered, `channels` [bm25, vector] and `ppr_used` true on every query, `vector_reason` "available", 20 results each. Fact hits: query 1 had 4 Facts with `fact_rank_min` 17; queries 2 to 5 had 0 Facts in the top 20 (`fact_rank_min` null). `companions` empty on all five |
| durations (ms) | ingest 42,031; workers 987,029; recall 2,631; crash_drain 7,742; total 1,049,148 |
| crash_drain | `equal: true`; before and after both show episodes 200, facts_active 421, generation `01a0d308-87a9-...`, `covered_ingest_seq` 200 |
| workers snapshot (settled) | embedding: pending 0, drained_total 200, quarantined_total 0, last_error null. extraction: state `active`, covered_ingest_seq 200, live_ingest_seq 200, in_flight 0, completed_total 200, failed_total 0, last_error `Error: lease_expired` |
| provider_errors | null |

The `lease_expired` in `last_error` is a transient. In run 3 the scheduler
leased a claim for 30 s and a slow haiku round trip crossed it; the scheduler
settled the expired lease and retried the source, which is why
`completed_total` is 200 and `failed_total` is 0. A re-arm bug that kept the
lane from retrying in run 2 was fixed in commit 650eca8 before run 3. The
field shows the last error string the lane saw, not an open failure. Two
later changes close the remaining gap: the scheduler lease is now 90 s
(`extraction-scheduler.ts`, `LEASE_MS`, three times the daemon's provider
timeout), and a source whose lease expires on every attempt of the budget is
cancelled into a durable omission that coverage can seal, instead of leaving
the generation stuck behind it (`store.ts` `cancelModelTask` accepting
`expired`/`worker_lost`; regression test `extraction-scheduler.test.ts`,
which drives lease expiry through the injected store clock).

Run 5 (`.omo/evidence/auto-pipeline/g7/e2e-run5/e2e-summary.json`, same
date, same corpus rule, code including both changes) confirms the shape with
a clean lane: status `passed`, 200 Episodes, 422 active Facts, 200 vectors,
relation links contrasts 8 / invalidates 0 / duplicates 7 (total 15),
`refused_claims` 0; recall 5/5 with `[bm25, vector]` and `ppr_used` on every
query (Fact hits 3, 0, 0, 0, 2); crash_drain `equal: true`; settled workers
embedding 200 drained / 0 quarantined, extraction `active`, covered 200 /
live 200, completed_total 200, failed_total 0, `last_error` null. Durations
(ms): ingest 45,024; workers 1,143,177; recall 2,386; total 1,207,162. The
sample in 9.2 is still drawn from run 3; the Fact texts differ between runs
only in wording since the extraction is a fresh haiku call per Episode.

Run 6 (`.omo/evidence/auto-pipeline/g7/e2e-run6/e2e-summary.json`, tree
88debb6, the merged PR's final code) repeats it: status `passed`, 200
Episodes, 417 active Facts, 200 vectors, relation links contrasts 7 /
invalidates 0 / duplicates 6 (total 13), `refused_claims` 0; recall 5/5 with
`[bm25, vector]` and `ppr_used`; crash_drain equal; extraction completed
200 / failed 0, `last_error` null. Durations (ms): ingest 53,222; workers
689,456; recall 2,656; total 775,913. Three runs, three Fact counts (421, 422,
417) and three link totals (21, 15, 13): the extraction is stable in shape and
varies in the margin where haiku splits or merges a claim.

Two run-shape notes from `deviations` matter for reading the sample. The
corpus is one copied Codex rollout plus deterministic Claude text-block
conversions to reach 200 Episodes, so many Episodes appear twice with
different IDs and millisecond-apart times (the pairs seq 4/5, 6/7, 8/9, 12/13,
16/17, 18/19 below). And the harness itself flagged "derived Fact ranking:
Facts outranked by Episodes in 4/5 queries".

### 9.2 Sample Episodes and the Facts they produced

Source: `sample-facts.json`, generation `01a0d308-87a9-768a-be95-1d33e6998242`,
20 Episodes. Excerpts are cut at 120 characters. Every Fact in the sample
carries its Episode's time (`time_basis` `episode_fallback`), so the time
column is given once per row. No Fact in the sample has a link; the 12
CONTRASTS of the run fall on Episodes outside this sample, and the 9
duplicates are refusals, which by construction leave no Fact to show. Fact
texts are quoted as stored, cut at about 110 characters where long.

| seq | Episode excerpt | Facts (text) | time (UTC) | links |
|---|---|---|---|---|
| 4 | GitHub Enterprise Cloud와 Server에서 감사 로그 범위가 조금 달라서, 현재 공식 문서 기준으로 확인해볼게요. GitHub 관련 질문이라 `github` 스킬의 안내를 따라 살펴보겠습니다. | (1) GitHub Enterprise Cloud와 Server에서 감사 로그 범위가 조금 다르다. (2) 공식 문서 기준으로 감사 로그 범위 차이를 확인할 계획이다. | 2026-08-09T19:31:17.021Z | none |
| 5 | same text as seq 4 (duplicate Episode) | (1) 화자는 공식 문서 기준으로 GitHub Enterprise Cloud와 Server의 감사 로그 범위를 확인할 계획이다. | 2026-08-09T19:31:17.021Z | none |
| 6 | 핵심은 "클론했다"는 행위 자체와 "어느 기기였는지"가 별개로 기록되는지입니다. 감사 로그 이벤트와 남는 접속 정보(IP·사용자 에이전트 등)를 나눠 확인 중입니다. | (1) 감사 로그 이벤트와 남는 접속 정보(IP·사용자 에이전트 등)를 나눠 확인 중입니다. | 2026-08-09T19:31:23.324Z | none |
| 7 | same text as seq 6 | (1) 감사 로그 이벤트와 남는 접속 정보(IP·사용자 에이전트 등)를 나눠 확인 중이다. | 2026-08-09T19:31:23.325Z | none |
| 8 | 어느 기기에서 클론했는지 **정확한 PC 이름·시리얼·MAC 주소까지 보이진 않습니다.** 다만 GitHub Enterprise 감사 로그에는 `git.clone` 이벤트가 남을 수 있어서 다음 정보는 확인 가능합니다. | (1) GitHub Enterprise 감사 로그에는 `git.clone` 이벤트가 남을 수 있으며, 클론한 계정, 저장소와 시각, 접속 IP (관리자가 활성화한 경우), HTTPS/SSH 같은 전송 방식, 그리고 `user_agent`를 확인할 수 있습니다. (2) 정확한 PC 이름·시리얼·MAC 주소는 GitHub Enterprise 감사 로그에 보이지 않습니다. (3) NAT나 VPN을 쓰면 여러 기기가 같은 IP로 보일 수 있습니다. (4) Enterprise Cloud에서 Git 이벤트는 웹 화면에는 안 나오고 REST API·내보내기·감사 로그 스트리밍으로만 확인되며, 기본 보존 기간이 7일입니다. (5) Enterprise Cloud에서 장기 추적을 위해서는 미리 스트리밍해야 합니다. (6) Enterprise Server는 관리자가 Git 이벤트 기록을 사전에 활성화해야 합니다. | 2026-08-09T19:31:37.468Z | none |
| 9 | same text as seq 8 | (1) GitHub Enterprise 감사 로그에는 `git.clone` 이벤트가 남을 수 있으며, 클론한 계정, 저장소와 시각, 접속 IP(관리자가 IP 표시를 활성화한 경우), HTTPS/SSH 같은 전송 방식, `user_agent`(Git 클라이언트 종류·버전) 정보를 확인 가능합니다. (2) NAT나 VPN을 쓰면 여러 기기가 같은 IP로 보일 수도 있습니다. (3) Enterprise Cloud에서 Git 이벤트가 웹 화면에는 안 나오고 REST API·내보내기·감사 로그 스트리밍으로 확인되며, 기본 보존 기간이 7일이라 장기 추적하려면 미리 스트리밍해야 합니다. (4) Enterprise Server는 관리자가 Git 이벤트 기록을 사전에 활성화해야 합니다. (5) 정확한 PC 이름·시리얼·MAC 주소는 감사 로그에 보이지 않습니다. | 2026-08-09T19:31:37.471Z | none |
| 10 | 그럼 내가 우리 집 와파에서 다른 기기로 깃헙 클론을 뜨면 원래 회사 맥북인지 내꺼인지 모르겟네? | (1) 사용자가 자신의 집 와파에서 다른 기기로 깃헙 클론을 뜰 때 원래 회사 맥북인지 자신의 기기인지 구분할 수 없다는 것을 알 수 없다고 말함. | 2026-08-09T19:32:09.041Z | none |
| 12 | 네, **회사 맥북과 개인 기기가 같은 집 Wi‑Fi를 사용했다면 GitHub 감사 로그만으로는 구별하기 어려울 가능성이 큽니다.** 두 기기 모두 외부에는 보통 같은 공인 IP로 보이고, ... | (1) GitHub 감사 로그만으로는 같은 집 Wi-Fi를 사용하는 회사 맥북과 개인 기기를 구별하기 어려울 가능성이 크다. (2) 같은 집 Wi-Fi를 사용하는 두 기기는 외부에는 보통 같은 공인 IP로 보이며, GitHub에는 기기명·MAC 주소·시리얼 번호가 기록되지 않는다. (3) user_agent, 인증 방식, SSH 키·토큰 정보가 다르면 어느 자격증명을 사용했는지 어느 정도 구분할 수 있으나, 그것만으로 실제 물리적 기기를 확정하기는 어렵다. (4) 회사 VPN·프록시·EDR/MDM의 기기 접속 로그, 회사 맥북에만 발급된 인증서나 SSH 키, SSO의 관리 기기 확인(Device Trust), ... 시스템이 있으면 기기를 식별할 수 있다. (5) GitHub 로그 단독으로는 기기를 구별하기 어려우나 회사 전체 보안 로그와 결합하면 회사 맥북인지 개인 기기인지 판별할 수 있다. (6) 회사 소스의 개인 기기 복제는 사규나 보안정책 위반이 될 수 있으므로, 허용 여부를 먼저 확인하는 것이 안전하다. | 2026-08-09T19:32:19.590Z | none |
| 13 | same text as seq 12 | (1) user_agent, 인증 방식, SSH 키·토큰 정보가 다르면 어느 자격증명을 사용했는지 어느 정도 구분할 수 있지만, 그것만으로 실제 물리적 기기를 확정하기는 어렵다. (2) 회사 VPN·프록시·EDR/MDM의 기기 접속 로그, ... 시스템이 있으면 회사 기기와 개인 기기를 식별할 수 있다. (3) 회사 소스의 개인 기기 복제는 사규나 보안정책 위반이 될 수 있으니, 허용 여부를 먼저 확인하는 편이 안전하다. | 2026-08-09T19:32:19.594Z | none |
| 16 | ZIP 다운로드도 흔적이 아예 없는 건 아닙니다. GitHub가 어떤 감사 이벤트로 기록하는지와 기기 식별 범위를 공식 문서 기준으로 확인해볼게요. | (1) ZIP 다운로드는 흔적이 완전히 없지는 않다. (2) GitHub의 공식 문서를 기준으로 GitHub가 어떤 감사 이벤트로 기록하는지와 기기 식별 범위를 확인할 계획이다. | 2026-08-09T19:32:55.100Z | none |
| 17 | same text as seq 16 | (1) ZIP 다운로드의 흔적이 완전히 없는 것은 아니다. | 2026-08-09T19:32:55.103Z | none |
| 18 | ZIP도 **기록됩니다.** Enterprise 감사 로그에 `repo.download_zip` 이벤트가 남아서 회사 관리자는 보통 다음을 볼 수 있습니다. | (1) ZIP 다운로드는 Enterprise 감사 로그에 `repo.download_zip` 이벤트로 기록된다. (2) 회사 관리자는 ZIP 다운로드 감사 로그에서 다운로드한 계정, 저장소, 다운로드 시각, 브라우저의 `user_agent` 정보, 설정이 활성화된 경우 접속 IP를 볼 수 있다. (3) 같은 집 Wi‑Fi에서는 공인 IP가 동일하지만, ZIP 다운로드는 브라우저를 사용하므로 `user_agent`에서 macOS/Windows와 브라우저 종류·버전 차이가 드러날 수 있다. (4) GitHub는 ZIP 다운로드 감사 로그에서 기기명·시리얼 번호를 직접 보여주지 않으므로, 이것만으로는 개인 기기라고 확정되지 않는다. (5) SSO Device Trust, 회사 VPN, MDM/EDR, 프록시 등의 기록과 합치면 개인 기기 여부를 식별하거나 ZIP 다운로드 자체를 차단할 수 있다. (6) ZIP 다운로드는 clone보다 안 들키는 방식이 아니며, 오히려 `repo.download_zip`이라는 명시적인 다운로드 이벤트가 감사 로그에 남는다. | 2026-08-09T19:33:10.834Z | none |
| 19 | same text as seq 18 | six Facts paraphrasing the seq 18 set: `repo.download_zip` is recorded; what the admin sees; `user_agent` reveals OS/browser; no device name or serial; SSO Device Trust/VPN/MDM/EDR/proxy records identify or block; ZIP is not stealthier than clone | 2026-08-09T19:33:10.838Z | none |
| 21 | 그럼 지금 그 로직을 우리 레포에 포팅해줄 수 있냐? 우리 최신 레포 보면 구조 있음 ㅇㅇ | (1) 사용자가 현재 로직을 자신들의 레포에 포팅해줄 것을 요청함 (2) 사용자의 최신 레포에는 구조가 있음 | 2026-08-15T12:14:52.928Z | none |
| 23 | Error: fetch failed / Error: fetch failed / Operation aborted / Error: Retry failed after ... (pasted tool error output) | (1) SSH inonono에 token hub가 있는데 이것이 작동하지 않는 것 같다. (2) 즉시 복구하고 근본 원인을 파악할 것을 요청한다. | 2026-09-08T03:23:42.925Z | none |
| 24 | same error paste as seq 23 | (1) 바로 복구하고 근본원인파악 필요 | 2026-09-08T03:25:40.120Z | none |
| 25 | 지금 서버에 token hub 가 Error: Provider stream start timed out after 90000ms, overload 뜨는 데 이거 ... | (1) 서버에 token hub에서 'Error: Provider stream start timed out after 90000ms, overload' 에러가 발생하고 있다. (2) claude-lb와 codex-lb에서도 동일한 에러가 발생하고 있다. (3) 해당 에러에 대한 상세한 분석을 요청한다. | 2026-08-24T05:09:05.943Z | none |
| 26 | jq가 비JSON 라인에서 끊겼습니다. 보완해서 전체를 다시 집계하고, 각 에러의 상세 필드를 봅니다. | (1) jq가 비JSON 라인에서 끊겼습니다. (2) 비JSON 라인에서의 jq 끊김을 보완해서 전체를 다시 집계하고, 각 에러의 상세 필드를 봅니다. | 2026-08-24T05:10:35.100Z | none |
| 27 | 두 개의 별개 타겟이 보입니다. 타겟 ID를 이름과 매핑하고, 809건이나 되는 model discovery 실패의 정체도 확인합니다. | (1) 두 개의 별개 타겟이 보인다. (2) 타겟 ID를 이름과 매핑할 필요가 있다. (3) 809건의 model discovery 실패가 발생했으며, 그 정체를 확인할 필요가 있다. | 2026-08-24T05:11:01.039Z | none |
| 28 | 타겟 매핑 완료: `static-2feff7a3` = codex.nekos.me (90초 타임아웃), `static-8a61cacc` = claude.nekos. ... | (1) static-2feff7a3이 codex.nekos.me로 매핑되며 90초 타임아웃이 설정됨 (2) static-8a61cacc이 claude.nekos.me로 매핑됨 (3) 타겟 매핑이 완료됨 | 2026-08-24T05:12:45.278Z | none |

Redaction: no excerpt or Fact in the sample contained a filesystem path under
`/Users` or a credential; nothing needed masking. Excerpts for seq 12, 13, 25
and 28 were cut mid-sentence to stay under the length limit.

What the table shows about the anamnesis side. The extractor produces many
Facts per Episode when the source is a list-shaped assistant answer (6 for seq
8, 12 and 18) and one or two for short user turns. Duplicate Episodes yield
paraphrase Facts that both survive as ACTIVE, for example seq 8 Fact 3 and seq
9 Fact 2, which differ by one particle. That is consistent with the D53
candidate rule (same MENTIONS Entity required) or with a `duplicate` verdict
under 0.6 confidence; the artifact doesn't say which. Fact time is the Episode
time in every sampled row; none of these Episodes contained an absolute date
for the prompt to lift.

### 9.3 What Graphiti would do with five of these Episodes

Method. Each paragraph predicts Graphiti's output from the dossier alone.
Graphiti wasn't run on these Episodes. Entity names are guesses about what
`extract_nodes` would return; merge likelihoods follow the dossier's 0.6
cosine floor and BM25 on name/summary (dossier §4). Every statement below is a
prediction.

**seq 4 (audit log scope differs between Cloud and Server).** `extract_nodes`
would likely return "GitHub Enterprise Cloud", "GitHub Enterprise Server" and
possibly "github skill" (dossier §2, `graphiti_core/prompts/extract_nodes.py`).
`extract_edges` would produce one RELATES_TO edge between the two products
with `fact` close to anamnesis Fact 1, `valid_at` set to the episode
`valid_at` since the sentence is present tense with no date (dossier §3,
`extract_edges.edge()` default). Anamnesis Fact 2, the plan to check the docs,
has one entity at best ("official documentation") and a speaker who isn't an
extracted entity; it would either be dropped or attached as a low-value edge
between "speaker" and "documentation". `resolve_node` would then run on seq 5,
see the identical names, and merge both entities into the seq 4 UUIDs
(dossier §4, `dedupe_nodes.resolve_node`); `resolve_edge` would see the seq 5
edge as a duplicate of the seq 4 edge and reuse its UUID (dossier §3,
`dedupe_edges.resolve_edge`). Anamnesis instead wrote Fact 1 on seq 4 and a
differently worded plan Fact on seq 5, both ACTIVE. Predicted call count: 1
extract_nodes + 2 resolve_node + 1 extract_edges + 1 resolve_edge = 5 per
Episode (dossier §2).

**seq 8 (what `git.clone` audit events do and don't show).** Entities:
"GitHub Enterprise", "git.clone", "Enterprise Cloud", "Enterprise Server",
"REST API", "audit log streaming", possibly "NAT" and "VPN". `resolve_node`
would probably merge "GitHub Enterprise" and "Enterprise Cloud" with the seq 4
"GitHub Enterprise Cloud" node on name similarity, and update its summary with
the retention and streaming detail (dossier §4, name/summary merging via
`resolve_extracted_nodes`). Edges: "Enterprise Cloud has default retention 7
days" and "Enterprise Server requires admin to enable Git events" map cleanly
to Entity->Entity or Entity->attribute edges. Anamnesis Fact 2 (device name,
serial and MAC not visible) is a negation with one entity and would need a
second entity such as "device identity" to exist as an edge. `valid_at`:
episode time for all, none has a date. On seq 9 every edge would be a
`resolve_edge` duplicate at index 0 and collapse into the seq 8 UUIDs (dossier
§3); anamnesis kept 11 Facts across the pair. Predicted calls: 1 + about 7
resolve_node + 1 + about 6 resolve_edge, roughly 15.

**seq 10 (user asks whether home Wi-Fi hides which laptop cloned).** Entities:
"user", "company MacBook", "home Wi-Fi", "GitHub". This is a question, not an
assertion. Graphiti's `extract_edges` prompt extracts facts from messages and
has no speech-act field in the dossier (dossier §2), so it would likely emit an
edge like "user cannot tell company MacBook from personal device on home
Wi-Fi" with `valid_at` = episode time, stated as if true. Anamnesis produced a
single hedged reported-speech Fact ("...구분할 수 없다는 것을 알 수 없다고
말함"), awkward but attributed. Neither system has a clean representation for
a question; Graphiti's is more confident than the source supports. Predicted
calls: 1 + 4 + 1 + 1 = 7.

**seq 25 (token hub returns "Provider stream start timed out after 90000ms,
overload").** Entities: "token hub", "server", "claude-lb", "codex-lb", and
possibly the error string as a concept. `resolve_node` would search earlier
episodes; "token hub" from seq 23 (SSH inonono) is a near-certain merge on
exact name, so the seq 25 summary would absorb "not working" from seq 23
(dossier §4). Edges: "token hub emits timeout/overload error", "claude-lb
emits same error", "codex-lb emits same error", each `valid_at` = episode
time 2026-08-24T05:09:05Z. Anamnesis Fact 3, the request for analysis, is a
speech act with no second entity and would have no home. Contradiction check:
seq 23's "token hub seems not to work" (2026-09-08) is later than seq 25 in
event time but was ingested earlier (lower seq), so when seq 25 arrives its
`valid_at` is older than the seq 23 candidate; the dossier's rule only
invalidates when the new edge is more recent (dossier §3), so no invalidation,
and the two edges coexist as they do in anamnesis. Predicted calls: 1 + 4 + 1
+ 3 = 9.

**seq 28 (target IDs mapped to hostnames).** Entities: "static-2feff7a3",
"codex.nekos.me", "static-8a61cacc", "claude.nekos.me". Edges: two clean
RELATES_TO triples, "static-2feff7a3 maps to codex.nekos.me (90 s timeout)"
and "static-8a61cacc maps to claude.nekos.me", `valid_at` = episode time.
This is the row where Graphiti's model fits best: two named endpoints per
fact, an attribute (timeout) that `extract_attributes_from_nodes` could type
(dossier §4). Anamnesis Fact 3 ("mapping complete") is a status statement
with no entity and would be dropped by Graphiti; anamnesis kept it as a
Fact with MENTIONS to nothing. Seq 27's "two distinct targets are visible"
would be a candidate the `resolve_edge` prompt sees as related but not
contradictory. Predicted calls: 1 + 4 + 1 + 2 = 8.

### 9.4 What the G7 run does not show

No Graphiti instance was executed on these Episodes. Everything in §9.3 is
inferred from the dossier's reading of Graphiti source and docs at commit
1980db2, and the dossier itself flags parts of §7 and §8 as unverified.
There's no Graphiti entity count, edge count, call count, latency or
contradiction outcome to put beside the anamnesis numbers, and the merge
predictions depend on embedding scores nobody computed.

The run also doesn't show the relation judge's precision. The summary counts
12 CONTRASTS and 9 duplicate refusals but records neither the candidate sets
nor the reasons, and none of the 20 sampled Episodes carries a link, so the
sample can't be used to check a single verdict. The `MaterializationOperation`
records D53 describes would be the place to look; they weren't exported into
this artifact. Finally, the corpus is one duplicated Codex rollout plus
converted Claude turns, not a natural multi-week personal history, so
duplicate pressure is artificially high and contradiction pressure is
artificially low.

## 10. Cost and latency

Anamnesis G7 run 3 (`e2e-summary.json`): 200 Episodes admitted, 421 active
Facts, 200 Episode vectors, 21 relation links, recall channels bm25 + vector
with `ppr_used` true on 5/5 queries; `durations.workers_ms` 987,029 for the
extraction, relation and embedding lanes together, `ingest_ms` 42,031,
`recall_ms` 2,631, `total_ms` 1,049,148; model `claude-haiku-4-5`;
`llm_min_interval_ms` 3000. The artifact gives no separate embedding
duration and no concurrency figure; neither is stated here.

| Quantity | Anamnesis (G7) | Graphiti (dossier) |
|---|---|---|
| LLM calls per Episode | 2 base calls (`claim`, `judge_claims`) plus 1 `judge_relations` call per validated claim whose same-Entity candidate list is non-empty: `judgeFactRelations` loops over every pending premise of the pipeline and calls the provider once each (`engine.ts` L199-215; `extract-claims.v2.md`; `judge-relations.v1.md`; D53). 2 when no claim has candidates; 2 + N for N candidate-bearing claims (an Episode with six such claims costs 8). Retries on provider failure are extra. Entity resolution and Fact write are deterministic (D52) | "~5" per episode: extract_nodes, dedupe_nodes, extract_edges, dedupe_edges, attributes (dossier §8, UNVERIFIED). Expanded: 1 extract_nodes + 1 resolve_node per entity + 1 extract_edges + 1 resolve_edge per edge + 1 extract_timestamps per edge missing times + attribute calls per typed node/edge (dossier §2) |
| Calls for 200 Episodes | Not measured: the artifact records no provider call count. The lower bound is 400 (2 per Episode); there is no fixed upper bound from the call shape because relation calls scale with candidate-bearing validated claims (421 active Facts across 200 Episodes means about 2.1 validated claims per Episode on average, so a rough expectation is 400 plus a few hundred relation calls, plus retries) | At least 1000 by the dossier's rough figure; §9.3 predictions run 5 to 15 per Episode on this corpus. No seconds are given in the dossier and none are invented here |
| Wall time per Episode | 987,029 ms / 200 = about 4.9 s per Episode of wall time across the worker lanes (arithmetic on `durations.workers_ms`), with up to 4 pipelines in flight, so roughly 20 s of lane time per Episode: 2 to 2+N sequential haiku round trips through the token-hub tunnel plus Neo4j writes. Run 6 (`e2e-run6/e2e-summary.json`, tree 88debb6) measured 689,456 ms for the same 200 Episodes, about 3.4 s per Episode: the spread between runs is provider latency, not configuration. The artifact doesn't expose the per-lane split | Dossier §7 cites "typical 1-5 s per episode with OpenAI API"; that section is UNVERIFIED and not a measurement |
| Embedding | 200 Episode vectors drained, `quarantined_total` 0, inside the same worker window (in run 6 the embedding lane was already idle at 200/200 when the extraction lane had covered 12 Episodes); no separate timing in the artifact | Per-node name embedding and per-edge fact embedding (dossier §1); no timing in the dossier |
| Concurrency | Not recorded in the artifact; design is a strict sequencer per generation (`docs/02-daemon-and-pipelines.md` §5) | `SEMAPHORE_LIMIT=20` (dossier §2) |
| Recall LLM cost | Zero by design (`docs/02-daemon-and-pipelines.md` §8) | Zero for RRF/MMR/node-distance; about 1 LLM call per candidate for cross-encoder (dossier §7, UNVERIFIED) |
| Recall latency | 2,631 ms for 5 queries in G7, about 526 ms each, measured through the harness on an ephemeral Neo4j; target p50 < 100 ms, p95 < 250 ms (`docs/05-recall.md` §9). The G7 figure is above target and includes client round trips | UNVERIFIED (dossier closing note) |

Two caveats. The anamnesis 3-call figure buys no entity dedup, so it's still
not like-for-like with Graphiti's per-entity fan-out; it does now buy
contradiction and duplicate judgement over same-Entity candidates. And the
wall time is provider-bound, not configuration-bound: the daemon applies no
inter-call pacing (the driver's `llm_min_interval_ms` field is inert for the
daemon path), so 987 s (run 3) versus 689 s (run 6) for the same corpus is the
variance of haiku round trips through the token-hub tunnel at 4 pipelines in
flight, and the 30 s lease cap in force during run 3 was tight enough for one
slow round trip to expire a lease (§9.1; raised to 90 s in e57db17). A per-call
latency histogram is needed before comparing model time with Graphiti.

## Open items for the maintainer

- Export relation judge operations (`MaterializationOperation.result.relations`)
  into the E2E artifact so a future comparison can check verdict precision on
  the sample instead of counting links.
- Decide whether same-Entity-only candidate selection is enough (D53), or
  whether a Fact fulltext or vector neighbour channel should feed the judge;
  the seq 8/9 paraphrases are the test case.
- Decide whether Entity alias handling is wanted, or whether "GitHub
  Enterprise" and "GitHub Enterprise Cloud" staying separate is acceptable
  for a personal corpus (§5).
- Fact ranking: Facts trailed Episodes in 4/5 recall queries (§6, §9.1).
- Re-verify dossier §7 and §8 before quoting any provider, index or cost
  figure from them outside this document.
- Record per-call provider latency in the daemon's worker status so §10 can
  separate model time from Neo4j and tunnel time (§10).

## Sources

Graphiti (all via `.omo/research/graphiti-dossier.md`, describing
getzep/graphiti main @ 1980db2):

- `graphiti_core/nodes.py` (EntityNode, EpisodicNode, CommunityNode, SagaNode)
- `graphiti_core/edges.py` (EntityEdge, EpisodicEdge, CommunityEdge,
  HasEpisodeEdge, NextEpisodeEdge)
- `graphiti_core/prompts/extract_nodes.py`
- `graphiti_core/prompts/extract_edges.py` (`edge()`, `extract_timestamps()`)
- `graphiti_core/prompts/dedupe_nodes.py` (`resolve_node()`)
- `graphiti_core/prompts/dedupe_edges.py` (`resolve_edge()`)
- `graphiti_core/utils/maintenance/node_operations.py`
  (`resolve_extracted_nodes`)
- `graphiti_core/utils/maintenance/edge_operations.py`
  (`_extract_edge_timestamps`, `resolve_edge_contradictions`)
- `graphiti_core/search/search_utils.py` (`node_similarity_search`,
  `node_fulltext_search`, `hybrid_node_search`)
- Dossier sections cited: §1 data model, §2 ingestion, §3 temporal model, §4
  entity resolution, §5 retrieval, §6 forgetting, §7 operations (UNVERIFIED),
  §8 limitations (UNVERIFIED)

Anamnesis:

- `.omo/evidence/auto-pipeline/g7/e2e/e2e-summary.json` (G7 run 3,
  2026-09-24)
- `.omo/evidence/auto-pipeline/g7/e2e/sample-facts.json`
- `app/anamnesis/prompts/judge-relations.v1.md`
- `app/anamnesis/prompts/extract-claims.v2.md`
- `docs/00-overview.md`, `docs/01-storage.md`, `docs/02-daemon-and-pipelines.md`,
  `docs/03-time.md`, `docs/04-forgetting.md`, `docs/05-recall.md`,
  `docs/06-envelope-ppr.md`, `docs/09-roadmap.md`, `docs/deploy.md`
- `docs/10-decision-log.md` D43, D46, D48, D49, D50, D51, D52, D53
- `packages/core/src/store.ts`, `packages/core/src/engine.ts`,
  `packages/protocol/src/semantic-claim.ts`, `packages/protocol/src/link.ts`
- `scripts/qa/e2e-real.ts`
- commit 650eca8 (extraction lane re-arm fix after lease expiry, run 2)
