# 09 — Roadmap

This is the only document that describes unbuilt work. Docs 00 to 08 describe
what the code does; when they mention a target, they point here. Every entry
names what ships today, so the boundary stays visible.

## Where the code is today

The repository (branch `anamnesis2`) holds `packages/protocol`,
`packages/core` and the `app/anamnesis` daemon with its ops CLI. It runs:

- direct `remember` into Neo4j with the two Episode digest versions, payload
  upload, lineage admission and room/space/reply caches (docs/01, docs/02 §3);
- the extraction scheduler with windowed claims, the optional Jev pre-gate,
  validated Fact writes, the relation judge and generation cutover
  (docs/02 §5);
- the Fact embedding lane (docs/01 §4);
- `recall` as `derived-hybrid-v1`: Episode exact-ID, BM25 and session
  channels, Fact BM25 and vector channels, uniform-seed PPR over seed-adjacent
  ConductingArc rows, policy and validity filters, exact budgets and one
  durable Usage per recall ([05-recall](05-recall.md));
- `report_outcome` with signed Episode credit (docs/04);
- content-scope policy by Episode or source, the identity lane, the markdown
  projection, journaled backup and restore, and the `graph.envelope`
  diagnostic read.

Neo4j holds memory and the cursors that resume work. Pipeline bookkeeping is
in state files under the runtime root (D54, D55). Collected raw data stays
outside the graph; the archive is the collection source of truth and Neo4j is
the memory (D56).

## Future register

Each entry is a target, not a contract. Promoting one means a decision-log
entry, code and tests in the same change.

| Target | Ships today | Evidence needed before it ships |
|---|---|---|
| Entity and relationship vector channels | Fact vectors only, one `vec_fact_<profileId>` index per profile; no Entity or `RELATES_TO` vector property or index | a profile and index scheme for the new targets, coverage rules, and a recall evaluation showing the channel helps |
| Fact session channel | Episode session channel (last 32 Episodes); the per-Fact session window is assembly, not a channel | a bounded per-Episode Fact read and an evaluation against the window |
| Entity identity channel and the `entities` block | Episode exact-ID lookup (`identity_mode: "exact_episode_id"`); `entities` is always empty; the D59 identity lane is not read by recall | linking extracted Entities to Persons, and a channel that doesn't leak denied mentions |
| Hub damping | every candidate seeds with weight 1; saturated sources are skipped only in `graph.envelope` | a degree signal recall can read cheaply, and an ablation (docs/07 §7) |
| Top-128 seed cap | no cap; every distinct candidate seeds | seed ranking, then the cap |
| 2,000-node / 20,000-link bounded online envelope and its timeout degradation | recall reads seed-adjacent rows with `LIMIT 1024` and no timeout; `graph.envelope` has 2,000/20,000 caps but recall doesn't call it | a two-hop mass-ordered read, its deadline and whole-channel drop, and truncation validation against full-view GDS |
| D46 occurrence grouping | every occurrence is its own Fact; recall returns up to four `CONTRASTS` peers per Fact with no counts or completeness flags | a pinned grouping key, the counted conflict scan with `conflict_total` / `conflict_truncated` / `conflict_redacted`, and their warnings |
| Dreaming | no `dream` RPC, job store, Community writer, synthesis or profile cache; `HAS_MEMBER`, `anamnesis.community/1` and `anamnesis.synthesis/1` are registered with no writer | a bounded export, a networkless GDS Leiden run, a pinned community generation and exact-support synthesis |

Other open items, each smaller:

- **m₀ priors.** Fact `m₀` is the judge's confidence. The `sub_kind` and
  `modality` prior tables are calibration targets.
- **Change versus correction.** The relation judge writes INVALIDATES at the
  invalidating Fact's own time. A correction mode that backdates to the
  target's time, and a replacement Fact that restores a wrongly invalidated
  one, are designed (archived D18) and not built.
- **Broader policy selectors.** Only `episode_id` and `source` with `content`
  scope exist. Entity, schema, sub-kind, modality and literal selectors and a
  `derived` scope were designed under D43.
- **Embedding profile lifecycle.** One fingerprint per profile today. Separate
  model and index fingerprints, a qualification record, a build lifecycle and
  a production-activation block were designed under D51.
- **Scale benches and truncation validation** (docs/07 §3, §4). No latency or
  scale number in this document set is a measurement.
- **Generation garbage collection.** Retired generations stay on disk.

## Not doing

- Multi-user operation or auth beyond the installation token and the optional
  TCP bearer. It is a personal engine.
- A second graph store outside Neo4j (SQLite, custom format) (D0).
- LLM calls on the recall path. Summarization and re-ranking belong to the
  caller.
- Transaction time / bitemporal storage (docs/03 §6). `known_by` is one
  `ingested_at` comparison, not a record-time axis (D57).
- Recursive INVALIDATES (docs/03 §4).
- Tick- or schedule-based forgetting updates (docs/04).
- Erasure. Policy suppresses serving and extraction; originals, backups and
  privileged raw access stay outside it (D43).
- Automatic execution of instructions found in ingested text. Policy is an
  authenticated RPC command only (D43).
- Token estimates. A `tokens` budget without a pinned installed tokenizer is
  rejected (D44).
- Per-Fact accessibility. Hits attach to Episodes; sibling Facts of a credited
  source refresh together (D1).
- Automatic contradiction winners by confidence or recency (D46).
- Automatic query translation or two-query fusion on the recall path (D48).
- English-normalized canonical Facts. `fact_language_policy` is a
  per-generation setting, and generated English renderings are neither stored
  nor indexed (D48).
- Corroboration by repetition. Occurrence count, root count and echo depth
  never move confidence, mass, utility, ranking or a conflict winner (D49).
- Truncating, chunking or zero-filling an embedding input to produce a vector
  (D54).
