//! Reasoning-advantage integration test.
//!
//! Exercises the product's differentiator end-to-end through the public
//! [`Memory`] front door: typed reasoning edges ([`Relation::Reason`]) plus
//! contradiction-as-tension ([`Relation::Contradicts`]). A ~10-turn conversation
//! records a database decision, its rationale, then a reversal that contradicts
//! the decision. A single "why did we switch?" query must surface the
//! contradiction **pair** as a structured tension and expose the causal chain via
//! typed neighbors — structure a flat cosine list cannot express.
//!
//! Hermetic: a byte-derived deterministic embedder (no model download, no
//! network), the same pattern the other `Memory` integration tests use
//! (`tests/readout_behavior.rs`).

use std::sync::Arc;

use anamnesis::Error;
use anamnesis::engine::{EdgeType, EmbeddingProvider, NodeId, Timestamp};
use anamnesis::memory::{Direction, Memory, Relation};

// ---------------------------------------------------------------------------
// Deterministic, model-free embedder (mirrors tests/readout_behavior.rs).
//
// Embeds text as a small vector derived from character bytes so identical texts
// always produce identical vectors and distinct texts produce distinct ones.
// No network / model download.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct HashEmbedder;

fn embed_text(text: &str) -> Vec<f32> {
    let bytes = text.as_bytes();
    let a = bytes.iter().step_by(1).map(|&b| b as f32).sum::<f32>();
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
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        Ok(texts.iter().map(|t| embed_text(t)).collect())
    }
    fn dimensions(&self) -> usize {
        4
    }
    fn model_name(&self) -> &str {
        "hash-stub"
    }
}

fn memory() -> Memory {
    let provider: Arc<dyn EmbeddingProvider> = Arc::new(HashEmbedder);
    Memory::in_memory_with_provider(provider).expect("in-memory Memory")
}

/// Node ids captured while building the demo conversation.
struct Scenario {
    decision: NodeId,
    decision_rationale: NodeId,
    reversal: NodeId,
    reversal_rationale: NodeId,
}

/// Build the ~10-turn "database choice, then reversal" conversation and wire the
/// typed reasoning + contradiction edges through the front door. Returns the key
/// node ids.
///
/// `wire_edges` gates the `relate` calls so the test can observe the RED state
/// (no tension, no typed chain) before the reasoning edges exist.
fn build_scenario(m: &mut Memory, wire_edges: bool) -> Scenario {
    let s = "demo";
    let mut ts = 1_000u64;
    let mut add = |m: &mut Memory, who: &str, text: &str| {
        let id = m.add(s, who, text, Timestamp(ts)).unwrap().episodic;
        ts += 60;
        id
    };

    // 1-3: context — the team is choosing a database.
    add(m, "user", "We need to pick a database for the new service.");
    add(
        m,
        "assistant",
        "The main contenders are Postgres and SQLite.",
    );
    add(
        m,
        "user",
        "It has to handle JSONB documents and per-tenant row security.",
    );

    // 4: the decision + 5: its rationale.
    let decision = add(m, "assistant", "Decision: we go with Postgres.");
    let decision_rationale = add(
        m,
        "assistant",
        "Postgres because we need JSONB and row-level security.",
    );

    // 6: work proceeds.
    add(
        m,
        "user",
        "Great, I'll set up the Postgres schema and migrations.",
    );

    // 7: the reversal + 8: its rationale.
    let reversal = add(
        m,
        "assistant",
        "We are reverting to SQLite — the ops overhead is too high for a single-node deploy.",
    );
    let reversal_rationale = add(
        m,
        "assistant",
        "SQLite keeps the single-node deploy simple with no separate database server to run.",
    );

    // 9-10: filler.
    add(m, "user", "Okay, I'll rewrite the migrations for SQLite.");
    add(
        m,
        "assistant",
        "I'll drop the row-level security policies and use application checks.",
    );

    m.flush_all().unwrap();

    if wire_edges {
        // Typed reasoning-chain edges — the front-door `relate` path.
        m.relate(decision, decision_rationale, Relation::Reason)
            .unwrap();
        m.relate(reversal, reversal_rationale, Relation::Reason)
            .unwrap();
        // The reversal contradicts the original decision.
        m.relate(reversal, decision, Relation::Contradicts).unwrap();
    }

    Scenario {
        decision,
        decision_rationale,
        reversal,
        reversal_rationale,
    }
}

