# anamnesis

**A local-first memory engine for agents and humans.**

anamnesis is a resident memory engine that runs on your machine. Agents
connect to it, send it what happened, and ask it what it knows. The graph and
its indexes live in a single local Neo4j; raw payload bytes live next to it on
disk. The repeated numeric work of remembering — spreading activation,
forgetting, fusion — runs in TypeScript inside the daemon, strictly bounded
per request. Neo4j GDS is used offline only, to measure how much that
bounding costs.

- **Three layers, three write disciplines** — originals (episodes, payload
  metadata, hit ledger) are CREATE-only; derived structure (facts, entities,
  communities, links, embeddings) is regenerable and versioned by generation;
  only caches are ever `SET`.
- **Natural language first** — facts are normalized sentences, not typed
  triples. Structure is minimal: seven edge roles, one event time on episodes
  and facts only.
- **One time axis** — `snapshot(T)` reconstructs what was true at any moment.
  Contradictions are dated invalidation events, never destructive updates;
  corrections are backdated; Entity/Community have no event time, while
  rebuildable visibility thresholds make recall bounded.
- **Forgetting without a clock** — `m(now) = m₀ × R(t, S)` (power-law
  retention, testing-effect reinforcement) is computed at read time from a
  hit ledger attached to episodes, so re-extraction never resets what the
  system has learned about its own usage. No tick daemon.
- **Bounded recall** — vector + BM25 + session candidates seed a 2-hop
  envelope (≤ 2,000 nodes / 20,000 links) pulled in one Neo4j transaction;
  personalized PageRank runs on it in TypeScript with retained-row
  normalization and a fixed uniform dangling rule; RRF fuses channels; mass weights the
  result. Deterministic. Partial results are never used silently.
- **Local sovereignty** — one npm install, one resident daemon
  (`anamnesisd`, TypeScript/Node), one local Neo4j container the CLI manages,
  bound to 127.0.0.1 with a per-install password. Everything lives under
  `~/.anamnesis/`; the data authority is the Neo4j database plus the
  `objects/` directory, and `anamnesis backup` captures both.

## Documents

Design documentation lives in [`docs/`](docs/). Documents 00–10 are
normative; code follows them.

| Doc | Question it answers |
|---|---|
| [00-overview](docs/00-overview.md) | What is this, what are the invariants, where is everything |
| [01-storage](docs/01-storage.md) | What is stored where — layers, elements, link lattice, generations, filesystem |
| [02-daemon-and-pipelines](docs/02-daemon-and-pipelines.md) | Who writes — single writer, RPC, `structure_revision`, spool, extraction, dreaming |
| [03-time](docs/03-time.md) | Which world — `snapshot(T)`, derived visibility, change vs correction, non-recursive `INVALIDATES` |
| [04-forgetting](docs/04-forgetting.md) | How alive — m₀, stability, hit ledger on episodes, commit protocol, replay |
| [05-recall](docs/05-recall.md) | How to retrieve — candidates → seeds → envelope → PPR → RRF → assembly, degradation ladder |
| [06-envelope-ppr](docs/06-envelope-ppr.md) | How far to look — budgets, fanout, hubs, retained-row normalization, convergence, determinism |
| [07-gds-validation](docs/07-gds-validation.md) | How accurate — solver validation, envelope validation, CI gates |
| [08-repo-and-release](docs/08-repo-and-release.md) | How it is built and shipped |
| [09-roadmap](docs/09-roadmap.md) | In what order — v0.1 → v0.3 |
| [10-decision-log](docs/10-decision-log.md) | Why — decisions from the 2026-09 design review |
| [background/](docs/background/) | Non-normative: competitor comparison, field lessons from the graphiti fork |

## Status

The design (docs 00–10) was rewritten in September 2026 and supersedes the
earlier drafts. Only `packages/protocol` (zod contract) and a first cut of
`packages/core` (Neo4j store, single-transaction ingest, idempotency with
divergence, lattice enforcement, idempotent links, `NEXT_EPISODE` wiring)
exist today; the package layout in [08-repo-and-release](docs/08-repo-and-release.md)
is the target. Known gaps between that code and the docs — payload bytes
stored as base64 on a node, no `revision_key`, a default Neo4j password, a
time required on every element kind — are closed in v0.1
([09-roadmap](docs/09-roadmap.md)).
