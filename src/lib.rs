//! Anamnesis — Cognitive graph engine for LLMs
//!
//! A Rust library providing a cognitive graph engine for LLM-based agents.
//! Models knowledge as a graph with physics-like properties:
//! - Attraction: Similar/related nodes cluster together
//! - Gravity: Important nodes attract new knowledge
//! - Perception: Input gating filters what enters the graph
//! - Forgetting: Time-based salience decay with reinforcement

pub mod api;
pub mod graph;
pub mod mechanics;
pub mod query;
pub mod storage;

pub use api::Engine;
pub use graph::{Edge, Graph, Node};
pub use storage::StorageAdapter;
