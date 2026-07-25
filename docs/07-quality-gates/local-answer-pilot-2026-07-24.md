# Local Answer-Accuracy Pilot — 2026-07-24

This report validates the local-only answer benchmark harness. It is **not a
headline benchmark score**: the sample is ten deterministically selected
questions (the first question of each type), with no confidence interval.

## Configuration

| Component | Value |
|---|---|
| Runtime | Ollama 0.31.1, loopback only |
| Baseline reader | `qwen2.5:latest` (`845dbda0ea48…`) |
| Strong reader | `qwen3.5:35b-a3b` (`3460ffeede54…`) |
| Separate judge | `gemma3:12b` (`f4031aab637d…`) |
| Embedding | `Xenova/bge-base-en-v1.5` |
| Retrieval | shipped `Memory` API, top 20, no speaker-cue ablation |
| LoCoMo selection | first question/type, adversarial excluded (4 questions) |
| LongMemEval-S selection | first question/type (6 questions) |

Dataset files:

| Dataset | SHA-256 |
|---|---|
| LoCoMo transformed snapshot | `f554f9e8c7f690981e6c6a8e11092183668218b27cbfa043a23bc751a607d0d3` |
| LongMemEval-S pinned snapshot | `08d8dad4be43ee2049a22ff5674eb86725d0ce5ff434cde2627e5e8e7e117894` |

The LoCoMo transformed snapshot preserves `session_*_date_time`. Category
labels use the official mapping: multi-hop, temporal, open-domain, single-hop,
and adversarial for categories 1 through 5 respectively.

## Results

| Dataset | Questions | 1. Gold + 7B | 2. Memory + 7B | 3. Memory + 35B | Package Recall@20 | Package Hit@20 |
|---|---:|---:|---:|---:|---:|---:|
| LoCoMo | 4 | 3/4 (75.0%) | 3/4 (75.0%) | 4/4 (100.0%) | 1.000 | 1.000 |
| LongMemEval-S | 6 | 3/6 (50.0%) | 3/6 (50.0%) | 2/6 (33.3%) | 0.917 | 1.000 |
| Combined pilot | 10 | 6/10 (60.0%) | 6/10 (60.0%) | 6/10 (60.0%) | — | — |

All 30 answer judgments parsed successfully. The judge prompt treats calendar
dates that differ by one day as incorrect and records its raw JSON response.

## What This Pilot Shows

- The old 7%/30% run is not a product score. Product-shaped ingestion,
  timestamps, embeddings, per-sample isolation, and a stricter separate judge
  produce a materially different diagnostic even on this tiny sample.
- LoCoMo temporal exposed a reader limitation: the 7B reader answered 8 May
  from evidence saying “yesterday” in a session dated 8 May; the 35B reader
  correctly answered 7 May.
- LongMemEval multi-session counting and temporal reasoning failed even with
  gold evidence for the 7B reader. Those failures cannot be attributed solely
  to retrieval.
- A larger reader is not monotonically safer. It recovered the preference
  question, but added conflicting details on knowledge-update and
  assistant-session questions, reducing the six-question LongMemEval result.
- There were zero cases where route 1 was correct and route 2 was incorrect in
  this pilot. That does not establish retrieval parity; the sample is too small.

## Required Next Measurement

Use the same frozen model manifests and run a larger stratified sample before
making a product claim. Report each dataset separately, retain raw contexts and
judge outputs, and manually audit a random subset of judge decisions. A full
split run should be treated as a scheduled benchmark job because LongMemEval
builds one isolated memory graph per question.
