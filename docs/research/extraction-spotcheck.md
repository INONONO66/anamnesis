# Source claims and English projections: six synthetic probes

Two models each processed six newly written synthetic Episodes, one Episode
per request. The prompt requested source-language claim content, a separate
English search projection, modality, source-faithfulness confidence and an
exact original quote. The projection was explicitly not new evidence.

This is a development diagnostic, not independently labeled production data.
No graph was modified. All twelve requests returned HTTP 200 and parseable
claim outputs; every confidence value was finite and in [0,1].

## Observations

| Case | GPT-5.5 none | Opus-5 thinking disabled |
|---|---|---|
| Planned contract termination, not yet completed | Preserved intention and current non-completion | Preserved intention and current non-completion |
| Named developer joining and research history | Both statements retained; no inferred gender | Both statements retained; no inferred gender |
| Explicit same-speaker bonus correction | Retained corrected 60,000 yen | Retained corrected 60,000 yen |
| Conflicting Tuesday/Wednesday reports | Kept both attributed reports and unverified status | Kept both attributed reports and unverified status |
| Environment metadata only | Zero claims | Zero claims |
| Distinct Latin/Cyrillic collector identifiers | Preserved distinct identifiers; quotes match | Collapsed identifiers in one claim/projection and changed its quote |

GPT returned 12 claims with no literal-quote errors. Opus returned 11 claims
with one literal-quote error. These are counts on six intentionally selected
cases, not estimates of either model's error rate.

The English projections also added unverified name renderings: GPT rendered
a Korean name as `Jihyun`; Opus added `(Sato)` to a Japanese source name.
Preserving the source name in the canonical record is therefore important.
Generated transliterations must not silently become verified identity aliases.

Some otherwise exact quotations omit the named subject or surrounding
context used in the claim. For example, a quote beginning "My research"
requires surrounding speaker context, and "correctly, 60,000 yen" needs its
bonus context. A substring pass does not prove that every claim detail is
supported by the selected substring alone.

GPT assigned confidence 1 to every emitted claim; Opus values ranged from
0.9 to 0.95, including 0.95 on the corrupted-identifier claim. These outputs
are not calibrated truth or hallucination probabilities and cannot substitute
for independent validation.

## Decision relevance

The [adjudication screen](adjudication-conformance.md) favored Opus on core
relation decisions, while this probe exposes a recurring Opus extraction
fidelity failure. Select and evaluate extraction and adjudication separately.
Neither model is approved as an unattended writer by these probes.

English projections remain optional, versioned retrieval aids. Original
claims, identifiers and evidence must remain recoverable, and a projection
must not acquire source authority or become an identity-normalization rule.

## Evidence and reproduction

The test manifest freezes source text, prompt, schema and SHA-256 values
before calls. Every request and response, token usage, wall time and detected
quote/confidence issue is retained on the experiment server:

```text
~/.config/anamnesis/extraction-spotcheck-20260906/manifest.json
~/.config/anamnesis/extraction-spotcheck-20260906/<model>-s01.json
...
~/.config/anamnesis/extraction-spotcheck-20260906/<model>-s06.json
```

The executed script is
`~/.config/anamnesis/anamnesis-extraction-spotcheck.mjs`. It uses the existing
server-only key and imports the previously verified native stream parser;
it does not embed credentials or write to Neo4j. Its output paths use
exclusive creation to avoid replacing captured evidence. It is a local
experiment script, not a shipped production extraction API.

All twelve artifacts were read and their claim content, modality, projections
and evidence compared against the manifest. Automated checks
covered exact quote substrings and confidence range; observations above
add manual semantic review. JSON/schema success alone was not counted as
source-faithfulness.

## Finalization disposition (D48, D50)

[D48](../10-decision-log.md) uses this probe only to require source-exact
canonical claims, identifiers, names, and evidence while keeping English
output optional and generation-scoped.
[D50](../10-decision-log.md) keeps extraction and adjudication as separate
model roles; this probe neither selects an extractor nor expands the
shadow/manual adjudication admission boundary
([02-daemon-and-pipelines §5.2](../02-daemon-and-pipelines.md)).
