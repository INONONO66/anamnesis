import fs from "node:fs/promises";
import { ExtractionGateError, type ExtractionGate } from "./extraction-gate.ts";
import { reportedModelMatches } from "./extraction.ts";

export const JEV_MODEL = "typesafe-ai/jev" as const;
const DEFAULT_BASE_URL = "https://ai-gateway.vercel.sh/v1";
const MAX_EPISODE_BYTES = 2048;
const MAX_BODY_BYTES = 64 * 1024;
const SYSTEM_PROMPT = "Score each numbered episode from 0 to 1 for how much durable, extractable personal/factual memory it contains (0 = chatter/boilerplate, 1 = rich facts). Respond with ONLY a JSON array of numbers, one per episode, in order.";
type Fetcher = (input: string | URL | Request, init?: RequestInit) => Promise<Response>;

interface JevKey {
  bearer: string;
  allowed_models?: string[];
}

function parseKey(value: unknown): JevKey {
  if (!value || typeof value !== "object" || !("bearer" in value) || typeof value.bearer !== "string" || value.bearer.length === 0 || /[\r\n]/.test(value.bearer)) {
    throw new TypeError("invalid JevGate key file");
  }
  if ("allowed_models" in value && value.allowed_models !== undefined &&
    (!Array.isArray(value.allowed_models) || value.allowed_models.some(model => typeof model !== "string"))) {
    throw new TypeError("invalid JevGate key file");
  }
  const allowed_models = "allowed_models" in value && Array.isArray(value.allowed_models) ? value.allowed_models : undefined;
  if (allowed_models !== undefined && !allowed_models.includes(JEV_MODEL)) throw new TypeError("JevGate key does not allow typesafe-ai/jev");
  return { bearer: value.bearer, ...(allowed_models === undefined ? {} : { allowed_models }) };
}

function truncated(text: string): string {
  return Buffer.from(text, "utf8").subarray(0, MAX_EPISODE_BYTES).toString("utf8");
}

function gateError(reason: "gate_unavailable" | "gate_rejected" | "gate_mismatch"): ExtractionGateError {
  return new ExtractionGateError(reason);
}

function parseScoresPayload(payload: unknown, count: number): number[] {
  if (!payload || typeof payload !== "object" || !("model" in payload) || typeof payload.model !== "string" ||
    !reportedModelMatches(JEV_MODEL, payload.model) || !("choices" in payload) || !Array.isArray(payload.choices) ||
    !payload.choices[0] || typeof payload.choices[0] !== "object" || !("message" in payload.choices[0]) ||
    !payload.choices[0].message || typeof payload.choices[0].message !== "object" ||
    !("content" in payload.choices[0].message) || typeof payload.choices[0].message.content !== "string") {
    throw gateError("gate_mismatch");
  }
  let scores: unknown;
  try { scores = JSON.parse(payload.choices[0].message.content); } catch { throw gateError("gate_mismatch"); }
  if (!Array.isArray(scores) || scores.length !== count || scores.some(score => typeof score !== "number" || !Number.isFinite(score))) {
    throw gateError("gate_mismatch");
  }
  return scores.map(score => Math.max(0, Math.min(1, score)));
}

export class JevGate implements ExtractionGate {
  readonly model = JEV_MODEL;
  private constructor(private readonly bearer: string, readonly baseUrl: string, private readonly request: Fetcher, private readonly timeoutMs: number) {}

  static async create(options: { keyFile: string; model?: string; baseUrl?: string; timeoutMs?: number; fetch?: Fetcher }): Promise<JevGate> {
    if (options.model !== undefined && options.model !== JEV_MODEL) throw new TypeError("JevGate only serves typesafe-ai/jev");
    const baseUrl = options.baseUrl ?? DEFAULT_BASE_URL;
    let parsed: URL;
    try { parsed = new URL(baseUrl); } catch { throw new TypeError("JevGate baseUrl must use https"); }
    if (parsed.protocol !== "https:") throw new TypeError("JevGate baseUrl must use https");
    const timeoutMs = options.timeoutMs ?? 10000;
    if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new TypeError("JevGate timeoutMs must be positive");
    let key: JevKey;
    try { key = parseKey(JSON.parse(await fs.readFile(options.keyFile, "utf8"))); }
    catch (error) {
      if (error instanceof TypeError && error.message.startsWith("JevGate")) throw error;
      throw new TypeError("unable to read JevGate key file", { cause: error });
    }
    return new JevGate(key.bearer, baseUrl.replace(/\/+$/, ""), options.fetch ?? fetch, timeoutMs);
  }

  toJSON(): { model: typeof JEV_MODEL; baseUrl: string } {
    return { model: this.model, baseUrl: this.baseUrl };
  }

  [Symbol.for("nodejs.util.inspect.custom")](): { model: typeof JEV_MODEL; baseUrl: string } {
    return this.toJSON();
  }

  private async requestScores(body: string, count: number): Promise<number[]> {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    let response: Response;
    try {
      const timeout = new Promise<Response>((_, reject) => {
        timer = setTimeout(() => { controller.abort(); reject(gateError("gate_unavailable")); }, this.timeoutMs);
      });
      response = await Promise.race([this.request(`${this.baseUrl}/chat/completions`, {
        method: "POST", signal: controller.signal,
        headers: { authorization: `Bearer ${this.bearer}`, "content-type": "application/json" }, body,
      }), timeout]);
    } catch { throw gateError("gate_unavailable"); }
    finally { if (timer) clearTimeout(timer); }
    if (!response.ok) {
      await response.body?.cancel();
      throw gateError(response.status === 429 || response.status >= 500 ? "gate_unavailable" : "gate_rejected");
    }
    let payload: unknown;
    try { payload = JSON.parse(await response.text()); } catch { throw gateError("gate_mismatch"); }
    return parseScoresPayload(payload, count);
  }

  async score(episodes: readonly { id: string; text: string }[]): Promise<number[]> {
    const body = JSON.stringify({
      model: JEV_MODEL, temperature: 0, max_tokens: 256,
      messages: [
        { role: "system", content: SYSTEM_PROMPT },
        { role: "user", content: JSON.stringify(episodes.map((episode, n) => ({ n, text: truncated(episode.text) }))) },
      ],
    });
    if (Buffer.byteLength(body, "utf8") > MAX_BODY_BYTES) throw gateError("gate_mismatch");
    return this.requestScores(body, episodes.length);
  }
}
