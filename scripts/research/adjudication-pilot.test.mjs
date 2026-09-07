// Failing-first tests for the bounded adjudication evaluator.
//
// These tests are offline by construction: every transport is injected or
// replayed from disk. Nothing here opens a socket, reads a credential file
// with real contents, or depends on wall-clock timing.

import { describe, expect, test } from "bun:test";
import { mkdtemp, readFile, readdir, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  GOLD_FIELD_NAMES,
  OUTPUT_SCHEMA,
  REASON_MAX_LENGTH,
  TIME_BASES,
  aggregate,
  buildModelPayload,
  buildRequest,
  createFetchTransport,
  loadCases,
  parseArgs,
  parseMessagesSse,
  parseResponsesSse,
  redactRequest,
  runCase,
  runPilot,
  scoreCase,
  validateModelOutput,
} from "./adjudication-pilot.mjs";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const FIXTURES = join(HERE, "adjudication-cases.json");

const LABELS = [
  "NEW",
  "DUPLICATE_OCCURRENCE",
  "ELABORATION",
  "CHANGE",
  "CORRECTION",
  "UNRESOLVED_CONTRADICTION",
];

async function loadFixtureCases() {
  return loadCases(JSON.parse(await readFile(FIXTURES, "utf8")));
}

function goldOutput(kase) {
  return {
    verdict: kase.gold_verdict,
    target_ids: [...kase.gold_target_ids],
    evidence_ids: [...kase.gold_evidence_ids],
    effective_time_basis: kase.gold_effective_time_basis,
    reason: "synthetic replay",
  };
}

const RESOLVED_GPT = "gpt-5.5-2026-02-01";
const RESOLVED_CLAUDE = "claude-opus-5-2026-02-01";

function sse(events) {
  return events.map((e) => `event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`).join("");
}

/** An SSE body for /v1/responses whose `response.completed` output is empty. */
function responsesSse(jsonText, { withUsage = true, terminal = "completed" } = {}) {
  const response = {
    id: "resp_synthetic",
    model: RESOLVED_GPT,
    output: [],
    ...(withUsage ? { usage: { input_tokens: 11, output_tokens: 7, total_tokens: 18 } } : {}),
  };
  const events = [
    { type: "response.created", response: { id: "resp_synthetic", model: RESOLVED_GPT } },
    { type: "response.output_text.delta", delta: jsonText.slice(0, 3) },
    { type: "response.output_text.done", text: jsonText },
  ];
  if (terminal === "completed") {
    events.push({ type: "response.completed", response: { ...response, status: "completed" } });
  } else if (terminal === "incomplete") {
    events.push({
      type: "response.incomplete",
      response: { ...response, status: "incomplete", incomplete_details: { reason: "max_output_tokens" } },
    });
  } else if (terminal === "completed_incomplete_status") {
    events.push({ type: "response.completed", response: { ...response, status: "incomplete" } });
  }
  return sse(events);
}

/** An SSE body for /v1/messages built from text deltas plus message_stop. */
function messagesSse(jsonText, { stop = true, stopReason = "end_turn" } = {}) {
  const mid = Math.floor(jsonText.length / 2);
  const events = [
    {
      type: "message_start",
      message: {
        id: "msg_synthetic",
        model: RESOLVED_CLAUDE,
        usage: { input_tokens: 13, output_tokens: 0 },
      },
    },
    { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
    { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: jsonText.slice(0, mid) } },
    { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: jsonText.slice(mid) } },
    { type: "content_block_stop", index: 0 },
    { type: "message_delta", delta: { stop_reason: stopReason }, usage: { output_tokens: 9 } },
    ...(stop ? [{ type: "message_stop" }] : []),
  ];
  return sse(events);
}

async function writeKeyFile() {
  const dir = await tempDir("adj-key-");
  const path = join(dir, "key.json");
  await writeFile(
    path,
    JSON.stringify({ bearer: "sk-test-not-a-real-key", base_url: "https://api.example.invalid" }),
    { mode: 0o600 },
  );
  return path;
}

function okTransport(body, status = 200) {
  return async () => ({ status, body, headers: { "x-request-id": "synthetic" } });
}

async function tempDir(prefix) {
  return mkdtemp(join(tmpdir(), prefix));
}

/** Build an offline replay directory from the fixtures and a per-case shaper. */
async function makeReplayDir(cases, model, shape) {
  const dir = await tempDir("adj-replay-");
  for (const kase of cases) {
    const shaped = shape(kase);
    const capture = {
      case_id: kase.id,
      model,
      http_status: shaped.http_status ?? 200,
      transport_error: shaped.transport_error ?? null,
      body:
        shaped.body ??
        (shaped.output === undefined
          ? ""
          : model === "claude-opus-5"
            ? messagesSse(JSON.stringify(shaped.output))
            : responsesSse(JSON.stringify(shaped.output))),
      latency_ms: 12,
    };
    await writeFile(join(dir, `${kase.id}.capture.json`), JSON.stringify(capture), { mode: 0o600 });
  }
  return dir;
}

