# Local LoCoMo Official-F1 Development Benchmark

Date: 2026-07-25

Status: development result, not a release or leaderboard claim.

This run replaces the earlier generic same-model-judge score with LoCoMo's
official deterministic category-aware token F1. It measures the shipped
`Memory` retrieval/package path with a local Qwen reader. The sample is frozen
for paired development comparisons, but it excludes adversarial questions and
is not the official full split.

## Frozen Protocol

| Item | Value |
|---|---|
| Source | [snap-research/locomo](https://github.com/snap-research/locomo), pinned upstream snapshot |
| Dataset fingerprint | FNV-1a64 `fdf74317b9a55716`, 1,567,996 bytes |
| Loader | `locomo-caption-v2+longmemeval-cleaned-v1`; includes `blip_caption` |
| Selection | 25 seeded questions/type; adversarial excluded |
| Seed | 42, stable hash stratification |
| Questions | 100 across all 10 conversations |
| Engine revision | `4f8821066beae6a80ff5a0de7f677a7dea721cab` plus benchmark-only working-tree changes |
| Embeddings | `Xenova/bge-base-en-v1.5` |
| Reader | `qwen3.5:35b-a3b`, digest `3460ffeede54…` |
| Runtime | Ollama 0.31.1, loopback only |
| Generation | non-thinking, temperature 0.7, top-p 0.8, top-k 20, presence penalty 1.5, seed 42 |
| Context/output | 32,768 / 512 tokens |
| Prompt | `official-format-v5` |
| Primary metric | official LoCoMo deterministic F1 |
| Uncertainty | deterministic 10,000-resample question bootstrap |
| Local LLM judge | disabled |

## Baseline Result

| Package cutoff | Official F1 | Bootstrap 95% CI | Package Recall | Package Hit | Mean context chars |
|---:|---:|---:|---:|---:|---:|
| 5 | 26.4% | 19.0–34.2% | 43.5% | 56.0% | 1,209 |
| **10** | **31.9%** | **24.2–39.7%** | 52.9% | 66.0% | 2,578 |
| 15 | 26.4% | 19.4–33.7% | 57.7% | 70.0% | 3,774 |
| 20 | 29.7% | 22.7–37.3% | 60.1% | 71.0% | 4,936 |

Top 10 is the best tested reader-facing cutoff. Recall rises monotonically
through top 20, while answer F1 does not. This is direct evidence that retrieval
recall alone is not an adequate product quality gate.

The top-20 dataset-annotated-evidence route scores 42.2% F1
(95% CI 34.2–50.2%). Its type breakdown is:

| Type | Annotated evidence | Memory top 10 | Memory top 20 |
|---|---:|---:|---:|
| Multi-hop | 38.2% | 29.9% | 33.7% |
| Open-domain | 39.5% | 26.0% | 24.1% |
| Single-hop | 78.4% | 60.9% | 52.8% |
| Temporal | 12.7% | 10.9% | 8.2% |

The 10.3-point annotated-evidence-to-top-10 gap is attributable to
retrieval/context selection. The low annotated-evidence temporal score shows a
separate reader/prompt/date-reasoning ceiling that should not be "fixed" by
changing graph dynamics.

For orientation only, the [official LoCoMo paper](https://aclanthology.org/2024.acl-long.747/)
reports 39.7% overall F1 for its GPT-3.5 dialog-RAG top-10 setup, 51.6% for
GPT-4 Turbo long context, and 87.9% for humans. Those use different readers and
the full benchmark, including adversarial questions, so they are not
apples-to-apples comparisons. The current 31.9% development score is useful
engineering evidence but is not yet a defensible headline memory-engine result.

## Rejected Package Experiments

Every experiment used the exact same 100 questions, reader, generation
settings, and official scorer. No candidate was retained in the engine.

| Candidate | Cutoff | F1 | Recall | Paired F1 change | Paired bootstrap 95% CI |
|---|---:|---:|---:|---:|---:|
| Prompt-only same-turn compaction | 20 | 29.1% | 60.1% | -0.6 pp | -3.9 to +2.6 pp |
| Always prefer Semantic view (`v1`) | 20 | 28.6% | 62.3% | -1.1 pp | -6.3 to +4.2 pp |
| Preserve higher-ranked view + Episodic L2 (`v2`) | 20 | 28.8% | 65.2% | -0.9 pp | -5.8 to +4.1 pp |
| Preserve higher-ranked view + Episodic L2 (`v2`) | 10 | 30.3% | 56.5% | -1.6 pp | -6.5 to +3.3 pp |
| Prompt-only Episodic hydration | 10 | 30.1% | 52.9% | -1.8 pp | -6.7 to +3.1 pp |

The package candidates increased evidence recall by as much as 5.1 points but
did not improve answer F1. They were reverted rather than promoted on a
retrieval-only metric. This preserves the graph's Episodic/Semantic dual-view
model, contradiction surfacing, provenance behavior, and public API.

## Decision and Next Gate

- Keep the engine unchanged.
- Use top 10 as the current local-reader development configuration, not yet as
  a shipped default.
- Move the next ablation to query-aware reranking/coverage. Do not mutate
  attraction, forgetting, reinforcement, or graph storage to compensate for
  reader distraction.
- Audit hit-positive answer losses separately from retrieval misses.
- Require a paired n=100 improvement whose 95% interval excludes zero before a
  product change is retained.
- Then run the full LoCoMo split, including adversarial questions, and publish
  that immutable report as the release-quality claim.

## Immutable Result Files

| Run | SHA-256 |
|---|---|
| Baseline top 5 | `5b90d6393eee6e76d779c2dcb4b02048acedb1af682bdafa52b04b3fd2925dad` |
| Baseline top 10 | `f4385f9077e61bf33c0ad2d514ec4a58765923521d13fcde9e257e5c1d61522b` |
| Baseline top 15 | `2ed3ee01058c8955e5778feb3e59c26469e04b4bfebae0f2d3d34f42b173c1eb` |
| Baseline top 20 + annotated evidence | `c4b824a5789796b2cf6425ffffe44fc2ad68a7c0cf12efc8035dd18efd18c79b` |
| Rejected prompt compaction top 20 | `f515868abcb2ef274a897ece0754ad235973db2f268253d1faf9d2e1938dd2a4` |
| Rejected v1 top 20 | `626b7e127de112fe789b280c0bf2912d7c7ca52a1a51693e36fac360ce17bdff` |
| Rejected v2 top 20 | `e6a3dbea631672fc07e6f929532a0664a21c09cddc2fad1e58f4a31244cb07af` |
| Rejected v2 top 10 | `f0ef9e61756b8021f7fbcbe78581bf7ac2b10f97aef7b0ab01054cb059290be1` |
| Rejected Episodic hydration top 10 | `3681354317a750e50dfe8ae23c1456303dba3946824f0ca7470f9d41db99dbce` |
