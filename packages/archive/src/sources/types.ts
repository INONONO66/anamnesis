import type { ArchiveRecord } from "../record.ts";

export type ParsedSession = {
  readonly sessionId: string;
  readonly records: readonly (Omit<ArchiveRecord, "seq" | "file"> & { readonly line: number })[];
};

export interface SourceAdapter {
  readonly kind: string;
  files(root: string): AsyncGenerator<string>;
  parse(path: string, bytes: Uint8Array): Promise<ParsedSession[]>;
}
