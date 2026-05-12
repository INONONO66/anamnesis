//! Display and formatting utilities for search results and graph information.
//!
//! Provides simple text-based formatting for stdout output without ANSI colors or TUI frameworks.

use anamnesis::api::{IngestResult, TickReport};
use anamnesis::graph::{Edge, Node, NodeId};
use anamnesis::query::{ContextPackage, Fragment, SearchResult, Tension};

/// Display a ContextPackage with all its sections.
///
/// Prints identity, knowledge, memories, tensions, and token usage.
pub fn display_context_package(pkg: &ContextPackage) {
    if !pkg.identity.is_empty() {
        println!("\n=== Identity ===");
        for frag in &pkg.identity {
            display_fragment(frag);
        }
    }

    if !pkg.knowledge.is_empty() {
        println!("\n=== Knowledge ===");
        for frag in &pkg.knowledge {
            display_fragment(frag);
        }
    }

    if !pkg.memories.is_empty() {
        println!("\n=== Memories ===");
        for frag in &pkg.memories {
            display_fragment(frag);
        }
    }

    if !pkg.tensions.is_empty() {
        println!("\n=== Tensions ===");
        for tension in &pkg.tensions {
            display_tension(tension);
        }
    }

    println!(
        "\nToken usage: {}/{} (identity: {}, knowledge: {}, memories: {})",
        pkg.token_usage.used,
        pkg.token_usage.total,
        pkg.token_usage.identity_used,
        pkg.token_usage.knowledge_used,
        pkg.token_usage.memories_used
    );
    println!("Agent tension: {:.3}", pkg.agent_tension);
}

/// Display a single fragment with name, relevance, and type.
fn display_fragment(frag: &Fragment) {
    println!(
        "  [{}] {} (relevance: {:.3})",
        frag.node_id.0, frag.name, frag.relevance
    );
    println!("    Type: {:?}", frag.node_type);
    if let Some(summary) = &frag.summary {
        println!("    Summary: {}", summary);
    }
    if let Some(content) = &frag.content {
        let preview = if content.chars().count() > 200 {
            let truncated: String = content.chars().take(200).collect();
            format!("{truncated}...")
        } else {
            content.clone()
        };
        println!("    Content: {}", preview);
    }
    println!(
        "    Origin: {}/{} ({})",
        frag.origin.agent_id,
        frag.origin.session_id,
        frag.origin.scope.as_str()
    );
}

/// Display a single tension between two nodes.
fn display_tension(tension: &Tension) {
    println!("  Node {} <-> Node {}", tension.node_a.0, tension.node_b.0);
    println!("    Weight: {:.3}", tension.edge_weight);
    if let Some(desc) = &tension.description {
        println!("    Description: {}", desc);
    }
}

/// Display a search result with trace information and context package.
pub fn display_search_result(result: &SearchResult) {
    println!("\n=== Search Result ===");
    println!(
        "Strategies used: {}",
        result.trace.strategies_used.join(", ")
    );
    println!("Seeds found: {}", result.trace.seed_count);
    println!("Spread iterations: {}", result.trace.spread_iterations);
    if let Some(model) = &result.trace.spreading_model {
        println!("Spreading model: {:?}", model);
    }
    if let Some(mode) = &result.trace.packaging_mode {
        println!("Packaging mode: {:?}", mode);
    }
    println!(
        "Edges skipped (invalid temporal): {}",
        result.trace.edge_count_skipped_invalid
    );
    println!("Convergence rounds: {}", result.trace.convergence_rounds);
    println!("Converged early: {}", result.trace.converged);

    display_context_package(&result.package);
}

/// Display a single node with all its metadata.
pub fn display_node(node: &Node) {
    println!("\nNode {}: {}", node.id.0, node.name);
    println!("  Type: {:?}", node.node_type);
    println!("  Salience: {:.3}", node.salience);
    println!("  Access count: {}", node.access_count);
    println!("  Tier: {:?}", node.tier);

    let preview = if node.content.chars().count() > 200 {
        let truncated: String = node.content.chars().take(200).collect();
        format!("{truncated}...")
    } else {
        node.content.clone()
    };
    println!("  Content: {}", preview);

    if !node.entity_tags.is_empty() {
        println!("  Entity tags: {}", node.entity_tags.join(", "));
    }

    println!(
        "  Origin: {}/{} ({})",
        node.origin.agent_id,
        node.origin.session_id,
        node.origin.scope.as_str()
    );
    println!(
        "  Created: {}, Accessed: {}",
        node.created_at.0, node.accessed_at.0
    );

    if let Some(summary) = &node.summary {
        println!("  Summary: {}", summary);
    }

    if node.embedding.is_some() {
        println!(
            "  Embedding: present ({} dims)",
            node.embedding.as_ref().unwrap().len()
        );
    }
}

/// Display neighbors of a node with edge information.
///
/// Takes a slice of (NodeId, Node reference, Edge reference) tuples.
pub fn display_neighbors(neighbors: &[(NodeId, &Node, &Edge)]) {
    if neighbors.is_empty() {
        println!("No neighbors.");
        return;
    }

    println!("Neighbors:");
    for (neighbor_id, neighbor_node, edge) in neighbors {
        let direction = if edge.source == *neighbor_id {
            "→"
        } else {
            "←"
        };
        println!(
            "  {} {}: {} [{:?}] weight={:.2}",
            direction, neighbor_id.0, neighbor_node.name, edge.edge_type, edge.weight
        );
    }
}

/// Display graph statistics.
pub fn display_stats(
    node_count: usize,
    edge_count: usize,
    snapshot_count: usize,
    avg_salience: f64,
) {
    println!("\nGraph Statistics:");
    println!("  Nodes: {}", node_count);
    println!("  Edges: {}", edge_count);
    println!("  Snapshots: {}", snapshot_count);
    println!("  Average salience: {:.3}", avg_salience);
}

/// Display an ingest result.
pub fn display_ingest_result(result: &IngestResult) {
    match result {
        IngestResult::Created(ids) => {
            println!("Ingest complete: Created {} new node(s)", ids.len());
            for id in ids {
                println!("  - Node {}", id.0);
            }
        }
        IngestResult::Reinforced {
            existing_id,
            similarity,
        } => {
            println!("Ingest complete: Reinforced existing node");
            println!("  Node: {}", existing_id.0);
            println!("  Similarity: {:.3}", similarity);
        }
    }
}

/// Display a tick report.
pub fn display_tick_report(report: &TickReport) {
    println!("\nTick complete:");
    println!("  Nodes decayed: {}", report.nodes_decayed);
    println!("  Nodes pruned: {}", report.nodes_pruned);
}
