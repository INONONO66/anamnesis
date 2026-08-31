# anamnesis

**A local-first memory engine for agents and humans.**

anamnesis is a resident memory engine that runs on your machine. Agents
connect to it, send it what happened, and ask it what it knows. Everything
lives in a single Neo4j store, split into two logical layers: an immutable
originals layer (episodes, payloads, hit ledger — CREATE-only) and a fully
rebuildable derived layer (claims, links, embeddings, caches).

- **Originals / derived separation** — originals are sacred; every derived
  structure can be deleted and rebuilt from the originals at any time.
- **Natural language first** — facts are normalized sentences, not typed
  triples. Structure is minimal: seven edge roles, one timestamp per element.
- **Time as a first-class axis** — every element carries exactly one event
  time; contradictions are recorded as dated invalidation events (which are
  just claims with an `INVALIDATES` edge), never as destructive updates;
  `snapshot(T)` reconstructs what was true at any moment.
- **Mass dynamics** — every element has an immutable birth mass; effective
  mass `m(T) = m₀ × R(t, S)` is evaluated at read time from a hit ledger
  (power-law decay, testing-effect reinforcement). No tick daemon.
- **Local sovereignty** — one npm install, one resident daemon
  (`anamnesisd`, TypeScript/Node), one local Neo4j container that the CLI
  manages. Everything lives under `~/.anamnesis/` and a single
  `neo4j-admin database dump` is a full backup.

## Documents

Design documentation lives in [`docs/`](docs/):

| Doc | Contents |
|---|---|
| [00-vision](docs/00-vision.md) | Philosophy, core concepts, design principles |
| [01-data-model](docs/01-data-model.md) | MemoryElement, MemoryLink, celestial labels, lattice, ingest semantics |
| [02-architecture](docs/02-architecture.md) | Daemon, Neo4j single store, process model, immutability discipline |
| [03-recall](docs/03-recall.md) | Recall pipeline, RRF × m(T), snapshot(T) |
| [04-pipelines](docs/04-pipelines.md) | Ingest (single transaction), extraction, entity resolution, dreaming |
| [05-comparison](docs/05-comparison.md) | Detailed comparison with Zep, Mem0, Supermemory, etc. |
| [06-repo-and-release](docs/06-repo-and-release.md) | Monorepo layout, build, npm distribution, CI |
| [07-roadmap](docs/07-roadmap.md) | Milestones v0.1 → v0.3 |
| [08-graphiti-lessons](docs/08-graphiti-lessons.md) | Field lessons from the graphiti fork |
| [09-cosmology](docs/09-cosmology.md) | Celestial model — node kinds, edge roles, mass dynamics summary |
| [10-dynamics-math](docs/10-dynamics-math.md) | Canonical math spec cards (decay, stability, PPR, RRF, dreaming) |
| [11-neo4j-dynamics](docs/11-neo4j-dynamics.md) | Where each computation lives on Neo4j (Cypher/GDS binding) |

## Status

Design documents are the source of truth; code follows them. The contract
(`packages/protocol`) and the v0.1 core (`packages/core` — Neo4j store,
single-transaction ingest, two-stage idempotency with divergence, celestial
labels, lattice enforcement, idempotent links, `NEXT_EPISODE` tree wiring)
are in place and tested. Mass dynamics, vector recall, and PPR land in v0.2.
