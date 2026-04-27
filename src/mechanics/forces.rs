//! Force components for relevance scoring.
//!
//! All force implementations are pure: no side effects, no storage access.

/// Per-node scalar inputs used by relevance force components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeContext {
    /// Final activation after spreading and any damping [0, 1].
    pub activation: f64,
    /// Similarity between query and node embeddings [0, 1].
    pub vector_similarity: f64,
    /// Current node salience [0, 1].
    pub salience: f64,
    /// Node mass from gravity mechanics [0, 1].
    pub mass: f64,
    /// Accumulated contradiction repulsion, where higher means stronger conflict.
    pub repulsion: f64,
    /// Identity prior for this node relative to the active agent [0, 1].
    pub identity_prior: f64,
}

impl NodeContext {
    /// Create a context from the four components used by the current final score.
    pub fn scoring(activation: f64, vector_similarity: f64, salience: f64, mass: f64) -> Self {
        Self {
            activation,
            vector_similarity,
            salience,
            mass,
            repulsion: 0.0,
            identity_prior: 0.0,
        }
    }
}

/// Query-level scalar inputs shared across force components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueryContext {
    /// Project/entity scope multiplier applied after component scoring [0, 1].
    pub scope_weight: f64,
}

impl Default for QueryContext {
    fn default() -> Self {
        Self { scope_weight: 1.0 }
    }
}

/// A pure relevance component.
pub trait Force {
    /// Compute this component's contribution input for a node/query pair.
    fn compute(&self, node: &NodeContext, query: &QueryContext) -> f64;

    /// Return this component's positive scoring weight.
    fn weight(&self) -> f64;
}

/// Compute a force's weighted contribution for a node/query pair.
pub fn weighted_contribution<F: Force + ?Sized>(
    force: &F,
    node: &NodeContext,
    query: &QueryContext,
) -> f64 {
    force.weight() * force.compute(node, query)
}

/// Spreading activation component.
#[derive(Debug, Clone, Copy, Default)]
pub struct ActivationForce;

/// Vector similarity component.
#[derive(Debug, Clone, Copy, Default)]
pub struct SimilarityForce;

/// Salience component.
#[derive(Debug, Clone, Copy, Default)]
pub struct SalienceForce;

/// Mass/gravity component.
#[derive(Debug, Clone, Copy, Default)]
pub struct MassForce;

/// Contradiction repulsion component.
#[derive(Debug, Clone, Copy, Default)]
pub struct RepulsionForce;

/// Agent identity prior component.
#[derive(Debug, Clone, Copy, Default)]
pub struct IdentityForce;

impl Force for ActivationForce {
    fn compute(&self, node: &NodeContext, _query: &QueryContext) -> f64 {
        node.activation
    }

    fn weight(&self) -> f64 {
        0.50
    }
}

impl Force for SimilarityForce {
    fn compute(&self, node: &NodeContext, _query: &QueryContext) -> f64 {
        node.vector_similarity
    }

    fn weight(&self) -> f64 {
        0.20
    }
}

impl Force for SalienceForce {
    fn compute(&self, node: &NodeContext, _query: &QueryContext) -> f64 {
        node.salience
    }

    fn weight(&self) -> f64 {
        0.15
    }
}

impl Force for MassForce {
    fn compute(&self, node: &NodeContext, _query: &QueryContext) -> f64 {
        node.mass
    }

    fn weight(&self) -> f64 {
        0.15
    }
}

impl Force for RepulsionForce {
    fn compute(&self, node: &NodeContext, _query: &QueryContext) -> f64 {
        (1.0 - non_negative(node.repulsion)).clamp(0.0, 1.0)
    }

    fn weight(&self) -> f64 {
        0.10
    }
}

impl Force for IdentityForce {
    fn compute(&self, node: &NodeContext, _query: &QueryContext) -> f64 {
        unit_interval(node.identity_prior)
    }

    fn weight(&self) -> f64 {
        0.10
    }
}

fn unit_interval(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn non_negative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}
