//! REPL loop with command dispatch for the Anamnesis demo.

use std::io::{self, Write};
use std::path::Path;

use anamnesis::embedding::EmbeddingProvider;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::storage::SqliteStorage;
use anamnesis::{
    CrystallizeRequest, Engine, FastEmbedProvider, IngestResult, Observation, Query, QueryConfig,
    SearchInput, SnapshotId, StorageAdapter,
};

use super::display::{
    display_context_package, display_node, display_search_result, display_stats,
    display_tick_report,
};
use super::extract::{ExtractedFact, extract_knowledge, map_node_type};
use super::ingest::{IngestConfig, ingest_directory, ingest_file};
use super::llm::{ChatMessage, LocalLlmClient};
use super::prompts::SYNTHESIS_SYSTEM_PROMPT;

pub struct ReplConfig {
    pub scope: String,
    pub agent_id: String,
    pub session_id: String,
}

pub struct Repl {
    engine: Engine<SqliteStorage>,
    llm: LocalLlmClient,
    embedder: FastEmbedProvider,
    config: ReplConfig,
    chat_history: Vec<ChatMessage>,
}

impl Repl {
    pub fn new(
        engine: Engine<SqliteStorage>,
        llm: LocalLlmClient,
        embedder: FastEmbedProvider,
        config: ReplConfig,
    ) -> Self {
        Self {
            engine,
            llm,
            embedder,
            config,
            chat_history: Vec::new(),
        }
    }

