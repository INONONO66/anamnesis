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

#[derive(Clone)]
pub struct AnamnesisServer {
    registry: Arc<Mutex<MemoryRegistry>>,
    tool_router: ToolRouter<Self>,
}

impl AnamnesisServer {
    pub fn new(registry: Arc<Mutex<MemoryRegistry>>) -> Self {
        Self { registry, tool_router: Self::tool_router() }
    }
}

fn internal(msg: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(msg.to_string(), None)
}

#[tool_router]
impl AnamnesisServer {
    #[tool(description = "Search memory for relevant prior knowledge. ALWAYS call before answering. Reading reinforces what it returns.")]
    async fn recall(
        &self,
        Parameters(p): Parameters<RecallParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let registry = self.registry.clone();
        let limit = p.limit.unwrap_or(20) as usize;
        let hits = tokio::task::spawn_blocking(move || {
            let mut g = registry.lock().expect("registry mutex poisoned");
            g.recall(&p.query, limit, p.namespace.as_deref())
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;

        let body = serde_json::json!({
            "hits": hits.iter().map(|h| serde_json::json!({
                "node_id": h.node_id.0,
                "text": h.text,
                "score": h.score,
                "at_ms": h.at.0,
                "speaker": h.speaker,
                "session": h.session,
            })).collect::<Vec<_>>()
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&body).map_err(internal)?,
        )]))
    }

    #[tool(description = "Store a distilled insight, decision, or lesson for future recall.")]
    async fn remember(
        &self,
        Parameters(p): Parameters<RememberParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let registry = self.registry.clone();
        let id = tokio::task::spawn_blocking(move || {
            let mut g = registry.lock().expect("registry mutex poisoned");
            g.remember(&p.content, p.namespace.as_deref())
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        Ok(CallToolResult::success(vec![Content::text(format!("stored node {id}"))]))
    }

    #[tool(description = "Ingest a full conversation transcript (ordered turns) via the windowing recipe.")]
    async fn ingest_conversation(
        &self,
        Parameters(p): Parameters<IngestParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let registry = self.registry.clone();
        let summary = tokio::task::spawn_blocking(move || {
            let turns: Vec<Turn> = p
                .turns
                .into_iter()
                .map(|t| Turn { speaker: t.speaker, text: t.text, at_ms: t.at_ms })
                .collect();
            let mut g = registry.lock().expect("registry mutex poisoned");
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
}

#[tool_handler]
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