describe("fixtures", () => {
  test("hold 36 synthetic cases, six per label, across three languages", async () => {
    const cases = await loadFixtureCases();
    expect(cases).toHaveLength(36);

    const byLabel = {};
    const byLang = {};
    for (const kase of cases) {
      byLabel[kase.gold_verdict] = (byLabel[kase.gold_verdict] ?? 0) + 1;
      byLang[kase.lang] = (byLang[kase.lang] ?? 0) + 1;
    }
    for (const label of LABELS) expect(byLabel[label]).toBe(6);
    expect(byLang).toEqual({ en: 12, ko: 12, ja: 12 });
    expect(new Set(cases.map((c) => c.id)).size).toBe(36);
  });

  test("declare synthetic provenance and never claim certification", async () => {
    const raw = JSON.parse(await readFile(FIXTURES, "utf8"));
    expect(raw.provenance).toBe("SYNTHETIC");
    expect(typeof raw.invalidation_note).toBe("string");
  });

  test("gold targets and gold evidence resolve inside their own case", async () => {
    const cases = await loadFixtureCases();
    for (const kase of cases) {
      const candidateIds = new Set((kase.candidates ?? []).map((c) => c.id));
      const snippetIds = new Set(kase.episode.snippets.map((s) => s.id));
      for (const id of kase.gold_target_ids) expect(candidateIds.has(id)).toBe(true);
      expect(kase.gold_evidence_ids.length).toBeGreaterThan(0);
      for (const id of kase.gold_evidence_ids) expect(snippetIds.has(id)).toBe(true);
      if (kase.gold_verdict === "NEW") expect(kase.gold_target_ids).toHaveLength(0);
      else expect(kase.gold_target_ids.length).toBeGreaterThan(0);
    }
  });

  test("time basis is normative only where docs/03 §5 pins it", async () => {
    const cases = await loadFixtureCases();
    for (const kase of cases) {
      if (kase.gold_verdict === "CORRECTION") {
        expect(kase.gold_effective_time_basis).toBe("target");
        expect(kase.gold_time_basis_normative).toBe(true);
      } else if (kase.gold_verdict === "CHANGE") {
        expect(kase.gold_effective_time_basis).toBe("proposed");
        expect(kase.gold_time_basis_normative).toBe(true);
      } else {
        expect(kase.gold_time_basis_normative).toBe(false);
      }
    }
  });

  test("candidate ids are opaque and encode no label or language", async () => {
    const cases = await loadFixtureCases();
    const seen = new Set();
    for (const kase of cases) {
      for (const candidate of kase.candidates ?? []) {
        expect(candidate.id).toMatch(/^c_[0-9a-f]{12}$/);
        expect(seen.has(candidate.id)).toBe(false);
        seen.add(candidate.id);
      }
      // Referential identity survives the remap.
      const ids = new Set((kase.candidates ?? []).map((c) => c.id));
      for (const id of kase.gold_target_ids) expect(ids.has(id)).toBe(true);
      for (const id of kase.proposed_claim.content_context_ids ?? []) expect(ids.has(id)).toBe(true);
      for (const candidate of kase.candidates ?? []) {
        for (const id of candidate.invalidates ?? []) expect(ids.has(id)).toBe(true);
      }
    }
    const raw = await readFile(FIXTURES, "utf8");
    expect(raw).not.toMatch(/f-(en|ko|ja)-\d/);
  });

  test("at least one case per label carries a distractor snippet", async () => {
    const cases = await loadFixtureCases();
    const withDistractor = new Set(
      cases.filter((c) => c.episode.snippets.length > 1).map((c) => c.gold_verdict),
    );
    for (const label of LABELS) expect(withDistractor.has(label)).toBe(true);
  });
});

describe("payload isolation", () => {
  test("no gold field reaches the model payload", async () => {
    const cases = await loadFixtureCases();
    for (const kase of cases) {
      const payload = buildModelPayload(kase);
      const serialized = JSON.stringify(payload);
      expect(serialized).not.toContain("gold");
      for (const field of GOLD_FIELD_NAMES) {
        expect(serialized).not.toContain(field);
      }
      expect(serialized).not.toContain(kase.gold_verdict);
    }
  });

  test("no label-bearing identifier reaches the model payload", async () => {
    const cases = await loadFixtureCases();
    for (const kase of cases) {
      const payload = buildModelPayload(kase);
      const serialized = JSON.stringify(payload);

      // Case, episode and claim ids all encode the gold label (new-, dup-,
      // elab-, chg-, cor-, unr-). None of them may reach the model.
      expect(serialized).not.toContain(kase.id);
      expect(serialized).not.toContain(kase.episode.id);
      expect(serialized).not.toContain(kase.proposed_claim.id);
      expect(payload.case_id).toBeUndefined();
      expect(payload.episode.id).toBeUndefined();
      expect(payload.proposed_claim.id).toBeUndefined();

      const stem = kase.id.split("-")[0];
      expect(serialized.toLowerCase()).not.toContain(`"${stem}-`);
    }
  });

  test("payload keeps original snippets and candidate ids", async () => {
    const cases = await loadFixtureCases();
    const kase = cases.find((c) => c.id === "cor-ja-403");
    const payload = buildModelPayload(kase);
    expect(payload.episode.snippets.map((s) => s.id)).toEqual(
      kase.episode.snippets.map((s) => s.id),
    );
    expect(payload.episode.snippets[0].text).toBe(kase.episode.snippets[0].text);
    expect(payload.candidates.map((c) => c.id)).toEqual(kase.candidates.map((c) => c.id));
  });
});

