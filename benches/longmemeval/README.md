# LongMemEval Benchmark

Measures Anamnesis accuracy on the LongMemEval benchmark.

## Setup

```bash
# Download dataset (requires Python + datasets package)
bash benches/longmemeval/download.sh

# Or: pip install datasets && python3 benches/longmemeval/download.py
```

## Running

```bash
# Run loader (check dataset availability)
cargo bench --bench longmemeval-loader

# Run baseline measurement (after dataset download)
cargo bench --bench longmemeval-baseline -- --output results.json --limit 50 --judge mock
```

## Dataset

- Source: https://github.com/xiaowu0162/LongMemEval
- License: Apache 2.0
- Paper: "LongMemEval: Benchmarking LLM long-term memory agents"

## Results

See `docs/cycle-1-baseline.local.md` for baseline and post-cycle-1 measurements.
