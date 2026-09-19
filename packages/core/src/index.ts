export { Store, GenerationReadinessError } from "./store.ts";
export { EmbeddingConfig, EmbeddingProfile, HttpEmbeddingProvider, embeddingProfileId, validateVector } from "./embedding.ts";
export type { EmbeddingProvider } from "./embedding.ts";
export { admittedBudget, packRecall, renderContext } from "./recall.ts";
export type {
  StoreOptions,
  PutResult,
  SearchHit,
  IntegrityIssue,
} from "./store.ts";

export { Engine, RememberInput, envConfig } from "./engine.ts";
export { HttpExtractionProvider, DeterministicExtractionProvider, validateProviderOutput, validateModelOutput, validateSourceSpans, ExtractionProviderConfig, ExtractionProviderError } from "./extraction.ts";
export type { ExtractionProvider } from "./extraction.ts";
export type { EngineOptions } from "./engine.ts";

export { EpisodeJournal, journaledRemember } from "./journal.ts";
export { materializeFacts, recallDerived } from "./fact-materialization.ts";
export type { RetainedClaim, RetainedExtractionAttempt, RetainedEpisode, MaterializedFact, ConductingArc } from "./fact-materialization.ts";
export { replayDynamics } from "./dynamics/state.ts";
export { DreamAdmission, DreamAdmissionError, FileDreamJobStore } from "./dreaming-admission.ts";
export type { DreamAdmissionInput, DreamFence, DreamJob, DreamLease, DreamPhase, DreamSourceReceipt, DreamJobStore, DreamExecutionReview, DreamExport, LeidenSpec, LeidenResult, DreamPublisher, DreamExecutor } from "./dreaming-admission.ts";
export { trustedDreamLeiden, DreamAdapterError, DreamLeidenInput, DreamLeidenOutput, DREAM_GDS_IMAGE, DREAM_GDS_VERSION, DREAM_ALGORITHM, DREAM_NETWORK, DREAM_LIMITS } from "./dream-leiden-adapter.ts";
export type { DreamLeidenAdapter, TrustedDreamLeidenConfig } from "./dream-leiden-adapter.ts";
export { solvePpr, exportFixedCsr, solveFixedCsr } from "./dynamics/ppr.ts";
export type { FixedCsr } from "./dynamics/ppr.ts";
export type { DynamicsEvent, DynamicsInput, DynamicsState } from "./dynamics/state.ts";
export type { ReplayOptions } from "./journal.ts";
