# Identity-Memory Integration Guide for Anamnesis

**Status**: Synthesis of 8 cognitive science models into actionable Rust implementation  
**Target**: Extend Anamnesis Engine to support personality/identity-aware memory dynamics  
**Scope**: Mechanics, query, and API layers

---

## 1. CORE ADDITIONS TO `graph/node.rs`

### 1.1 Extend Node Type with Identity & Dissonance Tracking

```rust
// In src/graph/node.rs

#[derive(Clone, Debug)]
pub struct Node {
    pub id: NodeId,
    pub content: String,
    pub embedding: Vec<f32>,
    pub salience: f64,
    pub created_at: u64,
    pub last_accessed: u64,
    pub knowledge_type: KnowledgeType,
    
    // NEW: Identity & personality tracking
    pub origin: Option<Origin>,                    // Which agent created this
    pub personality_congruence: f64,               // How aligned with agent's identity
    pub dissonance_score: f64,                     // Conflict with existing beliefs
    pub memory_tier: MemoryTier,                   // L0/L1/L2 (identity/learned/state)
    pub emotional_valence: f64,                    // [-1, 1] negative to positive
    pub consolidation_strength: f64,               // How strongly consolidated
}

#[derive(Clone, Debug, PartialEq)]
pub enum KnowledgeType {
    Episodic,        // Raw conversation turn
    Semantic,        // Extracted fact
    Procedural,      // How-to / execution pattern
    Entity,          // Named concept
    Event,           // Time-bound occurrence
    IdentityCore,    // L0: Immutable agent trait (no decay)
    IdentityLearned, // L1: Experience-formed trait (slow decay)
    IdentityState,   // L2: Current state (normal decay)
    Custom(String),
}

#[derive(Clone, Debug)]
pub struct Origin {
    pub agent_id: String,
    pub session_id: String,
    pub confidence: f64,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum MemoryTier {
    Core,              // Salience > 0.8
    Working,           // 0.4 – 0.8
    AccumulatedWisdom, // 0.1 – 0.4
    Archive,           // < 0.1
}

impl Node {
    pub fn memory_tier_from_salience(salience: f64) -> MemoryTier {
        match salience {
            s if s > 0.8 => MemoryTier::Core,
            s if s > 0.4 => MemoryTier::Working,
            s if s > 0.1 => MemoryTier::AccumulatedWisdom,
            _ => MemoryTier::Archive,
        }
    }
}
```

### 1.2 Extend EdgeType with Reasoning & Contradiction Edges

```rust
// In src/graph/edge.rs

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EdgeType {
    // Existing
    Semantic,
    Causal,
    Temporal,
    Custom(String),
    
    // NEW: Reasoning preservation
    Reason,               // Why a decision was made
    RejectedAlternative,  // Option considered and discarded
    Supersedes,           // Replaces outdated knowledge
    ReinforcedBy,         // Confirmed by repeated experience
    ConsolidatedFrom,     // Derived from multiple fragments
    
    // NEW: Cross-agent & structural
    Entity,               // Cross-agent shared entity link
    ExtractedFrom,        // Derived knowledge → source episode
    Contradicts,          // Conflicting assertions (repulsion in activation)
}
```

---

## 2. NEW MODULE: `mechanics/dissonance.rs`

### 2.1 Dissonance Detection & Scoring

