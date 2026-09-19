import { createHash } from "node:crypto";
import { readFileSync, statSync } from "node:fs";
import { isAbsolute } from "node:path";
import { createContext, Script } from "node:vm";
import { RPC_LIMITS } from "../../packages/protocol/src/rpc.ts";
import { RecallError, type Tokenizers } from "../../packages/core/src/recall.ts";

function object(value: unknown, keys: string[]): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).some(key => !keys.includes(key))) throw new Error("invalid tokenizer configuration");
  return value as Record<string, unknown>;
}
function absolute(value: unknown): string {
  if (typeof value !== "string" || !isAbsolute(value)) throw new Error("absolute tokenizer path required");
  return value;
}
function configuration(value: unknown) {
  const input = object(value, ["id", "path", "assets"]);
  if (typeof input["id"] !== "string" || input["id"].length > 256 || !/^.+@sha256:[0-9a-f]{64}$/.test(input["id"])) throw new Error("pinned tokenizer id required");
  if (!Array.isArray(input["assets"]) || input["assets"].length > 64) throw new Error("tokenizer assets list required (maximum 64)");
  const assets = input["assets"].map((value: unknown) => {
    const asset = object(value, ["name", "path"]);
    if (typeof asset["name"] !== "string" || !/^[A-Za-z0-9._-]{1,128}$/.test(asset["name"])) throw new Error("invalid tokenizer asset name");
    return { name: asset["name"], path: absolute(asset["path"]) };
  });
  return { id: input["id"], path: absolute(input["path"]), assets };
}
const hash = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex");
function snapshot(path: string, maximum: number): Buffer {
  const stat = statSync(path);
  if (!stat.isFile() || stat.size > maximum) throw new Error("tokenizer file exceeds limit or is not a file");
  const bytes = readFileSync(path);
  if (bytes.length > maximum) throw new Error("tokenizer file exceeds limit");
  return bytes;
}

/** Trusted operator code, not a security sandbox. Execute the same bytes that
 * were hashed; no module resolver or filesystem API is provided to the bundle. */
export function loadTokenizers(raw = process.env["ANAMNESIS_TOKENIZER_CONFIG"]): Tokenizers {
  if (raw === undefined) return new Map();
  const config = configuration(JSON.parse(raw));
  const code = snapshot(config.path, 16 * 1024 * 1024);
  const assets = Object.create(null) as Record<string, Uint8Array>;
  const manifest: [string, string][] = [];
  let remaining = 256 * 1024 * 1024;
  for (const asset of [...config.assets].sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0)) {
    if (Object.hasOwn(assets, asset.name)) throw new Error("duplicate tokenizer asset");
    const bytes = snapshot(asset.path, remaining); remaining -= bytes.length;
    assets[asset.name] = new Uint8Array(bytes);
    manifest.push([asset.name, hash(bytes)]);
  }
  const digest = hash(Buffer.from(JSON.stringify(["anamnesis-tokenizer-v1", hash(code), manifest])));
  if (!config.id.endsWith(`@sha256:${digest}`)) throw new Error("tokenizer digest mismatch");
  const module = { exports: {} };
  const provider = createContext({ module, assets, TextEncoder, TextDecoder });
  new Script(code.toString("utf8"), { filename: config.path }).runInContext(provider, { timeout: 5000 });
  // A separate lexical environment keeps arbitrary bundle declarations out of
  // the host bridge. Cross-context calls still execute under the VM timeout.
  const scope = createContext({ module, assets });
  new Script("encode = module.exports.createEncoder(assets); if (typeof encode !== 'function') throw new Error('encoder required');")
    .runInContext(scope, { timeout: 5000 });
  const invoke = new Script("encode(text)");
  const encode = (text: string): number => {
    if (Buffer.byteLength(text) > RPC_LIMITS.frame_bytes) throw new RecallError("resource_exhausted");
    scope["text"] = text;
    try {
      const count: unknown = invoke.runInContext(scope, { timeout: 1000 });
      if (typeof count !== "number" || !Number.isSafeInteger(count) || count < 0) throw new RecallError("invalid_budget");
      return count;
    } finally { scope["text"] = undefined; }
  };
  if (encode("") !== 0) throw new Error("tokenizer must encode empty context as zero tokens");
  return new Map([[config.id, encode]]);
}
