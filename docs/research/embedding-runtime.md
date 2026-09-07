# CPU embedding runtime probe: one host, one configuration

This probe checks how the pinned Qwen3-Embedding-0.6B Q8_0 artifact behaves
under llama.cpp on a CPU-only development host: what a normal request returns,
what an oversized request returns, and roughly how long a single request takes
at three input sizes. It is a diagnostic on one machine and one configuration.
It measures no throughput, no corpus duration, and no retrieval quality, and
nothing in it qualifies a production deployment.

Every request used synthetic public text. No production service, Neo4j
instance, private input, or credential was involved.

## Pinned configuration

| Element | Value |
|---|---|
| Artifact | `Qwen/Qwen3-Embedding-0.6B-GGUF`, revision `370f27d7550e0def9b39c1f16d3fbaa13aa67728`, file `Qwen3-Embedding-0.6B-Q8_0.gguf` |
| Artifact SHA-256 | `06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439`, 639,150,592 bytes |
| Server | llama.cpp `0.4.0-dev`, build 10819, commit `6a1a922d269908a29cbd4b49c27e6a8e7fd10fae` |
| Compute | CPU only (`-ngl 0`), 4 threads |
| Context and pooling | `-c 4096`, `--pooling last`, embeddings enabled |
| Binding | loopback `127.0.0.1:18089` |

The artifact identity above is the same one
[01-storage §4](../01-storage.md) pins as the experimental baseline profile,
and it resolves against the official Hugging Face revision API at
<https://huggingface.co/api/models/Qwen/Qwen3-Embedding-0.6B-GGUF/revision/370f27d7550e0def9b39c1f16d3fbaa13aa67728?blobs=true>.

## Command

The server ran from a per-user cache directory on the development host, with
no installation, download, or system change:

```sh
"$PILOT_HOME"/bin/llama-server \
  -m "$PILOT_HOME"/model/Qwen3-Embedding-0.6B-Q8_0.gguf \
  --host 127.0.0.1 --port 18089 -ngl 0 -t 4 -c 4096 --pooling last --embeddings
```

`--embeddings` is required by this build. Launching without it returned HTTP
501 with `This server does not support embeddings. Start it with --embeddings`;
that first process was stopped and the port confirmed free before the measured
launch. `/health` returned HTTP 200 about one wall second after start, and the
server log reported `n_threads = 4`, `n_ctx_slot = 4096`, and the model loaded.

## Observed HTTP results

| Request | Status | Observed result |
|---|---:|---|
| `GET /health` | 200 | `{"status":"ok"}` |
| `POST /v1/embeddings`, short input | 200 | One embedding of 1024 dimensions; every value finite |
| `POST /v1/embeddings`, oversized input | 400 | `exceed_context_size_error`, `n_prompt_tokens: 5002`, `n_ctx: 4096`, no vector returned |

The short input was a one-sentence synthetic probe string; its `/tokenize`
count on the same server was 19. The oversized input was the token `probe `
repeated 5,000 times. `/tokenize` counted 5,001 tokens for it, while the
embedding endpoint counted 5,002, because request framing adds one token. That
gap is the reason preflight has to tokenize the complete endpoint-formatted
request rather than trusting a raw `/tokenize` count.

The rejection is an observation about this request on this server, not a
general truncation guarantee. What it does establish is that this
configuration returned an error instead of a quietly shortened vector, which
is the behavior [D51](../10-decision-log.md) requires the worker to fail
closed on.

## Single-sample wall times

Each stratum used `probe ` repeated the indicated count, measured with
`POST /tokenize`, then sent once to `POST /v1/embeddings`.

| Input | Measured tokens | Embedding status | Wall time |
|---|---:|---:|---:|
| `probe ` repeated 128 times | 129 | 200 | 269 ms |
| `probe ` repeated 512 times | 513 | 200 | 789 ms |
| `probe ` repeated 2,048 times | 2,049 | 200 | 3,553 ms |

One sample per row. No warmup protocol, no repetition, no interval, no
concurrency. Treat these as an order-of-magnitude sanity check: CPU embedding
of a kilotoken input on this host costs seconds, not milliseconds.

## What this does not measure

- Exact boundary behavior at or just under 4,096 tokens.
- Batch and array requests, and any client that pre-truncates before sending.
- Worker-level behavior: retries, blocked entries, coverage advance.
- Sustained throughput, thread scaling beyond 4, memory, or contention with a
  concurrently loaded Neo4j.
- Full-corpus completion time. An earlier 6-113 hour projection extrapolated
  from single samples like these is withdrawn; nothing here supports a corpus
  estimate.
- Retrieval quality, and any Q8_0 versus FP16 parity claim.

Those belong to the trigger-gated M4 and M5 qualification packages described
in [07-gds-validation §7](../07-gds-validation.md), which run only if a CPU
embedding worker or a vector-index cutover is actually proposed for
production.

## Cleanup

The probe owned exactly one server process. It was terminated at the end of
the run, the port was confirmed to have no listener afterwards, and every
temporary request, response, log, and PID file the probe created was removed.
No artifact, binary, or model file was added or modified on the host.