describe("native request shapes", () => {
  test("/v1/responses pins reasoning effort none, stream, store false and a json schema", () => {
    const req = buildRequest({
      model: "gpt-5.5",
      baseUrl: "https://example.invalid",
      bearer: "secret-token",
      prompt: "PROMPT",
      payload: { case_id: "x" },
    });
    expect(req.url).toBe("https://example.invalid/v1/responses");
    expect(req.body.stream).toBe(true);
    expect(req.body.store).toBe(false);
    expect(req.body.reasoning.effort).toBe("none");
    expect(req.body.text.format.type).toBe("json_schema");
    expect(req.body.text.format.schema.properties.effective_time_basis).toBeDefined();
  });

  test("/v1/messages pins thinking disabled, stream, max_tokens and an output json schema", () => {
    const req = buildRequest({
      model: "claude-opus-5",
      baseUrl: "https://example.invalid",
      bearer: "secret-token",
      prompt: "PROMPT",
      payload: { case_id: "x" },
    });
    expect(req.url).toBe("https://example.invalid/v1/messages");
    expect(req.body.stream).toBe(true);
    expect(req.body.thinking.type).toBe("disabled");
    expect(typeof req.body.max_tokens).toBe("number");
    expect(req.body.output_config.format.type).toBe("json_schema");
  });

  test("both endpoints receive the identical semantic schema", () => {
    const common = {
      baseUrl: "https://example.invalid",
      bearer: "secret-token",
      prompt: "PROMPT",
      payload: {},
    };
    const gpt = buildRequest({ ...common, model: "gpt-5.5" });
    const opus = buildRequest({ ...common, model: "claude-opus-5" });
    expect(opus.body.output_config.format.schema).toEqual(gpt.body.text.format.schema);
    expect(opus.body.output_config.format.schema).toEqual(OUTPUT_SCHEMA);
  });

  test("no length bound rides on the wire schema", () => {
    const common = {
      baseUrl: "https://example.invalid",
      bearer: "secret-token",
      prompt: "PROMPT",
      payload: {},
    };
    const schemas = [
      buildRequest({ ...common, model: "gpt-5.5" }).body.text.format.schema,
      buildRequest({ ...common, model: "claude-opus-5" }).body.output_config.format.schema,
    ];
    // Length bounds are stripped before the model sees them, so sending one
    // buys nothing; the bound lives in validateModelOutput instead.
    for (const schema of schemas) {
      expect(schema.properties.reason.maxLength).toBeUndefined();
      expect(schema.properties.reason).toEqual({ type: "string" });
    }
  });

  test("the wire schema carries the nullable time basis as anyOf", () => {
    const schema = buildRequest({
      model: "claude-opus-5",
      baseUrl: "https://example.invalid",
      bearer: "secret-token",
      prompt: "PROMPT",
      payload: {},
    }).body.output_config.format.schema;
    const basis = schema.properties.effective_time_basis;

    // The permitted string alternatives plus a real JSON null, expressed as
    // anyOf rather than a union `type` with null inside a mixed `enum`.
    expect(basis).toEqual({
      anyOf: [{ type: "string", enum: ["proposed", "target"] }, { type: "null" }],
    });
    expect(basis.type).toBeUndefined();
    expect(basis.enum).toBeUndefined();

    const stringAlternative = basis.anyOf.find((a) => a.type === "string");
    expect(stringAlternative.enum).toEqual(["proposed", "target"]);
    expect(stringAlternative.enum).not.toContain(null);
    expect(basis.anyOf.some((a) => a.type === "null")).toBe(true);

    // The accepted value set is unchanged, real null included.
    for (const value of TIME_BASES) {
      expect(
        validateModelOutput({
          verdict: "NEW",
          target_ids: [],
          evidence_ids: ["s1"],
          effective_time_basis: value,
          reason: "r",
        }).ok,
      ).toBe(true);
    }
  });

  test("the reason limit is enforced locally", () => {
    expect(REASON_MAX_LENGTH).toBe(320);
    expect(OUTPUT_SCHEMA.properties.reason.maxLength).toBeUndefined();

    const base = {
      verdict: "NEW",
      target_ids: [],
      evidence_ids: ["s1"],
      effective_time_basis: "proposed",
    };
    expect(validateModelOutput({ ...base, reason: "x".repeat(320) }).ok).toBe(true);
    expect(validateModelOutput({ ...base, reason: "x".repeat(321) }).ok).toBe(false);
  });

  test("messages output_config.format carries only the fields the API accepts", () => {
    const req = buildRequest({
      model: "claude-opus-5",
      baseUrl: "https://example.invalid",
      bearer: "secret-token",
      prompt: "PROMPT",
      payload: {},
    });
    expect(Object.keys(req.body.output_config.format).sort()).toEqual(["schema", "type"]);
  });

  test("redaction strips credentials from anything persisted", () => {
    const req = buildRequest({
      model: "gpt-5.5",
      baseUrl: "https://example.invalid",
      bearer: "secret-token",
      prompt: "PROMPT",
      payload: { case_id: "x" },
    });
    const safe = redactRequest(req);
    const text = JSON.stringify(safe);
    expect(text).not.toContain("secret-token");
    expect(safe.headers.authorization).toBe("[redacted]");
  });
});

describe("stream parsing", () => {
  test("responses parser reads output_text.done when completed.output is empty", () => {
    const payload = '{"verdict":"NEW","target_ids":[],"evidence_ids":["s1"],"effective_time_basis":"proposed","reason":"r"}';
    const parsed = parseResponsesSse(responsesSse(payload));
    expect(parsed.text).toBe(payload);
    expect(parsed.usage.total_tokens).toBe(18);
  });

  test("responses parser fails loudly when no output text arrives", () => {
    const body = sse([
      { type: "response.created", response: { id: "r" } },
      { type: "response.completed", response: { id: "r", output: [], status: "completed" } },
    ]);
    expect(() => parseResponsesSse(body)).toThrow();
  });

  test("responses parser requires a terminal completed event", () => {
    const payload = '{"verdict":"NEW","target_ids":[],"evidence_ids":["s1"],"effective_time_basis":"proposed","reason":"r"}';
    // output_text.done arrived but the stream was cut before the terminal event.
    expect(() => parseResponsesSse(responsesSse(payload, { terminal: "none" }))).toThrow();
  });

  test("responses parser rejects an incomplete or failed response", () => {
    const payload = '{"verdict":"NEW","target_ids":[],"evidence_ids":["s1"],"effective_time_basis":"proposed","reason":"r"}';
    expect(() => parseResponsesSse(responsesSse(payload, { terminal: "incomplete" }))).toThrow();
    expect(() =>
      parseResponsesSse(responsesSse(payload, { terminal: "completed_incomplete_status" })),
    ).toThrow();
  });

  test("responses parser records the model the service actually resolved", () => {
    const payload = '{"verdict":"NEW","target_ids":[],"evidence_ids":["s1"],"effective_time_basis":"proposed","reason":"r"}';
    expect(parseResponsesSse(responsesSse(payload)).model).toBe(RESOLVED_GPT);
  });

  test("messages parser concatenates text deltas and requires message_stop", () => {
    const payload = '{"verdict":"CHANGE","target_ids":["f1"],"evidence_ids":["s1"],"effective_time_basis":"proposed","reason":"r"}';
    const parsed = parseMessagesSse(messagesSse(payload));
    expect(parsed.text).toBe(payload);
    expect(parsed.usage.output_tokens).toBe(9);
    expect(parsed.model).toBe(RESOLVED_CLAUDE);
    expect(() => parseMessagesSse(messagesSse(payload, { stop: false }))).toThrow();
  });

  test("messages parser rejects a truncated max_tokens stop", () => {
    const payload = '{"verdict":"CHANGE","target_ids":["f1"],"evidence_ids":["s1"],"effective_time_basis":"proposed","reason":"r"}';
    expect(() => parseMessagesSse(messagesSse(payload, { stopReason: "max_tokens" }))).toThrow();
  });
});

