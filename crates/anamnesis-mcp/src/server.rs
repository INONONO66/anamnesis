//! rmcp stdio server exposing recall / remember / ingest_conversation.

use std::sync::{Arc, Mutex};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};

use crate::memory::{MemoryRegistry, Turn};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RecallParams {
    /// Natural-language cue. ALWAYS call recall before answering so prior
    /// decisions, lessons, and context are surfaced.
    pub query: String,
    /// Max results (default 20).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Isolated memory namespace (default: the server default).
    #[serde(default)]
    pub namespace: Option<String>,
    /// When `false`, skip the reinforcing `used()` commit (a pure read). Omitted
    /// ⇒ the server's configured `reinforce_on_recall` default — existing callers
    /// are unchanged. The hook recall path passes `false`.
    #[serde(default)]
    pub reinforce: Option<bool>,
    /// Need-odds gate `τ`: if there are no hits or the top hit's score is `< τ`,
    /// return an empty context block (inject nothing). Omitted ⇒ no gate.
    #[serde(default)]
    pub gate_threshold: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RememberParams {
    /// A self-contained insight, decision, or lesson worth keeping. Call after
    /// concluding anything you'd want recalled in a future session.
    pub content: String,
    /// Isolated memory namespace (default: the server default).
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TurnInput {
    pub speaker: String,
    pub text: String,
    /// Optional unix-millis timestamp for this turn.
    #[serde(default)]
    pub at_ms: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IngestParams {
    /// Conversation/session id; turns in the same session chain temporally.
    pub session: String,
    /// Ordered turns to ingest via the windowing recipe.
    pub turns: Vec<TurnInput>,
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StatsParams {
    /// Isolated memory namespace (default: the server default).
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RelateParams {
    /// Source node id (the `node_id` from a prior `recall`).
    pub from_id: u64,
    /// Target node id (the `node_id` from a prior `recall`).
    pub to_id: u64,
    /// Relation type. One of: `causes`, `contradicts`, `supports`, `refutes`,
    /// `reason`, `rejected-alternative`, `belongs-to`, `related`. Use
    /// `custom:<label>` for a consumer-defined relation. Unknown labels error.
    pub relation: String,
    /// Isolated memory namespace (default: the server default).
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Clone)]
pub struct AnamnesisServer {
    registry: Arc<Mutex<MemoryRegistry>>,
    tool_router: ToolRouter<Self>,
}

impl AnamnesisServer {
    pub fn new(registry: Arc<Mutex<MemoryRegistry>>) -> Self {
        Self {
            registry,
            tool_router: Self::tool_router(),
        }
    }
}

fn internal(msg: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(msg.to_string(), None)
}

/// A caller-facing error (bad relation label, missing node id, etc.).
fn invalid_params(msg: impl std::fmt::Display) -> ErrorData {
    ErrorData::invalid_params(msg.to_string(), None)
}

#[tool_router]
impl AnamnesisServer {
    #[tool(
        description = "Search memory for relevant prior knowledge. ALWAYS call before answering. \
                       Returns a readable context block (identity / knowledge / memories / tensions \
                       with provenance) plus a compact ranked list of {node_id, score} — pass those \
                       node_ids to `relate` to link reasoning. Reading reinforces what it returns."
    )]
    async fn recall(
        &self,
        Parameters(p): Parameters<RecallParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let registry = self.registry.clone();
        let limit = p.limit.unwrap_or(20) as usize;
        let packaged = tokio::task::spawn_blocking(move || {
            // Recover from poisoning so one panicking handler doesn't brick the
            // server for the rest of its lifetime.
            let mut g = registry.lock().unwrap_or_else(|e| e.into_inner());
            g.recall_packaged_gated(
                &p.query,
                limit,
                p.namespace.as_deref(),
                p.reinforce,
                p.gate_threshold,
            )
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;

        // Compact id reference so the agent can feed node_ids to `relate`.
        let refs: Vec<_> = packaged
            .hits
            .iter()
            .map(|h| {
                serde_json::json!({
                    "node_id": h.node_id.0,
                    "score": h.score,
                })
            })
            .collect();
        let refs_json = serde_json::to_string(&refs).map_err(internal)?;

        // Primary text = the readable context block; then a compact id reference
        // block. The context block is empty when nothing packaged — fall back to a
        // clear "no relevant memory" note so the text is never blank.
        let context = if packaged.context.trim().is_empty() {
            "(no relevant memory)\n".to_string()
        } else {
            packaged.context
        };
        let text = format!("{context}## NODES (for `relate`)\n{refs_json}");

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Store a distilled insight, decision, or lesson for future recall.")]
    async fn remember(
        &self,
        Parameters(p): Parameters<RememberParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let registry = self.registry.clone();
        let id = tokio::task::spawn_blocking(move || {
            // Recover from poisoning so one panicking handler doesn't brick the
            // server for the rest of its lifetime.
            let mut g = registry.lock().unwrap_or_else(|e| e.into_inner());
            g.remember(&p.content, p.namespace.as_deref())
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "stored node {id}"
        ))]))
    }

    #[tool(
        description = "Ingest a full conversation transcript (ordered turns) via the windowing recipe."
    )]
    async fn ingest_conversation(
        &self,
        Parameters(p): Parameters<IngestParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let registry = self.registry.clone();
        let summary = tokio::task::spawn_blocking(move || {
            let turns: Vec<Turn> = p
                .turns
                .into_iter()
                .map(|t| Turn {
                    speaker: t.speaker,
                    text: t.text,
                    at_ms: t.at_ms,
                })
                .collect();
            // Recover from poisoning so one panicking handler doesn't brick the
            // server for the rest of its lifetime.
            let mut g = registry.lock().unwrap_or_else(|e| e.into_inner());
            g.ingest_conversation(&p.session, &turns, p.namespace.as_deref())
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "ingested {} turns ({} semantic nodes)",
            summary.episodic, summary.semantic
        ))]))
    }

    #[tool(
        description = "Link two remembered nodes with a typed reasoning relation. Pass node_ids from \
                       a prior `recall` (the NODES list) and a relation: causes, contradicts, \
                       supports, refutes, reason, rejected-alternative, belongs-to, related (or \
                       custom:<label>). Use this to record WHY: cause→effect, supporting/refuting \
                       evidence, decision rationale, or a conflict between two memories."
    )]
    async fn relate(
        &self,
        Parameters(p): Parameters<RelateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let registry = self.registry.clone();
        // Keep the endpoints/label for the success message after `p` is moved.
        let (from_id, to_id, relation) = (p.from_id, p.to_id, p.relation.clone());
        let edge = tokio::task::spawn_blocking(move || {
            // Recover from poisoning so one panicking handler doesn't brick the
            // server for the rest of its lifetime.
            let mut g = registry.lock().unwrap_or_else(|e| e.into_inner());
            g.relate(p.from_id, p.to_id, &p.relation, p.namespace.as_deref())
        })
        .await
        .map_err(internal)?
        // A bad relation label / missing endpoint is a caller error, not an
        // internal fault — surface it as an invalid-params error.
        .map_err(invalid_params)?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "linked node {from_id} -> node {to_id} ({relation}) as edge {edge}"
        ))]))
    }

    #[tool(
        description = "Report graph health/size stats for a namespace: node/edge counts, orphan and \
                       contradiction ratios, average salience/degree, staleness, and an overall \
                       health grade. Read-only."
    )]
    async fn stats(
        &self,
        Parameters(p): Parameters<StatsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let registry = self.registry.clone();
        let stats = tokio::task::spawn_blocking(move || {
            // Recover from poisoning so one panicking handler doesn't brick the
            // server for the rest of its lifetime.
            let mut g = registry.lock().unwrap_or_else(|e| e.into_inner());
            g.stats(p.namespace.as_deref())
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        // The readable text block IS the payload: the CLI `stats` one-shot prints
        // it verbatim, so the daemon-backed and `--embedded` paths render identically.
        Ok(CallToolResult::success(vec![Content::text(format_stats(
            &stats,
        ))]))
    }
}

/// Render a [`MemoryStats`](anamnesis::memory::MemoryStats) snapshot to the same
/// human-readable block the CLI prints. Shared so the `stats` MCP tool and the
/// `--embedded` CLI path produce byte-identical output.
pub fn format_stats(s: &anamnesis::memory::MemoryStats) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "nodes:                {}", s.node_count);
    let _ = writeln!(out, "edges:                {}", s.edge_count);
    let _ = writeln!(
        out,
        "orphans:              {} ({:.1}%)",
        s.orphan_count,
        s.orphan_ratio * 100.0
    );
    let _ = writeln!(
        out,
        "contradictions:       {} ({:.1}%)",
        s.contradiction_count,
        s.contradiction_ratio * 100.0
    );
    let _ = writeln!(out, "supersedes:           {}", s.supersede_count);
    let _ = writeln!(out, "retracted:            {}", s.retracted_count);
    let _ = writeln!(out, "missing embeddings:   {}", s.missing_embedding_count);
    let _ = writeln!(out, "avg salience:         {:.3}", s.avg_salience);
    let _ = writeln!(out, "avg degree:           {:.2}", s.average_degree);
    let _ = writeln!(out, "stale (>30d):         {:.1}%", s.stale_ratio * 100.0);
    let _ = writeln!(out, "salience entropy:     {:.3} bits", s.salience_entropy);
    let _ = writeln!(out, "peers:                {}", s.peer_count);
    let _ = writeln!(out, "health grade:         {:?}", s.grade);
    if !s.scope_distribution.is_empty() {
        let _ = writeln!(out, "scope distribution:");
        for (scope, count) in &s.scope_distribution {
            let _ = writeln!(out, "  {scope}: {count}");
        }
    }
    out
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AnamnesisServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo`/`Implementation` are `#[non_exhaustive]` in rmcp 1.7, so
        // they cannot be built with a struct literal from this crate. Use the
        // provided constructor + builder methods instead.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("anamnesis", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Persistent associative memory. Call `recall` before answering; \
                 call `remember` after any decision or lesson worth keeping.",
            )
    }
}
