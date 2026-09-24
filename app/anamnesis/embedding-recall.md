# Original-Episode embedding and hybrid recall increment

This implements `originals-hybrid-v1`, not the complete future pipeline in
`docs/05-recall.md`. `hello.capabilities.recall=true` names this operational
original-Episode API. It is not a semantic-quality claim. `extraction` remains
false; `embeddings` is true only with explicit provider configuration.

## Provider configuration

Set `ANAMNESIS_EMBEDDING_CONFIG` to a JSON object with:

- `endpoint`: HTTP(S) URL of an operator-configured embedding endpoint.
- `profile`: `model`, `model_incarnation` (64 lowercase SHA-256 hex),
  `dimensions` (1..4096), `document_prefix`, `query_prefix`, `max_input_bytes`
  (1..65536), `norm: "unit_l2"`, `norm_tolerance` (>0 and <=0.01).
- `timeout_ms`: 1..30000, default 5000.

The incarnation must identify frozen weights/runtime/pooling and associated
configuration, not a mutable model alias. The profile digest also binds both
prefixes, dimension, norm contract and input cap. The operator is responsible
for the truth of the provider's incarnation assertion; a digest echo cannot
attest to remote weights.

The server sends one POST with `model`, `model_incarnation`, `dimensions`,
`input` (the exact original/query with its pinned prefix), and `truncate:false`.
The response must contain the matching `model` and `model_incarnation`, and
`data: [{index: 0, embedding: [...]}]`. Provider responses are streamed under a
256 KiB cap. Inputs are never truncated; vectors are never padded or normalized.
A generic OpenAI-compatible endpoint without incarnation echo is deliberately
not sufficient. Use an explicitly configured compatible local service/proxy.
No remote credentials, model download, dependency or built-in semantic fallback
is supplied by this increment.

The deterministic local HTTP server in `embedding-recall.surface.mjs` is only
a test fixture with prescribed vectors. It is not an installed semantic model.
Neither its retrieval accuracy nor a private model's efficacy is claimed.

## RPC and durability

- `embedding.recover {operation_id, episode_id}` processes one original Episode
  with the configured profile. Input revision/digest and pending status commit
  before the provider call. Completion persists `succeeded`, `quarantined` or
  `deferred`. Only `provider_unavailable` (timeout, 5xx, refused socket) defers;
  every other reason quarantines. The row's `detail` names the failing branch
  (`http 503`, `timeout 30000ms`, a socket code, `8193 bytes > 8192`).
- Reusing a completed operation is a durable no-op. Retry a quarantined or
  deferred attempt with a new operation ID; reuse a pending ID after process
  interruption.
- A terminal outcome (`succeeded` or `quarantined`) retires the Episode's
  queued outbox entry in the same transaction, whether the daemon's embedding
  lane or an explicit `embedding.recover` produced it: an explicit quarantine
  leaves nothing for the worker and the Episode is eligible for requeue. A
  deferral leaves the entry, and its retry budget, untouched.
- The outbox drain keeps a deferred Episode queued with exponential backoff
  (30 s doubling, capped at 1 h) for at most 8 deferrals; the next transient
  failure quarantines it as `provider_unavailable_exhausted`. Never-deferred
  entries are served before retries.
- `embedding.requeue {limit?, reasons?}` returns quarantined Episodes of the
  configured profile to the outbox as fresh entries (budget reset, earlier
  attempt rows kept for audit), skipping Episodes that already hold a vector or
  a queued entry, and wakes the embedding lane. `anamnesis-ops embed-requeue
  [--limit N]` is the operator entry point.
- `embedding.status {operation_id}` returns durable attempt state. Status and
  recovery revalidate current Episode/source policy. A denied Episode does not
  become visible through its status record.
- Model/dimension/norm mismatch is quarantined. The first valid vector for an
  immutable Episode/profile is preserved. Different profiles have separate
  digest-named Neo4j vector indexes and cannot overwrite one another.
- Recovery is explicit and bounded, not an automatic ingestion worker or an
  outbox/spool consumer. It does not claim contiguous embedding coverage.

`recall {query, session?: {source,session}, T?, limit?, budget?}` uses at most
one exact Episode-ID identity hit, 64 fulltext hits, 64 vector hits, and the
last 32 session Episodes. Fulltext/vector overfetch is capped at 256. The
identity channel is exact Episode-ID lookup, not Entity/profile-cache identity.
Fulltext retains the existing Lucene escaping behavior. Query embedding
failure omits the vector channel with an explicit reason. Graph/index failures
reject; per-channel timeout degradation is not implemented.

Current Episode/source policy, candidate reads, cached dynamics, bounded prior
revision provenance, whole-bundle packing and receipt issuance share one Meta
write barrier. The daemon serial owner writes the response before acknowledging
another policy command. Receipt attribution is server-derived and includes only
actual packed primary IDs/ranks and immutable original source IDs. Current
policy also revalidates captured prior-revision provenance at feedback time.
Receipts retain response/candidate/query-vector snapshots and context/result/
selection hashes before delivery. Feedback uses the existing atomic ledger.
A failed send is not adoption. Auto mode persists the impression but this
increment does not add asynchronous exposure Hits.

Results are original Episodes only. Their source is themselves. CONTRASTS is
Fact-only in the current lattice, so production `companions` and `entities`
are empty. The core packer handles deduplicated mandatory companions and
companion-to-primary promotion, covered by unit fixtures; this does not claim
production Fact/conflict retrieval. Up to eight prior-revision records and
mandatory withheld/incomplete warnings are included intact. No text or warning
is removed to make a bundle fit.

