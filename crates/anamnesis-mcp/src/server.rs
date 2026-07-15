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
    /// Query-embedding cosine gate `τ_cos`: omitted ⇒ no cosine gate.
    #[serde(default)]
    pub cosine_gate: Option<f64>,
    /// If true, render only durable knowledge and omit episodic/capture fragments.
    #[serde(default)]
    pub knowledge_only: Option<bool>,
    /// Post-filter: drop hits whose node origin scope doesn't match.
    #[serde(default)]
    pub scope: Option<String>,
    /// Post-filter: drop hits whose node doesn't carry this entity tag.
    #[serde(default)]
    pub tag: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RememberParams {
    /// A self-contained insight, decision, or lesson worth keeping. Call after
    /// concluding anything you'd want recalled in a future session.
    pub content: String,
    /// Isolated memory namespace (default: the server default).
    #[serde(default)]
    pub namespace: Option<String>,
    /// Extra entity tags for this note, beyond the default recipe tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Consumer-defined metadata key-value pairs to stamp on the note.
    #[serde(default)]
    pub metadata: Option<std::collections::HashMap<String, String>>,
    /// Origin scope for this note (default: universal).
    #[serde(default)]
    pub scope: Option<String>,
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
    #[serde(default)]
    pub scope: Option<String>,
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
    /// Selects which namespace's extraction queue to pull from — the
    /// un-extracted queue is isolated per namespace. Omit (or pass `null`)
    /// to use the registry default namespace.
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

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateParams {
    /// Node id to edit (from a prior `recall`/`list`/`get`).
    pub id: u64,
    /// Replacement content — the node is re-embedded from this text.
    pub new_content: String,
    /// Isolated memory namespace (default: the server default).
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ForgetParams {
    /// Node id to remove (from a prior `recall`/`list`/`get`).
    pub id: u64,
    /// Why it's being forgotten (stored on the retraction; default: a generic note).
    #[serde(default)]
    pub reason: Option<String>,
    /// `true` ⇒ permanently delete (irreversible). Omitted/`false` ⇒ soft-delete
    /// (hidden from recall, still readable via `get`).
    #[serde(default)]
    pub hard: Option<bool>,
    /// Isolated memory namespace (default: the server default).
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SupersedeParams {
    /// Node id that replaces the old one.
    pub new_id: u64,
    /// Node id being replaced (its validity window is closed).
    pub old_id: u64,
    /// Isolated memory namespace (default: the server default).
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListParams {
    /// Minimum salience `[0, 1]` a node must have to be included (default: 0.0).
    #[serde(default)]
    pub min_salience: Option<f64>,
    /// Max results (default 100).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Restrict to one knowledge type: `identity`, `semantic`, `episodic`, or a
    /// consumer-defined label.
    #[serde(default)]
    pub node_type: Option<String>,
    /// Restrict to nodes carrying this entity tag.
    #[serde(default)]
    pub tag: Option<String>,
    /// Isolated memory namespace (default: the server default).
    #[serde(default)]
    pub namespace: Option<String>,
    /// Restrict to nodes whose origin scope matches exactly.
    #[serde(default)]
    pub scope: Option<String>,
    /// Restrict to nodes carrying this metadata pair, formatted `"key=value"`.
    #[serde(default)]
    pub metadata: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetParams {
    /// Node id to read (from a prior `recall`/`list`).
    pub id: u64,
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
                       with provenance) plus a compact ranked list of {node_id, score, cosine} — pass those \
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
            cosine_gate: p.cosine_gate,
            knowledge_only: p.knowledge_only,
            scope: p.scope,
            tag: p.tag,
            event_kind: Some(proto::RecallEventKind::Tool),
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
            tags: p.tags,
            metadata: p.metadata,
            scope: p.scope,
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
            scope: p.scope,
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
            recall: None,
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

    #[tool(
        description = "Edit an existing node's content in place (re-embeds it). Use to correct or \
                       refine a memory rather than storing a near-duplicate."
    )]
    async fn update(
        &self,
        Parameters(p): Parameters<UpdateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = Request::Update {
            id: p.id,
            new_content: p.new_content,
            namespace: p.namespace,
        };
        to_result(self.backend.call(req).await)
    }

    #[tool(
        description = "Remove a memory. Soft (default): hidden from recall/search but reversible \
                       and auditable via `get`. Hard (`hard: true`): permanently erased, use only \
                       for genuinely wrong or sensitive content."
    )]
    async fn forget(
        &self,
        Parameters(p): Parameters<ForgetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = Request::Forget {
            id: p.id,
            reason: p.reason,
            hard: p.hard,
            namespace: p.namespace,
        };
        to_result(self.backend.call(req).await)
    }

    #[tool(
        description = "Mark one memory as superseding another: closes the old node's validity \
                       window and opens the new one's, so time-scoped queries prefer the current \
                       fact while the history stays traceable."
    )]
    async fn supersede(
        &self,
        Parameters(p): Parameters<SupersedeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = Request::Supersede {
            new_id: p.new_id,
            old_id: p.old_id,
            namespace: p.namespace,
        };
        to_result(self.backend.call(req).await)
    }

    #[tool(
        description = "List memories ranked by salience, optionally narrowed by minimum salience, \
                       knowledge type, or entity tag. Returns a compact JSON array — use `get` for \
                       a single node's full detail."
    )]
    async fn list(
        &self,
        Parameters(p): Parameters<ListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = Request::List {
            min_salience: p.min_salience,
            limit: p.limit,
            node_type: p.node_type,
            tag: p.tag,
            namespace: p.namespace,
            scope: p.scope,
            metadata: p.metadata,
        };
        to_result(self.backend.call(req).await)
    }

    #[tool(description = "Read one memory's full detail as JSON, by node id.")]
    async fn get(&self, Parameters(p): Parameters<GetParams>) -> Result<CallToolResult, ErrorData> {
        let req = Request::Get {
            id: p.id,
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
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::memory::{PolicyStoreState, StubProvider};
    use crate::proto::RecallEventKind;

    #[tokio::test]
    async fn mcp_recall_forwards_tool_provenance_to_the_local_backend() {
        let registry = Arc::new(Mutex::new(MemoryRegistry::in_memory_with(
            Arc::new(StubProvider),
            false,
        )));
        let server = AnamnesisServer::local(registry.clone());

        server
            .recall(Parameters(RecallParams {
                query: "adapter cue".into(),
                limit: Some(7),
                namespace: Some("mcp-adapter".into()),
                reinforce: Some(false),
                gate_threshold: Some(0.25),
                cosine_gate: Some(0.5),
                knowledge_only: Some(true),
                scope: Some("project/anamnesis".into()),
                tag: Some("adapter-test".into()),
            }))
            .await
            .expect("recall should reach the local backend");

        let handles = {
            let mut registry = registry.lock().unwrap_or_else(|p| p.into_inner());
            registry
                .namespace_handles(Some("mcp-adapter"))
                .expect("resolve the recall namespace")
        };
        let _memory = handles.memory.lock().unwrap_or_else(|p| p.into_inner());
        let mut policy = MemoryRegistry::policy_store(&handles.policy)
            .expect("open the recall telemetry policy store");
        let PolicyStoreState::Ready(store) = &mut *policy else {
            panic!("recall should initialize telemetry");
        };
        let events = store
            .read_recall_events_for_test()
            .expect("read recall telemetry");
        assert_eq!(events.len(), 1);

        let event = &events[0];
        assert_eq!(event.event_kind, RecallEventKind::Tool);
        assert_eq!(event.namespace, "mcp-adapter");
        assert_eq!(event.query_chars, "adapter cue".chars().count() as u64);
        assert_eq!(event.scope.as_deref(), Some("project/anamnesis"));
        assert!(event.knowledge_only);
        assert_eq!(event.gate_threshold, Some(0.25));
        assert_eq!(event.cosine_gate, Some(0.5));
    }
}