/// RED baseline: with no reasoning edges wired, the identical query surfaces NO
/// contradiction tension. This is the "flat store" behaviour the demo improves
/// on — and it proves the tension in the GREEN test comes from the authored
/// edges, not from the text/embeddings alone.
#[test]
fn without_edges_no_tension_surfaces() {
    let mut m = memory();
    let _sc = build_scenario(&mut m, false);

    let recall = m
        .search_at("why did we switch databases?", 10, Timestamp(2_000))
        .unwrap();

    assert!(
        recall.package.tensions.is_empty(),
        "with no Contradicts edge there must be no tension; got {:?}",
        recall.package.tensions
    );
    assert!(
        !recall.as_context().contains("## TENSIONS"),
        "flat recall must not render a TENSIONS block:\n{}",
        recall.as_context()
    );
}

/// Assertion 1: the recall surfaces the contradiction pair (decision ↔ reversal)
/// as a structured tension, and the rendered context includes a TENSIONS block.
#[test]
fn why_query_surfaces_contradiction_tension() {
    let mut m = memory();
    let sc = build_scenario(&mut m, true);

    // Query at a domain time when both facts are valid together. A natural, modest
    // `limit`: tension endpoints are exempt from result-limit trimming, so the
    // contradiction pair reaches the caller even if an endpoint ranks below the cut
    // (the dedicated `small_limit_retains_tension_and_both_endpoints` test pins that
    // exemption at an even tighter limit).
    let recall = m
        .search_at("why did we switch databases?", 10, Timestamp(2_000))
        .unwrap();

    // Structured accessor (preferred): the Contradicts pair must be present as a
    // tension carrying positive query-local stress; neither endpoint suppressed.
    let tension = recall
        .package
        .tensions
        .iter()
        .find(|t| {
            (t.node_a == sc.decision && t.node_b == sc.reversal)
                || (t.node_a == sc.reversal && t.node_b == sc.decision)
        })
        .unwrap_or_else(|| {
            panic!(
                "expected a tension between decision {:?} and reversal {:?}; got tensions {:?}",
                sc.decision, sc.reversal, recall.package.tensions
            )
        });
    assert!(
        tension.stress > 0.0,
        "contradiction stress must be positive, got {}",
        tension.stress
    );
    assert_eq!(
        tension.evidence_sources.len(),
        2,
        "tension must name both endpoints as evidence"
    );
    assert!(
        tension.evidence_sources.contains(&sc.decision)
            && tension.evidence_sources.contains(&sc.reversal),
        "evidence sources must name both endpoints, got {:?}",
        tension.evidence_sources
    );

    // Both contradicting turns survive as fragments in the assembled package
    // (surfaced, never suppressed — ADR-0006). This is the LLM-facing result;
    // tension endpoints are exempt from result-limit trimming, so both are retained
    // even though the lower-ranked one falls outside the top-`limit` readout `hits`.
    let fragment_ids: Vec<NodeId> = recall
        .package
        .identity
        .iter()
        .chain(recall.package.knowledge.iter())
        .chain(recall.package.memories.iter())
        .map(|f| f.node_id)
        .collect();
    assert!(
        fragment_ids.contains(&sc.decision) && fragment_ids.contains(&sc.reversal),
        "both contradicting turns must survive as package fragments; got {fragment_ids:?}"
    );

    // The human-readable context must render a TENSIONS section referencing both
    // contradicting node ids (as_context renders `#A ⟂ #B`).
    let context = recall.as_context();
    assert!(
        context.contains("## TENSIONS"),
        "as_context must render a TENSIONS block:\n{context}"
    );
    assert!(
        context.contains(&format!("#{}", sc.decision.0))
            && context.contains(&format!("#{}", sc.reversal.0)),
        "TENSIONS block must reference both contradicting node ids:\n{context}"
    );
}

