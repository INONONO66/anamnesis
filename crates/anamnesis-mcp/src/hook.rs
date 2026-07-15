//! Claude Code hook entrypoint: `anamnesis-mcp hook <event>`.
//!
//! Claude Code fires a hook by spawning this binary, writing one JSON object to
//! its **stdin**, and reading **stdout** as `additionalContext` it injects into
//! the model's turn. We turn that into a **gated, read-only** recall against the
//! warm shared daemon and emit the Claude Code hook output JSON.
//!
//! Two events in v1:
//! - `hook session-start` — seed the session with up to `ANAMNESIS_HOOK_SEED_K`
//!   project memories (query = cwd basename; seed cosine gate decides whether to inject).
//! - `hook user-prompt`   — activation-**gated** recall on the submitted prompt
//!   (`gate = τ`, cosine gate = `τ_cos`, top-`k = ANAMNESIS_HOOK_TOPK`);
//!   below either gate ⇒ inject nothing.
//!
//! **Fail-open is mandatory.** Every error path (bad stdin, daemon down/unreachable,
//! timeout, tool error) prints a valid *empty* output (nothing) and returns
//! `Ok(())` so `main` exits 0. A `hook` invocation must NEVER block or erase the
//! user's prompt (a non-zero exit on `UserPromptSubmit` erases the prompt), so we
//! never propagate an error and never panic on the recall path.

use std::io::Read;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::cli::HookEvent;
use crate::client::call_oneshot;
use crate::config::Config;
use crate::proto::{RecallEventKind, Request, TurnInput};
use crate::transcript::{ParsedTurn, parse_transcript, resolve_transcript};

/// One-line nudge prepended to a `user-prompt` injection so the agent reinforces
/// memory it actually uses (hook recall is read-only; reinforcement is deliberate).
const USER_PROMPT_NUDGE: &str = "(anamnesis: relevant memory below — if you use or build on it, call the recall/relate tools so it reinforces.)";

/// The sentinel the `recall` tool emits as its context when nothing packaged.
/// We treat a payload whose context is this sentinel as "inject nothing".
const NO_MEMORY_SENTINEL: &str = "(no relevant memory)";

/// Trailer the `recall` tool appends after the readable context block. Splitting
/// on it lets us inspect the human-readable context independently of the compact
/// `{node_id, score, cosine}` list the agent uses for `relate`.
const NODES_TRAILER: &str = "## NODES (for `relate`)";

/// Run a hook event end-to-end. **Never returns `Err`** (fail-open): any failure
/// prints nothing and yields `Ok(())`.
pub async fn run(event: &HookEvent) -> Result<()> {
    let stdin = read_stdin();
    let cfg = Config::from_env();
    let output = match event {
        HookEvent::SessionStart => run_session_start(&cfg, &stdin).await,
        HookEvent::UserPrompt => run_user_prompt(&cfg, &stdin).await,
        HookEvent::Stop | HookEvent::PreCompact | HookEvent::SessionEnd => {
            run_capture(&cfg, &stdin, event).await
        }
    };
    // `output` is `Some(json)` only when there is something to inject; `None` is
    // the below-`τ` / error / empty no-op (print nothing, exit 0).
    if let Some(json) = output {
        // `writeln!` (not `println!`) so a broken pipe — Claude Code closing the
        // hook's stdout before reading — is swallowed instead of panicking (Rust
        // ignores SIGPIPE). The recall work is already complete; a failed write
        // must not turn into a non-zero exit on the fail-open path.
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "{json}");
    }
    Ok(())
}

/// Read ALL of stdin to a string. stdin can only be consumed once; an I/O error
/// (or no stdin at all) yields an empty string, which the parsers tolerate.
///
/// This blocking read is NOT under `ANAMNESIS_HOOK_TIMEOUT_MS` (that bounds only
/// the recall). It relies on Claude Code writing the hook JSON and closing stdin,
/// which makes it return promptly at EOF; the `hooks.json` OS-level timeout is the
/// backstop for a hypothetical never-closed stdin.
fn read_stdin() -> String {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    buf
}

/// The agent-facing extraction nudge, or None when the queue is below threshold.
fn extraction_signal(pending: usize, threshold: usize) -> Option<String> {
    if pending >= threshold && threshold > 0 {
        Some(format!(
            "🧠 {pending} captured turns await reasoning extraction. Call `extract_pending` to \
             pull them and record decisions / cause→effect / contradictions via `relate`/`remember`."
        ))
    } else {
        None
    }
}

/// Query the daemon's extraction status and return the signal if over threshold.
/// Best-effort: any failure (timeout, daemon down, parse error) ⇒ None.
async fn session_extraction_signal(cfg: &Config) -> Option<String> {
    let req = Request::ExtractionStatus { namespace: None };
    let timeout = Duration::from_millis(cfg.hook_timeout_ms);
    let text = tokio::time::timeout(timeout, call_oneshot(cfg, req))
        .await
        .ok()?
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let pending = v.get("pending")?.as_u64()? as usize;
    extraction_signal(pending, cfg.extract_threshold_n)
}