## Exact budgets and limits

`limit` is an integer 0..64 (default 10). Budget limits are nonnegative safe
integers. UTF-8 bytes and Unicode scalar counts apply to the exact canonical
JSON-lines `context_text`, including escaped content and record separators.
Malformed Unicode and unknown units reject. Token budgets require an installed
version/digest-pinned encoder. No encoder or model vocabulary ships enabled;
unconfigured/unknown IDs reject, never estimate. A trusted core caller can
supply a registry; the Node daemon loads the explicit operator installation
below. Requests select only an installed ID, never code, paths or assets.

The default output budget is 65536 UTF-8 bytes; set
`ANAMNESIS_RECALL_DEFAULT_BYTES` (0..1048576) to override it. Whole prospective
bundles must fit both that budget and the 1 MiB serialized RPC cap, including
duplicated structured/context fields and reserved envelope space. Final wire
encoding checks the actual cap again. Zero budget or zero limit yields an empty
context and a durable, feedback-capable empty receipt.

### Operator-installed exact tokenizer (Node)

Install one self-contained CommonJS **script bundle** and every required
vocabulary/WASM/data asset in operator-controlled local files. No dependencies
are added or downloaded by this loader. Bundle any JS dependencies ahead of
time using the operator's existing tools. Native addons, Node module imports,
network access and asynchronous encoder initialization are not supported.
The ABI is:

```js
module.exports.createEncoder = function (assets) {
  // assets is a name -> Uint8Array map of verified startup snapshots.
  // Initialize the actual exact tokenizer from its vocabulary/WASM assets here.
  // All required assets must be present; throw otherwise.
  return function encode(contextText) { return exactEncoder.encode(contextText).length; };
};
```

`exactEncoder` above denotes the operator's bundled implementation, not an
included library. Standard JS globals (including synchronous WebAssembly),
TextEncoder and TextDecoder are available. No require/import resolver, process,
filesystem or fetch API is provided. The VM is **not a security sandbox**: only
trusted deterministic operator code is allowed. Pin special-token behavior,
normalization and all encoder options in the executable, not mutable environment
or remote data. Empty context must count as zero. Output must be a synchronous,
nonnegative safe integer; negative, fractional, nonfinite, unsafe, string and
Promise counts reject without a receipt. No fallback units are allowed.

Set the server environment variable (not an RPC field):

```json
{"id":"my-encoder-v1@sha256:<manifest digest>","path":"/absolute/encoder.cjs","assets":[{"name":"vocabulary","path":"/absolute/vocabulary.bin"}]}
```

Both object shapes are strict. `assets` is mandatory (use `[]` when all data is
embedded), maximum 64 entries with unique ASCII names matching
`[A-Za-z0-9._-]{1,128}`; every path must be absolute. Missing files, omitted
assets under the old pin, unknown fields, unpinned IDs and digest mismatches
abort startup before the listening event. With no environment variable the
registry is empty. One configured encoder per daemon is supported.

Compute the lowercase SHA-256 manifest digest exactly as follows, using the
installed bytes, not paths or a claimed provider identity:

```js
const H = bytes => createHash('sha256').update(bytes).digest('hex');
const manifest = assets.slice().sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0)
  .map(asset => [asset.name, H(readFileSync(asset.path))]);
const digest = H(Buffer.from(JSON.stringify([
  'anamnesis-tokenizer-v1', H(readFileSync(encoderPath)), manifest
])));
```

`createHash` and `readFileSync` come from `node:crypto` and `node:fs` in this
operator-side snippet. Append this digest to the versioned name as `@sha256:`.
The aggregate pins the executable and named asset bytes; changing either
requires a new ID. Paths may relocate without changing identity. Startup reads
and hashes snapshots, then executes those same code bytes and supplies those
same asset bytes; changing disk files afterward does not change the running
registry. Restart to apply a new installation. The operator is responsible for
matching the claimed model vocabulary and keeping code/assets deterministic.

Limits: script 16 MiB, total assets 256 MiB, script/factory initialization 5 s
each, each count invocation 1 s. Oversized files, initialization errors and
runtime failures reject, never estimate. Counting receives only the complete
prospective canonical JSON-lines text, maximum 1 MiB UTF-8, never raw payloads
or a nominal-budget-sized allocation. Cross-record merges therefore work.
The unchanged full serialized RPC cap also applies. The synchronous provider
must fit these bounds; this is not a general native-tokenizer hosting service.

`tokenizer.fixture.cjs` and its vocabulary are an exact **synthetic byte-token
vocabulary with explicit merges**, including `}\n{` across record boundaries.
They prove loader/packing contracts only, not any real model vocabulary or
semantic quality. No production vocabulary conformance claim is made.

No PPR, Fact generations, extraction, dreaming, occurrence/lineage analysis,
coverage-prefix ledger, hidden-source inference, or latency/semantic efficacy
claim is made. Ordering is deterministic for captured inputs within the pinned
numeric runtime; ANN/index state and elapsed server time are not replay tokens.
The legacy privileged `Engine.recall(string, options)` remains the existing
fulltext helper; the new authority-bearing core path is `Store.recall` /
`Engine.recallHybrid` and the `recall` RPC.
