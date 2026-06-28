//! Transcript sourcing + parsing for capture hooks.
//!
//! Two structurally different schemas (measured):
//! - Claude Code: `{"type":"user"|"assistant","message":{"role","content"},"timestamp":ISO}`
//! - Codex rollout: `{"timestamp":ISO,"type":"response_item","payload":{"type":"message","role","content":[{"text"}]}}`
//! Both reduce to user+assistant `ParsedTurn`s; all other line/role kinds are dropped.

use std::path::PathBuf;

use serde_json::Value;

/// Resolve transcript contents: `transcript_path` if readable, else locate by
/// `session_id` (Codex `~/.codex/sessions/**/rollout-*<sid>*.jsonl`, then CC
/// `~/.claude/projects/<cwd-slug>/<sid>.jsonl`). Returns None if nothing found.
pub fn resolve_transcript(
    transcript_path: Option<&str>,
    session_id: Option<&str>,
    cwd: Option<&str>,
) -> Option<String> {
    if let Some(p) = transcript_path {
        if let Ok(s) = std::fs::read_to_string(p) {
            return Some(s);
        }
    }
    let sid = session_id?;
    if let Some(p) = newest_codex_rollout(sid) {
        if let Ok(s) = std::fs::read_to_string(p) {
            return Some(s);
        }
    }
    if let Some(p) = cc_transcript_path(sid, cwd) {
        if let Ok(s) = std::fs::read_to_string(p) {
            return Some(s);
        }
    }
    None
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Newest `rollout-*<sid>*.jsonl` under ~/.codex/sessions (recursive walk).
fn newest_codex_rollout(sid: &str) -> Option<PathBuf> {
    let root = home()?.join(".codex/sessions");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.starts_with("rollout-") && name.ends_with(".jsonl") && name.contains(sid) {
                    if let Ok(mtime) = e.metadata().and_then(|m| m.modified()) {
                        if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
                            best = Some((mtime, path));
                        }
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// CC transcript: ~/.claude/projects/<cwd-with-slashes-as-dashes>/<sid>.jsonl.
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
        if let Some(turn) = parse_codex_line(&v).or_else(|| parse_cc_line(&v)) {
            out.push(turn);
        }
    }
    out
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
    Some(ParsedTurn { speaker: role.to_string(), text, at_ms: iso_ms(v.get("timestamp")) })
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
    Some(ParsedTurn { speaker: role.to_string(), text, at_ms: iso_ms(v.get("timestamp")) })
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
}