/// `SessionStart`: seed the session with project memories.
///
/// Parses `{ source, cwd, ... }`, derives the project cue from the cwd basename,
/// and does an **ungated, read-only** recall (`reinforce = false`, no gate, limit
/// = `hook_seed_k`).
///
/// The extraction nudge is **independent of the seed**: a session with pending
/// captured turns but no cwd cue (or an empty recall) still gets the nudge —
/// otherwise the Stage-2 trigger would silently vanish exactly when it matters.
/// Seed and signal are assembled by [`assemble_session_block`]; `None` only when
/// BOTH are absent.
async fn run_session_start(cfg: &Config, stdin: &str) -> Option<String> {
    let parsed = parse_session_start(stdin);
    let cue = parsed.as_ref().and_then(|p| project_cue(p.cwd.as_deref()));
    let scope = parsed
        .as_ref()
        .and_then(|p| project_scope(p.cwd.as_deref()));
    let seed = match cue {
        Some(cue) => {
            gated_recall(
                cfg,
                &cue,
                HookRecallPolicy {
                    limit: cfg.hook_seed_k,
                    reinforce: Some(false),
                    gate_threshold: None,
                    cosine_gate: Some(cfg.hook_seed_cosine_gate),
                    knowledge_only: true,
                    scope,
                    event_kind: RecallEventKind::SessionStart,
                },
            )
            .await
        }
        None => None,
    };
    let signal = session_extraction_signal(cfg).await;
    assemble_session_block(seed, signal).map(|block| render_session_start(&block))
}

/// Combine the seed recall and the extraction signal into one injectable block.
/// Either alone injects; both join with a blank line; neither ⇒ `None`.
fn assemble_session_block(seed: Option<String>, signal: Option<String>) -> Option<String> {
    match (seed, signal) {
        (Some(seed), Some(sig)) => Some(format!("{seed}\n\n{sig}")),
        (Some(seed), None) => Some(seed),
        (None, Some(sig)) => Some(sig),
        (None, None) => None,
    }
}

/// `UserPromptSubmit`: activation-gated recall on the submitted prompt.
///
/// Parses `{ prompt, cwd, ... }`, runs a **gated, read-only** recall
/// (`reinforce = false`, `gate = τ`, limit = `hook_topk`). Below `τ` (or any
/// failure) ⇒ `None` (inject nothing). Otherwise prepends the reinforcement nudge.
async fn run_user_prompt(cfg: &Config, stdin: &str) -> Option<String> {
    let parsed = parse_user_prompt(stdin)?;
    let prompt = parsed.prompt?;
    if prompt.trim().is_empty() {
        return None;
    }
    let cwd = parsed.cwd.clone();
    let transcript_path = parsed.transcript_path.clone();
    let session_id = parsed.session_id.clone();
    let context_turns = cfg.hook_context_turns;
    let budget = Duration::from_millis(cfg.hook_timeout_ms);
    let start = Instant::now();
    let recent = if context_turns == 0 {
        None
    } else {
        let transcript_cwd = cwd.clone();
        run_detached_with_timeout(budget, move || {
            let contents = resolve_transcript(
                transcript_path.as_deref(),
                session_id.as_deref(),
                transcript_cwd.as_deref(),
            )?;
            let turns = parse_transcript(&contents);
            recent_context(&turns, context_turns, 500, 2000)
        })
    };
    let query = user_prompt_query(&prompt, cwd.as_deref(), recent.as_deref());
    let scope = project_scope(cwd.as_deref());
    let remaining = remaining_budget(start, budget)?;
    let req = build_hook_recall_request(
        &query,
        HookRecallPolicy {
            limit: cfg.hook_topk,
            reinforce: Some(false),
            gate_threshold: Some(cfg.hook_threshold),
            cosine_gate: Some(cfg.hook_cosine_gate),
            knowledge_only: true,
            scope,
            event_kind: RecallEventKind::UserPrompt,
        },
    );
    let block = recall_request_with_timeout(cfg, req, remaining).await?;
    Some(render_user_prompt(&block))
}

/// Parameters that define a hook's read-only recall request.
struct HookRecallPolicy {
    limit: usize,
    reinforce: Option<bool>,
    gate_threshold: Option<f64>,
    cosine_gate: Option<f64>,
    knowledge_only: bool,
    scope: Option<String>,
    event_kind: RecallEventKind,
}

/// Issue one read-only `recall` against the warm daemon under the fail-open
/// timeout. Returns the injectable block (the recall text with any empty/sentinel
/// payload collapsed to `None`) or `None` on timeout / daemon error / empty recall.
async fn gated_recall(cfg: &Config, query: &str, policy: HookRecallPolicy) -> Option<String> {
    let req = build_hook_recall_request(query, policy);
    let timeout = Duration::from_millis(cfg.hook_timeout_ms);
    recall_request_with_timeout(cfg, req, timeout).await
}

async fn recall_request_with_timeout(
    cfg: &Config,
    req: Request,
    timeout: Duration,
) -> Option<String> {
    let outcome = tokio::time::timeout(timeout, call_oneshot(cfg, req)).await;
    interpret_recall(outcome)
}

fn build_hook_recall_request(query: &str, policy: HookRecallPolicy) -> Request {
    Request::Recall {
        query: query.to_string(),
        limit: Some(policy.limit as u32),
        namespace: None,
        reinforce: policy.reinforce,
        gate_threshold: policy.gate_threshold,
        cosine_gate: policy.cosine_gate,
        knowledge_only: Some(policy.knowledge_only),
        scope: policy.scope,
        tag: None,
        event_kind: Some(policy.event_kind),
    }
}