describe("output schema validation", () => {
  test("accepts a well-formed verdict object", () => {
    const res = validateModelOutput({
      verdict: "CORRECTION",
      target_ids: ["c_000000000000"],
      evidence_ids: ["s1"],
      effective_time_basis: "target",
      reason: "r",
    });
    expect(res.ok).toBe(true);
  });

  test("rejects unknown verdicts and unknown time bases", () => {
    expect(
      validateModelOutput({
        verdict: "MERGE",
        target_ids: [],
        evidence_ids: ["s1"],
        effective_time_basis: "proposed",
        reason: "r",
      }).ok,
    ).toBe(false);
    expect(
      validateModelOutput({
        verdict: "NEW",
        target_ids: [],
        evidence_ids: ["s1"],
        effective_time_basis: "episode",
        reason: "r",
      }).ok,
    ).toBe(false);
  });

  test("accepts a null time basis as a schema value", () => {
    expect(
      validateModelOutput({
        verdict: "NEW",
        target_ids: [],
        evidence_ids: ["s1"],
        effective_time_basis: null,
        reason: "r",
      }).ok,
    ).toBe(true);
  });

  test("rejects undeclared extra keys", () => {
    expect(
      validateModelOutput({
        verdict: "NEW",
        target_ids: [],
        evidence_ids: ["s1"],
        effective_time_basis: "proposed",
        reason: "r",
        confidence: 0.91,
      }).ok,
    ).toBe(false);
  });

  test("rejects a missing effective_time_basis key", () => {
    expect(
      validateModelOutput({
        verdict: "NEW",
        target_ids: [],
        evidence_ids: ["s1"],
        reason: "r",
      }).ok,
    ).toBe(false);
  });

  test("enforces the declared reason length", () => {
    expect(
      validateModelOutput({
        verdict: "NEW",
        target_ids: [],
        evidence_ids: ["s1"],
        effective_time_basis: "proposed",
        reason: "x".repeat(321),
      }).ok,
    ).toBe(false);
  });
});

describe("scoring", () => {
  test("a perfect answer is exact on verdict, target, evidence and basis", async () => {
    const cases = await loadFixtureCases();
    for (const kase of cases) {
      const row = scoreCase(kase, goldOutput(kase));
      expect(row.verdict_exact).toBe(true);
      expect(row.target_exact).toBe(true);
      expect(row.evidence_exact).toBe(true);
      expect(row.time_basis_exact).toBe(true);
      expect(row.core_exact).toBe(true);
      expect(row.full_exact).toBe(true);
    }
  });

  test("target ids are compared as an exact set, not a superset", async () => {
    const cases = await loadFixtureCases();
    const kase = cases.find((c) => c.id === "dup-ko-102");
    // Every candidate, i.e. a strict superset of the single gold target.
    const allCandidates = kase.candidates.map((c) => c.id);
    expect(allCandidates.length).toBeGreaterThan(kase.gold_target_ids.length);
    const row = scoreCase(kase, { ...goldOutput(kase), target_ids: allCandidates });
    expect(row.verdict_exact).toBe(true);
    expect(row.target_exact).toBe(false);
    expect(row.core_exact).toBe(false);
  });

  test("evidence ids are scored, not merely carried", async () => {
    const cases = await loadFixtureCases();
    const kase = cases.find((c) => c.episode.snippets.length > 1);
    const wrong = kase.episode.snippets.find((s) => !kase.gold_evidence_ids.includes(s.id));
    const row = scoreCase(kase, { ...goldOutput(kase), evidence_ids: [wrong.id] });
    expect(row.verdict_exact).toBe(true);
    expect(row.evidence_exact).toBe(false);
    expect(row.full_exact).toBe(false);
  });

  test("a wrong basis on a correction is a scored mode/time error", async () => {
    const cases = await loadFixtureCases();
    const kase = cases.find((c) => c.gold_verdict === "CORRECTION");
    const row = scoreCase(kase, { ...goldOutput(kase), effective_time_basis: "proposed" });
    expect(row.time_basis_exact).toBe(false);
    expect(row.time_basis_normative).toBe(true);
    expect(row.time_basis_error).toBe(true);
    expect(row.core_exact).toBe(false);
  });

  test("a basis difference on NEW is advisory, not a normative error", async () => {
    const cases = await loadFixtureCases();
    const kase = cases.find((c) => c.gold_verdict === "NEW");
    const row = scoreCase(kase, { ...goldOutput(kase), effective_time_basis: null });
    expect(row.time_basis_exact).toBe(false);
    expect(row.time_basis_normative).toBe(false);
    expect(row.time_basis_error).toBe(false);
    expect(row.core_exact).toBe(true);
  });

  test("calling a change a correction is a mode error even with the right target", async () => {
    const cases = await loadFixtureCases();
    const kase = cases.find((c) => c.gold_verdict === "CHANGE");
    const row = scoreCase(kase, {
      ...goldOutput(kase),
      verdict: "CORRECTION",
      effective_time_basis: "target",
    });
    expect(row.verdict_exact).toBe(false);
    expect(row.target_exact).toBe(true);
    expect(row.time_basis_error).toBe(true);
  });
});

