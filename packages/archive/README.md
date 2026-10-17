# @anamnesis/archive

Lossless, content-addressed archive of every session source anamnesis can read. Backfill turns sources into Episodes and drops what recall does not need; the archive keeps everything so a later extraction pass can re-read the originals without the source machine. `admit` replays archived records into a running daemon, so the archive is a path into the graph that no longer needs the source machine.

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

### admit

```
bun packages/archive/src/cli.ts admit --archive <dir> --socket <uds path> --token <token> [--source <kind>] [--since <iso-datetime>]
```

Walks the archive's records in a fixed order (source, then session id, then the manifest's record order for that session: `occurred_at`, file hash, line, seq) and sends each one to the daemon as a single Episode through the `remember` RPC on the UDS socket. It uses the daemon's own socket client (`app/anamnesis/client.ts` plus the payload upload from `app/anamnesis/source.ts`) and builds the Episode with backfill's builders for that source kind, so the bodies match; a `payload_hash` is sent only for records whose masked text exceeds the inline limit and is the hash of that masked text. With `--source` only that kind is admitted, otherwise every source present in the manifest.

With `--since <iso-datetime>` a record whose `occurred_at` is earlier than the bound is skipped: it is not sent, it is counted as `skipped` in the report (which also echoes the bound as `since`), and it never moves the cursor. A record exactly at the bound is admitted. The cursor is positional, so a resumed run re-applies the bound to the records after the acknowledged position; records before it are neither revisited nor counted again.

The daemon decides what counts as new. Each `remember` answer is tallied as one of:

- `created`: the Episode was not in the graph;
- `duplicate`: an Episode with the same origin and revision was already there;
- `failed`: the daemon rejected the record (the reason goes to stderr).

Stdout carries one JSON object per admitted source (one line when `--source` is given):

```json
{"source":"omo","records":1234,"created":1200,"duplicates":34,"failed":0,"skipped":0,"since":null,"resumed_from":null}
```

`resumed_from` is the cursor the run continued after, or `null` for a run that started from the first record.

#### Cursor and resume

Progress is kept per source in `<archive>/admit/<source>.cursor.json`:

```json
{
  "status": "in_progress",
  "last": { "session_id": "...", "index": 41 },
  "created": 1200,
  "duplicates": 34,
  "updated_at": "2026-10-17T09:30:00.000Z"
}
```

The file is rewritten after every successful `remember`, through a temp file and an atomic rename, so a crash can't leave a half-written cursor. `last` names the record that was just acknowledged (session id and zero-based index in that session's manifest record order, which spans every file of the session).

On start:

- `status: "in_progress"` means a previous run stopped early. The new run skips everything up to and including `last`, starts at the next record, and reports that cursor as `resumed_from`.
- `status: "complete"`, or no cursor file at all, means a full run from the first record. Records the daemon already holds come back as duplicates, so re-running a finished source is safe and shows up in the report as `duplicates`.

A run that reaches the end writes the cursor with `status: "complete"`. On SIGTERM or SIGINT the command finishes the `remember` already in flight, writes the cursor, and exits with 143 or 130. The cursor is never lost to a signal. A record the daemon refuses is counted as `failed` and the run continues past it (the reason goes to stderr; a later full run retries it); a transport or retryable error aborts the run with the cursor at the last acknowledged record, so the next run retries from there.

## Record contract

Every record carries `origin` (`source`, `session`, `actor`, `record`), `seq`, `role`, `occurred_at`, `text`, optional `reasoning`, `kind`, `canonical_kind`, a `revision` (`sha256(occurred_at + "\n" + text)`) and `file` (`sha256`, `path`, `line`). For any record backfill also emits, `origin` and `occurred_at` are identical to the backfill Episode, so archive records and Episodes join on `origin` without a mapping table. Text is stored verbatim; nothing is normalised or truncated.

## What is dropped

Only three things never become records:

- tool-call and tool-result parts with no prose (the raw copy still has them);
- derived transcripts that re-encode a session already archived (`runtime/transcripts/`, `._*` sidecars, Claude `journal.jsonl`);
- files under a session root that are not transcripts, such as omo `extensions/goal/*.history.jsonl` goal history (no session header).

Everything else, including thinking blocks, system events, compaction markers, oversized agentlog events and sessions backfill skips, is kept.