```rust
// src/mechanics/dissonance.rs

use crate::graph::{Graph, NodeId, Node};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct DissonanceMetric {
    pub score: f64,
    pub conflicting_nodes: Vec<NodeId>,
    pub resolution_cost: f64,
    pub classification: DissonanceClass,
}

#[derive(Clone, Debug, Copy, PartialEq)]
pub enum DissonanceClass {
    Novel,      // < 0.2: new, non-conflicting
    Familiar,   // < 0.1 AND similarity > 0.9: already known
    Dissonant,  // > 0.5: contradicts important beliefs
}

/// Compute dissonance score for a new observation against existing beliefs
pub fn compute_dissonance(
    new_embedding: &[f32],
    new_content: &str,
    graph: &Graph,
    threshold: f64,
) -> Result<DissonanceMetric, String> {
    let mut total_conflict = 0.0;
    let mut conflicting_nodes = Vec::new();
    
    // Find semantically similar nodes
    let similar_nodes = graph.find_similar_nodes(new_embedding, 0.7)?;
    
    for node_id in similar_nodes {
        let node = graph.get_node(node_id)?;
        
        // Compute cosine similarity
        let similarity = cosine_similarity(new_embedding, &node.embedding);
        
        // Check if content contradicts (simple heuristic: opposite polarity)
        let contradicts = detect_contradiction(new_content, &node.content);
        
        if similarity > 0.8 && contradicts {
            // Conflict magnitude: how different despite similarity
            let conflict = 1.0 - similarity;
            
            // Importance: salience × centrality
            let importance = node.salience * graph.centrality(node_id).unwrap_or(0.0);
            
            total_conflict += conflict * importance;
            conflicting_nodes.push(node_id);
        }
    }
    
    let classification = match total_conflict {
        s if s < 0.1 => DissonanceClass::Familiar,
        s if s < 0.2 => DissonanceClass::Novel,
        _ => DissonanceClass::Dissonant,
    };
    
    Ok(DissonanceMetric {
        score: total_conflict,
        conflicting_nodes,
        resolution_cost: total_conflict * 0.3,
        classification,
    })
}

/// Simple contradiction detection (can be enhanced with LLM)
fn detect_contradiction(text_a: &str, text_b: &str) -> bool {
    let negation_words = ["not", "no", "never", "false", "wrong", "incorrect"];
    
    let a_negated = negation_words.iter().any(|w| text_a.contains(w));
    let b_negated = negation_words.iter().any(|w| text_b.contains(w));
    
    // Contradiction if one is negated and the other isn't
    a_negated != b_negated
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    
    let dot_product: f64 = a.iter()
        .zip(b.iter())
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum();
    
    let norm_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    
    dot_product / (norm_a * norm_b)
}
```

---

## 3. NEW MODULE: `mechanics/identity_bias.rs`

### 3.1 Self-Reference Effect & Identity Priors

```rust
// src/mechanics/identity_bias.rs

use crate::graph::{Graph, NodeId, Node};

/// Compute self-reference bonus for identity-aligned knowledge
pub fn compute_self_reference_bonus(
    observation_embedding: &[f32],
    identity_node_id: NodeId,
    graph: &Graph,
) -> Result<f64, String> {
    let identity_node = graph.get_node(identity_node_id)?;
    
    let similarity = cosine_similarity(observation_embedding, &identity_node.embedding);
    
    // Empirically tuned: α ≈ 0.4
    let alpha = 0.4;
    
    Ok(alpha * similarity.max(0.0))
}

/// Compute personality congruence (how aligned with agent's traits)
pub fn compute_personality_congruence(
    node: &Node,
    agent_personality_traits: &[f32],
) -> f64 {
    if node.embedding.is_empty() || agent_personality_traits.is_empty() {
        return 0.0;
    }
    
    cosine_similarity(&node.embedding, agent_personality_traits)
}

/// Bayesian belief update with identity prior
pub fn bayesian_belief_update(
    current_salience: f64,
    new_evidence_strength: f64,
    identity_prior: f64,
) -> f64 {
    // P(belief | identity) = prior
    let prior = identity_prior;
    
    // P(evidence | belief) = likelihood
    let likelihood = new_evidence_strength;
    
    // Posterior = (likelihood × prior) / (likelihood × prior + (1 - likelihood) × (1 - prior))
    let posterior = (likelihood * prior) / 
                    (likelihood * prior + (1.0 - likelihood) * (1.0 - prior));
    
    // Update salience with posterior
    current_salience * posterior
}

/// Consolidation strength based on personality congruence
pub fn consolidation_strength(
    base_consolidation: f64,
    personality_congruence: f64,
) -> f64 {
    base_consolidation * (1.0 + personality_congruence)
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    
    let dot_product: f64 = a.iter()
        .zip(b.iter())
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum();
    
    let norm_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    
    dot_product / (norm_a * norm_b)
}
```

---

## 4. EXTEND `mechanics/forgetting.rs`

### 4.1 Complementary Learning Systems (CLS) Decay

