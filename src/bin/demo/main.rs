//! Anamnesis demo REPL with local LLM integration.
//!
//! Demonstrates the cognitive graph engine with a local LLM backend.
//!
//! Run: `cargo run --features demo --bin anamnesis-demo -- --model glm-4.7-flash:latest`

mod display;
mod extract;
mod ingest;
mod llm;
mod prompts;
mod repl;

use anamnesis::{
    Engine, EngineConfig, FastEmbedProvider, StorageAdapter, embedding::EmbeddingProvider,
    storage::SqliteStorage,
};
use clap::Parser;
use llm::LocalLlmClient;
use repl::{Repl, ReplConfig};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "anamnesis-demo")]
#[command(about = "Cognitive graph engine with local LLM integration", long_about = None)]
struct Args {
    /// Local LLM API URL (Ollama-compatible /api/chat and /api/tags)
    #[arg(long, default_value = "http://localhost:11434")]
    llm_url: String,

    /// Model name to use with the local LLM backend
    #[arg(long)]
    model: String,

    /// Path to SQLite database
    #[arg(long, default_value = "~/.anamnesis/demo.db")]
    db_path: String,

    /// Scope for knowledge (project/domain identifier)
    #[arg(long, default_value = "demo")]
    scope: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Expand ~ in db_path
    let db_path = if args.db_path.starts_with('~') {
        let home = std::env::var("HOME")?;
        args.db_path.replace("~", &home)
    } else {
        args.db_path
    };

    // Ensure parent directory exists
    if let Some(parent) = PathBuf::from(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let llm = LocalLlmClient::new(args.llm_url.clone(), args.model.clone());
    println!("Checking local LLM health at {}...", args.llm_url);
    if let Err(e) = llm.health_check().await {
        eprintln!("✗ Failed to connect to local LLM backend");
        eprintln!("Error: {e}");
        eprintln!("\nExpected an Ollama-compatible server exposing /api/tags and /api/chat.");
        eprintln!("Example: ollama serve && ollama pull {}", args.model);
        std::process::exit(1);
    }
    println!("✓ Local LLM backend is running and healthy");

    // Initialize embedding provider
    println!("Initializing embedding provider (may download model on first run)...");
    let provider = FastEmbedProvider::new()?;
    println!(
        "✓ Embedding provider ready: {} ({}-d)",
        provider.model_name(),
        provider.dimensions()
    );

    // Initialize storage
    println!("Initializing SQLite storage at {}...", db_path);
    let storage = SqliteStorage::open(&db_path)?;
    println!("✓ Storage initialized");

    // Initialize engine with demo-friendly config (more permissive gating)
    println!("Initializing Anamnesis engine...");
    let config = EngineConfig::new()
        .with_novelty_threshold(0.10)
        .with_confidence_threshold(0.30);
    let engine = Engine::with_storage(config, storage);
    println!("✓ Engine initialized");

    let node_count = engine.graph().storage().node_count();

    println!("\nAnamnesis Demo v{}", env!("CARGO_PKG_VERSION"));
    println!("Cognitive dynamics engine for LLMs\n");
    println!();
    println!("  Model:    {}", args.model);
    println!("  Database: {}", db_path);
    println!("  Scope:    {}", args.scope);
    println!("  Nodes:    {}", node_count);
    println!();
    println!("Type /help for available commands");
    println!();

    let repl_config = ReplConfig {
        scope: args.scope,
        agent_id: "demo-agent".to_string(),
        session_id: format!("session-{}", std::process::id()),
    };

    let mut repl = Repl::new(engine, llm, provider, repl_config);
    repl.run().map_err(|e| e.into())
}
