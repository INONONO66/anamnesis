//! Reasoning-advantage demo — why a graph memory beats a flat vector list.
//!
//! A ~10-turn conversation picks a database (Postgres), records the *reason*, then
//! reverses the decision (SQLite) — a reversal that **contradicts** the original
//! choice. We wire two kinds of typed edge through the public `Memory` front door:
//!
//!   * `Relation::Reason`      — decision → its rationale (a why-chain)
//!   * `Relation::Contradicts` — reversal → the original decision (a tension)
//!
//! Then we ask one question — "why did we switch databases?" — and show two views
//! of the *same* nodes:
//!
//!   1. **Graph recall** surfaces a `## TENSIONS` block (the contradiction pair)
//!      plus the reasoning chain — structure.
//!   2. **Flat cosine ranking** over the same node embeddings returns a bare list:
//!      the contradiction and the why-chain are invisible.
//!
//! Runs offline and instantly — a deterministic byte-hash embedder, no model
//! download.
//!
//! Run: `cargo run -p anamnesis-engine --example reasoning_demo`

use std::sync::Arc;

use anamnesis::Memory;
use anamnesis::engine::{EmbeddingProvider, NodeId, Timestamp};
use anamnesis::memory::Relation;

// ---------------------------------------------------------------------------
// Deterministic, model-free embedder — identical texts embed identically, and
// distinct texts embed distinctly. No network, no model download.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct HashEmbedder;

fn embed_text(text: &str) -> Vec<f32> {
    let bytes = text.as_bytes();
    let a = bytes.iter().map(|&b| b as f32).sum::<f32>();
    let b = bytes
        .iter()
        .skip(1)
        .step_by(2)
        .map(|&b| b as f32)
        .sum::<f32>();
    let c = bytes
        .iter()
        .skip(2)
        .step_by(3)
        .map(|&b| b as f32)
        .sum::<f32>();
    let d = bytes.len() as f32;
    let mag = (a * a + b * b + c * c + d * d).sqrt().max(1.0);
    vec![a / mag, b / mag, c / mag, d / mag]
}

impl EmbeddingProvider for HashEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, anamnesis::Error> {
        Ok(texts.iter().map(|t| embed_text(t)).collect())
    }
    fn dimensions(&self) -> usize {
        4
    }
    fn model_name(&self) -> &str {
        "hash-stub"
    }
}