```rust
// In src/mechanics/forgetting.rs - ADD to existing module

/// Memory tier determines decay rate (CLS model)
pub fn decay_by_tier(
    salience: f64,
    elapsed_time: u64,
    memory_tier: MemoryTier,
) -> f64 {
    let (lambda, consolidation_factor) = match memory_tier {
        MemoryTier::Core => (0.001, 0.95),              // Slow decay, high consolidation
        MemoryTier::Working => (0.01, 0.85),            // Medium decay
        MemoryTier::AccumulatedWisdom => (0.05, 0.70),  // Faster decay
        MemoryTier::Archive => (0.1, 0.50),             // Rapid decay
    };
    
    // Exponential decay: S(t) = S₀ × exp(-λ × t)
    let decayed = salience * (-lambda as f64 * elapsed_time as f64).exp();
    
    // Consolidation factor: nodes in higher tiers resist decay
    decayed * consolidation_factor
}

/// Reinforcement on access (touch)
pub fn reinforcement_on_access(
    current_salience: f64,
    access_count: u32,
    recency_weight: f64,
) -> f64 {
    let base_reinforcement = 0.1;
    let access_bonus = (access_count as f64).log2() * 0.05;
    
    (current_salience + base_reinforcement + access_bonus) * recency_weight
}
```

---

## 5. EXTEND `query/spreading_activation.rs`

### 5.1 Identity-Constrained Retrieval

```rust
// In src/query/spreading_activation.rs - ADD new function

use crate::graph::{Graph, NodeId};
use std::collections::HashMap;

/// Query with identity as retrieval anchor
pub fn query_identity_constrained(
    graph: &Graph,
    identity_node_id: NodeId,
    budget: usize,
    hops: usize,
) -> Result<Vec<NodeId>, String> {
    let mut activation: HashMap<NodeId, f64> = HashMap::new();
    activation.insert(identity_node_id, 1.0);
    
    let decay_factor = 0.8;
    
    // Spread activation through hops
    for _ in 0..hops {
        let mut new_activation = activation.clone();
        
        for (node_id, act) in &activation {
            // Get neighbors from graph
            let neighbors = graph.get_neighbors(*node_id)?;
            
            for neighbor_id in neighbors {
                let edge_weight = graph.get_edge_weight(*node_id, neighbor_id)?;
                let new_act = act * edge_weight * decay_factor;
                
                *new_activation.entry(neighbor_id).or_insert(0.0) += new_act;
            }
        }
        
        activation = new_activation;
    }
    
    // Extract top-k by activation
    let mut sorted: Vec<_> = activation.into_iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    
    Ok(sorted.into_iter()
        .take(budget)
        .map(|(id, _)| id)
        .collect())
}

/// Personality-biased retrieval ranking
pub fn rerank_by_personality(
    nodes: Vec<NodeId>,
    graph: &Graph,
    personality_traits: &[f32],
) -> Result<Vec<NodeId>, String> {
    let mut ranked: Vec<_> = nodes.into_iter()
        .map(|id| {
            let node = graph.get_node(id).ok()?;
            let congruence = cosine_similarity(&node.embedding, personality_traits);
            Some((id, congruence))
        })
        .collect::<Option<Vec<_>>>()?;
    
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    
    Ok(ranked.into_iter().map(|(id, _)| id).collect())
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    
    let dot_product: f64 = a.iter()
        .zip(b.iter())
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum();
    
    let norm_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    
    dot_product / (norm_a * norm_b)
}
```

---

## 6. EXTEND `api/engine.rs`

### 6.1 Integrate Dissonance-Aware Ingestion

