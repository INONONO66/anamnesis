//! Transcript sourcing + parsing for capture hooks.
//!
//! Two structurally different schemas (measured):
//! - Claude Code: `{"type":"user"|"assistant","message":{"role","content"},"timestamp":ISO}`
//! - Codex rollout: `{"timestamp":ISO,"type":"response_item","payload":{"type":"message","role","content":[{"text"}]}}`
//!
//! Both reduce to user+assistant `ParsedTurn`s; all other line/role kinds are dropped.

use std::path::PathBuf;

use serde_json::Value;

/// Only the transcript TAIL is ever read (a hook needs the last ≤50 turns, not
/// the whole session): bounds per-`Stop` I/O to a constant instead of O(session)
/// — which would otherwise compound to O(n²) over a long session — and caps
/// memory for arbitrarily large transcripts.
const TRANSCRIPT_TAIL_BYTES: u64 = 256 * 1024;

/// Hard cap on directory entries visited when searching `~/.codex/sessions` —
/// a huge session tree must never blow the hook's time budget.
const ROLLOUT_WALK_CAP: usize = 4096;

/// Resolve transcript contents: `transcript_path` if readable, else locate by
/// `session_id` (Codex `~/.codex/sessions/**/rollout-*<sid>*.jsonl`, then CC
/// `~/.claude/projects/<cwd-slug>/<sid>.jsonl`). Returns None if nothing found.
///
/// Reads only the last [`TRANSCRIPT_TAIL_BYTES`] of the file (bounded I/O).
pub fn resolve_transcript(
    transcript_path: Option<&str>,
    session_id: Option<&str>,
    cwd: Option<&str>,
) -> Option<String> {
    if let Some(p) = transcript_path
        && let Some(s) = read_transcript_tail(std::path::Path::new(p), TRANSCRIPT_TAIL_BYTES)
    {
        return Some(s);
    }
    let sid = session_id?;
    if let Some(p) = newest_codex_rollout(sid)
        && let Some(s) = read_transcript_tail(&p, TRANSCRIPT_TAIL_BYTES)
    {
        return Some(s);
    }
    if let Some(p) = cc_transcript_path(sid, cwd)
        && let Some(s) = read_transcript_tail(&p, TRANSCRIPT_TAIL_BYTES)
    {
        return Some(s);
    }
    None
}

