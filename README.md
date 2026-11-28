# anamnesis

**A local-first memory engine for agents and humans.**

anamnesis is a resident memory engine that runs on your machine. Agents
connect to it, send it what happened, and ask it what it knows. The graph and
its indexes live in a single local Neo4j; raw payload bytes live next to it on
disk. The repeated numeric work of remembering (spreading activation,
forgetting, fusion) runs in TypeScript inside the daemon, bounded per
request. Neo4j GDS is used offline only, to check the solver. Dreaming is a
later-stage design and isn't part of this daemon
([09-roadmap](docs/09-roadmap.md)).

- **Three layers, three write disciplines.** Originals (episodes, payload
  metadata, hit ledger) are CREATE-only; derived structure (facts, entities,
  links, embeddings) is regenerable and versioned by generation; only caches
  are ever `SET`.
- **Natural language first.** Facts are normalized sentences, not typed
  triples. Structure is minimal: a closed set of eight edge roles
  (`MENTIONS`, `RELATES_TO`, `NEXT_EPISODE`, `REPLIES_TO`, `HAS_MEMBER`,
  `DERIVED_FROM`, `INVALIDATES`, `CONTRASTS`, defined in
  `packages/protocol/src/link.ts`) and one event time, on episodes and facts
  only.
- **One time axis.** `snapshot(T)` reconstructs what was true at a given
  moment. Contradictions are dated invalidation events, never destructive
  updates, and corrections are backdated. Entities have no event time.
- **Forgetting without a clock.** `m(now) = m₀ × R(t, S)` (power-law
  retention, testing-effect reinforcement) is computed at read time from a
  hit ledger attached to episodes, so re-extraction never resets what the
  system has learned about its own usage. No tick daemon.
- **Bounded recall.** Episode exact-ID, BM25 and session channels plus Fact
  vector and BM25 channels produce candidates. Personalized PageRank runs in
  TypeScript, seeded uniformly from those candidates over at most 1,024
  seed-adjacent arcs read in the recall transaction, with retained-row
  normalization and a fixed uniform dangling rule. RRF fuses the channels;
  mass and utility weight the result. Ordering is deterministic. The 2-hop
  envelope of 2,000 nodes / 20,000 links, hub damping and timeout degradation
  are targets in [09-roadmap](docs/09-roadmap.md#where-the-code-is-today),
  not code.
- **Local sovereignty.** One resident daemon (TypeScript on Node) and one
  local Neo4j container started with docker compose, bound to 127.0.0.1 with
  a per-install password. The runtime root defaults to `~/.anamnesis/`; the
  data authority is the Neo4j database plus the objects directory, and
  `anamnesis-ops backup` captures both.

## Documents

Design documentation lives in [`docs/`](docs/). Documents 00 to 10 are the
contracts. [09-roadmap](docs/09-roadmap.md#where-the-code-is-today) separates
what the code does today from what is still a target; don't read a target as
shipped behavior.

| Doc | Question it answers |
|---|---|
| [00-overview](docs/00-overview.md) | What is this, what are the invariants, where is everything |
| [01-storage](docs/01-storage.md) | What is stored where: layers, elements, link lattice, generations, filesystem |
| [02-daemon-and-pipelines](docs/02-daemon-and-pipelines.md) | Who writes: single writer, RPC, extraction |
| [03-time](docs/03-time.md) | Which world: `snapshot(T)`, derived visibility, change vs correction, non-recursive `INVALIDATES` |
| [04-forgetting](docs/04-forgetting.md) | How alive: m₀, stability, hit ledger on episodes, reinforcement, replay |
| [05-recall](docs/05-recall.md) | How to retrieve: candidates → PPR → RRF → assembly |
| [06-envelope-ppr](docs/06-envelope-ppr.md) | How far to look: budgets, normalization, convergence, determinism |
| [07-gds-validation](docs/07-gds-validation.md) | How accurate: solver validation against GDS, CI gates |
| [08-repo-and-release](docs/08-repo-and-release.md) | How it is built and shipped |
| [09-roadmap](docs/09-roadmap.md) | What runs today, and what comes next in what order |
| [10-decision-log](docs/10-decision-log.md) | Why: decisions since the 2026-09 design review |
| [deploy](docs/deploy.md) | How to install, run, load, back up and restore |
| [runtime-storage-development](docs/runtime-storage-development.md) | How to run storage tests against an owned Neo4j |
| [app/anamnesis](app/anamnesis/README.md) | How to configure and operate the daemon and `anamnesis-ops` |
| [background/](docs/background/) | Non-normative, dated: competitor comparison, field lessons from the graphiti fork |
| [research/](docs/research/) | Non-normative, dated: probes and design memos behind the decisions |

## Status

The September 2026 design (docs 00–10) supersedes earlier drafts. The code is two
packages, `packages/protocol` (the zod contract and its JSON Schema export) and
`packages/core` (storage, extraction, embeddings, recall), plus the
`app/anamnesis` daemon and its operator CLI. What runs today is listed in
[09-roadmap](docs/09-roadmap.md#where-the-code-is-today). Production runs the
daemon under the systemd units in `deploy/vm/` ([deploy](docs/deploy.md));
cleaned corpora are loaded with `anamnesis-ops load`
([deploy](docs/deploy.md#loading-a-cleaned-corpus)). The
[main roadmap issue](https://github.com/INONONO66/anamnesis/issues/186)
tracks remaining work.
