# anamnesis

**A local-first memory engine for agents and humans.**

anamnesis is a resident memory engine that runs on your machine. Agents connect
to it, send it what happened, and ask it what it knows. Underneath, an
immutable vault preserves every original record forever, while a fully
rebuildable memory space extracts, links, and recalls natural-language facts
over a single event-time axis.

- **Vault / engine separation** — originals are append-only and sacred; every
  derived structure (facts, links, embeddings, scores) can be deleted and
  rebuilt from the vault at any time.
- **Natural language first** — facts are normalized sentences, not typed
  triples. Structure is minimal: four link roles, one timestamp per element.
- **Time as a first-class axis** — every element carries exactly one event
  time; contradictions are recorded as dated invalidation events, never as
  destructive updates; `snapshot(T)` reconstructs what was true at any moment.
- **No servers, no Docker** — one npm install, one resident daemon
  (`anamnesisd`, TypeScript/Node), two SQLite files, one LanceDB directory. Everything
  lives in `~/.anamnesis/` and a directory copy is a full backup.

## Documents

Design documentation lives in [`docs/`](docs/):

| Doc | Contents |
|---|---|
| [00-vision](docs/00-vision.md) | Philosophy, core concepts, design principles |
| [01-data-model](docs/01-data-model.md) | MemoryElement, MemoryLink, time model, invalidation |
| [02-architecture](docs/02-architecture.md) | Daemon, storage layout, process model, listeners |
| [03-recall](docs/03-recall.md) | Recall pipeline, PPR, mass/decay, snapshot(T) |
| [04-pipelines](docs/04-pipelines.md) | Ingest, extraction, entity resolution, dreaming |
| [05-comparison](docs/05-comparison.md) | Detailed comparison with Zep, Mem0, Supermemory, etc. |
| [06-repo-and-release](docs/06-repo-and-release.md) | Monorepo layout, build, npm distribution, CI |
| [07-roadmap](docs/07-roadmap.md) | Milestones v0.1 → v0.3 |

## Status

Design phase. This branch (`anamnesis2`) is a ground-up redesign; documents
first, code second. The contract types (`packages/protocol`) are in place.