/// Read at most the last `max_bytes` of a file as UTF-8 (lossy). When the
/// window starts mid-file, everything through the first `\n` is dropped so the
/// result begins on a whole JSONL line. `None` on any I/O error (fail-open).
fn read_transcript_tail(path: &std::path::Path, max_bytes: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let truncated = len > max_bytes;
    if truncated {
        f.seek(SeekFrom::Start(len - max_bytes)).ok()?;
    }
    let mut buf = Vec::with_capacity(len.min(max_bytes) as usize);
    f.read_to_end(&mut buf).ok()?;
    let s = String::from_utf8_lossy(&buf);
    if truncated {
        // Drop the partial first line so parsing starts on a line boundary; a
        // window that is one giant partial line has nothing parseable (None).
        s.find('\n').map(|i| s[i + 1..].to_string())
    } else {
        Some(s.into_owned())
    }
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Newest `rollout-*<sid>*.jsonl` under ~/.codex/sessions (bounded walk:
/// at most [`ROLLOUT_WALK_CAP`] entries are visited; best-so-far wins).
fn newest_codex_rollout(sid: &str) -> Option<PathBuf> {
    let root = home()?.join(".codex/sessions");
    newest_rollout_under(&root, sid, ROLLOUT_WALK_CAP)
}

fn newest_rollout_under(root: &std::path::Path, sid: &str, cap: usize) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut visited = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            visited += 1;
            if visited > cap {
                return best.map(|(_, p)| p);
            }
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(name) = path.file_name().and_then(|s| s.to_str())
                && name.starts_with("rollout-")
                && name.ends_with(".jsonl")
                && name.contains(sid)
                && let Ok(mtime) = e.metadata().and_then(|m| m.modified())
                && best.as_ref().is_none_or(|(t, _)| mtime > *t)
            {
                best = Some((mtime, path));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// CC transcript: `~/.claude/projects/<cwd-with-slashes-as-dashes>/<sid>.jsonl`.
fn cc_transcript_path(sid: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let cwd = cwd?;
    let slug = cwd.replace(['/', '.'], "-"); // matches CC's project-dir slugging
    let p = home()?
        .join(".claude/projects")
        .join(slug)
        .join(format!("{sid}.jsonl"));
    p.exists().then_some(p)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTurn {
    pub speaker: String,
    pub text: String,
    pub at_ms: Option<u64>,
}

/// Parse a whole transcript (JSONL) into ordered user+assistant turns.
/// Auto-detects CC vs Codex; unrecognized/garbage lines are skipped (fail-open).
pub fn parse_transcript(contents: &str) -> Vec<ParsedTurn> {
    let mut out = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(mut turn) = parse_codex_line(&v).or_else(|| parse_cc_line(&v)) {
            // Scrub at the parse boundary so every consumer (capture-hook Ingest
            // and any future reader) gets redacted text — plaintext keys never
            // reach the daemon or the SQLite store.
            turn.text = redact_secrets(&turn.text);
            out.push(turn);
        }
    }
    out
}

/// Redact obvious secrets (API keys, tokens) from a turn's text before it is
/// persisted. Conservative by design — precision over recall: only unmistakable,
/// prefix-anchored key shapes are replaced with `[REDACTED:<kind>]`. Pure and
/// total: never panics; secret-free text is returned byte-for-byte unchanged.
///
/// A match is only attempted at a word boundary (start, or after a non-alnum
/// byte), so a token embedded inside a longer identifier (e.g. `disk-…`) cannot
/// trip the `sk-` rule. All recognized prefixes/bodies are ASCII, so byte
/// lengths always land on a UTF-8 char boundary.
pub fn redact_secrets(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut prev_alnum = false;
    while i < input.len() {
        if !prev_alnum && let Some((len, kind)) = match_secret_at(&input[i..]) {
            out.push_str("[REDACTED:");
            out.push_str(kind);
            out.push(']');
            i += len;
            prev_alnum = false;
            continue;
        }
        let Some(ch) = input[i..].chars().next() else {
            break;
        };
        out.push(ch);
        prev_alnum = ch.is_ascii_alphanumeric();
        i += ch.len_utf8();
    }
    out
}

/// Length of the leading run of bytes satisfying `allow`.
fn secret_run_len(bytes: &[u8], allow: fn(u8) -> bool) -> usize {
    let mut n = 0;
    while n < bytes.len() && allow(bytes[n]) {
        n += 1;
    }
    n
}

fn is_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

/// Match one known secret shape anchored at the START of `s`; returns the byte
/// length consumed and the kind label. Patterns are deliberately narrow.
fn match_secret_at(s: &str) -> Option<(usize, &'static str)> {
    // `sk-[A-Za-z0-9]{20,}` — OpenAI-style key.
    if let Some(n) = match_run(s, "sk-", 20, is_alnum) {
        return Some((n, "openai-key"));
    }
    // `gh[pousr]_[A-Za-z0-9]{20,}` — GitHub PAT / OAuth / refresh families.
    if let Some(n) = match_github(s) {
        return Some((n, "github-token"));
    }
    // `AKIA[0-9A-Z]{16,}` — AWS access key id.
    if let Some(n) = match_aws(s) {
        return Some((n, "aws-key"));
    }
    // `Bearer[ \t]+[A-Za-z0-9._-]{20,}` — bearer credential (often a JWT).
    if let Some(n) = match_bearer(s) {
        return Some((n, "bearer-token"));
    }
    None
}

/// `<prefix>` then a run of ≥ `min_body` bytes satisfying `allow`.
fn match_run(s: &str, prefix: &str, min_body: usize, allow: fn(u8) -> bool) -> Option<usize> {
    let rest = s.strip_prefix(prefix)?;
    let n = secret_run_len(rest.as_bytes(), allow);
    (n >= min_body).then_some(prefix.len() + n)
}

fn match_github(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 4
        || &b[0..2] != b"gh"
        || b[3] != b'_'
        || !matches!(b[2], b'p' | b'o' | b'u' | b's' | b'r')
    {
        return None;
    }
    let n = secret_run_len(&b[4..], is_alnum);
    (n >= 20).then_some(4 + n)
}

fn match_aws(s: &str) -> Option<usize> {
    let rest = s.strip_prefix("AKIA")?;
    let n = secret_run_len(rest.as_bytes(), |c| {
        c.is_ascii_uppercase() || c.is_ascii_digit()
    });
    (n >= 16).then_some(4 + n)
}

fn match_bearer(s: &str) -> Option<usize> {
    let rest = s.strip_prefix("Bearer")?;
    let ws = secret_run_len(rest.as_bytes(), |c| c == b' ' || c == b'\t');
    if ws == 0 {
        return None;
    }
    let body = secret_run_len(&rest.as_bytes()[ws..], |c| {
        c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || c == b'-'
    });
    (body >= 20).then_some("Bearer".len() + ws + body)
}

/// Codex rollout: `type=="response_item"`, `payload.type=="message"`.
fn parse_codex_line(v: &Value) -> Option<ParsedTurn> {
    if v.get("type")?.as_str()? != "response_item" {
        return None;
    }
    let payload = v.get("payload")?;
    if payload.get("type")?.as_str()? != "message" {
        return None;
    }
    let role = payload.get("role")?.as_str()?;
    if role != "user" && role != "assistant" {
        return None;
    }
    let text = payload
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() {
        return None;
    }
    Some(ParsedTurn {
        speaker: role.to_string(),
        text,
        at_ms: iso_ms(v.get("timestamp")),
    })
}

/// Claude Code: top-level `type=="user"|"assistant"`, `message.{role,content}`.
fn parse_cc_line(v: &Value) -> Option<ParsedTurn> {
    let ty = v.get("type")?.as_str()?;
    if ty != "user" && ty != "assistant" {
        return None;
    }
    let message = v.get("message")?;
    let role = message.get("role").and_then(Value::as_str).unwrap_or(ty);
    if role != "user" && role != "assistant" {
        return None;
    }
    let text = match message.get("content")? {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => return None,
    };
    if text.is_empty() {
        return None;
    }
    Some(ParsedTurn {
        speaker: role.to_string(),
        text,
        at_ms: iso_ms(v.get("timestamp")),
    })
}

/// Parse an ISO-8601 timestamp Value to epoch-ms. Best-effort: None on absence.
/// Avoids a chrono dep — parses the fixed `YYYY-MM-DDTHH:MM:SS(.sss)Z` shape.
fn iso_ms(v: Option<&Value>) -> Option<u64> {
    let s = v?.as_str()?;
    parse_iso8601_to_ms(s)
}

/// Convert an ISO-8601 UTC timestamp `YYYY-MM-DDTHH:MM:SS[.fff]Z` to epoch-ms.
///
/// Uses Howard Hinnant's days-from-civil algorithm — no chrono dep.
/// Independently verified: `2026-06-26T06:20:56.351Z` → 1782454856351.
fn parse_iso8601_to_ms(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    if s.len() < 19 || bytes[4] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let h: u64 = s.get(11..13)?.parse().ok()?;
    let mi: u64 = s.get(14..16)?.parse().ok()?;
    let se: u64 = s.get(17..19)?.parse().ok()?;
    let ms: u64 = s.get(20..23).and_then(|f| f.parse().ok()).unwrap_or(0);
    // days-from-civil (Howard Hinnant), valid for Gregorian dates.
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + (h * 3600 + mi * 60 + se) as i64;
    u64::try_from(secs).ok().map(|s| s * 1000 + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_transcript_path_when_readable() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("t.jsonl");
        std::fs::write(&f, "hello").unwrap();
        let got = resolve_transcript(Some(f.to_str().unwrap()), None, None);
        assert_eq!(got.as_deref(), Some("hello"));
    }

    #[test]
    fn resolve_none_when_path_missing_and_no_session() {
        assert!(resolve_transcript(Some("/no/such/file.jsonl"), None, None).is_none());
        assert!(resolve_transcript(None, None, None).is_none());
    }

    #[test]
    fn tail_read_small_file_reads_whole() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("small.jsonl");
        std::fs::write(&f, "hello").unwrap();
        assert_eq!(read_transcript_tail(&f, 1024).as_deref(), Some("hello"));
    }

    /// A file larger than the window: the partial first line inside the window
    /// is dropped, and the remaining whole lines still parse.
    #[test]
    fn tail_read_drops_partial_first_line_and_parses() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("big.jsonl");
        let line1 = format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{}\"}}}}\n",
            "x".repeat(300)
        );
        let line2 = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"tail ok\"}}\n";
        std::fs::write(&f, format!("{line1}{line2}")).unwrap();
        // Window covers line2 plus a slice of line1's tail — cuts mid-line-1.
        let tail = read_transcript_tail(&f, (line2.len() + 20) as u64).unwrap();
        assert!(!tail.contains("xxx"), "partial first line dropped: {tail}");
        let turns = parse_transcript(&tail);
        assert_eq!(turns.len(), 1, "only the whole line parses: {turns:?}");
        assert_eq!(turns[0].text, "tail ok");
    }

    /// The rollout walk finds the newest matching file in nested dirs and never
    /// exceeds its visit cap (cap-1 walk returns without hanging).
    #[test]
    fn rollout_walk_finds_newest_and_respects_cap() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("2026/07/01");
        std::fs::create_dir_all(&sub).unwrap();
        let old = sub.join("rollout-a-sid1.jsonl");
        let newer = sub.join("rollout-b-sid1.jsonl");
        std::fs::write(&old, "old").unwrap();
        std::fs::write(&newer, "new").unwrap();
        // Deterministic mtimes (no sleeps): old ← now-10s.
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(past)
            .unwrap();
        let found = newest_rollout_under(dir.path(), "sid1", 4096).unwrap();
        assert_eq!(found, newer, "newest mtime wins");
        // Tiny cap: returns best-so-far (possibly None) without hanging.
        let _ = newest_rollout_under(dir.path(), "sid1", 1);
    }

    const CC: &str = r#"{"type":"mode","mode":"normal","sessionId":"x"}
{"type":"file-history-snapshot","messageId":"m"}
{"type":"user","message":{"role":"user","content":"why sqlite?"},"timestamp":"2026-06-26T06:20:56.351Z"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"jsonb + rls"}]},"timestamp":"2026-06-26T06:21:00.000Z"}
{"type":"system","content":"noise"}"#;

    const CODEX: &str = r#"{"timestamp":"2026-06-02T01:42:22.775Z","type":"session_meta","payload":{"id":"x"}}
{"timestamp":"2026-06-02T01:42:22.781Z","type":"event_msg","payload":{"type":"task_started"}}
{"timestamp":"2026-06-02T01:42:30.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"why sqlite?"}]}}
{"timestamp":"2026-06-02T01:42:35.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"jsonb + rls"}]}}"#;

    #[test]
    fn parses_cc_user_and_assistant_only() {
        let turns = parse_transcript(CC);
        assert_eq!(turns.len(), 2, "only user+assistant turns: {turns:?}");
        assert_eq!(turns[0].speaker, "user");
        assert_eq!(turns[0].text, "why sqlite?");
        assert_eq!(turns[1].speaker, "assistant");
        assert_eq!(turns[1].text, "jsonb + rls");
        assert!(turns[0].at_ms.unwrap() > 0, "timestamp parsed to epoch-ms");
    }

    #[test]
    fn parses_codex_rollout_messages_only() {
        let turns = parse_transcript(CODEX);
        assert_eq!(turns.len(), 2, "session_meta/event_msg dropped: {turns:?}");
        assert_eq!(turns[0].speaker, "user");
        assert_eq!(turns[0].text, "why sqlite?");
        assert_eq!(turns[1].speaker, "assistant");
        assert_eq!(turns[1].text, "jsonb + rls");
    }

    #[test]
    fn empty_or_garbage_yields_no_turns() {
        assert!(parse_transcript("").is_empty());
        assert!(parse_transcript("not json\n{also not}").is_empty());
    }

    /// Independently verified via Python:
    ///   datetime(2026,6,26,6,20,56,351000,tz=utc).timestamp()*1000 == 1782454856351
    #[test]
    fn iso_instant_known_value() {
        assert_eq!(
            parse_iso8601_to_ms("2026-06-26T06:20:56.351Z"),
            Some(1782454856351u64)
        );
    }

    #[test]
    fn redacts_common_secret_shapes_but_not_prose() {
        // Given: prose wrapping three unmistakable secret shapes.
        let sk = "sk-ABCDEFGHIJKLMNOPQRSTUVWX1234567890";
        let gh = "ghp_0123456789abcdefghijABCDEFGHIJ012345";
        let aws = "AKIAIOSFODNN7EXAMPLE";
        let input = format!("my key is {sk}, token {gh}, and aws {aws} done");

        // When
        let out = redact_secrets(&input);

        // Then: no raw secret literal survives; each becomes a typed marker.
        assert!(!out.contains(sk), "openai key must be scrubbed: {out}");
        assert!(!out.contains(gh), "github token must be scrubbed: {out}");
        assert!(!out.contains(aws), "aws key must be scrubbed: {out}");
        assert!(out.contains("[REDACTED:openai-key]"), "{out}");
        assert!(out.contains("[REDACTED:github-token]"), "{out}");
        assert!(out.contains("[REDACTED:aws-key]"), "{out}");
        // Surrounding prose is preserved verbatim.
        assert!(out.starts_with("my key is "), "{out}");
        assert!(out.ends_with(" done"), "{out}");
    }

    #[test]
    fn redaction_leaves_ordinary_prose_unchanged() {
        // Given: ordinary chat + a short word + normal code tokens, none key-shaped.
        let prose = "Let's play basketball after we refactor parse_transcript(); \
                     the sha was abc123 and PI is 3.14159.";
        // When / Then: a no-op — precision means zero false positives on prose.
        assert_eq!(redact_secrets(prose), prose);
    }

    #[test]
    fn redaction_is_conservative_at_word_boundaries() {
        // Given: a word ending in "sk" before a hyphen must NOT trip the sk- rule.
        let disk = "the disk-usage-report-was-large-today-really";
        // When / Then: unchanged (no 20+ alnum run, and not at a word boundary).
        assert_eq!(redact_secrets(disk), disk);

        // Given: a Bearer credential.
        let bear = "Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345";
        // When
        let out = redact_secrets(bear);
        // Then: the token is scrubbed, the header prefix preserved.
        assert!(
            !out.contains("abcdefghijklmnopqrstuvwxyz012345"),
            "bearer token must be scrubbed: {out}"
        );
        assert!(out.contains("[REDACTED:bearer-token]"), "{out}");
        assert!(out.starts_with("Authorization: "), "{out}");
    }
}