/// Map a recall outcome (timeout-wrapped daemon call) to an injectable block.
///
/// This is the **fail-open core**, factored out so it is unit-testable without a
/// live daemon. ANY failure — timeout elapsed (`Err(_)`), daemon down / tool error
/// (`Ok(Err(_))`) — yields `None` (inject nothing). A success goes through
/// [`injectable_block`], which itself returns `None` for the empty/gated payload.
fn interpret_recall<E>(
    outcome: Result<Result<String, E>, tokio::time::error::Elapsed>,
) -> Option<String> {
    match outcome {
        Ok(Ok(text)) => injectable_block(&text),
        Ok(Err(_)) | Err(_) => None,
    }
}

#[cfg(test)]
pub(crate) fn interpret_recall_for_test<E>(
    outcome: Result<Result<String, E>, tokio::time::error::Elapsed>,
) -> Option<String> {
    interpret_recall(outcome)
}

/// The cwd basename, used as the SessionStart seed query. `None` for an absent /
/// empty / root path (nothing project-specific to seed on).
fn project_cue(cwd: Option<&str>) -> Option<String> {
    let cwd = cwd?;
    let base = std::path::Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)?;
    if base.trim().is_empty() {
        None
    } else {
        Some(base)
    }
}

fn project_scope(cwd: Option<&str>) -> Option<String> {
    let base = project_cue(cwd)?;
    let normalized: String = base
        .chars()
        .map(|ch| {
            let ch = ch.to_ascii_lowercase();
            if ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    (!normalized.is_empty()).then(|| format!("project/{normalized}"))
}

fn recent_context(
    turns: &[ParsedTurn],
    take: usize,
    per_turn_chars: usize,
    total_chars: usize,
) -> Option<String> {
    if take == 0 || per_turn_chars == 0 || total_chars == 0 {
        return None;
    }
    let start = turns.len().saturating_sub(take);
    let mut lines: Vec<String> = turns[start..]
        .iter()
        .map(|t| {
            let text: String = t.text.chars().take(per_turn_chars).collect();
            format!("{}: {}", t.speaker, text)
        })
        .filter(|line| !line.trim().is_empty())
        .collect();
    while !lines.is_empty() && lines.join("\n").chars().count() > total_chars {
        lines.remove(0);
    }
    let joined = lines.join("\n");
    (!joined.trim().is_empty()).then_some(joined)
}

fn user_prompt_query(prompt: &str, cwd: Option<&str>, recent: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(cue) = project_cue(cwd) {
        parts.push(format!("project: {cue}"));
    }
    if let Some(recent) = recent.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("recent conversation:\n{recent}"));
    }
    parts.push(format!("current user prompt: {prompt}"));
    parts.join("\n")
}

/// Collapse a `recall` tool text payload into an injectable block, or `None` when
/// there is nothing to inject.
///
/// The payload is `"{context}## NODES (for `relate`)\n{refs_json}"`. We strip the
/// `## NODES` trailer to inspect the human-readable context: if it is empty or the
/// `(no relevant memory)` sentinel, the gate tripped (or the recall was empty) ⇒
/// `None`. Otherwise we keep the FULL payload (context + NODES) as the block, so
/// the agent still gets the `{node_id, score, cosine}` list for `relate`.
fn injectable_block(text: &str) -> Option<String> {
    let context = match text.split_once(NODES_TRAILER) {
        Some((before, _)) => before,
        None => text,
    };
    let context = context.trim();
    if context.is_empty() || context == NO_MEMORY_SENTINEL {
        return None;
    }
    let block = text.trim_end();
    if block.is_empty() {
        None
    } else {
        Some(block.to_string())
    }
}

/// Render the `SessionStart` hook output JSON.
fn render_session_start(block: &str) -> String {
    hook_output("SessionStart", block)
}

/// Render the `UserPromptSubmit` hook output JSON, prepending the reinforcement nudge.
fn render_user_prompt(block: &str) -> String {
    let body = format!("{USER_PROMPT_NUDGE}\n\n{block}");
    hook_output("UserPromptSubmit", &body)
}

/// Build the exact Claude Code hook output envelope:
/// `{"hookSpecificOutput":{"hookEventName":<event>,"additionalContext":<ctx>}}`.
/// Serialized via `serde_json` so `additionalContext` is always correctly escaped.
fn hook_output(event_name: &str, additional_context: &str) -> String {
    let v = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": event_name,
            "additionalContext": additional_context,
        }
    });
    // A fixed-shape object of strings cannot fail to serialize.
    serde_json::to_string(&v).unwrap_or_default()
}