/// A surfaced tension must survive result-limit trimming even when one of its
/// endpoints would fall outside the top-`limit` fragments: tension endpoints are
/// exempt from trimming (assemble.rs `apply_result_limit`). With a SMALL limit
/// (5) the contradiction pair would previously be dropped whenever either endpoint
/// ranked below the cut; after the exemption the tension and BOTH its endpoint
/// fragments are retained.
#[test]
fn small_limit_retains_tension_and_both_endpoints() {
    let mut m = memory();
    let sc = build_scenario(&mut m, true);

    // A limit well below the recalled-set size so trimming is forced to run.
    let recall = m
        .search_at("why did we switch databases?", 5, Timestamp(2_000))
        .unwrap();

    // The contradiction pair must still be present as a tension.
    let tension_present = recall.package.tensions.iter().any(|t| {
        (t.node_a == sc.decision && t.node_b == sc.reversal)
            || (t.node_a == sc.reversal && t.node_b == sc.decision)
    });
    assert!(
        tension_present,
        "tension must survive a small limit; got tensions {:?}",
        recall.package.tensions
    );

    // Both endpoint fragments must be retained across the buckets (the exemption
    // unions their ids into the allowed set before the retains).
    let fragment_ids: Vec<NodeId> = recall
        .package
        .identity
        .iter()
        .chain(recall.package.knowledge.iter())
        .chain(recall.package.memories.iter())
        .map(|f| f.node_id)
        .collect();
    assert!(
        fragment_ids.contains(&sc.decision) && fragment_ids.contains(&sc.reversal),
        "both tension endpoints must be retained as fragments; got {fragment_ids:?}"
    );

    // The exemption is minimal: the package may exceed `limit` only by the few
    // exempted tension endpoints, never by the whole recalled set. Two endpoints
    // here, so the package is at most limit + 2.
    assert!(
        recall.package.total_fragments() <= 5 + 2,
        "non-tension trimming must still hold; package size {} exceeds limit + 2 endpoints",
        recall.package.total_fragments()
    );
}

/// Assertion 2: the causal chain is traceable via typed neighbors — the reversal
/// node exposes a `Contradicts` edge to the decision and a `Reason` edge to its
/// own rationale.
#[test]
fn reversal_neighbors_expose_typed_reasoning_chain() {
    let mut m = memory();
    let sc = build_scenario(&mut m, true);

    let neighbors = m.neighbors(sc.reversal).unwrap();

    // Contradicts edge → decision (authored reversal -> decision, so outgoing).
    let contradicts = neighbors
        .iter()
        .find(|n| n.edge_type == EdgeType::Contradicts)
        .expect("reversal must have a Contradicts neighbor");
    assert_eq!(
        contradicts.node, sc.decision,
        "Contradicts edge must point at the original decision"
    );
    assert_eq!(contradicts.direction, Direction::Outgoing);

    // Reason edge → the reversal's own rationale (authored reversal -> rationale).
    let reason = neighbors
        .iter()
        .find(|n| n.edge_type == EdgeType::Reason && n.node == sc.reversal_rationale)
        .expect("reversal must have a Reason edge to its rationale");
    assert_eq!(reason.direction, Direction::Outgoing);
}

/// Assertion 3: the decision node likewise exposes its rationale via a `Reason`
/// edge, so the whole why-chain is walkable from either endpoint.
#[test]
fn decision_neighbors_expose_reason_edge() {
    let mut m = memory();
    let sc = build_scenario(&mut m, true);

    let neighbors = m.neighbors(sc.decision).unwrap();
    let reason = neighbors
        .iter()
        .find(|n| n.edge_type == EdgeType::Reason && n.node == sc.decision_rationale)
        .expect("decision must have a Reason edge to its rationale");
    assert_eq!(reason.direction, Direction::Outgoing);
}