```rust
// In src/api/engine.rs - MODIFY ingest() method

pub fn ingest_with_dissonance_awareness(
    &mut self,
    observation: Observation,
) -> Result<Vec<NodeId>, String> {
    // Step 1: Compute dissonance
    let dissonance = mechanics::dissonance::compute_dissonance(
        &observation.embedding,
        &observation.content,
        &self.graph,
        0.5,  // threshold
    )?;
    
    // Step 2: Handle based on classification
    match dissonance.classification {
        DissonanceClass::Familiar => {
            // Already known: reinforce existing node instead
            if let Some(existing_id) = self.find_exact_match(&observation)? {
                self.touch(existing_id)?;
                return Ok(vec![existing_id]);
            }
        }
        DissonanceClass::Dissonant => {
            // High conflict: reduce salience of conflicting beliefs
            for conflicting_id in &dissonance.conflicting_nodes {
                let node = self.graph.get_node_mut(*conflicting_id)?;
                node.salience *= 0.6;  // Reduce by 40%
                
                // Mark as superseded
                node.dissonance_score = dissonance.score;
            }
            
            // Create Contradicts edges
            let new_node_id = self.create_node(&observation)?;
            for conflicting_id in &dissonance.conflicting_nodes {
                self.graph.add_edge(
                    new_node_id,
                    *conflicting_id,
                    EdgeType::Contradicts,
                    dissonance.score,
                )?;
            }
            
            return Ok(vec![new_node_id]);
        }
        DissonanceClass::Novel => {
            // New knowledge: proceed normally
        }
    }
    
    // Step 3: Standard ingestion with identity bias
    let mut node = self.create_node(&observation)?;
    
    // Apply self-reference bonus if identity node exists
    if let Some(identity_id) = self.get_identity_node()? {
        let bonus = mechanics::identity_bias::compute_self_reference_bonus(
            &observation.embedding,
            identity_id,
            &self.graph,
        )?;
        node.salience *= (1.0 + bonus);
    }
    
    Ok(vec![node.id])
}

/// Multi-agent entity linking (reflect_batch)
pub fn reflect_batch(
    &mut self,
    sessions: &[SessionSummary],
) -> Result<ReflectReport, String> {
    let mut entity_links_created = 0;
    
    for i in 0..sessions.len() {
        for j in (i + 1)..sessions.len() {
            let session_i = &sessions[i];
            let session_j = &sessions[j];
            
            // Find shared entities (metadata matching, no LLM)
            let shared = self.find_shared_entities(
                &session_i.node_ids,
                &session_j.node_ids,
            )?;
            
            // Create Entity edges
            for (node_i, node_j) in shared {
                self.graph.add_edge(
                    node_i,
                    node_j,
                    EdgeType::Entity,
                    1.0,
                )?;
                entity_links_created += 1;
            }
        }
    }
    
    Ok(ReflectReport {
        entity_links_created,
        sessions_processed: sessions.len(),
    })
}
```

### 6.2 Identity-Aware Query

```rust
// In src/api/engine.rs - ADD new method

pub fn query_with_identity(
    &self,
    identity_node_id: NodeId,
    budget: usize,
) -> Result<Vec<NodeId>, String> {
    // Start spreading activation from identity node
    query::spreading_activation::query_identity_constrained(
        &self.graph,
        identity_node_id,
        budget,
        3,  // hops
    )
}

pub fn query_personality_biased(
    &self,
    seed_node_id: NodeId,
    personality_traits: &[f32],
    budget: usize,
) -> Result<Vec<NodeId>, String> {
    // Standard query
    let results = self.query(seed_node_id, budget)?;
    
    // Re-rank by personality congruence
    query::spreading_activation::rerank_by_personality(
        results,
        &self.graph,
        personality_traits,
    )
}
```

---

## 7. NEW TYPES: `api/types.rs`

### 7.1 Multi-Agent & Personality Types

```rust
// In src/api/types.rs - ADD

#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub agent_id: String,
    pub session_id: String,
    pub node_ids: Vec<NodeId>,
}

#[derive(Clone, Debug)]
pub struct ReflectReport {
    pub entity_links_created: usize,
    pub sessions_processed: usize,
}

#[derive(Clone, Debug)]
pub struct PersonalityProfile {
    pub traits: Vec<f32>,  // Embedding of personality traits
    pub agent_id: String,
}

#[derive(Clone, Debug)]
pub struct AgentIdentity {
    pub agent_id: String,
    pub core_traits: Vec<f32>,      // L0: immutable
    pub learned_traits: Vec<f32>,   // L1: slow decay
    pub current_state: Vec<f32>,    // L2: normal decay
}
```

---

## 8. TESTS: `tests/identity_memory_integration.rs`

