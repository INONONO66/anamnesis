# @anamnesis/archive

Lossless, content-addressed archive of every session source anamnesis can read. Backfill turns sources into Episodes and drops what recall does not need; the archive keeps everything so a later extraction pass can re-read the originals without the source machine.

## Layout

```
<archive>/
  raw/<sha256[0:2]>/<sha256>          byte-identical copy of every source file, keyed by content hash
  records/<source>/<sessionId>.jsonl  one ArchiveRecord per line (see src/record.ts)
  manifest.sqlite                     sessions, session_paths, file_sessions, texts, records
```

A source file is copied once per sha256. A file that appears at two paths (synced copy, backup) is recorded as one session with two `session_paths` rows. Collecting the same root twice changes nothing.

## CLI

```
bun packages/archive/src/cli.ts collect <source> <root> --archive <dir>
```

`source` is one of `omo`, `pi`, `gjc`, `misc`, `claude`, `codex`, `slack`, `notion`, `agentlog`. The command prints `source<TAB>sessions<TAB>records` and exits 2 on usage errors or an unknown source.

Examples for this Mac:

```
bun packages/archive/src/cli.ts collect omo    ~/.omo/agent/sessions --archive ~/anamnesis-archive
bun packages/archive/src/cli.ts collect claude ~/.claude/projects    --archive ~/anamnesis-archive
bun packages/archive/src/cli.ts collect codex  ~/.codex/sessions     --archive ~/anamnesis-archive
```

## Record contract

Every record carries `origin` (`source`, `session`, `actor`, `record`), `seq`, `role`, `occurred_at`, `text`, optional `reasoning`, `kind`, `canonical_kind`, a `revision` (`sha256(occurred_at + "\n" + text)`) and `file` (`sha256`, `path`, `line`). For any record backfill also emits, `origin` and `occurred_at` are identical to the backfill Episode, so archive records and Episodes join on `origin` without a mapping table. Text is stored verbatim; nothing is normalised or truncated.

## What is dropped

Only three things never become records:

- tool-call and tool-result parts with no prose (the raw copy still has them);
- derived transcripts that re-encode a session already archived (`runtime/transcripts/`, `._*` sidecars, Claude `journal.jsonl`);
- files under a session root that are not transcripts, such as omo `extensions/goal/*.history.jsonl` goal history (no session header).

Everything else, including thinking blocks, system events, compaction markers, oversized agentlog events and sessions backfill skips, is kept.