/// Parsed `SessionStart` stdin. Only the fields the seed needs are load-bearing;
/// everything else (unknown keys) is ignored so the parse tolerates schema drift.
#[derive(Debug, Default, serde::Deserialize)]
struct SessionStartInput {
    #[serde(default)]
    #[allow(dead_code)]
    source: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// Parsed `UserPromptSubmit` stdin. `prompt` is the recall query; `cwd` is kept
/// for parity / future use. Unknown fields are ignored.
#[derive(Debug, Default, serde::Deserialize)]
struct UserPromptInput {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
}

/// Parse `SessionStart` stdin tolerantly. Malformed/empty JSON ⇒ `None` (fail-open).
fn parse_session_start(stdin: &str) -> Option<SessionStartInput> {
    serde_json::from_str(stdin.trim()).ok()
}

/// Parse `UserPromptSubmit` stdin tolerantly. Malformed/empty JSON ⇒ `None` (fail-open).
fn parse_user_prompt(stdin: &str) -> Option<UserPromptInput> {
    serde_json::from_str(stdin.trim()).ok()
}

/// Parsed `Stop`/`PreCompact`/`SessionEnd` stdin. Only the three fields the
/// capture handler needs are load-bearing; unknown keys are ignored so the parse
/// tolerates schema drift.
#[derive(Debug, Default, serde::Deserialize)]
struct CaptureInput {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// Parse capture stdin tolerantly. Malformed/empty JSON ⇒ `None` (fail-open).
fn parse_capture_input(stdin: &str) -> Option<CaptureInput> {
    serde_json::from_str(stdin.trim()).ok()
}

/// Select the turns to ingest based on the hook event.
///
/// `Stop` ⇒ a small recent window (≤8 turns; cheap, fires per turn). The window
/// is wider than a bare user+assistant pair because tool-use / tool-result turns
/// are filtered out by the transcript parser, which means the last 2 text-bearing
/// turns can both be `assistant` (e.g. when a tool_use/tool_result exchange
/// intervened). Dedup in the daemon makes the overlap between successive `Stop`
/// events free. `PreCompact`/`SessionEnd` ⇒ a wide tail (cap 50) that acts as
/// the real backstop, flushing everything before the context window is compacted
/// or the session ends.
fn select_turns<'a>(turns: &'a [ParsedTurn], event: &HookEvent) -> Vec<&'a ParsedTurn> {
    let take = match event {
        HookEvent::Stop => 8,
        _ => 50,
    };
    let start = turns.len().saturating_sub(take);
    turns[start..].iter().collect()
}

fn is_noise_turn(text: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "# AGENTS.md",
        "<system-reminder>",
        "<command-name>",
        "<command-message>",
        "Caveat: the messages below",
        "[Request interrupted",
    ];
    let text = text.trim();
    PREFIXES.iter().any(|prefix| text.starts_with(prefix))
}

fn capture_turn_inputs(selected: Vec<&ParsedTurn>) -> Vec<TurnInput> {
    selected
        .into_iter()
        .filter(|t| !is_noise_turn(&t.text))
        .map(|t| TurnInput {
            speaker: t.speaker.clone(),
            text: t.text.clone(),
            at_ms: t.at_ms,
        })
        .collect()
}

struct PreparedCapture {
    session: String,
    turns: Vec<TurnInput>,
    scope: Option<String>,
}

fn run_detached_with_timeout<T, F>(timeout: Duration, work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    if timeout.is_zero() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = work();
        let _ = tx.send(result);
    });
    rx.recv_timeout(timeout).ok().flatten()
}

fn remaining_budget(start: Instant, budget: Duration) -> Option<Duration> {
    budget.checked_sub(start.elapsed()).filter(|d| !d.is_zero())
}

fn prepare_capture(input: CaptureInput, event: HookEvent) -> Option<PreparedCapture> {
    let scope = project_scope(input.cwd.as_deref());
    let contents = resolve_transcript(
        input.transcript_path.as_deref(),
        input.session_id.as_deref(),
        input.cwd.as_deref(),
    )?;
    let all = parse_transcript(&contents);
    if all.is_empty() {
        return None;
    }
    let selected = select_turns(&all, &event);
    let turns = capture_turn_inputs(selected);
    if turns.is_empty() {
        return None;
    }
    let session = input.session_id.unwrap_or_else(|| "capture".to_string());
    Some(PreparedCapture {
        session,
        turns,
        scope,
    })
}

