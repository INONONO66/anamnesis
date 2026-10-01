export { Store, GenerationReadinessError } from "./store.ts";
export type { AuthoritySnapshot } from "./store.ts";
export { EmbeddingConfig, EmbeddingProfile, HttpEmbeddingProvider, embeddingProfileId, validateVector } from "./embedding.ts";
export type { EmbeddingProvider } from "./embedding.ts";
export { OpenAiEmbeddingProvider } from "./openai-embedding-provider.ts";
export type { OpenAiEmbeddingProviderOptions, OpenAiEmbeddingResult } from "./openai-embedding-provider.ts";
export { admittedBudget, packRecall, renderContext } from "./recall.ts";
export type {
  StoreOptions,
  PutResult,
  SearchHit,
  IntegrityIssue,
} from "./store.ts";

export { Engine, RememberInput, envConfig } from "./engine.ts";
export { HttpExtractionProvider, DeterministicExtractionProvider, validateProviderOutput, validateModelOutput, validateSourceSpans, ExtractionProviderConfig, ExtractionProviderError } from "./extraction.ts";
export { OpenAiChatExtractionProvider } from "./openai-extraction-provider.ts";
export type { OpenAiChatExtractionProviderOptions, ExtractionDialect } from "./openai-extraction-provider.ts";
export type { ExtractionProvider } from "./extraction.ts";
export type { EngineOptions } from "./engine.ts";

export { EpisodeJournal, journaledRemember } from "./journal.ts";
export { materializeFacts, recallDerived } from "./fact-materialization.ts";
export type { RetainedClaim, RetainedExtractionAttempt, RetainedEpisode, MaterializedFact, ConductingArc } from "./fact-materialization.ts";
export { replayDynamics } from "./dynamics/state.ts";
export { solvePpr, exportFixedCsr, solveFixedCsr } from "./dynamics/ppr.ts";
export type { FixedCsr } from "./dynamics/ppr.ts";
export type { DynamicsEvent, DynamicsInput, DynamicsState } from "./dynamics/state.ts";
export type { ReplayOptions } from "./journal.ts";
export { type InstallationContext } from "./store/policy.ts";
export { RecallTransportInput, CommitReceiptInput } from "./store/receipts.ts";
export { ObjectStore } from "./objects.ts";
export { RecallError, type Tokenizers } from "./recall.ts";
export { ExtractionScheduler, type ExtractionTurn } from "./extraction-scheduler.ts";
export { PacedExtractionProvider } from "./paced-extraction-provider.ts";
export { elementDigest, verifyLineageRetry } from "./store/digest.ts";
export { DurableSpool, type SpoolEntry } from "./spool.ts";
export { backoffMs, managedRestartDelayMs, modelTaskRetryDelayMs } from "./backoff.ts";
