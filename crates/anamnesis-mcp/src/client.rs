//! Thin MCP-over-Unix-socket client for the CLI one-shots.
//!
//! Once the shared daemon owns the DB (and its `<db>.lock`), a CLI one-shot can
//! no longer open the DB directly — the lock would reject it. So `recall`,
//! `remember`, `relate`, and `stats` become **clients** of the daemon instead:
//!
//! 1. [`crate::launcher::ensure_daemon`] — derive the per-DB socket, connect, and
//!    (if no daemon is up yet) spawn a detached one and retry-connect.
//! 2. `()` (the unit client handler) `.serve(stream)` — run the MCP `initialize`
//!    handshake over the socket, exactly like a real MCP host would.
//! 3. one `tools/call` for the requested tool, then read the result.
//! 4. extract the result's text payload, print it, and disconnect (`cancel`).
//!
//! This reuses all of rmcp's client machinery (framing, handshake, request
//! correlation) rather than hand-rolling JSON-RPC, so the wire protocol is
//! guaranteed identical to what the daemon's server side expects.

use anyhow::{Context, Result, anyhow, bail};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value};

use crate::config::Config;
use crate::launcher::ensure_daemon;

/// Run one tool against the shared daemon for `cfg`'s resolved DB and return the
/// joined text payload of the result.
///
/// Ensures the daemon is up, performs the MCP `initialize` handshake, issues a
/// single `tools/call`, then disconnects cleanly. `arguments` is the tool's
/// parameter object (already shaped to the tool's input schema).
pub async fn call_tool_oneshot(
    cfg: &Config,
    tool: &'static str,
    arguments: Map<String, Value>,
) -> Result<String> {
    // 1) ensure the daemon and connect (reuses the launcher's spawn/retry logic).
    let stream = ensure_daemon(&cfg.default_db)
        .await
        .context("connect to the anamnesis daemon")?;

    // 2) MCP initialize handshake. `()` is rmcp's no-capability client handler;
    //    `serve` drives initialize and returns a running client session.
    let client = ().serve(stream).await.context("MCP initialize handshake with the daemon")?;

    // 3) one tools/call.
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .with_context(|| format!("tools/call {tool}"));

    // 4) always disconnect, even if the call failed, so we don't pin the daemon's
    //    grace timer open. Then surface the call's outcome.
    let disconnect = client.cancel().await;
    let result = result?;
    if let Err(e) = disconnect {
        tracing::debug!("client disconnect returned: {e}");
    }

    text_payload(result)
}

/// Join a [`CallToolResult`]'s text content blocks into a single string.
///
/// A tool that reports `is_error` surfaces as a Rust error carrying that text, so
/// a CLI one-shot exits non-zero with the daemon's message (e.g. an unknown
/// relation label) rather than printing it as if it were a success.
fn text_payload(result: CallToolResult) -> Result<String> {
    let text: String = result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");

    if result.is_error.unwrap_or(false) {
        if text.is_empty() {
            bail!("daemon returned a tool error with no message");
        }
        return Err(anyhow!(text));
    }
    Ok(text)
}

/// Build a `tools/call` argument object from `(key, value)` pairs, skipping any
/// `None` values so an omitted optional (e.g. `namespace`) is left off entirely
/// rather than sent as JSON `null`.
pub fn args<I>(pairs: I) -> Map<String, Value>
where
    I: IntoIterator<Item = (&'static str, Option<Value>)>,
{
    pairs
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k.to_string(), v)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_skips_none_keeps_some() {
        let m = args([
            ("query", Some(Value::String("hello".into()))),
            ("namespace", None),
            ("limit", Some(Value::from(5u32))),
        ]);
        assert_eq!(m.get("query").and_then(|v| v.as_str()), Some("hello"));
        assert_eq!(m.get("limit").and_then(|v| v.as_u64()), Some(5));
        assert!(
            !m.contains_key("namespace"),
            "a None optional must be omitted, not sent as null"
        );
    }

    #[test]
    fn text_payload_joins_content_and_errors_on_is_error() {
        use rmcp::model::Content;
        let ok = CallToolResult::success(vec![Content::text("a"), Content::text("b")]);
        assert_eq!(text_payload(ok).unwrap(), "a\nb");

        let err = CallToolResult::error(vec![Content::text("unknown relation")]);
        let e = text_payload(err).unwrap_err();
        assert!(
            e.to_string().contains("unknown relation"),
            "tool error text must propagate: {e}"
        );
    }
}