describe("terminal rows and failure accounting", () => {
  const model = "gpt-5.5";

  async function caseById(id) {
    const cases = await loadFixtureCases();
    return cases.find((c) => c.id === id);
  }

  test("a transport throw still emits one terminal row", async () => {
    const kase = await caseById("new-en-001");
    let calls = 0;
    const row = await runCase({
      kase,
      model,
      transport: async () => {
        calls += 1;
        throw new Error("ECONNRESET");
      },
    });
    expect(calls).toBe(1); // no automatic retries
    expect(row.status).toBe("failed");
    expect(row.failure_kind).toBe("transport");
    expect(row.case_id).toBe(kase.id);
    expect(row.full_exact).toBe(false);
  });

  test("a non-2xx response is a counted http failure, never a dropped case", async () => {
    const kase = await caseById("dup-en-101");
    const row = await runCase({
      kase,
      model,
      transport: okTransport("upstream unavailable", 503),
    });
    expect(row.status).toBe("failed");
    expect(row.failure_kind).toBe("http");
    expect(row.http_status).toBe(503);
    expect(row.raw_output).toContain("upstream unavailable");
  });

  test("an unparseable stream is a counted parse failure with the raw body preserved", async () => {
    const kase = await caseById("elab-en-201");
    const row = await runCase({
      kase,
      model,
      transport: okTransport("event: response.created\ndata: {\"type\":\"response.created\"}\n\n"),
    });
    expect(row.status).toBe("failed");
    expect(row.failure_kind).toBe("parse");
    expect(row.raw_output.length).toBeGreaterThan(0);
  });

  test("non-JSON model text is a parse failure", async () => {
    const kase = await caseById("elab-en-201");
    const row = await runCase({
      kase,
      model,
      transport: okTransport(responsesSse("I think this one is a duplicate.")),
    });
    expect(row.status).toBe("failed");
    expect(row.failure_kind).toBe("parse");
  });

  test("a schema-invalid verdict is a counted schema failure", async () => {
    const kase = await caseById("chg-en-301");
    const row = await runCase({
      kase,
      model,
      transport: okTransport(
        responsesSse(
          JSON.stringify({
            verdict: "SUPERSEDES",
            target_ids: [...kase.gold_target_ids],
            evidence_ids: ["s1"],
            effective_time_basis: "proposed",
            reason: "r",
          }),
        ),
      ),
    });
    expect(row.status).toBe("failed");
    expect(row.failure_kind).toBe("schema");
  });

  test("a target id outside the candidate set is a counted target failure", async () => {
    const kase = await caseById("chg-en-301");
    const row = await runCase({
      kase,
      model,
      transport: okTransport(
        responsesSse(
          JSON.stringify({
            verdict: "CHANGE",
            target_ids: ["f-does-not-exist"],
            evidence_ids: ["s1"],
            effective_time_basis: "proposed",
            reason: "r",
          }),
        ),
      ),
    });
    expect(row.status).toBe("failed");
    expect(row.failure_kind).toBe("target_invalid");
    expect(row.target_exact).toBe(false);
  });

  test("an evidence id outside the episode is a counted evidence failure", async () => {
    const kase = await caseById("chg-en-301");
    const row = await runCase({
      kase,
      model,
      transport: okTransport(
        responsesSse(
          JSON.stringify({
            verdict: "CHANGE",
            target_ids: [...kase.gold_target_ids],
            evidence_ids: ["s99"],
            effective_time_basis: "proposed",
            reason: "r",
          }),
        ),
      ),
    });
    expect(row.status).toBe("failed");
    expect(row.failure_kind).toBe("evidence_invalid");
    expect(row.evidence_exact).toBe(false);
  });

  test("a good answer through the messages transport scores", async () => {
    const kase = await caseById("cor-ko-402");
    const row = await runCase({
      kase,
      model: "claude-opus-5",
      transport: okTransport(messagesSse(JSON.stringify(goldOutput(kase)))),
    });
    expect(row.status).toBe("scored");
    expect(row.failure_kind).toBe(null);
    expect(row.full_exact).toBe(true);
    expect(row.usage.output_tokens).toBe(9);
    expect(row.resolved_model).toBe(RESOLVED_CLAUDE);
  });
});

describe("bounded transport", () => {
  test("a live fetch carries an abort signal and fails on its own timeout", async () => {
    let sawSignal = false;
    const transport = createFetchTransport({
      timeoutMs: 5,
      fetchImpl: (_url, init) =>
        new Promise((_resolve, reject) => {
          sawSignal = Boolean(init.signal);
          init.signal.addEventListener("abort", () => reject(init.signal.reason ?? new Error("aborted")));
        }),
    });

    await expect(
      transport({ url: "https://example.invalid/v1/responses", method: "POST", headers: {}, body: {} }),
    ).rejects.toThrow();
    expect(sawSignal).toBe(true);
  });

  test("a timed-out request becomes one terminal transport failure", async () => {
    const cases = await loadFixtureCases();
    const kase = cases[0];
    const transport = createFetchTransport({
      timeoutMs: 5,
      fetchImpl: (_url, init) =>
        new Promise((_resolve, reject) => {
          init.signal.addEventListener("abort", () => reject(init.signal.reason ?? new Error("aborted")));
        }),
    });
    const row = await runCase({ kase, model: "gpt-5.5", transport });
    expect(row.status).toBe("failed");
    expect(row.failure_kind).toBe("transport");
  });
});