```rust
#[cfg(test)]
mod tests {
    use anamnesis::*;
    
    #[test]
    fn test_dissonance_detection() {
        let graph = Graph::new();
        
        // Create conflicting observations
        let obs1 = Observation {
            content: "auth uses factory pattern".into(),
            embedding: vec![0.8, 0.2, 0.1],
            confidence: 0.9,
            node_type: "semantic".into(),
        };
        
        let obs2 = Observation {
            content: "auth does not use factory pattern".into(),
            embedding: vec![0.75, 0.25, 0.15],
            confidence: 0.85,
            node_type: "semantic".into(),
        };
        
        // Compute dissonance
        let dissonance = mechanics::dissonance::compute_dissonance(
            &obs2.embedding,
            &obs2.content,
            &graph,
            0.5,
        ).unwrap();
        
        assert!(dissonance.score > 0.5);
        assert_eq!(dissonance.classification, DissonanceClass::Dissonant);
    }
    
    #[test]
    fn test_self_reference_bonus() {
        let bonus = mechanics::identity_bias::compute_self_reference_bonus(
            &vec![0.9, 0.1, 0.0],  // Similar to identity
            identity_node_id,
            &graph,
        ).unwrap();
        
        assert!(bonus > 0.3);  // Should be significant
    }
    
    #[test]
    fn test_bayesian_update() {
        let updated = mechanics::identity_bias::bayesian_belief_update(
            0.5,   // current salience
            0.8,   // evidence strength
            0.7,   // identity prior (strong)
        );
        
        assert!(updated > 0.5);  // Should increase
    }
    
    #[test]
    fn test_identity_constrained_query() {
        let mut engine = Engine::new();
        
        // Create identity node
        let identity_obs = Observation {
            content: "I am a careful engineer".into(),
            embedding: vec![0.9, 0.1, 0.0],
            confidence: 1.0,
            node_type: "identity_core".into(),
        };
        let identity_id = engine.ingest(identity_obs).unwrap()[0];
        
        // Query from identity
        let results = engine.query_with_identity(identity_id, 10).unwrap();
        
        assert!(!results.is_empty());
    }
}
```

---

## 9. INTEGRATION CHECKLIST

- [ ] Add `Origin`, `MemoryTier`, `DissonanceClass` types to `graph/node.rs`
- [ ] Extend `EdgeType` with reasoning & contradiction edges
- [ ] Create `mechanics/dissonance.rs` module
- [ ] Create `mechanics/identity_bias.rs` module
- [ ] Extend `mechanics/forgetting.rs` with CLS decay
- [ ] Extend `query/spreading_activation.rs` with identity-constrained retrieval
- [ ] Modify `api/engine.rs::ingest()` to use dissonance awareness
- [ ] Add `api/engine.rs::reflect_batch()` for multi-agent linking
- [ ] Add `api/engine.rs::query_with_identity()` and `query_personality_biased()`
- [ ] Add types to `api/types.rs`
- [ ] Write integration tests
- [ ] Update `README.md` with identity-memory examples
- [ ] Benchmark dissonance detection & identity-constrained queries

---

## 10. USAGE EXAMPLE

```rust
use anamnesis::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut engine = Engine::new();
    
    // Create agent identity (L0: core traits)
    let identity_obs = Observation {
        content: "I am a careful, methodical engineer who values correctness".into(),
        embedding: vec![0.9, 0.1, 0.0, 0.2],
        confidence: 1.0,
        node_type: "identity_core".into(),
    };
    let identity_id = engine.ingest(identity_obs)?[0];
    
    // Ingest knowledge aligned with identity
    let aligned_obs = Observation {
        content: "Always write tests before implementation".into(),
        embedding: vec![0.85, 0.15, 0.05, 0.25],
        confidence: 0.95,
        node_type: "semantic".into(),
    };
    let aligned_id = engine.ingest_with_dissonance_awareness(aligned_obs)?[0];
    
    // Ingest contradictory knowledge
    let contradictory_obs = Observation {
        content: "Skip tests to move faster".into(),
        embedding: vec![0.2, 0.8, 0.9, 0.1],
        confidence: 0.6,
        node_type: "semantic".into(),
    };
    let contradictory_id = engine.ingest_with_dissonance_awareness(contradictory_obs)?[0];
    
    // Query from identity (retrieves aligned knowledge preferentially)
    let identity_query = engine.query_with_identity(identity_id, 10)?;
    println!("Identity-constrained retrieval: {:?}", identity_query);
    
    // Personality-biased retrieval
    let personality = vec![0.9, 0.1, 0.0, 0.2];  // Careful, methodical
    let personality_results = engine.query_personality_biased(aligned_id, &personality, 10)?;
    println!("Personality-biased results: {:?}", personality_results);
    
    Ok(())
}
```

---

## 11. REFERENCES

- **Dissonance Detection**: Clemente et al. (2026), "In Praise of Stubbornness"
- **Self-Reference Effect**: Buksowicz et al. (2024), "The Self-Reference Effect"
- **AGM Belief Revision**: Bonanno (2026), "The logic of KM belief update"
- **Bayesian Updating**: Stanford Encyclopedia of Philosophy (2026), "Epistemic Logic"
- **CLS Model**: Lerma-Torres (2026), "Human-Like Lifelong Memory"
- **Self-Memory System**: Conway (2005), "The Self-Memory System"
- **Multi-Agent Epistemic Logic**: Aldini & Fusco (2026), "Bridging concurrency theory"

