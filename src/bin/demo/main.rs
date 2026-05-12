//! Anamnesis demo REPL with Ollama integration.
//!
//! Demonstrates the cognitive graph engine with a local LLM backend.
//!
//! Run: `cargo run --features demo --bin anamnesis-demo -- --model llama2`

//! Anamnesis demo REPL with Ollama integration.
//!
//! Demonstrates the cognitive graph engine with a local LLM backend.
//!
//! Run: `cargo run --features demo --bin anamnesis-demo -- --model llama2`

//! Anamnesis demo REPL with Ollama integration.
//!
//! Demonstrates the cognitive graph engine with a local LLM backend.
//!
//! Run: `cargo run --features demo --bin anamnesis-demo -- --model llama2`

#[allow(dead_code)]
mod display;
#[allow(dead_code)]
mod extract;
#[allow(dead_code)]
mod ingest;
#[allow(dead_code)]
mod ollama;
mod repl;

use anamnesis::{
    Engine, EngineConfig, FastEmbedProvider, StorageAdapter, embedding::EmbeddingProvider,
    storage::SqliteStorage,
};
use clap::Parser;
use ollama::OllamaClient;
use repl::{Repl, ReplConfig};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "anamnesis-demo")]
#[command(about = "Cognitive graph engine with Ollama integration", long_about = None)]
struct Args {
    /// Ollama API URL
    #[arg(long, default_value = "http://localhost:11434")]
    ollama_url: String,

    /// Model name to use with Ollama
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

    // Health check: verify Ollama is running
    println!("Checking Ollama health at {}...", args.ollama_url);
    let health_url = format!("{}/api/tags", args.ollama_url);
    match reqwest::Client::new().get(&health_url).send().await {
        Ok(response) => {
            if response.status().is_success() {
                println!("✓ Ollama is running and healthy");
            } else {
                eprintln!(
                    "✗ Ollama health check failed with status: {}",
                    response.status()
                );
                eprintln!("Make sure Ollama is running: ollama serve");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("✗ Failed to connect to Ollama at {}", args.ollama_url);
            eprintln!("Error: {}", e);
            eprintln!("\nMake sure Ollama is running:");
            eprintln!("  1. Install Ollama from https://ollama.ai");
            eprintln!("  2. Run: ollama serve");
            eprintln!(
                "  3. In another terminal, pull a model: ollama pull {}",
                args.model
            );
            std::process::exit(1);
        }
    }

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

    let ollama = OllamaClient::new(args.ollama_url, args.model.clone());

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

    let mut repl = Repl::new(engine, ollama, provider, repl_config);
    repl.run().map_err(|e| e.into())
}
