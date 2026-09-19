/** JSON Schema keeps the protocol consumable outside TypeScript. */
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";
import {
  KNOWN_SCHEMAS,
  MemoryElement,
  MemoryLink,
  RpcRequest,
  RpcResponse,
  SCHEMA_LABELS,
  TIME_BEARING,
} from "../src/index.ts";
import { Generation, ExtractionAttempt, ModelTask, Coverage, ExtractionModelOutput, CompleteExtractionAttempt, AdvanceExtractionCoverage, SelectExtractionGeneration, ExtractionSelection, ReadExtractionCoverage, ExtractionCoverageRead } from "../src/extraction.ts";

import { SemanticClaim, SemanticClaimBatch } from "../src/semantic-claim.ts";
import { EchoLineage, EpisodeLineageInput } from "../src/episode-lineage.ts";

import { ProposeRetainedClaim, MaterializeRetainedClaim, ReviewRetainedClaim, SemanticResolution, SemanticReviewOutput, SemanticReviewPremises, RetainedSemanticProposal } from "../src/materialization.ts";
import { ExtractionJudgeInput, ExtractionDisposition, ExtractionPipeline, CreateExtractionPipeline } from '../src/extraction-audit.ts';

export function protocolJsonSchemas() {
  const element = z.toJSONSchema(MemoryElement, { target: "draft-2020-12" });
  // Zod refinements do not export automatically. Keep event-time admission
  // conditional on the shared registry, not required for every element kind.
  element.allOf = [{
    if: {
      properties: {
        schema: {
          enum: KNOWN_SCHEMAS.filter((schema) => TIME_BEARING[SCHEMA_LABELS[schema]]),
        },
      },
      required: ["schema"],
    },
    then: { required: ["time"] },
  }];
  return {
    "memory-element": element,
    "memory-link": z.toJSONSchema(MemoryLink, { target: "draft-2020-12" }),
    // Input mode preserves optional caller-supplied defaults on Episode fields.
    "rpc-request": z.toJSONSchema(RpcRequest, { target: "draft-2020-12", io: "input" }),
    "rpc-response": z.toJSONSchema(RpcResponse, { target: "draft-2020-12" }),
    // Structural wire shapes only. Cross-field byte/digest/source/state checks
    // are enforced by the protocol parser and fenced Store, not JSON Schema.
    "episode-lineage-input": z.toJSONSchema(EpisodeLineageInput, { target: "draft-2020-12" }),
    "echo-lineage": z.toJSONSchema(EchoLineage, { target: "draft-2020-12" }),
    "semantic-claim": z.toJSONSchema(SemanticClaim, { target: "draft-2020-12" }),
    "semantic-claim-batch": z.toJSONSchema(SemanticClaimBatch, { target: "draft-2020-12" }),
    "extraction-audit-input": z.toJSONSchema(ExtractionJudgeInput, { target: "draft-2020-12" }),
    "extraction-audit-disposition": z.toJSONSchema(ExtractionDisposition, { target: "draft-2020-12" }),
    "extraction-audit-pipeline": z.toJSONSchema(ExtractionPipeline, { target: "draft-2020-12" }),
    "extraction-audit-create": z.toJSONSchema(CreateExtractionPipeline, { target: "draft-2020-12" }),
    "extraction-generation": z.toJSONSchema(Generation, { target: "draft-2020-12" }),
    "extraction-attempt": z.toJSONSchema(ExtractionAttempt, { target: "draft-2020-12" }),
    "model-task": z.toJSONSchema(ModelTask, { target: "draft-2020-12" }),
    "extraction-coverage": z.toJSONSchema(Coverage, { target: "draft-2020-12" }),
    "extraction-model-output": z.toJSONSchema(ExtractionModelOutput, { target: "draft-2020-12" }),
    "extraction-complete": z.toJSONSchema(CompleteExtractionAttempt, { target: "draft-2020-12" }),
    "extraction-coverage-advance": z.toJSONSchema(AdvanceExtractionCoverage, { target: "draft-2020-12" }),
    "extraction-select": z.toJSONSchema(SelectExtractionGeneration, { target: "draft-2020-12" }),
    "extraction-selection": z.toJSONSchema(ExtractionSelection, { target: "draft-2020-12" }),
    "extraction-coverage-request": z.toJSONSchema(ReadExtractionCoverage, { target: "draft-2020-12" }),
    "extraction-coverage-read": z.toJSONSchema(ExtractionCoverageRead, { target: "draft-2020-12" }),
    "materialization-propose": z.toJSONSchema(ProposeRetainedClaim, { target: "draft-2020-12" }),
    "materialization-input": z.toJSONSchema(MaterializeRetainedClaim, { target: "draft-2020-12" }),
    "materialization-review": z.toJSONSchema(ReviewRetainedClaim, { target: "draft-2020-12" }),
    "materialization-resolution": z.toJSONSchema(SemanticResolution, { target: "draft-2020-12" }),
    "materialization-output": z.toJSONSchema(SemanticReviewOutput, { target: "draft-2020-12" }),
    "materialization-premises": z.toJSONSchema(SemanticReviewPremises, { target: "draft-2020-12" }),
    "materialization-proposal": z.toJSONSchema(RetainedSemanticProposal, { target: "draft-2020-12" }),
  };
}

if (import.meta.main) {
  const outDir = join(import.meta.dir, "..", "schemas");
  await mkdir(outDir, { recursive: true });
  for (const [name, json] of Object.entries(protocolJsonSchemas())) {
    await writeFile(
      join(outDir, `${name}.schema.json`),
      JSON.stringify(json, null, 2) + "\n",
    );
    console.log(`exported schemas/${name}.schema.json`);
  }
}
