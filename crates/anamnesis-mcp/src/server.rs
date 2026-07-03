//! The rmcp/MCP adapter — the ONE place MCP lives.
//!
//! `AnamnesisServer` is the agent-facing MCP surface (`recall`/`remember`/
//! `relate`/`ingest_conversation`/`stats`). It owns no engine state: each tool
//! call builds a [`proto::Request`] and runs it against a [`Backend`] —
//! `Local` (in-process `dispatch`, the `--embedded serve` path) or `Daemon` (the
//! bespoke client to the shared daemon, the default path). Everything off this
//! file (daemon, hook, CLI clients) is MCP-free; see ADR-0012.

use std::sync::{Arc, Mutex};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};

use crate::client::DaemonClient;
use crate::dispatch;
use crate::memory::MemoryRegistry;
use crate::proto::{self, ErrKind, Request, Response};

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
pub struct ExtractPendingParams {
    /// Max turns to pull this batch (default: all pending).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Reserved for a future per-namespace queue. Currently ignored — the
    /// un-extracted queue is a default-namespace global, so this field has no
    /// effect. Pass `null` or omit it.
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

/// Where an [`AnamnesisServer`] sends its requests.
#[derive(Clone)]
pub enum Backend {
    /// In-process: own the registry and run `dispatch` directly (the
    /// `--embedded serve` path — no daemon, no socket).
    Local(Arc<Mutex<MemoryRegistry>>),
    /// Forward to the shared daemon over the bespoke client (the default path).
    /// `tokio::sync::Mutex` because the connection is held across `.await` and
    /// calls must be serialized over the single connection.
    Daemon(Arc<tokio::sync::Mutex<DaemonClient>>),
}

impl Backend {
    /// Run one request and return the daemon-shaped reply (transport failures on
    /// the daemon path, and dispatch-task panics on the local path, map to an
    /// internal error so a single bad call never breaks the session).
    async fn call(&self, req: Request) -> Response {
        match self {
            Backend::Local(registry) => {
                let registry = registry.clone();
                // Pass the `Arc` itself, not a held `MutexGuard`: `dispatch`
                // applies its own two-phase locking (brief global lock to
                // resolve a namespace handle, then only the per-namespace lock
                // for the expensive work), the same registry-lock-starvation
                // fix as the daemon's `serve_connection`. Poison recovery lives
                // inside `dispatch`'s own lock sites now.
                tokio::task::spawn_blocking(move || dispatch::dispatch(&registry, req))
                    .await
                    .unwrap_or_else(|e| Response::internal(format!("dispatch task panicked: {e}")))
            }
            Backend::Daemon(client) => {
                let mut c = client.lock().await;
                match c.call(&req).await {
                    Ok(resp) => resp,
                    Err(e) => Response::internal(e),
                }
            }
        }
    }
}

/// Map a daemon [`Response`] back to the MCP `CallToolResult` / `ErrorData`,
/// preserving the caller-vs-internal error distinction.
fn to_result(resp: Response) -> Result<CallToolResult, ErrorData> {
    match resp {
        Response::Ok { text } => Ok(CallToolResult::success(vec![Content::text(text)])),
        Response::Err {
            kind: ErrKind::InvalidParams,
            message,
        } => Err(ErrorData::invalid_params(message, None)),
        Response::Err {
            kind: ErrKind::Internal,
            message,
        } => Err(ErrorData::internal_error(message, None)),
    }
}

#[derive(Clone)]
pub struct AnamnesisServer {
    backend: Backend,
    tool_router: ToolRouter<Self>,
}

impl AnamnesisServer {
    /// In-process server that owns the DB directly (`--embedded serve`).
    pub fn local(registry: Arc<Mutex<MemoryRegistry>>) -> Self {
        Self {
            backend: Backend::Local(registry),
            tool_router: Self::tool_router(),
        }
    }

    /// Adapter that forwards every tool call to the shared daemon (default).
    pub fn daemon(client: DaemonClient) -> Self {
        Self {
            backend: Backend::Daemon(Arc::new(tokio::sync::Mutex::new(client))),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl AnamnesisServer {
    #[tool(
        description = "Search memory for relevant prior knowledge. ALWAYS call before answering. \
                       Returns a readable context block (knowledge / memories / tensions \
                       with provenance) plus a compact ranked list of {node_id, score} — pass those \
                       node_ids to `relate` to link reasoning. Reading reinforces what it returns."
    )]
    async fn recall(
        &self,
        Parameters(p): Parameters<RecallParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = Request::Recall {
            query: p.query,
            limit: p.limit,
            namespace: p.namespace,
            reinforce: p.reinforce,
            gate_threshold: p.gate_threshold,
        };
        to_result(self.backend.call(req).await)
    }

    #[tool(description = "Store a distilled insight, decision, or lesson for future recall.")]
    async fn remember(
        &self,
        Parameters(p): Parameters<RememberParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = Request::Remember {
            content: p.content,
            namespace: p.namespace,
        };
        to_result(self.backend.call(req).await)
    }

    #[tool(
        description = "Ingest a full conversation transcript (ordered turns) via the windowing recipe."
    )]
    async fn ingest_conversation(
        &self,
        Parameters(p): Parameters<IngestParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let turns = p
            .turns
            .into_iter()
            .map(|t| proto::TurnInput {
                speaker: t.speaker,
                text: t.text,
                at_ms: t.at_ms,
            })
            .collect();
        let req = Request::Ingest {
            session: p.session,
            turns,
            namespace: p.namespace,
            capture: None,
        };
        to_result(self.backend.call(req).await)
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
        let req = Request::Relate {
            from_id: p.from_id,
            to_id: p.to_id,
            relation: p.relation,
            namespace: p.namespace,
        };
        to_result(self.backend.call(req).await)
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
        let req = Request::Stats {
            namespace: p.namespace,
        };
        to_result(self.backend.call(req).await)
    }

    #[tool(
        description = "Pull un-extracted raw conversation turns awaiting reasoning extraction. \
                       Returns a JSON array of {node_id, content}. For each, distill decisions, \
                       cause→effect, contradictions, and problem→resolution, then record them with \
                       `relate` (use the node_ids) and `remember` — promptly: pulled turns are \
                       marked pending and are redelivered only once if a pull is abandoned."
    )]
    async fn extract_pending(
        &self,
        Parameters(p): Parameters<ExtractPendingParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = Request::PullPending {
            limit: p.limit,
            namespace: p.namespace,
        };
        to_result(self.backend.call(req).await)
    }
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
