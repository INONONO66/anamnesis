import { z } from "zod";

export const TimePrecision = z.enum([
  "second",
  "minute",
  "day",
  "month",
  "year",
]);
export type TimePrecision = z.infer<typeof TimePrecision>;

export const TimePoint = z
  .object({
    value: z.iso.datetime({ offset: true }),
    precision: TimePrecision,
  })
  .strict();
export type TimePoint = z.infer<typeof TimePoint>;

/** The tuple is globally unique so repeated ingestion remains idempotent. */
export const Origin = z
  .object({
    source: z.string().min(1),
    session: z.string().min(1),
    actor: z.string().min(1),
    record: z.string().min(1),
  })
  .strict();
export type Origin = z.infer<typeof Origin>;

/** Unknown schema IDs remain valid so the registry can evolve independently. */
export const ElementSchemaId = z
  .string()
  .regex(
    /^anamnesis\.[a-z][a-z0-9-]*\/[1-9][0-9]*$/,
    "expected `anamnesis.<kind>/<version>` (e.g. anamnesis.claim/1)",
  );
export type ElementSchemaId = z.infer<typeof ElementSchemaId>;

export const Celestial = z.enum(["Episode", "Entity", "Fact", "Community"]);
export type Celestial = z.infer<typeof Celestial>;

const SCHEMA_REGISTRY = {
  "anamnesis.original-message/1": "Episode",
  "anamnesis.original-document/1": "Episode",
  "anamnesis.entity/1": "Entity",
  "anamnesis.claim/1": "Fact",
  "anamnesis.mapping/1": "Fact",
  "anamnesis.synthesis/1": "Fact",
  "anamnesis.community/1": "Community",
} as const satisfies Record<string, Celestial>;

export type KnownSchema = keyof typeof SCHEMA_REGISTRY;
export const KNOWN_SCHEMAS = Object.keys(SCHEMA_REGISTRY) as KnownSchema[];
/** Unknown schemas intentionally receive only the common `Element` label. */
export const SCHEMA_LABELS: Readonly<Record<KnownSchema, Celestial>> =
  SCHEMA_REGISTRY;

/** The tag remains structural because dynamics, not storage, consumes it. */
export const ClaimSubKind = z.enum([
  "fact",
  "state",
  "event",
  "preference",
  "procedure",
  "decision",
  "summary",
]);
export type ClaimSubKind = z.infer<typeof ClaimSubKind>;

/**
 * Every element has one event or observation time. An entity's TimePoint records
 * when the anchor was observed; entities are not modeled as timeless.
 */
export const MemoryElement = z
  .object({
    /** UUIDv7 preserves creation order in the identifier. */
    id: z.uuidv7(),
    schema: ElementSchemaId,
    time: TimePoint,
    content: z.string().min(1),
    origin: Origin,
    /** Effective mass is derived at recall so decay needs no mutable state. */
    mass: z.number().min(0).max(1).default(0.5),
    /** Values stay JSON-safe because properties are persisted and exported. */
    properties: z.record(z.string(), z.json()).default({}),
  })
  .strict()
  .superRefine((element, context) => {
    if (
      element.schema === "anamnesis.claim/1" &&
      "sub_kind" in element.properties &&
      !ClaimSubKind.safeParse(element.properties.sub_kind).success
    ) {
      context.addIssue({
        code: "custom",
        path: ["properties", "sub_kind"],
        message: "expected a recognized claim sub_kind",
      });
    }
  });
export type MemoryElement = z.infer<typeof MemoryElement>;
export type MemoryElementInput = z.input<typeof MemoryElement>;