/// Capture handler: read the transcript, send selected turns as raw Episodic
/// (capture=true ⇒ dedup + enqueue). Silent (returns None); fail-open.
///
/// The ENTIRE capture — transcript discovery + parse (blocking fs work, moved
/// off the async thread) AND the daemon call — runs under ONE
/// `hook_timeout_ms` budget, so a slow disk / large session tree can never
/// block the hook past its deadline.
async fn run_capture(cfg: &Config, stdin: &str, event: &HookEvent) -> Option<String> {
    if !cfg.capture_enabled {
        return None;
    }
    let input = parse_capture_input(stdin)?;
    let event = event.clone();
    let budget = Duration::from_millis(cfg.hook_timeout_ms);
    let start = Instant::now();
    let prepared = run_detached_with_timeout(budget, move || prepare_capture(input, event))?;
    let remaining = remaining_budget(start, budget)?;
    let req = Request::Ingest {
        session: prepared.session,
        turns: prepared.turns,
        namespace: None,
        capture: Some(true),
        scope: prepared.scope,
    };
    let _ = tokio::time::timeout(remaining, call_oneshot(cfg, req)).await;
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{RecallEventKind, Response, decode_line, encode_line};
    use serde_json::Value;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    // --- stdin parsing: tolerate the real shapes + unknown fields, fail-open on junk ---

    #[test]
    fn parses_session_start_with_extra_fields() {
        let json = r#"{
            "session_id": "abc123",
            "transcript_path": "/x/y.jsonl",
            "cwd": "/Users/me/dev/anamnesis",
            "hook_event_name": "SessionStart",
            "source": "startup",
            "model": "claude-sonnet-4-6"
        }"#;
        let p = parse_session_start(json).expect("valid SessionStart JSON parses");
        assert_eq!(p.cwd.as_deref(), Some("/Users/me/dev/anamnesis"));
        assert_eq!(p.source.as_deref(), Some("startup"));
    }

    #[test]
    fn parses_user_prompt_with_extra_fields() {
        let json = r#"{
            "session_id": "abc123",
            "transcript_path": "/x/y.jsonl",
            "cwd": "/Users/me/dev/anamnesis",
            "permission_mode": "default",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "What files changed in the last commit?"
        }"#;
        let p = parse_user_prompt(json).expect("valid UserPromptSubmit JSON parses");
        assert_eq!(
            p.prompt.as_deref(),
            Some("What files changed in the last commit?")
        );
        assert_eq!(p.cwd.as_deref(), Some("/Users/me/dev/anamnesis"));
        assert_eq!(p.session_id.as_deref(), Some("abc123"));
        assert_eq!(p.transcript_path.as_deref(), Some("/x/y.jsonl"));
    }

    #[test]
    fn malformed_stdin_parses_to_none() {
        assert!(parse_session_start("not json at all").is_none());
        assert!(parse_user_prompt("{ broken").is_none());
        assert!(parse_session_start("").is_none());
        assert!(parse_user_prompt("").is_none());
    }

    #[test]
    fn missing_fields_default_to_none() {
        // A valid JSON object that simply omits the keys we read.
        let p = parse_session_start(r#"{"hook_event_name":"SessionStart"}"#).unwrap();
        assert!(p.cwd.is_none());
        let q = parse_user_prompt(r#"{"hook_event_name":"UserPromptSubmit"}"#).unwrap();
        assert!(q.prompt.is_none());
    }

    // --- project cue (SessionStart seed query) ---

    #[test]
    fn project_cue_is_cwd_basename() {
        assert_eq!(
            project_cue(Some("/Users/me/dev/anamnesis")).as_deref(),
            Some("anamnesis")
        );
        assert_eq!(project_cue(Some("anamnesis")).as_deref(), Some("anamnesis"));
    }

    #[test]
    fn project_cue_none_for_absent_or_root() {
        assert!(project_cue(None).is_none());
        assert!(project_cue(Some("/")).is_none());
        assert!(project_cue(Some("")).is_none());
    }

    #[test]
    fn project_scope_normalizes_basename() {
        assert_eq!(
            project_scope(Some("/Users/me/dev/Anamnesis Hook")).as_deref(),
            Some("project/anamnesis-hook")
        );
        assert_eq!(
            project_scope(Some("/Users/me/dev/anamnesis.rs")).as_deref(),
            Some("project/anamnesis.rs")
        );
        assert!(project_scope(None).is_none());
        assert!(project_scope(Some("/")).is_none());
        assert!(project_scope(Some("")).is_none());
    }

    #[test]
    fn project_scope_preserves_distinct_korean_basenames() {
        let memory = project_scope(Some("/Users/me/dev/메모리"));
        let notebook = project_scope(Some("/Users/me/dev/노트북"));

        assert_eq!(memory.as_deref(), Some("project/메모리"));
        assert_eq!(notebook.as_deref(), Some("project/노트북"));
        assert_ne!(memory, notebook);
    }

    #[test]
    fn recent_context_takes_tail_and_caps_by_chars() {
        let turns: Vec<ParsedTurn> = (0..10)
            .map(|i| ParsedTurn {
                speaker: "user".into(),
                text: format!("턴{i} 한국어내용"),
                at_ms: None,
            })
            .collect();
        let ctx = recent_context(&turns, 3, 500, 2000).expect("tail context");
        assert!(ctx.contains("턴9") && ctx.contains("턴7") && !ctx.contains("턴6"));

        let long = vec![ParsedTurn {
            speaker: "user".into(),
            text: "가".repeat(600),
            at_ms: None,
        }];
        let capped = recent_context(&long, 3, 500, 2000).expect("capped context");
        assert!(capped.chars().count() < 520);
    }

    #[test]
    fn user_prompt_query_layers_project_recent_prompt() {
        let q = user_prompt_query(
            "다 별론데?",
            Some("/Users/me/dev/anamnesis"),
            Some("user: cosine 게이트 얘기"),
        );
        assert!(q.contains("project: anamnesis"));
        assert!(q.contains("recent conversation:\nuser: cosine 게이트 얘기"));
        assert!(q.ends_with("current user prompt: 다 별론데?"));
    }

    #[test]
    fn hook_recall_request_sets_gates_and_knowledge_only() {
        let cfg = short_circuit_cfg();
        let req = build_hook_recall_request(
            "q",
            HookRecallPolicy {
                limit: cfg.hook_topk,
                reinforce: Some(false),
                gate_threshold: Some(cfg.hook_threshold),
                cosine_gate: Some(cfg.hook_cosine_gate),
                knowledge_only: true,
                scope: Some("project/anamnesis".to_string()),
                event_kind: RecallEventKind::UserPrompt,
            },
        );
        let Request::Recall {
            cosine_gate,
            event_kind,
            knowledge_only,
            limit,
            scope,
            ..
        } = req
        else {
            panic!("expected recall request");
        };
        assert_eq!(cosine_gate, Some(crate::config::DEFAULT_HOOK_COSINE_GATE));
        assert_eq!(knowledge_only, Some(true));
        assert_eq!(limit, Some(3));
        assert_eq!(event_kind, Some(RecallEventKind::UserPrompt));
        assert_eq!(scope.as_deref(), Some("project/anamnesis"));
    }

    // --- injectable_block: gate the recall tool text into something or nothing ---

    #[test]
    fn injectable_block_keeps_real_context_and_nodes() {
        let text = "## MEMORIES\n- the auth bug was a race\n\n## NODES (for `relate`)\n[{\"node_id\":1,\"score\":29.0}]";
        let block = injectable_block(text).expect("a real recall is injectable");
        assert!(block.contains("## MEMORIES"));
        assert!(block.contains("the auth bug was a race"));
        // The node list is kept so the agent can `relate`.
        assert!(block.contains("## NODES (for `relate`)"));
        assert!(block.contains("node_id"));
    }

    #[test]
    fn injectable_block_none_for_no_memory_sentinel() {
        // The gated-out / empty recall the tool emits.
        let text = "(no relevant memory)\n## NODES (for `relate`)\n[]";
        assert!(injectable_block(text).is_none());
    }

    #[test]
    fn injectable_block_none_for_empty_context() {
        assert!(injectable_block("## NODES (for `relate`)\n[]").is_none());
        assert!(injectable_block("").is_none());
        assert!(injectable_block("   \n  ").is_none());
    }

    // --- output JSON shapes: EXACT Claude Code hook envelope ---

    #[test]
    fn session_start_output_shape_is_exact() {
        let out = render_session_start("## KNOWLEDGE\n- uses the warm daemon");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["hookEventName"],
            Value::from("SessionStart")
        );
        assert_eq!(
            v["hookSpecificOutput"]["additionalContext"],
            Value::from("## KNOWLEDGE\n- uses the warm daemon")
        );
        // Only the one envelope key.
        assert_eq!(v.as_object().unwrap().len(), 1);
    }

    #[test]
    fn user_prompt_output_shape_has_nudge_then_block() {
        let out = render_user_prompt("## MEMORIES\n- prior decision X");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["hookEventName"],
            Value::from("UserPromptSubmit")
        );
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        // Nudge comes first, then the recall block.
        assert!(ctx.starts_with(USER_PROMPT_NUDGE));
        assert!(ctx.contains("## MEMORIES"));
        assert!(ctx.contains("prior decision X"));
        let nudge_at = ctx.find(USER_PROMPT_NUDGE).unwrap();
        let mem_at = ctx.find("## MEMORIES").unwrap();
        assert!(nudge_at < mem_at, "nudge must precede the block");
    }

    #[test]
    fn output_json_escapes_special_chars() {
        // A context with quotes/newlines must round-trip through valid JSON.
        let out = render_session_start("line1\n\"quoted\"\tend");
        let v: Value = serde_json::from_str(&out).expect("output is valid JSON");
        assert_eq!(
            v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap(),
            "line1\n\"quoted\"\tend"
        );
    }

    // --- fail-open core: any recall failure ⇒ inject nothing (no daemon spawned) ---
    //
    // `interpret_recall` is the fail-open decision point. We exercise it with
    // simulated outcomes (a tool error, a timeout `Elapsed`) instead of a live
    // daemon, so these tests are hermetic: they never spawn a process, never touch
    // a socket, and never load the embedding model.

    /// Build a real `tokio::time::error::Elapsed` to simulate a hook timeout.
    async fn elapsed() -> tokio::time::error::Elapsed {
        tokio::time::timeout(Duration::from_millis(0), std::future::pending::<()>())
            .await
            .expect_err("a 0ms timeout over a never-ready future always elapses")
    }

    #[tokio::test]
    async fn interpret_recall_timeout_injects_nothing() {
        // Err(Elapsed) ⇒ the daemon was too slow ⇒ inject nothing.
        let outcome: Result<Result<String, std::io::Error>, _> = Err(elapsed().await);
        assert!(
            interpret_recall(outcome).is_none(),
            "timeout ⇒ inject nothing"
        );
    }

    #[test]
    fn interpret_recall_daemon_error_injects_nothing() {
        // Ok(Err(_)) ⇒ daemon down / unreachable / tool error ⇒ inject nothing.
        let outcome: Result<Result<String, &str>, tokio::time::error::Elapsed> = Ok(Err(
            "connect to the anamnesis daemon: No such file or directory",
        ));
        assert!(
            interpret_recall(outcome).is_none(),
            "daemon error ⇒ inject nothing"
        );
    }

    #[test]
    fn interpret_recall_empty_payload_injects_nothing() {
        // Ok(Ok(sentinel)) ⇒ a successful but gated-out/empty recall ⇒ nothing.
        let outcome: Result<Result<String, &str>, tokio::time::error::Elapsed> = Ok(Ok(
            "(no relevant memory)\n## NODES (for `relate`)\n[]".to_string(),
        ));
        assert!(interpret_recall(outcome).is_none());
    }

    #[test]
    fn interpret_recall_real_payload_injects_block() {
        // Ok(Ok(real text)) ⇒ inject the block.
        let outcome: Result<Result<String, &str>, tokio::time::error::Elapsed> = Ok(Ok(
            "## MEMORIES\n- a prior lesson\n## NODES (for `relate`)\n[{\"node_id\":1,\"score\":12.0}]"
                .to_string(),
        ));
        let block = interpret_recall(outcome).expect("a real payload injects");
        assert!(block.contains("a prior lesson"));
    }

    #[test]
    fn detached_timeout_returns_before_slow_work_finishes() {
        let start = std::time::Instant::now();
        let out = run_detached_with_timeout(Duration::from_millis(30), || {
            std::thread::sleep(Duration::from_millis(250));
            Some("late")
        });
        assert!(out.is_none());
        assert!(start.elapsed() < Duration::from_millis(150));
    }

    // --- capture: parse_capture_input + select_turns (hermetic, no daemon) ---

    #[test]
    fn parses_capture_stdin_fields() {
        let json = r#"{"session_id":"abc","transcript_path":"/x/y.jsonl","cwd":"/d/anamnesis","hook_event_name":"Stop"}"#;
        let p = parse_capture_input(json).unwrap();
        assert_eq!(p.session_id.as_deref(), Some("abc"));
        assert_eq!(p.transcript_path.as_deref(), Some("/x/y.jsonl"));
        assert_eq!(p.cwd.as_deref(), Some("/d/anamnesis"));
    }

    #[test]
    fn stop_selects_recent_window_others_select_tail() {
        let turns: Vec<crate::transcript::ParsedTurn> = (0..60)
            .map(|i| crate::transcript::ParsedTurn {
                speaker: if i % 2 == 0 {
                    "user".into()
                } else {
                    "assistant".into()
                },
                text: format!("t{i}"),
                at_ms: Some(1000 + i),
            })
            .collect();
        // Stop ⇒ a small recent window (≤8) including the latest turn.
        let stop = select_turns(&turns, &HookEvent::Stop);
        assert!(stop.len() <= 8 && stop.last().unwrap().text == "t59");
        // PreCompact ⇒ wider tail (cap 50).
        let pc = select_turns(&turns, &HookEvent::PreCompact);
        assert_eq!(pc.len(), 50);
        assert_eq!(pc.last().unwrap().text, "t59");
    }

    #[test]
    fn noise_turns_are_filtered_from_capture() {
        assert!(is_noise_turn("# AGENTS.md instructions for /Users/x"));
        assert!(is_noise_turn(
            "<system-reminder>use the newest message</system-reminder>"
        ));
        assert!(is_noise_turn("<command-name>Read</command-name>"));
        assert!(is_noise_turn(
            "<command-message>opened file</command-message>"
        ));
        assert!(is_noise_turn(
            "Caveat: the messages below were generated elsewhere"
        ));
        assert!(is_noise_turn("[Request interrupted by user]"));
        assert!(!is_noise_turn("실제 사용자 발화"));

        let turns = vec![
            crate::transcript::ParsedTurn {
                speaker: "user".into(),
                text: "# AGENTS.md instructions for /Users/x".into(),
                at_ms: Some(1),
            },
            crate::transcript::ParsedTurn {
                speaker: "assistant".into(),
                text: "<system-reminder>noise</system-reminder>".into(),
                at_ms: Some(2),
            },
        ];
        let selected = select_turns(&turns, &HookEvent::Stop);
        assert!(capture_turn_inputs(selected).is_empty());
    }

    // --- extraction_signal: pure helper, no daemon ---

    #[test]
    fn extraction_signal_only_over_threshold() {
        assert!(
            extraction_signal(25, 20).is_some(),
            "over threshold ⇒ signal"
        );
        assert!(extraction_signal(20, 20).is_some(), "at threshold ⇒ signal");
        assert!(extraction_signal(3, 20).is_none(), "under ⇒ none");
        assert!(
            extraction_signal(5, 0).is_none(),
            "zero threshold ⇒ disabled"
        );
        let s = extraction_signal(25, 20).unwrap();
        assert!(s.contains("extract_pending"), "names the tool: {s}");
    }

    /// Seed and signal are independent: either alone injects — a pending queue
    /// must surface the nudge even when there is no cwd cue / empty recall.
    #[test]
    fn assemble_session_block_covers_all_cases() {
        assert_eq!(
            assemble_session_block(None, None),
            None,
            "nothing ⇒ inject nothing"
        );
        assert_eq!(
            assemble_session_block(Some("SEED".into()), None).as_deref(),
            Some("SEED")
        );
        assert_eq!(
            assemble_session_block(None, Some("SIG".into())).as_deref(),
            Some("SIG"),
            "signal alone must inject (no seed gating)"
        );
        let both = assemble_session_block(Some("SEED".into()), Some("SIG".into())).unwrap();
        assert_eq!(both, "SEED\n\nSIG");
    }

    // --- handler short-circuits: these return BEFORE any daemon call (hermetic) ---

    /// A config whose recall would fail-open if reached — but these inputs never
    /// reach the daemon (they short-circuit on parse/empty-prompt/no-cwd), so no
    /// daemon is ever spawned. A tiny timeout bounds the (never-taken) recall.
    fn short_circuit_cfg() -> Config {
        Config {
            default_db: std::path::PathBuf::from("/dev/null/anamnesis-never-reached.db"),
            default_namespace: "default".into(),
            reinforce_on_recall: false,
            hook_threshold: 13.0,
            hook_cosine_gate: crate::config::DEFAULT_HOOK_COSINE_GATE,
            hook_seed_cosine_gate: crate::config::DEFAULT_HOOK_SEED_COSINE_GATE,
            hook_context_turns: crate::config::DEFAULT_HOOK_CONTEXT_TURNS,
            hook_topk: 3,
            hook_seed_k: 5,
            hook_timeout_ms: 1,
            capture_enabled: true,
            extract_threshold_n: 20,
            embed_model: crate::config::DEFAULT_EMBED_MODEL.to_string(),
            auto_migrate_embeddings: true,
        }
    }
    fn live_hook_cfg(temp: &tempfile::TempDir) -> Config {
        let mut cfg = short_circuit_cfg();
        cfg.default_db = temp.path().join("hook-provenance.db");
        cfg.hook_context_turns = 0;
        cfg.hook_timeout_ms = 1_000;
        cfg
    }

    async fn serve_hook_requests(listener: UnixListener, count: usize) -> Vec<Request> {
        let mut requests = Vec::with_capacity(count);
        for _ in 0..count {
            let (stream, _) = listener
                .accept()
                .await
                .expect("hook connects to mock daemon");
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();
            let line = lines
                .next_line()
                .await
                .expect("read mock request")
                .expect("hook sends a request");
            let request = decode_line::<Request>(&line).expect("mock decodes request");
            let text = match &request {
                Request::Recall { .. } => {
                    "## MEMORIES\n- mock daemon memory\n## NODES (for `relate`)\n[]"
                }
                Request::ExtractionStatus { .. } => r#"{"pending":0}"#,
                other => panic!("unexpected hook request: {other:?}"),
            };
            let response = encode_line(&Response::ok(text)).expect("mock encodes response");
            writer
                .write_all(response.as_bytes())
                .await
                .expect("mock writes response");
            requests.push(request);
        }
        requests
    }

    #[tokio::test]
    async fn user_prompt_recall_reports_user_prompt_provenance_to_daemon() {
        let temp = tempfile::tempdir().expect("temporary hook database directory");
        let cfg = live_hook_cfg(&temp);
        let socket =
            crate::daemon::socket_path_for_db(&cfg.default_db).expect("derive mock daemon socket");
        let listener = UnixListener::bind(&socket).expect("bind mock daemon socket");
        let daemon = tokio::spawn(serve_hook_requests(listener, 1));

        let output = run_user_prompt(
            &cfg,
            r#"{"hook_event_name":"UserPromptSubmit","prompt":"recall this","cwd":"/tmp/project"}"#,
        )
        .await;

        assert!(output.is_some(), "successful recall remains injectable");
        let requests = daemon.await.expect("mock daemon completes");
        let [
            Request::Recall {
                event_kind,
                gate_threshold,
                cosine_gate,
                knowledge_only,
                ..
            },
        ] = requests.as_slice()
        else {
            panic!("expected exactly one recall request, got {requests:?}");
        };
        assert_eq!(*event_kind, Some(RecallEventKind::UserPrompt));
        assert_eq!(*gate_threshold, Some(cfg.hook_threshold));
        assert_eq!(*cosine_gate, Some(cfg.hook_cosine_gate));
        assert_eq!(*knowledge_only, Some(true));
    }

    #[tokio::test]
    async fn session_start_recall_reports_session_start_provenance_to_daemon() {
        let temp = tempfile::tempdir().expect("temporary hook database directory");
        let cfg = live_hook_cfg(&temp);
        let socket =
            crate::daemon::socket_path_for_db(&cfg.default_db).expect("derive mock daemon socket");
        let listener = UnixListener::bind(&socket).expect("bind mock daemon socket");
        let daemon = tokio::spawn(serve_hook_requests(listener, 2));

        let output = run_session_start(
            &cfg,
            r#"{"hook_event_name":"SessionStart","cwd":"/tmp/project","source":"startup"}"#,
        )
        .await;

        assert!(output.is_some(), "successful seed remains injectable");
        let requests = daemon.await.expect("mock daemon completes");
        let Request::Recall {
            event_kind,
            gate_threshold,
            cosine_gate,
            knowledge_only,
            ..
        } = &requests[0]
        else {
            panic!(
                "first session-start request must be recall: {:?}",
                requests[0]
            );
        };
        assert_eq!(*event_kind, Some(RecallEventKind::SessionStart));
        assert_eq!(*gate_threshold, None);
        assert_eq!(*cosine_gate, Some(cfg.hook_seed_cosine_gate));
        assert_eq!(*knowledge_only, Some(true));
        assert!(
            matches!(&requests[1], Request::ExtractionStatus { .. }),
            "session-start must preserve its extraction-status check"
        );
    }

    #[tokio::test]
    async fn user_prompt_malformed_stdin_injects_nothing() {
        // Parse fails ⇒ `None` before any recall.
        let out = run_user_prompt(&short_circuit_cfg(), "garbage not json").await;
        assert!(
            out.is_none(),
            "malformed stdin ⇒ inject nothing, no daemon call"
        );
    }

    #[tokio::test]
    async fn user_prompt_empty_prompt_injects_nothing() {
        // Blank prompt ⇒ `None` before any recall.
        let stdin = r#"{"hook_event_name":"UserPromptSubmit","prompt":"   "}"#;
        let out = run_user_prompt(&short_circuit_cfg(), stdin).await;
        assert!(out.is_none(), "blank prompt ⇒ no recall, inject nothing");
    }

    #[tokio::test]
    async fn user_prompt_missing_prompt_injects_nothing() {
        // Valid JSON, no `prompt` key ⇒ `None` before any recall.
        let stdin = r#"{"hook_event_name":"UserPromptSubmit","cwd":"/tmp/x"}"#;
        let out = run_user_prompt(&short_circuit_cfg(), stdin).await;
        assert!(out.is_none(), "no prompt ⇒ inject nothing");
    }

    #[tokio::test]
    async fn session_start_no_cwd_injects_nothing() {
        // No cwd cue ⇒ `None` before any recall.
        let stdin = r#"{"hook_event_name":"SessionStart","source":"startup"}"#;
        let out = run_session_start(&short_circuit_cfg(), stdin).await;
        assert!(out.is_none(), "no cwd cue ⇒ inject nothing");
    }

    #[tokio::test]
    async fn session_start_malformed_stdin_injects_nothing() {
        let out = run_session_start(&short_circuit_cfg(), "::not json::").await;
        assert!(
            out.is_none(),
            "malformed stdin ⇒ inject nothing, no daemon call"
        );
    }
}