    pub fn run(&mut self) -> Result<(), String> {
        println!("Type /help for available commands, /quit to exit.\n");

        loop {
            print!("anamnesis> ");
            io::stdout()
                .flush()
                .map_err(|e| format!("failed to flush prompt: {e}"))?;

            let mut line = String::new();
            let bytes_read = io::stdin()
                .read_line(&mut line)
                .map_err(|e| format!("failed to read input: {e}"))?;
            if bytes_read == 0 {
                break;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('/') {
                let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
                let cmd = parts[0];
                let args = parts.get(1).copied().unwrap_or("");

                match cmd {
                    "/quit" | "/exit" => break,
                    "/help" => self.cmd_help(),
                    "/chat" => self.cmd_chat(args),
                    "/ingest" => self.cmd_ingest(args),
                    "/search" => self.cmd_search(args),
                    "/time-search" => self.cmd_time_search(args),
                    "/node" => self.cmd_node(args),
                    "/neighbors" => self.cmd_neighbors(args),
                    "/tick" => self.cmd_tick(args),
                    "/snapshot" => self.cmd_snapshot(args),
                    "/restore" => self.cmd_restore(args),
                    "/crystallize" => self.cmd_crystallize(),
                    "/stats" => self.cmd_stats(),
                    _ => println!("Unknown command. Type /help for available commands."),
                }
            } else {
                self.cmd_chat(trimmed);
            }
        }

        println!("Goodbye.");
        Ok(())
    }

    fn cmd_help(&self) {
        println!("\nAvailable Commands\n");

        println!("CHAT & CONVERSATION");
        println!("  /chat <message>          Chat with the LLM");
        println!("                           Example: /chat What is the auth module?");
        println!();

        println!("DATA INGESTION");
        println!("  /ingest <path>           Ingest a file or directory");
        println!("                           Example: /ingest ./src/auth.rs");
        println!();

        println!("SEARCH & RETRIEVAL");
        println!("  /search <query>          Full-text and semantic search");
        println!("                           Example: /search authentication patterns");
        println!("  /time-search <sec> [q]   Search nodes from last N seconds");
        println!("                           Example: /time-search 3600 auth");
        println!();

        println!("GRAPH INSPECTION");
        println!("  /node <id>               View a specific node");
        println!("                           Example: /node 42");
        println!("  /neighbors <id> [depth]  Show connected nodes (default depth=1)");
        println!("                           Example: /neighbors 42 2");
        println!();

        println!("TIME & DECAY");
        println!("  /tick [seconds]          Advance time and apply decay (default=3600)");
        println!("                           Example: /tick 86400");
        println!();

        println!("SNAPSHOTS & CONSOLIDATION");
        println!("  /snapshot [label]        Create a snapshot of the graph");
        println!("                           Example: /snapshot before-refactor");
        println!("  /restore <id>            Restore a previous snapshot");
        println!("                           Example: /restore 1");
        println!("  /crystallize             Synthesize recent knowledge");
        println!("                           Example: /crystallize");
        println!();

        println!("SYSTEM");
        println!("  /stats                   Show graph statistics");
        println!("  /help                    Show this help message");
        println!("  /quit, /exit             Exit the REPL");
        println!();
    }

    fn cmd_chat(&mut self, args: &str) {
        let user_msg = args.trim();
        if user_msg.is_empty() {
            println!("Usage: /chat <message>");
            return;
        }

        let user_msg = user_msg.to_string();
        self.chat_history.push(ChatMessage {
            role: "user".to_string(),
            content: user_msg.clone(),
        });

        let response = match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.llm.chat(&self.chat_history))
        }) {
            Ok(response) => response,
            Err(e) => {
                eprintln!("Error: Failed to connect to local LLM backend.");
                eprintln!("Details: {e}");
                let _ = self.chat_history.pop();
                return;
            }
        };

        println!("\nAssistant: {response}\n");
        self.chat_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: response.clone(),
        });
        self.trim_chat_history();

        let turn_content = format!("User: {user_msg}\nAssistant: {response}");
        if self
            .ingest_conversation_turn(&user_msg, &turn_content)
            .is_err()
        {
            return;
        }

        let extraction = match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(extract_knowledge(&self.llm, &user_msg, &response))
        }) {
            Ok(result) => result,
            Err(e) => {
                eprintln!("Warning: Knowledge extraction failed: {e}");
                println!("Extracted 0 knowledge fragments");
                return;
            }
        };

        let mut ingested = 0;
        for fact in &extraction.facts {
            if self.ingest_extracted_fact(fact).is_ok() {
                ingested += 1;
            }
        }
        println!("Extracted {ingested} knowledge fragments");
    }

    fn ingest_conversation_turn(&mut self, user_msg: &str, content: &str) -> Result<(), ()> {
        let embedding = match self.embed_text(content) {
            Ok(embedding) => embedding,
            Err(e) => {
                eprintln!("Failed to embed conversation turn: {e}");
                return Err(());
            }
        };

        let observation = Observation {
            name: truncate_chars(user_msg, 50),
            summary: None,
            content: content.to_string(),
            embedding: Some(embedding),
            confidence: 0.9,
            node_type: KnowledgeType::Episodic,
            entity_tags: Vec::new(),
            origin: match self.origin(0.9) {
                Ok(origin) => origin,
                Err(e) => {
                    eprintln!("Failed to create origin: {e}");
                    return Err(());
                }
            },
            timestamp: Timestamp::now(),
        };

        match self.engine.ingest(observation) {
            Ok(result) => {
                println!("Stored conversation turn as Episodic node");
                self.print_ingest_result(&result);
                Ok(())
            }
            Err(e) => {
                eprintln!("Failed to store conversation turn: {e}");
                Err(())
            }
        }
    }

    fn ingest_extracted_fact(&mut self, fact: &ExtractedFact) -> Result<(), ()> {
        let embedding = match self.embed_text(&fact.content) {
            Ok(embedding) => embedding,
            Err(e) => {
                eprintln!("Failed to embed extracted fact '{}': {e}", fact.name);
                return Err(());
            }
        };

        let observation = Observation {
            name: fact.name.clone(),
            summary: None,
            content: fact.content.clone(),
            embedding: Some(embedding),
            confidence: fact.confidence,
            node_type: map_node_type(&fact.node_type),
            entity_tags: fact.entity_tags.clone(),
            origin: match self.origin(fact.confidence) {
                Ok(origin) => origin,
                Err(e) => {
                    eprintln!("Failed to create origin for '{}': {e}", fact.name);
                    return Err(());
                }
            },
            timestamp: Timestamp::now(),
        };

        match self.engine.ingest(observation) {
            Ok(result) => {
                self.print_ingest_result(&result);
                Ok(())
            }
            Err(e) => {
                eprintln!("Failed to ingest extracted fact '{}': {e}", fact.name);
                Err(())
            }
        }
    }

    fn embed_text(&self, text: &str) -> Result<Vec<f64>, anamnesis::Error> {
        let embeddings = self.embedder.embed_f64(&[text])?;
        embeddings.into_iter().next().ok_or_else(|| {
            anamnesis::Error::InvalidInput("embedding provider returned no vectors".to_string())
        })
    }

    fn origin(&self, confidence: f64) -> Result<Origin, anamnesis::Error> {
        Ok(Origin {
            agent_id: self.config.agent_id.clone(),
            session_id: self.config.session_id.clone(),
            scope: ScopePath::new(self.config.scope.clone())?,
            confidence,
        })
    }

    fn trim_chat_history(&mut self) {
        if self.chat_history.len() > 20 {
            let excess = self.chat_history.len() - 20;
            self.chat_history.drain(0..excess);
        }
    }

    fn print_ingest_result(&self, result: &IngestResult) {
        match result {
            IngestResult::Created(ids) => {
                for id in ids {
                    println!("  - Node {}", id.0);
                }
            }
            IngestResult::Reinforced {
                existing_id,
                similarity,
            } => {
                println!(
                    "  - Reinforced node {} (similarity {:.3})",
                    existing_id.0, similarity
                );
            }
        }
    }

    fn cmd_search(&self, args: &str) {
        let query = args.trim();
        if query.is_empty() {
            println!("Usage: /search <query>");
            return;
        }

        let embedding = match self.embed_text(query) {
            Ok(emb) => Some(emb),
            Err(e) => {
                eprintln!("Warning: Embedding failed, using text-only search: {e}");
                None
            }
        };

        let scope = match ScopePath::new(self.config.scope.clone()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: Invalid scope '{}': {e}", self.config.scope);
                return;
            }
        };

        let search_input = SearchInput {
            text: query.to_string(),
            query_embedding: embedding,
            agent_id: None,
            scope,
            now: Timestamp::now(),
            limit: 10,
            context: None,
            entity_tags: Vec::new(),
            seed_limit: None,
        };

        match self.engine.search(search_input) {
            Ok(result) => {
                display_search_result(&result);
                let pkg = &result.package;
                if pkg.identity.is_empty()
                    && pkg.knowledge.is_empty()
                    && pkg.memories.is_empty()
                    && result.trace.seed_count > 0
                {
                    println!(
                        "Note: {} seed(s) found but no results after spreading activation.",
                        result.trace.seed_count
                    );
                    println!(
                        "Episodic nodes are excluded from search packaging. Try /neighbors <id> instead."
                    );
                }
            }
            Err(e) => eprintln!("Error: Search failed: {e}"),
        }
    }

    fn cmd_time_search(&self, args: &str) {
        let args = args.trim();
        if args.is_empty() {
            println!("Usage: /time-search <seconds> [query]");
            return;
        }

        let parts: Vec<&str> = args.splitn(2, ' ').collect();
        let seconds: u64 = match parts[0].parse() {
            Ok(s) => s,
            Err(_) => {
                println!("Usage: /time-search <seconds> [query]");
                return;
            }
        };

        let since = Timestamp(Timestamp::now().0.saturating_sub(seconds * 1000));
        let query_text = parts.get(1).copied().unwrap_or("").trim();

        if !query_text.is_empty() {
            let embedding = match self.embed_text(query_text) {
                Ok(emb) => Some(emb),
                Err(e) => {
                    eprintln!("Warning: Embedding failed, using text-only search: {e}");
                    None
                }
            };

            let scope = match ScopePath::new(self.config.scope.clone()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error: Invalid scope: {e}");
                    return;
                }
            };

            let search_input = SearchInput {
                text: query_text.to_string(),
                query_embedding: embedding,
                agent_id: None,
                scope,
                now: Timestamp::now(),
                limit: 20,
                context: None,
                entity_tags: Vec::new(),
                seed_limit: None,
            };

            match self.engine.search(search_input) {
                Ok(mut result) => {
                    let storage = self.engine.graph().storage();
                    result.package.identity.retain(|f| {
                        storage
                            .get_node(f.node_id)
                            .is_ok_and(|n| n.created_at.0 >= since.0)
                    });
                    result.package.knowledge.retain(|f| {
                        storage
                            .get_node(f.node_id)
                            .is_ok_and(|n| n.created_at.0 >= since.0)
                    });
                    result.package.memories.retain(|f| {
                        storage
                            .get_node(f.node_id)
                            .is_ok_and(|n| n.created_at.0 >= since.0)
                    });
                    println!("Results filtered to last {seconds}s:");
                    display_search_result(&result);
                }
                Err(e) => eprintln!("Error: Search failed: {e}"),
            }
        } else {
            let query = Query::Temporal {
                since,
                node_types: None,
                limit: 20,
            };

            match self.engine.query(&query, &QueryConfig::default()) {
                Ok(package) => display_context_package(&package),
                Err(e) => eprintln!("Error: Time search failed: {e}"),
            }
        }
    }

    fn cmd_node(&self, args: &str) {
        let id: u64 = match args.trim().parse() {
            Ok(id) => id,
            Err(_) => {
                println!("Usage: /node <id>");
                return;
            }
        };

        let storage = self.engine.graph().storage();
        match storage.get_node(NodeId(id)) {
            Ok(node) => display_node(node),
            Err(_) => eprintln!("Error: Node {} not found.", id),
        }
    }

    fn cmd_neighbors(&self, args: &str) {
        let args = args.trim();
        if args.is_empty() {
            println!("Usage: /neighbors <id> [depth]");
            return;
        }

        let parts: Vec<&str> = args.split_whitespace().collect();
        let id: u64 = match parts[0].parse() {
            Ok(id) => id,
            Err(_) => {
                println!("Usage: /neighbors <id> [depth]");
                return;
            }
        };
        let depth: usize = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);

        let query = Query::Neighborhood {
            entity: NodeId(id),
            depth,
        };

        match self.engine.query(&query, &QueryConfig::default()) {
            Ok(package) => display_context_package(&package),
            Err(e) => eprintln!("Error: Neighborhood query failed: {e}"),
        }
    }

    fn cmd_ingest(&mut self, args: &str) {
        let path_str = args.trim();
        if path_str.is_empty() {
            println!("Usage: /ingest <path>");
            return;
        }

        let path = Path::new(path_str);
        if !path.exists() {
            eprintln!("Error: File or directory not found: {path_str}");
            return;
        }

        let config = IngestConfig::default();
        let result = if path.is_file() {
            ingest_file(
                &mut self.engine,
                &self.embedder,
                path,
                &self.config.scope,
                &config,
            )
        } else if path.is_dir() {
            ingest_directory(
                &mut self.engine,
                &self.embedder,
                path,
                &self.config.scope,
                &config,
            )
        } else {
            eprintln!("Error: Path is neither a file nor a directory: {path_str}");
            return;
        };

        match result {
            Ok(report) => {
                println!("\nIngest complete:");
                println!("  Files processed: {}", report.files_processed);
                println!("  Chunks total: {}", report.chunks_total);
                println!("  Nodes created: {}", report.nodes_created);
                println!("  Nodes reinforced: {}", report.nodes_reinforced);
                if !report.errors.is_empty() {
                    println!("  Errors: {}", report.errors.len());
                    for err in &report.errors {
                        println!("    - {err}");
                    }
                }
            }
            Err(e) => eprintln!("Error: Ingest failed: {e}"),
        }
    }

    fn cmd_tick(&mut self, args: &str) {
        let seconds: u64 = if args.is_empty() {
            3600
        } else {
            match args.trim().parse() {
                Ok(s) => s,
                Err(_) => {
                    println!("Usage: /tick [seconds]  (default: 3600)");
                    return;
                }
            }
        };

        let future = Timestamp(Timestamp::now().0.saturating_add(seconds * 1000));
        match self.engine.tick(future) {
            Ok(report) => {
                display_tick_report(&report);
                println!("Simulated {seconds}s of decay");
            }
            Err(e) => eprintln!("Error: Tick failed: {e}"),
        }
    }

    fn cmd_snapshot(&mut self, args: &str) {
        let label = if args.trim().is_empty() {
            format!("manual-{}", Timestamp::now().0)
        } else {
            args.trim().to_string()
        };

        let id = self.engine.snapshot(&label);
        println!("Snapshot created: {label} (ID: {})", id.0);
    }

    fn cmd_restore(&mut self, args: &str) {
        let id: u64 = match args.trim().parse() {
            Ok(id) => id,
            Err(_) => {
                println!("Usage: /restore <snapshot-id>");
                self.print_available_snapshots();
                return;
            }
        };

        match self.engine.restore(&SnapshotId(id)) {
            Ok(()) => println!("Restored snapshot {id}"),
            Err(e) => {
                eprintln!("Error: Snapshot {} not found: {e}", id);
                self.print_available_snapshots();
            }
        }
    }

    fn cmd_crystallize(&mut self) {
        let since = Timestamp(Timestamp::now().0.saturating_sub(86400));
        let query = Query::Temporal {
            since,
            node_types: Some(vec![KnowledgeType::Episodic]),
            limit: 50,
        };

        let package = match self.engine.query(&query, &QueryConfig::default()) {
            Ok(pkg) => pkg,
            Err(e) => {
                eprintln!("Error: Failed to query recent nodes: {e}");
                return;
            }
        };

        let all_fragments: Vec<_> = package
            .identity
            .iter()
            .chain(package.knowledge.iter())
            .chain(package.memories.iter())
            .collect();

        if all_fragments.is_empty() {
            println!("No recent episodic nodes to crystallize.");
            return;
        }

        let source_ids: Vec<_> = all_fragments.iter().map(|f| f.node_id).collect();

        let mut content_parts = Vec::new();
        for frag in &all_fragments {
            if let Some(content) = &frag.content {
                content_parts.push(content.as_str());
            }
        }

        if content_parts.is_empty() {
            println!("No content in recent nodes to synthesize.");
            return;
        }

        let user_content = content_parts.join("\n---\n");
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: user_content,
        }];

        let synthesis = match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(
                self.llm
                    .chat_with_system(SYNTHESIS_SYSTEM_PROMPT, &messages),
            )
        }) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("Error: LLM synthesis failed. Details: {e}");
                return;
            }
        };

        let embedding = match self.embed_text(&synthesis) {
            Ok(emb) => Some(emb),
            Err(e) => {
                eprintln!("Warning: Failed to embed synthesis: {e}");
                None
            }
        };

        let origin = match self.origin(0.8) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("Error: Failed to create origin: {e}");
                return;
            }
        };

        let request = CrystallizeRequest {
            name: truncate_chars(&synthesis, 60),
            summary: Some(truncate_chars(&synthesis, 200)),
            content: synthesis,
            embedding,
            source_ids: source_ids.clone(),
            source_relevances: None,
            node_type: KnowledgeType::Semantic,
            confidence: 0.8,
            origin,
            entity_tags: Vec::new(),
            timestamp: Timestamp::now(),
        };

        match self.engine.crystallize(request) {
            Ok(result) => {
                println!("\nCrystallization complete:");
                println!("  Synthesis node: {}", result.node_id.0);
                println!(
                    "  Sources consolidated: {}",
                    result.consolidation_edges.len()
                );
                println!("  Initial salience: {:.3}", result.initial_salience);
                println!("  Consistency score: {:.3}", result.consistency_score);
                if result.circular_evidence_warning {
                    println!("  Warning: circular evidence detected");
                }
                if result.single_source_warning {
                    println!("  Warning: all sources from single origin");
                }
            }
            Err(e) => eprintln!("Error: Crystallization failed: {e}"),
        }
    }

    fn cmd_stats(&self) {
        let storage = self.engine.graph().storage();
        let node_count = storage.node_count();
        let edge_count = storage.edge_count();
        let snapshot_count = self.engine.list_snapshots().len();

        let node_ids = storage.all_node_ids();
        let avg_salience = if node_ids.is_empty() {
            0.0
        } else {
            let total: f64 = node_ids
                .iter()
                .filter_map(|id| storage.get_salience(*id).ok())
                .sum();
            total / node_ids.len() as f64
        };

        display_stats(node_count, edge_count, snapshot_count, avg_salience);
    }

    fn print_available_snapshots(&self) {
        let snapshots = self.engine.list_snapshots();
        if snapshots.is_empty() {
            println!("No snapshots available.");
        } else {
            println!("Available snapshots:");
            for (id, label, ts) in &snapshots {
                println!("  ID {} — {label} (at {})", id.0, ts.0);
            }
        }
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}
