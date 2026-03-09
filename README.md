# Anamnesis

Cognitive graph engine for LLMs — knowledge with attraction, gravity, perception, and forgetting.

> Named after Plato's theory of anamnesis (ἀνάμνησις) — recollection of innate knowledge.

## Overview

Anamnesis is a Rust library that provides a cognitive graph engine for LLM-based agents. It models knowledge as a graph with physics-like properties:

- **Attraction**: Similar/related nodes cluster together (embedding similarity + co-occurrence)
- **Gravity**: Important nodes (high centrality) attract new knowledge
- **Perception**: Input gating — filters what enters the graph (novelty, confidence, budget)
- **Forgetting**: Time-based salience decay with reinforcement on access

## Structure

```
src/
├── graph/       # Core types: Node, Edge, Graph
├── mechanics/   # Cognitive dynamics: attraction, gravity, perception, forgetting
├── query/       # Query engine: spreading activation, subgraph extraction
├── storage/     # StorageAdapter trait + implementations
└── api/         # Public API surface
```

## Build & Test

```bash
cargo build          # Build
cargo test           # Run tests
cargo doc --open     # Generate docs
cargo bench          # Run benchmarks (when added)
```

## License

MIT