describe("aggregate", () => {
  test("attempted equals expected and every label is counted", async () => {
    const cases = await loadFixtureCases();
    const rows = [];
    for (const kase of cases) rows.push(scoreCase(kase, goldOutput(kase)));
    const agg = aggregate({ rows, expected: cases.length, model: "gpt-5.5" });

    expect(agg.attempted).toBe(36);
    expect(agg.expected).toBe(36);
    expect(agg.complete).toBe(true);
    for (const label of LABELS) {
      expect(agg.per_label[label].n).toBe(6);
      expect(agg.per_label[label].verdict_exact).toBe(6);
      expect(agg.per_label[label].target_exact).toBe(6);
      expect(agg.per_label[label].evidence_exact).toBe(6);
      expect(agg.per_label[label].full_exact).toBe(6);
    }
    expect(agg.failures.total).toBe(0);
    expect(agg.false_invalidation).toBe(0);
    expect(agg.missed_invalidation).toBe(0);
    expect(agg.wrong_target_given_verdict).toBe(0);
    expect(agg.target_mismatch_total).toBe(0);
    expect(agg.wrong_mode_or_time).toBe(0);
    expect(agg.unresolved_missed).toBe(0);
    expect(agg.unresolved_invalidated).toBe(0);
    expect(agg.unresolved_misclassified).toBe(0);
  });

  test("never reports a confidence or accuracy claim", async () => {
    const cases = await loadFixtureCases();
    const rows = cases.map((k) => scoreCase(k, goldOutput(k)));
    const agg = aggregate({ rows, expected: cases.length, model: "gpt-5.5" });
    const keys = JSON.stringify(agg).toLowerCase();
    expect(keys).not.toContain("accuracy");
    expect(keys).not.toContain("confidence");
    expect(agg.provenance).toBe("SYNTHETIC");
  });

  test("splits a missed conflict from an elected winner on gold UNRESOLVED", async () => {
    const cases = await loadFixtureCases();
    const unresolved = cases.filter((c) => c.gold_verdict === "UNRESOLVED_CONTRADICTION");
    const [missed, elected] = unresolved;

    const rows = cases.map((kase) => {
      if (kase.id === missed.id) {
        // Non-invalidating verdict: the conflict is never noticed.
        return scoreCase(kase, { ...goldOutput(kase), verdict: "NEW", target_ids: [] });
      }
      if (kase.id === elected.id) {
        // Invalidating verdict: one side is declared the winner.
        return scoreCase(kase, { ...goldOutput(kase), verdict: "CORRECTION" });
      }
      return scoreCase(kase, goldOutput(kase));
    });
    const agg = aggregate({ rows, expected: cases.length, model: "gpt-5.5" });

    expect(agg.unresolved_missed).toBe(1);
    expect(agg.unresolved_invalidated).toBe(1);
    expect(agg.unresolved_misclassified).toBe(2);
    // The missed one writes nothing; only the elected one invalidates.
    expect(agg.false_invalidation).toBe(1);
    expect(JSON.stringify(agg)).not.toContain("unresolved_resolved");
  });

  test("separates target mismatch given a correct verdict from total mismatch", async () => {
    const cases = await loadFixtureCases();
    const withTargets = cases.filter((c) => c.gold_target_ids.length > 0);
    const keptVerdict = withTargets[0];
    const lostVerdict = withTargets.find(
      (c) => c.id !== keptVerdict.id && c.gold_verdict !== "NEW",
    );

    const rows = cases.map((kase) => {
      if (kase.id === keptVerdict.id) {
        // Right verdict, wrong anchor.
        return scoreCase(kase, { ...goldOutput(kase), target_ids: ["c_ffffffffffff"] });
      }
      if (kase.id === lostVerdict.id) {
        // Wrong verdict drags the target with it.
        return scoreCase(kase, { ...goldOutput(kase), verdict: "NEW", target_ids: [] });
      }
      return scoreCase(kase, goldOutput(kase));
    });
    const agg = aggregate({ rows, expected: cases.length, model: "gpt-5.5" });

    expect(agg.wrong_target_given_verdict).toBe(1); // conditional on verdict_exact
    expect(agg.target_mismatch_total).toBe(2); // both rows, regardless of verdict
    expect(agg.target_mismatch_total).toBeGreaterThanOrEqual(agg.wrong_target_given_verdict);
    expect(JSON.stringify(agg)).not.toContain("\"wrong_target\":");
  });

  test("records interpretation limits without mutating gold labels", async () => {
    const cases = await loadFixtureCases();
    const rows = cases.map((k) => scoreCase(k, goldOutput(k)));
    const agg = aggregate({ rows, expected: cases.length, model: "gpt-5.5" });

    const flagged = agg.interpretation_limits.map((l) => l.case_id);
    expect(flagged).toContain("new-en-004");
    expect(flagged).toContain("unr-en-501");
    // Documented only: the gold labels they describe are untouched.
    expect(cases.find((c) => c.id === "new-en-004").gold_verdict).toBe("NEW");
    for (const limit of agg.interpretation_limits) {
      expect(cases.some((c) => c.id === limit.case_id)).toBe(true);
    }
  });

  test("counts false invalidation, missed invalidation and resolved unresolveds", async () => {
    const cases = await loadFixtureCases();
    const rows = [];
    for (const kase of cases) {
      let out = goldOutput(kase);
      if (kase.id === "dup-en-101") {
        out = { ...out, verdict: "CHANGE" }; // benign relation called an invalidation
      } else if (kase.id === "chg-en-301") {
        out = { ...out, verdict: "ELABORATION" }; // invalidation missed
      } else if (kase.id === "unr-en-501") {
        out = { ...out, verdict: "CHANGE" }; // unresolved conflict resolved by fiat
      }
      rows.push(scoreCase(kase, out));
    }
    const agg = aggregate({ rows, expected: cases.length, model: "gpt-5.5" });

    expect(agg.attempted).toBe(36);
    expect(agg.false_invalidation).toBe(2); // dup-en-101 and unr-en-501
    expect(agg.missed_invalidation).toBe(1);
    expect(agg.unresolved_invalidated).toBe(1); // CHANGE elects a winner
    expect(agg.unresolved_missed).toBe(0);
    expect(agg.per_label.DUPLICATE_OCCURRENCE.verdict_exact).toBe(5);
    expect(agg.per_label.CHANGE.verdict_exact).toBe(5);
    expect(agg.per_label.UNRESOLVED_CONTRADICTION.verdict_exact).toBe(5);
  });

  test("failed rows are counted by kind and never silently dropped", async () => {
    const cases = await loadFixtureCases();
    const rows = cases.map((k) => scoreCase(k, goldOutput(k)));
    rows[0] = {
      ...rows[0],
      status: "failed",
      failure_kind: "http",
      verdict_exact: false,
      target_exact: false,
      evidence_exact: false,
      time_basis_exact: false,
      core_exact: false,
      full_exact: false,
    };
    rows[1] = {
      ...rows[1],
      status: "failed",
      failure_kind: "parse",
      verdict_exact: false,
      target_exact: false,
      evidence_exact: false,
      time_basis_exact: false,
      core_exact: false,
      full_exact: false,
    };
    const agg = aggregate({ rows, expected: cases.length, model: "gpt-5.5" });

    expect(agg.attempted).toBe(36);
    expect(agg.complete).toBe(true);
    expect(agg.failures.total).toBe(2);
    expect(agg.failures.by_kind.http).toBe(1);
    expect(agg.failures.by_kind.parse).toBe(1);
    expect(agg.scored).toBe(34);
    expect(agg.per_label.NEW.n).toBe(6);
    expect(agg.per_label.NEW.full_exact).toBeLessThan(6);
  });

  test("a short run is reported incomplete", async () => {
    const cases = await loadFixtureCases();
    const rows = cases.slice(0, 30).map((k) => scoreCase(k, goldOutput(k)));
    const agg = aggregate({ rows, expected: cases.length, model: "gpt-5.5" });
    expect(agg.attempted).toBe(30);
    expect(agg.complete).toBe(false);
  });
});

