/**
 * Originals are immutable once stored, so masking has to happen before the
 * write boundary. Patterns stay narrow: a false positive destroys real content
 * permanently, while a missed secret is recoverable by re-ingesting a masked
 * revision.
 */
const PATTERNS: readonly RegExp[] = [
  /xox[abposr]-[A-Za-z0-9-]{10,}/g,
  /ghp_[A-Za-z0-9]{36,}/g,
  /github_pat_[A-Za-z0-9_]{22,}/g,
  /sk-[A-Za-z0-9]{20,}/g,
  /AKIA[0-9A-Z]{16}/g,
  /-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----/g,
  /eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}/g,
  /\b(?:api[_-]?key|secret|token|password|passwd)\b["'\s:=]{1,4}[A-Za-z0-9/+_-]{20,}/gi,
];

export const REDACTION = "[REDACTED]";

export interface MaskResult {
  text: string;
  redactions: number;
}

export function maskSecrets(text: string): MaskResult {
  let masked = text;
  let redactions = 0;
  for (const pattern of PATTERNS) {
    masked = masked.replace(pattern, () => {
      redactions += 1;
      return REDACTION;
    });
  }
  return { text: masked, redactions };
}