/// Plain cosine over two f64 vectors (`0.0` on length mismatch or a zero vector).
fn cosine(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn main() -> Result<(), anamnesis::Error> {
    let provider: Arc<dyn EmbeddingProvider> = Arc::new(HashEmbedder);
    // Keep our own handle so we can embed the query for the flat-cosine contrast.
    let query_provider = provider.clone();
    let mut m = Memory::in_memory_with_provider(provider)?;

    let s = "demo";
    let mut ts = 1_000u64;
    let mut add = |m: &mut Memory, who: &str, text: &str| -> Result<NodeId, anamnesis::Error> {
        let id = m.add(s, who, text, Timestamp(ts))?.episodic;
        ts += 60;
        Ok(id)
    };

    // ── Build the conversation ───────────────────────────────────────────────
    add(
        &mut m,
        "user",
        "We need to pick a database for the new service.",
    )?;
    add(
        &mut m,
        "assistant",
        "The main contenders are Postgres and SQLite.",
    )?;
    add(
        &mut m,
        "user",
        "It has to handle JSONB documents and per-tenant row security.",
    )?;
    let decision = add(&mut m, "assistant", "Decision: we go with Postgres.")?;
    let decision_why = add(
        &mut m,
        "assistant",
        "Postgres because we need JSONB and row-level security.",
    )?;
    add(
        &mut m,
        "user",
        "Great, I'll set up the Postgres schema and migrations.",
    )?;
    let reversal = add(
        &mut m,
        "assistant",
        "We are reverting to SQLite — the ops overhead is too high for a single-node deploy.",
    )?;
    let reversal_why = add(
        &mut m,
        "assistant",
        "SQLite keeps the single-node deploy simple with no separate database server to run.",
    )?;
    add(
        &mut m,
        "user",
        "Okay, I'll rewrite the migrations for SQLite.",
    )?;
    add(
        &mut m,
        "assistant",
        "I'll drop the row-level security policies and use application checks.",
    )?;
    m.flush_all()?;

    // ── Wire typed reasoning-chain edges through the front door ───────────────
    m.relate(decision, decision_why, Relation::Reason)?;
    m.relate(reversal, reversal_why, Relation::Reason)?;
    m.relate(reversal, decision, Relation::Contradicts)?;

    let query = "why did we switch databases?";

    // A natural, modest `limit`. Tension endpoints are exempt from result-limit
    // trimming (assemble.rs `apply_result_limit`), so the contradiction pair
    // reaches us even when an endpoint ranks below the cut — no need to oversize
    // `limit` to the whole recalled set.
    let recall = m.search_at(query, 10, Timestamp(2_000))?;

    // ── View 1: graph recall — structure ─────────────────────────────────────
    // `recall.as_context()` renders the full IDENTITY/KNOWLEDGE/MEMORIES/TENSIONS
    // block for LLM injection; here we print a compact digest of the *structure*
    // it exposes that a flat store cannot.
    println!("query: {query:?}\n");
    println!("=== graph recall (structure: tensions + reasons) ===\n");

    println!("tensions (contradictions surfaced, never suppressed):");
    for t in &recall.package.tensions {
        let a = m.engine().graph().get_node(t.node_a)?;
        let b = m.engine().graph().get_node(t.node_b)?;
        println!(
            "  #{} ⟂ #{}  (stress {:.2})",
            t.node_a.0, t.node_b.0, t.stress
        );
        println!("    ↳ {}", one_line(&a.content));
        println!("    ↳ {}", one_line(&b.content));
    }

    // Walk the reasoning chain from the reversal via typed neighbors.
    println!("\nwhy-chain from the reversal (typed edges):");
    for n in m.neighbors(reversal)? {
        use anamnesis::engine::EdgeType;
        let label = match n.edge_type {
            EdgeType::Contradicts => "contradicts",
            EdgeType::Reason => "because",
            _ => continue,
        };
        let target = m.engine().graph().get_node(n.node)?;
        println!("  reversal --{label}--> {}", one_line(&target.content));
    }

    println!("\ntop recalled memories:");
    for h in recall.hits.iter().take(4) {
        println!("  [{:.1}] {}", h.score, one_line(&h.text));
    }

    // ── View 2: flat cosine ranking — a bare list ────────────────────────────
    // Rank the ENTIRE episodic corpus by cosine to the query, independently of the
    // graph — exactly what a flat vector store does. We iterate every episodic node
    // (not the graph-surfaced recall set), so the contrast is honest: the flat list
    // finds the relevant turns, but nothing in it says they conflict or why.
    let q_embedding = query_provider.embed_f64(&[query])?.remove(0);
    let mut ranked: Vec<(f64, NodeId, String)> = Vec::new();
    for id in m.engine().graph().all_node_ids() {
        let node = m.engine().graph().get_node(id)?;
        if !matches!(node.node_type, anamnesis::engine::KnowledgeType::Episodic) {
            continue;
        }
        let sim = node
            .embedding
            .as_ref()
            .map(|e| cosine(&q_embedding, e))
            .unwrap_or(0.0);
        ranked.push((sim, id, one_line(&node.content)));
    }
    // Deterministic order: cosine desc, then node id for ties.
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.0.cmp(&b.1.0)));

    println!("\n=== flat vector ranking (cosine to the query) ===\n");
    println!("a list with no structure — the contradiction and the why-chain are invisible:\n");
    for (sim, _id, text) in ranked.iter().take(5) {
        println!("  {sim:.3}  {text}");
    }
    println!("\n(the flat list never says these two turns *conflict*, nor why either was chosen.)");

    Ok(())
}

/// First line of a node's content, trimmed for display.
fn one_line(content: &str) -> String {
    let line = content.lines().next().unwrap_or("").trim();
    if line.chars().count() > 72 {
        let mut s: String = line.chars().take(69).collect();
        s.push_str("...");
        s
    } else {
        line.to_string()
    }
}
