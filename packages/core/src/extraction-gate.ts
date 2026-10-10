/** Cheap pre-gate: one call per window, one score in [0,1] per input in input order. */
export interface ExtractionGate {
  readonly model: string;
  score(episodes: readonly { id: string; text: string }[]): Promise<number[]>;
}

export class ExtractionGateError extends Error {
  constructor(readonly reason: "gate_unavailable" | "gate_rejected" | "gate_mismatch", message?: string) {
    super(message ?? reason);
    this.name = "ExtractionGateError";
  }
}