describe("argument parsing", () => {
  test("defaults concurrency to 2 and requires a key file only when live", () => {
    const replay = parseArgs([
      "--fixtures", FIXTURES,
      "--model", "gpt-5.5",
      "--out-dir", "/tmp/out",
      "--replay", "/tmp/replay",
    ]);
    expect(replay.concurrency).toBe(2);
    expect(replay.replay).toBe("/tmp/replay");
    expect(replay.keyFile).toBe(null);

    expect(() =>
      parseArgs(["--fixtures", FIXTURES, "--model", "gpt-5.5", "--out-dir", "/tmp/out"]),
    ).toThrow();
  });

  test("rejects unknown models and non-positive concurrency", () => {
    const base = ["--fixtures", FIXTURES, "--out-dir", "/tmp/out", "--replay", "/tmp/r"];
    expect(() => parseArgs([...base, "--model", "gpt-4o"])).toThrow();
    expect(() => parseArgs([...base, "--model", "gpt-5.5", "--concurrency", "0"])).toThrow();
  });
});

describe("offline replay run", () => {
  test("replays all 36 cases without a key or a network call", async () => {
    const cases = await loadFixtureCases();
    const replayDir = await makeReplayDir(cases, "gpt-5.5", (k) => ({ output: goldOutput(k) }));
    const outDir = await tempDir("adj-out-");

    const result = await runPilot({
      fixtures: FIXTURES,
      model: "gpt-5.5",
      outDir,
      replay: replayDir,
      concurrency: 2,
      keyFile: null,
      transport: () => {
        throw new Error("replay must not use a transport");
      },
    });

    expect(result.rows).toHaveLength(36);
    expect(result.aggregate.attempted).toBe(36);
    expect(result.aggregate.complete).toBe(true);
    expect(result.aggregate.failures.total).toBe(0);
    for (const label of LABELS) expect(result.aggregate.per_label[label].full_exact).toBe(6);

    const files = await readdir(outDir);
    expect(files).toContain("aggregate.json");
    expect(files).toContain("rows.jsonl");

    const lines = (await readFile(join(outDir, "rows.jsonl"), "utf8")).trim().split("\n");
    expect(lines).toHaveLength(36);
    expect(new Set(lines.map((l) => JSON.parse(l).case_id)).size).toBe(36);
  });

  test("captures are private and hold no credential material", async () => {
    const cases = await loadFixtureCases();
    const replayDir = await makeReplayDir(cases.slice(0, 4), "gpt-5.5", (k) => ({
      output: goldOutput(k),
    }));
    const outDir = await tempDir("adj-out-");

    await runPilot({
      fixtures: FIXTURES,
      model: "gpt-5.5",
      outDir,
      replay: replayDir,
      concurrency: 2,
      keyFile: null,
      only: cases.slice(0, 4).map((c) => c.id),
    });

    for (const name of ["aggregate.json", "rows.jsonl"]) {
      const info = await stat(join(outDir, name));
      expect(info.mode & 0o777).toBe(0o600);
    }
    const captureDir = join(outDir, "captures");
    const captures = await readdir(captureDir);
    expect(captures.length).toBe(4);
    for (const name of captures) {
      const info = await stat(join(captureDir, name));
      expect(info.mode & 0o777).toBe(0o600);
      const text = await readFile(join(captureDir, name), "utf8");
      expect(text).not.toContain("Bearer ");
      expect(text.toLowerCase()).not.toContain("authorization\":\"");
    }
  });

  test("a run captured through the runner replays to identical rows", async () => {
    const cases = await loadFixtureCases();
    const only = [
      "new-en-001", // success
      "dup-en-101", // http failure
      "elab-en-201", // transport failure
      "chg-en-301", // schema failure
      "cor-en-401", // parse failure
    ];
    const selected = cases.filter((c) => only.includes(c.id));
    const keyFile = await writeKeyFile();
    const liveOut = await tempDir("adj-live-");

    const transport = (request) => {
      const text = JSON.stringify(request.body);
      const kase = selected.find((c) => text.includes(c.candidates?.[0]?.id ?? c.episode.snippets[0].text));
      if (kase.id === "dup-en-101") return Promise.resolve({ status: 500, body: "internal error" });
      if (kase.id === "elab-en-201") return Promise.reject(new Error("socket hang up"));
      if (kase.id === "chg-en-301") {
        return Promise.resolve({
          status: 200,
          body: responsesSse(JSON.stringify({ ...goldOutput(kase), verdict: "MERGE" })),
        });
      }
      if (kase.id === "cor-en-401") {
        return Promise.resolve({ status: 200, body: responsesSse("not json at all") });
      }
      return Promise.resolve({ status: 200, body: responsesSse(JSON.stringify(goldOutput(kase))) });
    };

    const live = await runPilot({
      fixtures: FIXTURES,
      model: "gpt-5.5",
      outDir: liveOut,
      keyFile,
      concurrency: 2,
      only,
      transport,
    });

    expect(live.rows).toHaveLength(5);
    const liveByCase = Object.fromEntries(live.rows.map((r) => [r.case_id, r]));
    expect(liveByCase["new-en-001"].status).toBe("scored");
    expect(liveByCase["dup-en-101"].failure_kind).toBe("http");
    expect(liveByCase["elab-en-201"].failure_kind).toBe("transport");
    expect(liveByCase["chg-en-301"].failure_kind).toBe("schema");
    expect(liveByCase["cor-en-401"].failure_kind).toBe("parse");

    // Replay reads exactly what the runner wrote - no handcrafted fixture.
    const replayOut = await tempDir("adj-replay-out-");
    const replayed = await runPilot({
      fixtures: FIXTURES,
      model: "gpt-5.5",
      outDir: replayOut,
      replay: live.captureDir,
      concurrency: 2,
      keyFile: null,
      only,
      transport: () => {
        throw new Error("replay must not use a transport");
      },
    });

    const shape = (rows) =>
      rows
        .map((r) => ({
          case_id: r.case_id,
          status: r.status,
          failure_kind: r.failure_kind,
          http_status: r.http_status ?? null,
          verdict: r.verdict,
          target_ids: r.target_ids,
          evidence_ids: r.evidence_ids,
          effective_time_basis: r.effective_time_basis,
          full_exact: r.full_exact,
          resolved_model: r.resolved_model ?? null,
        }))
        .sort((a, b) => a.case_id.localeCompare(b.case_id));

    expect(shape(replayed.rows)).toEqual(shape(live.rows));
    expect(replayed.aggregate.failures.total).toBe(4);
  });

  test("the report records the resolved model and the prompt and fixture hashes", async () => {
    const cases = await loadFixtureCases();
    const replayDir = await makeReplayDir(cases, "gpt-5.5", (k) => ({ output: goldOutput(k) }));
    const outDir = await tempDir("adj-out-");

    const result = await runPilot({
      fixtures: FIXTURES,
      model: "gpt-5.5",
      outDir,
      replay: replayDir,
      concurrency: 2,
      keyFile: null,
    });

    expect(result.aggregate.prompt_sha256).toMatch(/^[0-9a-f]{64}$/);
    expect(result.aggregate.fixtures_sha256).toMatch(/^[0-9a-f]{64}$/);
    expect(result.aggregate.resolved_models).toEqual([RESOLVED_GPT]);
    for (const row of result.rows) expect(row.resolved_model).toBe(RESOLVED_GPT);
  });

  test("a replayed run with mixed failures still emits one row per configured case", async () => {
    const cases = await loadFixtureCases();
    const replayDir = await makeReplayDir(cases, "claude-opus-5", (k) => {
      if (k.id === "new-en-001") return { http_status: 500, body: "boom" };
      if (k.id === "dup-en-101") return { body: "event: message_start\ndata: {}\n\n" };
      if (k.id === "elab-en-201") return { transport_error: "socket hang up" };
      if (k.id === "chg-en-301") {
        return { output: { ...goldOutput(k), target_ids: ["f-nope"] } };
      }
      return { output: goldOutput(k) };
    });
    const outDir = await tempDir("adj-out-");

    const result = await runPilot({
      fixtures: FIXTURES,
      model: "claude-opus-5",
      outDir,
      replay: replayDir,
      concurrency: 2,
      keyFile: null,
    });

    expect(result.rows).toHaveLength(36);
    expect(result.aggregate.attempted).toBe(36);
    expect(result.aggregate.complete).toBe(true);
    expect(result.aggregate.failures.total).toBe(4);
    expect(result.aggregate.failures.by_kind.http).toBe(1);
    expect(result.aggregate.failures.by_kind.parse).toBe(1);
    expect(result.aggregate.failures.by_kind.transport).toBe(1);
    expect(result.aggregate.failures.by_kind.target_invalid).toBe(1);
    expect(result.aggregate.scored).toBe(32);

    const ids = new Set(result.rows.map((r) => r.case_id));
    expect(ids.size).toBe(36);
  });
});
