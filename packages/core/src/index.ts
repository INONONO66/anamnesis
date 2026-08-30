export { Vault, VaultRecordInput } from "./vault.ts";
export type {
  VaultRecord,
  AppendResult,
  OutboxEntry,
  IntegrityIssue,
} from "./vault.ts";

export { MemoryStore } from "./store.ts";
export type { SearchHit, StoredLink } from "./store.ts";

export { Engine, defaultRoot } from "./engine.ts";
export type { EngineOptions } from "./engine.ts";
