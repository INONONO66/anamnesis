import { mkdir, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { loadProviderConfig } from "../../app/anamnesis/config.ts";
import { OpenAiEmbeddingProvider } from "../../packages/core/src/openai-embedding-provider.ts";
import { OpenAiChatExtractionProvider } from "../../packages/core/src/openai-extraction-provider.ts";
import { EmbeddingError } from "../../packages/core/src/embedding.ts";
import { ExtractionProviderError, validateSourceSpans } from "../../packages/core/src/extraction.ts";
import { ExtractionModelOutput } from "../../packages/protocol/src/extraction.ts";

// Run with the Goal B environment and provider-tunnel.sh. API key files are
// consumed only by loadProviderConfig; config and credentials are never logged.
const rawResponseFile = fileURLToPath(new URL("../../.omo/evidence/runtime-complete/g1/live-raw-response.log", import.meta.url));
let modelContent: string | undefined;
const text = "Alice works on the Anamnesis project. Alice decided to store project backups every Friday.";
const receipt: Record<string, unknown> = { input: text };
let stage = "configuration";
try {
  const config = await loadProviderConfig();
  assert.ok(config.llm.baseUrl, "ANAMNESIS_LLM_BASE_URL required");
  assert.ok(config.llm.apiKey, "ANAMNESIS_LLM_API_KEY_FILE required");
  if (config.embedding) {
    const profile = {
      model: config.embedding.model,
      // Probe-only identity, not a claim that the model alias pins server weights.
      model_incarnation: createHash("sha256").update(`g1-live-probe:${config.embedding.model}`).digest("hex"),
      dimensions: config.embedding.dimensions,
      document_prefix: "", query_prefix: "", max_input_bytes: 65536,
      norm: "unit_l2" as const, norm_tolerance: 0.01,
    };
    stage = "embedding";
    const embedding = new OpenAiEmbeddingProvider({ ...config.embedding, profile, timeoutMs: 30000 });
    const vector = await embedding.embed(text, "document");
    const norm = Math.hypot(...vector);
    assert.equal(vector.length, config.embedding.dimensions);
    assert.ok(vector.every(Number.isFinite));
    assert.ok(Math.abs(norm - 1) <= profile.norm_tolerance);
    receipt.embedding = { dimensions: vector.length, l2_norm: norm };
  } else {
    receipt.embedding = "disabled";
    console.log("embeddings:disabled");
  }
  receipt.model = config.llm.model;
  receipt.dialect = config.llm.dialect;
  stage = "extraction";
  const extraction = new OpenAiChatExtractionProvider({
    dialect: config.llm.dialect,
    baseUrl: config.llm.baseUrl, apiKey: config.llm.apiKey, model: config.llm.model,
    systemPrompt: config.systemPrompt, timeoutMs: 30000,
    fetch: async (url, init) => {
      const response = await fetch(url, init);
      receipt.extraction_http_status = response.status;
      if (response.ok) {
        const raw = await response.clone().text();
        try {
          const body = JSON.parse(raw);
          const content = config.llm.dialect === "anthropic_messages" ? body.content?.[0]?.text : body.choices?.[0]?.message?.content;
          if (typeof content === "string") modelContent = content;
        } catch { receipt.response_json = "invalid"; }
      }
      return response;
    },
  });
  const output = ExtractionModelOutput.parse(await extraction.extract({ task: "claim", text }));
  assert.equal(output.task, "claim");
  assert.ok(output.task === "claim");
  assert.ok(output.claims.length > 0, "expected at least one grounded claim");
  validateSourceSpans(text, output.claims.map(claim => claim.evidence));
  // The current ExtractionModelOutput ABI names the exact evidence quote
  // evidence.text; record it as evidence_quote for this live receipt.
  const claims = output.claims.map(claim => {
    const evidence_quote = claim.evidence.text;
    assert.ok(evidence_quote.length > 0 && text.includes(evidence_quote));
    return { text: claim.text, evidence_quote };
  });
  receipt.extraction = { claims, model_incarnation: extraction.modelIncarnation };
  console.log(`extraction PASS: ${claims.length} grounded claims`);
  receipt.status = "passed";
} catch (error) {
  if (error instanceof ExtractionProviderError && error.reason === "provider_mismatch" && modelContent !== undefined) {
    await mkdir(dirname(rawResponseFile), { recursive: true });
    await writeFile(rawResponseFile, modelContent);
  }
  // Only raw model content is diagnostic evidence; never log credentials,
  // request headers, provider error messages, or the complete response envelope.
  if (error instanceof ExtractionProviderError || error instanceof EmbeddingError) receipt.reason = error.reason;
  else if (error instanceof assert.AssertionError) receipt.reason = "assertion_failed";
  receipt.status = "failed";
  receipt.stage = stage;
  process.exitCode = 1;
} finally {
  console.log(JSON.stringify(receipt, null, 2));
}
